//! The ROM library: the ROMs `--rom-dir` offers the frontend (M8-03, #157).
//!
//! A scan and two and a half endpoints. [`Library::scan`] runs once at startup and
//! again on `POST /api/roms/rescan`, and it is the only code in this binary that
//! walks a directory. A request never names a file: it names an [`Entry::id`], the
//! SHA-1 of a path the scan wrote, and [`Library::lookup`] is the only way to get
//! from one to bytes. That is the whole of the security argument, and it is why no
//! handler here takes a path, a name, or an extension from the client.
//!
//! The listing rules are the owner's collection: about 2,775 files in one
//! directory, the ROMs named `*.md` (binary, despite the extension) with some
//! `*.bin`, and everything else - screenshots, `.txt` dumps, cheat lists - is not a
//! ROM. So: four extensions, nothing over 10 MiB, subdirectories included.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use axum::body::Body;
use axum::extract::{Path as PathParam, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::{AppState, not_found};

/// The extensions offered, matched case-insensitively: a collection that is `.md`
/// to one tool is `.MD` to another, and neither is a different ROM.
const EXTENSIONS: [&str; 4] = ["md", "bin", "gen", "smd"];

/// The largest file offered, 10 MiB. A Genesis ROM is 64 KiB to 8 MiB - the last
/// licensed cartridge shipped at 512 KiB - so anything past this is not a
/// cartridge whatever its name says, and listing a whole disk of them over a
/// shared link is not what a library is for.
const MAX_SIZE: u64 = 10 * 1024 * 1024;

/// What a ROM download says it is. `application/octet-stream`, because a `.md`
/// cartridge is binary and `text/markdown` would be a lie the browser would
/// believe.
const OCTET_STREAM: &str = "application/octet-stream";

/// The cache rule for ROM bytes: `private` because the library belongs to whoever
/// runs this server, and an hour because the bytes under an id do not change - a
/// changed file is a changed id, or nothing.
const ROM_CACHE: &str = "private, max-age=3600";

/// One ROM, as the frontend sees it.
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    /// The SHA-1 of `file`, lowercase hex: the only handle a request gets.
    pub id: String,
    /// The file name without its extension, for display: `Sonic The Hedgehog
    /// (USA, Europe)` for `Sonic The Hedgehog (USA, Europe).md`.
    pub name: String,
    /// The path relative to the rom dir, with `/` separators. Informational - the
    /// id is what a request sends - and never accepted from one.
    pub file: String,
    /// The size in bytes, so the UI can say how big the download is before
    /// starting it.
    pub size: u64,
}

/// What `--rom-dir` held when it was scanned.
#[derive(Debug)]
pub struct Library {
    /// The canonical root, so that a link found inside it can be judged by
    /// whether what it resolves to is underneath.
    root: PathBuf,
    /// The listing, in the order it is served: by name, case-insensitively.
    entries: Vec<Entry>,
    /// `id` -> index into `entries`. A `usize` rather than an `Entry` so there is
    /// one copy of each listing, and the lookup is one hash either way.
    by_id: HashMap<String, usize>,
}

impl Library {
    /// Walk `root` and list every ROM in it.
    ///
    /// Called once at startup and again by `POST /api/roms/rescan`; a few thousand
    /// `stat`s is well under a second, so there is no incremental version of this.
    /// A root that cannot be read is an error, and a file the walk cannot look at
    /// is too: a library that is quietly short is what `cli::check_rom_dir` exists
    /// to prevent. Only a link that goes nowhere, or out of the root, is passed
    /// over, because those are ordinary states of a directory someone else
    /// organized.
    pub fn scan(root: impl AsRef<Path>) -> io::Result<Self> {
        // Canonical first. `--rom-dir` may be a link to a mount, which
        // `cli::check_rom_dir` allows, and `starts_with` below only means anything
        // against a root that has no links left in it.
        let root = root.as_ref().canonicalize()?;
        let mut entries = Vec::new();
        // An explicit stack: the depth is whatever the collection's subdirectories
        // are, and this is a loop over files rather than a reason to recurse.
        let mut directories = vec![root.clone()];
        while let Some(directory) = directories.pop() {
            for child in fs::read_dir(directory)? {
                let child = child?;
                let file_type = child.file_type()?;
                let path = child.path();
                if file_type.is_dir() {
                    // `file_type` does not resolve links, so this is a real
                    // subdirectory only: a link to a directory is not followed - one
                    // that pointed above the root would make this never finish - and
                    // a link to a file is offered or refused by `offer` below.
                    directories.push(path);
                    continue;
                }
                if let Some(entry) = offer(&root, &path, file_type)? {
                    entries.push(entry);
                }
            }
        }

        entries.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.file.cmp(&b.file))
        });
        let by_id = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect();
        let library = Self {
            root,
            entries,
            by_id,
        };
        tracing::info!(
            root = %library.root.display(),
            roms = library.entries.len(),
            "rom library scanned"
        );
        Ok(library)
    }

    /// The whole listing, in the order `/api/roms` serves it.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry an id names, or `None` for an id the scan did not write.
    ///
    /// This is the only way a request reaches a file, which is why a request
    /// carries an id: a miss is a 404, and there is nothing in a miss to resolve
    /// against the filesystem.
    pub fn lookup(&self, id: &str) -> Option<&Entry> {
        self.by_id.get(id).map(|index| &self.entries[*index])
    }

    /// The file to read for `entry`.
    ///
    /// Only ever called with an [`Entry`] this scan produced. Joining a client's
    /// own string to `root` is the thing this API never does.
    pub fn path_of(&self, entry: &Entry) -> PathBuf {
        self.root.join(&entry.file)
    }

    /// The entries to list for a `?q=`, which is a case-insensitive substring of
    /// `name`. No query, or an empty one, is every entry.
    pub fn matching(&self, query: Option<&str>) -> Vec<&Entry> {
        let needle = query.unwrap_or_default().to_lowercase();
        self.entries
            .iter()
            .filter(|entry| entry.name.to_lowercase().contains(&needle))
            .collect()
    }
}

impl From<Library> for LibrarySlot {
    fn from(library: Library) -> Self {
        Self {
            scanned: Arc::new(RwLock::new(Some(Arc::new(library)))),
        }
    }
}

/// Decide whether one directory entry is a ROM, and list it if it is.
///
/// `file_type` is the walk's own [`fs::FileType`], which does not resolve links: a
/// link and a file are told apart without another `lstat`, so a plain file costs
/// one `stat` and 2,775 of them cost one system call each.
fn offer(root: &Path, path: &Path, file_type: fs::FileType) -> io::Result<Option<Entry>> {
    let metadata = if file_type.is_symlink() {
        // A link is offered only when what it resolves to is in the root too: the
        // id lookup already keeps a request out of the filesystem, and this is
        // what keeps the listing off the rest of the disk.
        match path.canonicalize() {
            Ok(target) if target.starts_with(root) => fs::metadata(target)?,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    } else if file_type.is_file() {
        path.metadata()?
    } else {
        // A fifo, a socket, a device: not a cartridge.
        return Ok(None);
    };
    // `is_file` because a link to a directory inside the root resolves to
    // something that is not a cartridge, and the size cap is the ticket's: a
    // Genesis ROM is nowhere near 10 MiB.
    if !metadata.is_file() || metadata.len() > MAX_SIZE {
        return Ok(None);
    }

    let Some(relative) = relative_path(root, path) else {
        // Either not under the root, or a name that cannot be put in a JSON
        // response. Neither is a ROM this library can offer.
        return Ok(None);
    };
    let extension = relative.rsplit('.').next().unwrap_or(&relative);
    if !EXTENSIONS
        .iter()
        .any(|wanted| wanted.eq_ignore_ascii_case(extension))
    {
        return Ok(None);
    }
    Ok(Some(Entry {
        id: sha1_hex(&relative),
        name: display_name(&relative).to_owned(),
        size: metadata.len(),
        file: relative,
    }))
}

/// `path` relative to `root`, in `/` separators. `None` when `path` is not under
/// `root`, when a component is not `Normal`, or when a name is not valid UTF-8.
///
/// `root` is canonical and `path` came out of walking it, so the middle case
/// cannot happen; it is written out because a `..` appearing here would be exactly
/// the thing this file is organized around not allowing.
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let mut relative = String::new();
    for part in path.strip_prefix(root).ok()?.components() {
        let Component::Normal(part) = part else {
            return None;
        };
        relative.push_str(part.to_str()?);
        relative.push('/');
    }
    relative.pop();
    Some(relative)
}

/// The display name: the last component without its extension.
///
/// A leading dot is a hidden file rather than an extension, so a name that is all
/// dots keeps itself whole.
fn display_name(relative: &str) -> &str {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// The id of a file: the lowercase hex SHA-1 of its path relative to the rom dir.
///
/// Not of its contents, because hashing 2,775 cartridges at startup is a slow
/// version of the same list: what is needed is a name for a file that is stable
/// across restarts, cannot be a path, and does not leak where the files are.
fn sha1_hex(relative: &str) -> String {
    sha1_smol::Sha1::from(relative).digest().to_string()
}

/// The library a running server holds: one [`Library`], replaceable while requests
/// are in flight, because `POST /api/roms/rescan` swaps it and a request must not
/// be able to see the filesystem through a lock it is not waiting on.
#[derive(Clone, Default)]
pub struct LibrarySlot {
    /// `None` is no `--rom-dir`, which every handler answers with a 404 and
    /// `/api/info` with `"library": false`.
    scanned: Arc<RwLock<Option<Arc<Library>>>>,
}

impl LibrarySlot {
    /// No library: a server started without `--rom-dir`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// One scan of `root`, now, for a server that is starting up.
    pub fn scan(root: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Library::scan(root)?.into())
    }

    /// Whether there is a library at all.
    pub fn is_configured(&self) -> bool {
        self.read().is_some()
    }

    /// The current scan, as one `Arc::clone`: a request holds a listing that
    /// outlives a rescan that happens while it is running.
    pub fn get(&self) -> Option<Arc<Library>> {
        self.read().clone()
    }

    /// Walk the root again and offer what is there now.
    ///
    /// `Ok(None)` when there is no library to rescan, which is not a failure but
    /// is a 404. On `Err` the previous scan is left serving.
    pub fn rescan(&self) -> io::Result<Option<usize>> {
        let Some(root) = self.get().map(|library| library.root.clone()) else {
            return Ok(None);
        };
        let library = Arc::new(Library::scan(root)?);
        let roms = library.entries.len();
        *self.write() = Some(library);
        Ok(Some(roms))
    }

    /// The lock, read.
    ///
    /// A panic on the other side of it lost some other request; taking the value
    /// back is better than poisoning the library for every request after it.
    fn read(&self) -> RwLockReadGuard<'_, Option<Arc<Library>>> {
        self.scanned
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Option<Arc<Library>>> {
        self.scanned
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// `GET /api/roms`: the listing as JSON, narrowed by `?q=` when there is one.
pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<List>,
) -> Response {
    match state.library.get() {
        Some(library) => axum::Json(library.matching(filter.q.as_deref())).into_response(),
        None => not_found(),
    }
}

/// The query of `GET /api/roms`. Anything else in the query is ignored, and
/// ignored quietly: this is a filter, not an API surface to police.
#[derive(Deserialize)]
pub(crate) struct List {
    q: Option<String>,
}

/// `GET /api/roms/{id}`: the bytes of one ROM.
pub(crate) async fn rom(
    State(state): State<Arc<AppState>>,
    PathParam(id): PathParam<String>,
) -> Response {
    let Some(library) = state.library.get() else {
        return not_found();
    };
    let Some(entry) = library.lookup(&id) else {
        return not_found();
    };
    match tokio::fs::read(library.path_of(entry)).await {
        Ok(bytes) => (
            [
                (CONTENT_TYPE, HeaderValue::from_static(OCTET_STREAM)),
                (CACHE_CONTROL, HeaderValue::from_static(ROM_CACHE)),
            ],
            Body::from(bytes),
        )
            .into_response(),
        // The scan is a snapshot, so a ROM can be moved out from under it. That is
        // the same answer as an id that never was, with the reason in the log
        // rather than in the response.
        Err(error) => {
            tracing::warn!(%error, file = %entry.file, "rom from the scan cannot be read");
            not_found()
        }
    }
}

/// `POST /api/roms/rescan`: look at the directory again.
pub(crate) async fn rescan(State(state): State<Arc<AppState>>) -> Response {
    match state.library.rescan() {
        Ok(Some(roms)) => axum::Json(Rescanned { files: roms }).into_response(),
        Ok(None) => not_found(),
        Err(error) => {
            tracing::warn!(%error, "rom rescan failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "rescan failed").into_response()
        }
    }
}

/// The body of a successful rescan: enough for whoever ran it to see it counted.
#[derive(Serialize)]
struct Rescanned {
    files: usize,
}
