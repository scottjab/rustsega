//! The ROM library endpoints (M8-03, #157): what `--rom-dir` offers the
//! frontend, and everything it must never offer.
//!
//! `crates/server/tests/api.rs` owns `/healthz` and `/api/info`; this file owns
//! `GET /api/roms`, `GET /api/roms/{id}` and `POST /api/roms/rescan`. The router is
//! driven in-process, and every library is a `tempfile::tempdir()` full of fake
//! cartridges written here: a real ROM is never opened, and nothing from
//! `$RUSTSEGA_ROM_DIR` is needed for these to pass on a clean machine.
//!
//! The interesting cases are the ones where a request tries to name a file rather
//! than an id. An id is the SHA-1 of a path the *server* wrote while scanning, and
//! looking one up is the only way to reach a file at all, so a traversal attempt
//! has nowhere to go - which is why `path_traversal_*` goes through the router
//! rather than calling [`Library`] directly.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::header::{ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderName, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use server::AppState;
use server::assets::PlaceholderAssets;
use server::library::{Library, LibrarySlot};
use tempfile::{TempDir, tempdir};
use tower::ServiceExt;

/// The size a listed file may have at most, and one byte past it.
const CAP: u64 = 10 * 1024 * 1024;

/// The size tower-http will not compress under (`SizeAbove::DEFAULT_MIN_SIZE`),
/// so a fixture meant to be caught by the compressor has to clear it.
const MIN_COMPRESS_SIZE: usize = 32;

/// The contents of the file next to every test rom dir: what a traversal is
/// trying to get the server to read.
const SECRET: &[u8] = b"do not read me";

/// A directory holding `files` (path relative to the root, contents), to hand to
/// [`Library::scan`].
///
/// The [`TempDir`] is returned rather than dropped: it is what deletes the tree
/// afterwards.
fn rom_dir(files: &[(&str, &[u8])]) -> TempDir {
    let dir = tempdir().expect("a temp dir to pretend to be ~/games/roms/genesis");
    write_files(dir.path(), files);
    dir
}

/// Write `files`, named relative to `dir`, into it.
fn write_files(dir: &Path, files: &[(&str, &[u8])]) {
    for (path, bytes) in files {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().expect("every fixture is in a directory")).unwrap();
        fs::write(full, bytes).unwrap();
    }
}

/// A file of exactly `size` bytes, without writing `size` of anything: the
/// scanner is meant to look at the length, so `set_len` is the honest fixture and
/// a sparse file costs nothing.
fn sparse(dir: &Path, name: &str, size: u64) {
    let mut file = fs::File::create(dir.join(name)).expect("a file to set the length of");
    file.write_all(&[0xA9])
        .expect("one byte to replace with the length");
    file.set_len(size).expect("a file of the requested size");
}

/// State whose library is a scan of `dir`.
fn state_with(dir: &Path) -> Arc<AppState> {
    Arc::new(AppState {
        assets: Arc::new(PlaceholderAssets),
        library: LibrarySlot::scan(dir).expect("the fixture directory scans"),
    })
}

/// State with no library at all: what `rustsega` is without `--rom-dir`.
fn state_without_library() -> Arc<AppState> {
    Arc::new(AppState::new(Arc::new(PlaceholderAssets)))
}

async fn request(state: Arc<AppState>, method: Method, uri: &str) -> Response<Body> {
    request_with_headers(state, method, uri, &[]).await
}

/// A request carrying extra headers. The only thing this file needs them for is
/// `Accept-Encoding`, which is how #417 asks the router for a compressed answer
/// and finds out whether a ROM download refused to give one.
async fn request_with_headers(
    state: Arc<AppState>,
    method: Method,
    uri: &str,
    headers: &[(HeaderName, &str)],
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(name.clone(), *value);
    }
    server::router(state)
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("the router answers without failing")
}

/// A cartridge big enough to be worth compressing: 1 KiB of a repeating pattern,
/// which gzip would shrink happily.
///
/// The size is the point. `CompressionLayer`'s default predicate leaves anything
/// of [`MIN_COMPRESS_SIZE`] bytes or fewer alone, so the 4- and 1-byte fixtures
/// elsewhere in this file would let a test of #417 go green with the bug still
/// in place.
fn rom_bytes() -> Vec<u8> {
    (0..1024).map(|i| (i % 251) as u8).collect()
}

async fn get(state: Arc<AppState>, uri: &str) -> Response<Body> {
    request(state, Method::GET, uri).await
}

async fn text(response: Response<Body>) -> String {
    String::from_utf8(body(response).await).expect("the API answers in UTF-8")
}

async fn body(response: Response<Body>) -> Vec<u8> {
    response
        .collect()
        .await
        .expect("the body is readable")
        .to_bytes()
        .to_vec()
}

/// The ids a scan wrote, in listing order.
fn ids(library: &Library) -> Vec<&str> {
    library
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect()
}

/// The files a scan found, in listing order.
fn files(library: &Library) -> Vec<&str> {
    library
        .entries()
        .iter()
        .map(|entry| entry.file.as_str())
        .collect()
}

/// The `id` of the entry whose `file` is `name`.
fn id_of(library: &Library, name: &str) -> String {
    library
        .entries()
        .iter()
        .find(|entry| entry.file == name)
        .unwrap_or_else(|| panic!("{name} is not listed: {:?}", files(library)))
        .id
        .clone()
}

/// The SHA-1 a path relative to the rom dir has to hash to, computed here rather
/// than in the library so the test says which side of the contract it is checking.
fn expected_id(relative: &str) -> String {
    sha1_smol::Sha1::from(relative).digest().to_string()
}

/// The `name` of every entry of a list response, in order.
fn names(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_else(|error| panic!("the list is a JSON array, not {json}: {error}"))
        .into_iter()
        .map(|entry| {
            entry["name"]
                .as_str()
                .expect("every entry has a name")
                .to_owned()
        })
        .collect()
}

fn link(from: &Path, to: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(to, from).expect("a symlink");
    #[cfg(not(unix))]
    std::os::windows::fs::symlink_file(to, from).expect("a symlink");
}

#[test]
fn scan_lists_known_extensions_recursively_and_ignores_the_rest() {
    let dir = rom_dir(&[
        ("a.md", &[1]),
        ("b.bin", &[2]),
        ("c.txt", &[3]),
        ("sub/d.gen", &[4]),
        ("sub/e.smd", &[5]),
        ("sub/note.md.txt", &[6]),
    ]);

    let library = Library::scan(dir.path()).expect("the fixture scans");

    assert_eq!(
        files(&library),
        ["a.md", "b.bin", "sub/d.gen", "sub/e.smd"],
        "the four supported extensions, recursively; a name that merely ends in one is not one"
    );
}

#[test]
fn scan_ignores_a_file_bigger_than_the_cap_and_keeps_one_exactly_at_it() {
    let dir = tempdir().unwrap();
    sparse(dir.path(), "big.md", CAP + 1);
    sparse(dir.path(), "exact.md", CAP);

    let library = Library::scan(dir.path()).expect("the fixture scans");

    assert_eq!(
        files(&library),
        ["exact.md"],
        "10 MiB is the cap, so 10 MiB and 1 byte is not offered"
    );
    assert_eq!(library.entries()[0].size, CAP);
}

#[test]
fn entry_id_is_the_sha1_of_the_path_relative_to_the_rom_dir() {
    let dir = rom_dir(&[("a.md", &[1]), ("sub/d.gen", &[4])]);
    let library = Library::scan(dir.path()).expect("the fixture scans");

    assert_eq!(id_of(&library, "a.md"), expected_id("a.md"));
    assert_eq!(id_of(&library, "sub/d.gen"), expected_id("sub/d.gen"));
    for entry in library.entries() {
        assert!(
            !entry.id.contains('/') && !entry.id.contains('.'),
            "an id is hex, never a path: {:?}",
            entry.id
        );
    }
}

#[test]
fn ids_are_the_same_after_a_second_scan_of_the_same_directory() {
    // An id is a request's only handle on a file, so it has to survive a restart.
    let dir = rom_dir(&[("a.md", &[1]), ("sub/d.gen", &[4])]);

    let first = Library::scan(dir.path()).unwrap();
    let second = Library::scan(dir.path()).unwrap();

    assert_eq!(files(&first), files(&second));
    assert_eq!(ids(&first), ids(&second));
}

#[test]
fn scan_of_a_directory_that_is_not_there_is_an_error() {
    let missing = tempdir().unwrap().path().join("nope");

    let error = Library::scan(&missing).expect_err("a missing root cannot be listed");

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn a_directory_that_is_a_symlink_is_a_valid_root() {
    // `--rom-dir` may be a link to a mount: `cli::check_rom_dir` follows links, and
    // the scan has to agree with it rather than reject the same path.
    let target = tempdir().unwrap();
    fs::write(target.path().join("a.md"), [1]).unwrap();
    let links = tempdir().unwrap();
    let dir = links.path().join("roms");
    #[cfg(unix)]
    std::os::unix::fs::symlink(target.path(), &dir).expect("a symlink to a directory");
    #[cfg(not(unix))]
    std::os::windows::fs::symlink_dir(target.path(), &dir).expect("a symlink to a directory");

    let library = Library::scan(&dir).expect("a linked root scans");

    assert_eq!(files(&library), ["a.md"]);
}

#[tokio::test]
async fn list_response_is_the_documented_json_object() {
    let name = "Sonic The Hedgehog (USA, Europe).md";
    let dir = rom_dir(&[(name, &[0u8; 4096])]);
    // The size in the response has to be the real one, so grow the fixture to the
    // ticket's example rather than pretend 4096 is 524288.
    let file = dir.path().join(name);
    fs::File::options()
        .write(true)
        .open(&file)
        .and_then(|f| f.set_len(524288))
        .expect("a 512 KiB fixture");

    let response = get(state_with(dir.path()), "/api/roms").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        text(response).await,
        format!(
            "[{{\"id\":\"{}\",\"name\":\"Sonic The Hedgehog (USA, Europe)\",\"file\":\"{name}\",\"size\":524288}}]",
            expected_id(name)
        )
    );
}

#[tokio::test]
async fn list_is_sorted_by_name_case_insensitively() {
    let dir = rom_dir(&[("zeta.md", &[1]), ("Alpha.md", &[2]), ("mike.bin", &[3])]);

    let response = get(state_with(dir.path()), "/api/roms").await;

    assert_eq!(names(&text(response).await), ["Alpha", "mike", "zeta"]);
}

#[tokio::test]
async fn query_filters_by_name_case_insensitively() {
    let dir = rom_dir(&[
        ("Sonic The Hedgehog.md", &[1]),
        ("Sonic 2.md", &[2]),
        ("Ghouls N Ghosts.bin", &[3]),
    ]);
    let state = state_with(dir.path());

    let response = text(get(Arc::clone(&state), "/api/roms?q=sonic").await).await;
    assert_eq!(names(&response), ["Sonic 2", "Sonic The Hedgehog"]);

    let response = text(get(Arc::clone(&state), "/api/roms?q=SONIC%202").await).await;
    assert_eq!(
        names(&response),
        ["Sonic 2"],
        "the filter is not case-sensitive"
    );

    let response = text(get(Arc::clone(&state), "/api/roms?q=never").await).await;
    assert_eq!(response, "[]");

    let response = text(get(state, "/api/roms?q=").await).await;
    assert_eq!(
        names(&response),
        ["Ghouls N Ghosts", "Sonic 2", "Sonic The Hedgehog"],
        "an empty filter is no filter"
    );
}

#[tokio::test]
async fn fetch_returns_the_bytes_as_octet_stream_for_a_private_cache() {
    let dir = rom_dir(&[("a.md", &[0xDE, 0xAD, 0xBE, 0xEF]), ("sub/d.gen", &[7])]);
    let library = Library::scan(dir.path()).unwrap();
    let state = state_with(dir.path());

    let response = get(
        Arc::clone(&state),
        &format!("/api/roms/{}", id_of(&library, "a.md")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/octet-stream"
    );
    assert_eq!(
        response
            .headers()
            .get(CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap(),
        "private, max-age=3600"
    );
    assert_eq!(body(response).await, vec![0xDE, 0xAD, 0xBE, 0xEF]);

    let response = get(
        state,
        &format!("/api/roms/{}", id_of(&library, "sub/d.gen")),
    )
    .await;
    assert_eq!(body(response).await, vec![7], "a nested file is served too");
}

/// #417: a ROM download arrives as the bytes on disk, whatever the client asked
/// to be handed.
#[tokio::test]
async fn rom_downloads_are_not_compressed_even_when_the_client_asks_for_it() {
    // One `CompressionLayer` covers the whole router, and its default predicate
    // compresses anything that is not an image, gRPC or an event stream. A
    // cartridge is `application/octet-stream`, so every download was deflated on
    // every request: CPU spent on bytes a Genesis ROM barely shrinks, and
    // repeated per client rather than once, because the cache rule is `private`.
    let bytes = rom_bytes();
    assert!(
        bytes.len() > MIN_COMPRESS_SIZE,
        "under the compressor's floor this test proves nothing"
    );
    let dir = rom_dir(&[("big.md", bytes.as_slice())]);
    let library = Library::scan(dir.path()).unwrap();
    let state = state_with(dir.path());
    let uri = format!("/api/roms/{}", id_of(&library, "big.md"));

    // Both the encodings this binary is built with (`compression-gzip` and
    // `compression-br`), plus the combination a browser actually sends.
    for encoding in ["gzip", "br", "gzip, br"] {
        let response = request_with_headers(
            Arc::clone(&state),
            Method::GET,
            &uri,
            &[(ACCEPT_ENCODING, encoding)],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK, "{encoding}");
        assert_eq!(
            response.headers().get(CONTENT_ENCODING),
            None,
            "{encoding}: a ROM download must not come back `{}`",
            response
                .headers()
                .get(CONTENT_ENCODING)
                .map(|v| v.to_str().unwrap_or("?"))
                .unwrap_or_default()
        );
        assert_eq!(
            body(response).await,
            bytes,
            "{encoding}: byte for byte the file, not a stream to decode"
        );
    }
}

/// The other half of #417: what is left of the API still compresses, so the fix
/// turned the compressor off for ROMs and not for the router.
#[tokio::test]
async fn rom_listing_is_still_compressed_when_the_client_asks_for_it() {
    // Enough entries that the listing clears the 32 byte floor on its own.
    let names: Vec<String> = (0..40)
        .map(|i| format!("a-rom-with-a-longish-name-{i:02}.md"))
        .collect();
    let files: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (name.as_str(), &[0xABu8][..]))
        .collect();
    let dir = rom_dir(&files);
    let state = state_with(dir.path());

    // `/api/info` is deliberately not here: its whole body is 34 bytes, two
    // past the compressor's floor, so whether it compresses would depend on how
    // long the version string happens to be.
    let response = request_with_headers(
        Arc::clone(&state),
        Method::GET,
        "/api/roms",
        &[(ACCEPT_ENCODING, "gzip")],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_ENCODING)
            .map(|value| value.to_str().unwrap()),
        Some("gzip"),
        "the listing is JSON, and JSON is what compression is for"
    );
}

#[tokio::test]
async fn an_unknown_id_is_not_found() {
    let dir = rom_dir(&[("a.md", &[1])]);
    let state = state_with(dir.path());

    let response = get(state, "/api/roms/0000000000000000000000000000000000000000").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Requests that reach the `GET /api/roms/{id}` handler, in every spelling of
/// traversal that survives URI parsing. The ticket names `..%2F..%2Fetc%2Fpasswd`.
const TRAVERSAL_IDS: &[&str] = &[
    "/api/roms/..%2F..%2Fetc%2Fpasswd",
    "/api/roms/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
    "/api/roms/..%2f..%2f..%2fetc%2fpasswd.md",
    "/api/roms/%00",
    // A file name is not an id: what a request may name is the hash, not the file.
    "/api/roms/a.md",
    "/api/roms/sub%2Fd.gen",
];

/// Requests that are not id-shaped at all, so they miss the library route and are
/// answered by the fallback. That none of them is answered with a file has always
/// been this file's claim; that they are 404 rather than the app shell at 200 is
/// #418's, and both are asserted below.
const NOT_IDS: &[&str] = &[
    "/api/roms/",
    "/api/roms/....//....//etc/passwd",
    "/api/roms/../../secret.md",
    "/api/roms/a.md/download",
];

/// A rom dir, and a `secret.md` next to it rather than in it: what a traversal is
/// trying to get the server to read. The returned [`TempDir`] is the parent of
/// both, so it is what cleans them up.
fn rom_dir_with_a_neighbour(files: &[(&str, &[u8])]) -> (TempDir, PathBuf) {
    let parent = tempdir().expect("a temp dir to keep a rom dir and a neighbour in");
    fs::write(parent.path().join("secret.md"), SECRET).unwrap();
    let roms = parent.path().join("roms");
    fs::create_dir(&roms).unwrap();
    write_files(&roms, files);
    (parent, roms)
}

#[tokio::test]
async fn path_traversal_in_the_id_is_not_found() {
    let (_parent, roms) = rom_dir_with_a_neighbour(&[("a.md", &[1]), ("sub/d.gen", &[2])]);
    let state = state_with(&roms);

    for attempt in TRAVERSAL_IDS {
        let response = get(Arc::clone(&state), attempt).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{attempt}");
    }
}

#[tokio::test]
async fn no_request_that_is_not_an_id_is_answered_with_a_file() {
    let (_parent, roms) = rom_dir_with_a_neighbour(&[("a.md", &[1])]);
    let state = state_with(&roms);

    for attempt in NOT_IDS {
        let response = get(Arc::clone(&state), attempt).await;
        // #418: one slash off an endpoint is a request for an endpoint that does
        // not exist, not a client-side route, so the fallback answers it 404 under
        // `/api/` instead of handing back the shell at 200.
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{attempt}");
        let bytes = body(response).await;
        assert!(
            !bytes.starts_with(SECRET),
            "{attempt} must not be answered with the file next to the rom dir"
        );
    }
}

#[tokio::test]
async fn a_symlink_out_of_the_rom_dir_is_not_listed() {
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.md"), b"not for the library").unwrap();
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.md"), [1]).unwrap();
    link(
        &dir.path().join("link.md"),
        &outside.path().join("secret.md"),
    );

    let library = Library::scan(dir.path()).expect("the fixture scans");

    assert_eq!(
        files(&library),
        ["a.md"],
        "a link out of the root is a file the library does not own"
    );

    // And it is not fetchable either: nothing in the listing refers to it.
    let state = state_with(dir.path());
    let listed = text(get(state, "/api/roms").await).await;
    assert!(!listed.contains("link.md"), "{listed}");
}

#[tokio::test]
async fn a_symlink_to_a_file_inside_the_rom_dir_is_listed_and_served() {
    // The target is what gets checked, not the fact of being a link, so a
    // collection organized with links inside itself still works.
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("discs")).unwrap();
    fs::write(dir.path().join("discs/real.md"), b"rom bytes").unwrap();
    link(
        &dir.path().join("link.md"),
        &dir.path().join("discs/real.md"),
    );

    let library = Library::scan(dir.path()).expect("the fixture scans");
    let id = id_of(&library, "link.md");
    let state = state_with(dir.path());

    let response = get(state, &format!("/api/roms/{id}")).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, b"rom bytes".to_vec());
}

#[tokio::test]
async fn without_a_rom_dir_both_rom_endpoints_are_not_found() {
    let state = state_without_library();

    let response = get(Arc::clone(&state), "/api/roms").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = get(state, "/api/roms/0000000000000000000000000000000000000000").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rescan_picks_up_a_file_added_after_startup() {
    let dir = rom_dir(&[("a.md", &[1])]);
    let state = state_with(dir.path());
    fs::write(dir.path().join("new.md"), [9]).unwrap();

    let before = text(get(Arc::clone(&state), "/api/roms").await).await;
    assert_eq!(names(&before), ["a"]);

    let response = request(Arc::clone(&state), Method::POST, "/api/roms/rescan").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(text(response).await, "{\"files\":2}");

    let after = text(get(state, "/api/roms").await).await;
    assert_eq!(names(&after), ["a", "new"]);
}

#[tokio::test]
async fn rescan_without_a_rom_dir_is_not_found() {
    let response = request(state_without_library(), Method::POST, "/api/roms/rescan").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_file_that_vanished_after_the_scan_is_not_found_rather_than_an_error() {
    let dir = rom_dir(&[("a.md", &[1])]);
    let library = Library::scan(dir.path()).unwrap();
    let id = id_of(&library, "a.md");
    let state = state_with(dir.path());
    fs::remove_file(dir.path().join("a.md")).unwrap();

    let response = get(state, &format!("/api/roms/{id}")).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
