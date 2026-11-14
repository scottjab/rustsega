//! The ROM library the server offers (M8-04, #158).
//!
//! [`crate::rom`] and [`crate::rom_picker`] are the same pair for a file the player
//! picked up; this is the pair for a ROM the server is offering instead, and it is
//! split the same way: the decisions and the round trip here, [`crate::ui::library_view`]
//! for the drawing.
//!
//! What is decided here is the shape of the listing and the three questions the view
//! asks of it - which entries a search and a region chip leave, what one row says,
//! and which rows a scroll has to draw - plus [`Library`], which holds the answer and
//! asks the server for it. [`crate::recents`] put its IndexedDB round trip in the
//! same place for the same reason: launching something is a decision, and only the
//! rows of it are drawing.
//!
//! The listing is not small: the owner's collection is about 2,775 files, which is
//! why every function here works on indices into `all` rather than on entries. A
//! filtered list is a `Vec<usize>`, so searching is a few thousand `contains` calls
//! and no cloning of names, and the virtualized list is a window of the same
//! indices.

use leptos::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::rom;
use crate::rom::LoadedRom;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// One ROM, exactly as `GET /api/roms` serves it (M8-03, #157).
///
/// A copy of the server's `Entry` rather than a dependency on the server crate:
/// `web` and `server` are built separately, and the wire format is the contract
/// between them. The field names are the wire format, so they are not free to
/// change.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RomEntry {
    /// The handle to fetch: SHA-1 of the path, and the only thing a request may
    /// name. Never shown.
    pub id: String,
    /// The file name without its extension, which is what a row shows and what
    /// both the search and the region chips read: `Sonic The Hedgehog 2 (World)`.
    pub name: String,
    /// The path relative to the server's ROM directory, `/` separated. Kept for
    /// the day a row wants to say where a ROM came from; the id is what a request
    /// sends.
    pub file: String,
    /// The size in bytes, so a row can say how big the download is before
    /// starting it.
    pub size: u64,
}

/// The region a library view can narrow its list to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RegionFilter {
    /// Every entry, whatever its name says.
    #[default]
    All,
    /// North America: `(USA)`, and `(World)` too - see [`RegionFilter::matches`].
    Usa,
    /// `(Europe)`, and `(World)`.
    Europe,
    /// `(Japan)`, and `(World)`.
    Japan,
}

impl RegionFilter {
    /// The chips, in the order the view shows them.
    pub const ALL: [Self; 4] = [Self::All, Self::Usa, Self::Europe, Self::Japan];

    /// What the chip says.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Usa => "USA",
            Self::Europe => "Europe",
            Self::Japan => "Japan",
        }
    }

    /// Whether an entry belongs to this region: whether any of the tags of `name`
    /// is this region, or is `(World)`.
    ///
    /// `World` counts for all three because that is what a World release is: one
    /// cartridge sold everywhere, and in a set named by [dat file] it is the tag a
    /// great many games carry alone. The tag is matched whole and
    /// case-insensitively, because a collection spells the same region `USA` and
    /// `U.S.A.` in different dumps and `usa` in the ones that were retagged by
    /// hand.
    ///
    /// [dat]: https://www.retrodev.com/waitko/manual/nointro-dats.html
    pub fn matches(self, name: &str) -> bool {
        if self == Self::All {
            return true;
        }
        parse_tags(name)
            .1
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case("world") || self.tag_is_self(tag))
    }

    /// Whether `tag` names this region itself, `World` excepted - which the caller
    /// has already asked about.
    fn tag_is_self(self, tag: &str) -> bool {
        match self {
            Self::All => true,
            Self::Usa => tag.eq_ignore_ascii_case("usa") || tag.eq_ignore_ascii_case("us"),
            Self::Europe => tag.eq_ignore_ascii_case("europe") || tag.eq_ignore_ascii_case("eu"),
            Self::Japan => tag.eq_ignore_ascii_case("japan") || tag.eq_ignore_ascii_case("jp"),
        }
    }
}

/// What kind of thing one parenthesised tag of a file name is.
///
/// A row shows tags rather than hiding them - they are how two entries with the
/// same title are told apart - but not all of them deserve the same weight: the
/// region and the revision are what a player is choosing with, and a `Beta` or a
/// `Proto` is a warning about the dump rather than a fact worth colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagKind {
    /// A region: `(USA)`, `(Europe)`, `(Japan)`, `(World)`.
    Region,
    /// A revision: `(Rev A)`, `(Rev 1)`.
    Revision,
    /// A status that says this is not the shipping cartridge: `(Beta)`, `(Proto)`,
    /// `(Demo)`, `(Sample)`.
    Status,
    /// Anything else - a language, a publisher, a `[!]`.
    Other,
}

impl TagKind {
    /// Whether a row should mute a tag of this kind.
    pub fn is_muted(self) -> bool {
        self == Self::Status
    }
}

/// Which entries of `all` the search `query` and `region` leave, as indices.
///
/// Every whitespace-separated word of `query` has to appear, case-insensitively,
/// somewhere in the entry's name - in any order, since a player types the words
/// they remember rather than the name they are looking for ("sonic 2" finds
/// `Sonic The Hedgehog 2 (World)`). An empty query leaves everything, and the
/// indices come back in the order the server sent them, which is already sorted by
/// name (`Library::matching` in the server, M8-03 #157).
pub fn filter_roms(all: &[RomEntry], query: &str, region: RegionFilter) -> Vec<usize> {
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    all.iter()
        .enumerate()
        .filter(|(_index, entry)| {
            let name = entry.name.to_lowercase();
            words.iter().all(|word| name.contains(word)) && region.matches(&entry.name)
        })
        .map(|(index, _entry)| index)
        .collect()
}

/// The name of a ROM split into the title to show and the tags beside it.
///
/// `Sonic The Hedgehog 2 (World) (Rev A)` becomes `Sonic The Hedgehog 2` and
/// `[World, Rev A]`. Parentheses are the whole of the syntax, because they are the
/// whole of the syntax ROM sets use: a `dat file` name is a title followed by any
/// number of parenthesised facts, and a comma inside one of them is a list of
/// facts and not a new one (`(USA, Europe)` is two tags).
///
/// Unbalanced parentheses are a name, not an error: an unclosed group is kept in
/// the title rather than swallowing the rest of it.
pub fn parse_tags(name: &str) -> (String, Vec<String>) {
    let mut title = String::with_capacity(name.len());
    let mut tags: Vec<String> = Vec::new();
    let mut group = String::new();
    let mut inside = false;
    for character in name.chars() {
        match character {
            '(' => {
                inside = true;
                group.clear();
            }
            ')' if inside => {
                push_group(&mut tags, &group);
                inside = false;
            }
            ')' => {}
            _ if inside => group.push(character),
            _ => title.push(character),
        }
    }
    // `(USA` with no closer: the parenthesis was a character in somebody's title.
    if inside {
        title.push('(');
        title.push_str(&group);
    }
    let mut title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() && !name.trim().is_empty() {
        // A name that was nothing but groups still needs something to show.
        title = name.trim().to_owned();
    }
    (title, tags)
}

/// One `(...)` group's worth of tags: split on commas, trimmed, empties dropped.
fn push_group(tags: &mut Vec<String>, group: &str) {
    for part in group.split(',') {
        let part = part.trim();
        if !part.is_empty() {
            tags.push(part.to_owned());
        }
    }
}

/// Which kind `tag` is.
pub fn tag_kind(tag: &str) -> TagKind {
    let tag = tag.trim().to_lowercase();
    match tag.as_str() {
        "usa" | "us" | "u.s.a." | "europe" | "eu" | "japan" | "jp" | "world" | "asia"
        | "brazil" | "australia" | "korea" | "france" | "germany" | "spain" | "italy"
        | "netherlands" | "sweden" | "finland" | "uk" | "united kingdom" => TagKind::Region,
        "beta" | "proto" | "prototype" | "demo" | "sample" | "preview" | "pirated" => {
            TagKind::Status
        }
        _ if tag.starts_with("rev") || tag.starts_with("version") || tag.starts_with("ver") => {
            TagKind::Revision
        }
        _ => TagKind::Other,
    }
}

/// The range of rows a virtualized list has to draw.
///
/// `total` rows of `row_h`, scrolled `scroll_top`, in a viewport `viewport_h` tall,
/// with `overscan` extra rows above and below so a fast flick has already-drawn
/// rows to move into rather than blank background. Returns an empty range for an
/// empty list, and always a range that is inside `0..total`.
///
/// The row height is a constant rather than a measurement (see
/// [`ROW_HEIGHT_PX`]): the alternative is measuring every row after drawing it, and
/// a list whose scroll height moves while it scrolls is a list with a scrollbar
/// that jumps.
pub fn visible_window(
    total: usize,
    scroll_top: f64,
    row_h: f64,
    viewport_h: f64,
    overscan: usize,
) -> std::ops::Range<usize> {
    if total == 0 || !row_h.is_finite() || !scroll_top.is_finite() || !viewport_h.is_finite() {
        return 0..0;
    }
    if row_h <= 0.0 {
        return 0..0;
    }
    let total = total as isize;
    let overscan = overscan as isize;
    let rows = (viewport_h.max(0.0) / row_h).ceil() as isize + 2 * overscan;
    let first = (scroll_top / row_h).floor() as isize - overscan;
    // A stale `scroll_top` - the list shrank under the scroll, which is what
    // typing a search does - would otherwise land past the end and draw nothing.
    let first = first.clamp(0, (total - 1).max(0));
    let last = (first + rows).min(total);
    first as usize..last as usize
}

/// The entry a row of the virtualized window shows.
///
/// `visible_window` counts rows of the *filtered* list, so a row number reaches an
/// entry by way of `shown` - and the two are the same number only before a search.
/// Confusing them is a list that shows one game and launches another, which is what
/// the browser found the first time this was written.
pub fn entry_at<'a>(
    all: &'a [RomEntry],
    shown: &'a [usize],
    row: usize,
) -> Option<&'a RomEntry> {
    all.get(*shown.get(row)?)
}

/// The height of one row of the library list, in CSS pixels.
///
/// `style.css` sets `--library-row` to the same number, and the rows are absolutely
/// positioned at multiples of it - which is why the two have to agree, and why
/// `the_row_height_matches_the_stylesheet` in `crates/web/tests/` says so.
pub const ROW_HEIGHT_PX: f64 = 56.0;

/// How many extra rows [`visible_window`] draws outside the viewport.
pub const OVERSCAN: usize = 4;

/// `bytes` as a person would say it: `512 B`, `1 MB`, `1.5 MB`.
///
/// Binary steps, because that is what a ROM is measured in and what every ROM
/// tool prints - 512 KiB is `512 KB` here, as it is on the file name.
pub fn format_size(bytes: u64) -> String {
    const KILOBYTES: u64 = 1024;
    const MEGABYTES: u64 = 1024 * 1024;
    if bytes < KILOBYTES {
        return format!("{bytes} B");
    }
    if bytes < MEGABYTES {
        return format!("{} KB", scaled(bytes, KILOBYTES));
    }
    format!("{} MB", scaled(bytes, MEGABYTES))
}

/// `bytes / unit` with one decimal, and no `.0` on a whole number.
fn scaled(bytes: u64, unit: u64) -> String {
    let value = bytes as f64 / unit as f64;
    let text = format!("{value:.1}");
    text.strip_suffix(".0").unwrap_or(&text).to_owned()
}

/// `count` with thousands separators: `2,775`.
///
/// The collection is thousands of games and the count is the one number the view
/// shows in full, so it is grouped by hand rather than through `Intl` - which the
/// shell would have to reach through `js_sys` for one string.
pub fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.char_indices() {
        // A comma before every group of three that follows it, which is what puts
        // the first one after `1` and not after `12`.
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// What `GET /api/info` is asked for: the one request that says whether this page
/// has a library to show at all.
const INFO_PATH: &str = "/api/info";

/// The listing's path: `GET /api/roms`, the whole collection, unfiltered - the
/// search is `filter_roms`, run on the result, because a 2,775-entry list is a few
/// hundred kilobytes and one round trip beats one per keystroke.
const LIST_PATH: &str = "/api/roms";

/// Whether the library is on screen: the shell's one answer, held in a context so the
/// top bar's button, the empty state's section, the view itself and [`crate::menu`]
/// all agree about it (M8-04, #158).
///
/// A newtype rather than a bare `RwSignal<bool>` because the shell has other boolean
/// signals in its contexts, and "which one is this" should not be a question.
#[derive(Clone, Copy)]
pub struct LibraryOpen(pub RwSignal<bool>);

impl LibraryOpen {
    /// Closed, which is where the shell starts: the library has to be asked for.
    pub fn new() -> Self {
        Self(RwSignal::new(false))
    }
}

impl Default for LibraryOpen {
    fn default() -> Self {
        Self::new()
    }
}

/// The one sentence for a ROM the server named and could not hand over: the scan is
/// a snapshot, so a file can be moved out from under it, and a fetch can fail for
/// the hundred reasons a network has.
const UNAVAILABLE: &str = "That game could not be downloaded from the server";

/// The library as the shell holds it: what the server has offered, and what the
/// player has typed into the search box so far.
///
/// `Copy`, like [`crate::recents::Recents`] and [`crate::emulator::Controls`]: the
/// signals are handles and the listing is behind one, so a view can take the whole
/// library by value and still not touch 2,775 names.
#[derive(Clone, Copy)]
pub struct Library {
    /// Whether this page is served by a server that has a ROM library.
    ///
    /// `false` until `/api/info` says otherwise, which is what hides the whole
    /// feature on a page served by `trunk serve` or any other static host: the
    /// request fails, nothing is shown, and no player is offered a library that
    /// answers 404.
    pub available: RwSignal<bool>,
    /// Whether the listing is on its way from the server.
    pub loading: RwSignal<bool>,
    /// The whole listing, in the order the server sent it - which is sorted by name,
    /// so the filtered list is too and nothing here sorts again.
    entries: RwSignal<Vec<RomEntry>>,
    /// What the search box holds.
    pub query: RwSignal<String>,
    /// Which region the chips leave in.
    pub region: RwSignal<RegionFilter>,
    /// The id of the ROM whose bytes are on their way: the row that shows a spinner
    /// while a megabyte crosses the loopback.
    pub fetching: RwSignal<Option<String>>,
    /// Where a failed round trip says so.
    toasts: Toasts,
}

impl Library {
    /// An empty library that has not asked the server anything yet.
    pub fn new(toasts: Toasts) -> Self {
        Self {
            available: RwSignal::new(false),
            loading: RwSignal::new(false),
            entries: RwSignal::new(Vec::new()),
            query: RwSignal::new(String::new()),
            region: RwSignal::new(RegionFilter::default()),
            fetching: RwSignal::new(None),
            toasts,
        }
    }

    /// `all` as the server sent it, for a view that needs an entry the filtered
    /// indices name. Read with `with`, never cloned: the listing is thousands of
    /// names.
    pub fn with_entries<T>(self, f: impl FnOnce(&[RomEntry]) -> T) -> T {
        self.entries.with(|all| f(all))
    }

    /// The entries the search box and the region chip leave, as indices into
    /// [`Library::with_entries`] - which is what the list draws and what the count
    /// counts.
    pub fn shown(&self) -> Signal<Vec<usize>> {
        let entries = self.entries;
        let query = self.query;
        let region = self.region;
        Signal::derive(move || {
            let query = query.get();
            entries.with(|all| filter_roms(all, &query, region.get()))
        })
    }

    /// Ask the server for the listing again.
    pub fn refresh(self) {
        self.loading.set(true);
        spawn_local(async move {
            let entries = fetch_json(LIST_PATH).await;
            self.loading.set(false);
            match entries {
                Ok(value) => match serde_json::from_value(value) {
                    Ok(entries) => self.entries.set(entries),
                    Err(_) => self.toasts.push(ToastKind::Warn, UNAVAILABLE),
                },
                Err(()) => self.toasts.push(ToastKind::Warn, UNAVAILABLE),
            }
        });
    }

    /// Fetch the bytes of one entry and load them the way the file picker would.
    ///
    /// That is the whole point of this method: a library launch goes through
    /// [`rom::load_rom`] into the same cartridge signal, so SRAM, recents and the
    /// crash handling all behave exactly as they do for a file the player picked up.
    pub fn launch(self, roms: RwSignal<Option<LoadedRom>>, open: LibraryOpen, id: &str) {
        let id = id.to_string();
        // Read now, from the listing as it stands: the row the player pressed is
        // the row whose name and id go with the bytes, and a search that narrows
        // the list mid-download must not rename the ROM on its way in.
        let Some(name) = self.entries.with(|entries| {
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.name.clone())
        }) else {
            return;
        };
        self.fetching.set(Some(id.clone()));
        let toasts = self.toasts;
        spawn_local(async move {
            let bytes = fetch_bytes(&format!("/api/roms/{id}")).await;
            // Whatever the answer, no row is waiting on it any more - and on the
            // way out of a failure this is the only thing that clears the spinner.
            self.fetching.set(None);
            let Ok(bytes) = bytes else {
                toasts.push(ToastKind::Warn, UNAVAILABLE);
                return;
            };
            // `load_rom` judges the bytes, not the name: a library entry whose
            // bytes are not a cartridge gets the same sentence a dropped file gets.
            match rom::load_rom(&name, bytes) {
                // The library has done its job: the picture it was covering is the
                // picture the player wants next.
                Ok(rom) => {
                    roms.set(Some(rom));
                    open.0.set(false);
                }
                Err(_) => toasts.push(ToastKind::Danger, rom::NOT_A_ROM),
            }
        });
    }
}

/// The library, mounted for as long as the component that calls this is.
///
/// Asks `/api/info` once, on the way in, and only then the listing: a page with no
/// API behind it gets no spinner, no toast and no library button, which is what
/// `trunk serve` and every static host is (M8-04, #158).
pub fn use_library() -> Library {
    let library = Library::new(expect_context::<Toasts>());
    spawn_local(async move {
        let Ok(info) = fetch_json(INFO_PATH).await else {
            return;
        };
        if info.get("library").and_then(Value::as_bool) != Some(true) {
            return;
        }
        library.available.set(true);
        library.refresh();
    });
    library
}

/// `GET`s `path` and answers the response, or `Err` for anything that is not a
/// 2xx: no window to fetch from, a failed request, a status that is not ok.
async fn fetch(path: &str) -> Result<web_sys::Response, ()> {
    let promise = web_sys::window().ok_or(())?.fetch_with_str(path);
    let value = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| ())?;
    let response: web_sys::Response = value.dyn_into().map_err(|_| ())?;
    if !response.ok() {
        return Err(());
    }
    Ok(response)
}

/// The JSON body of `path`, through `JSON.stringify` and `serde_json` rather than a
/// second deserializer for `JsValue`.
async fn fetch_json(path: &str) -> Result<Value, ()> {
    let response = fetch(path).await?;
    let value = wasm_bindgen_futures::JsFuture::from(response.json().map_err(|_| ())?)
        .await
        .map_err(|_| ())?;
    let text = String::from(js_sys::JSON::stringify(&value).map_err(|_| ())?);
    serde_json::from_str(&text).map_err(|_| ())
}

/// The body of `path` as bytes, for a ROM.
async fn fetch_bytes(path: &str) -> Result<Vec<u8>, ()> {
    let response = fetch(path).await?;
    let buffer = wasm_bindgen_futures::JsFuture::from(response.array_buffer().map_err(|_| ())?)
        .await
        .map_err(|_| ())?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry with everything else uninteresting: the tests below read names.
    fn entry(name: &str) -> RomEntry {
        RomEntry {
            id: format!("id-{name}"),
            name: name.to_string(),
            file: format!("{name}.md"),
            size: 512 * 1024,
        }
    }

    fn names<'a>(
        all: &'a [RomEntry],
        indices: impl IntoIterator<Item = &'a usize>,
    ) -> Vec<&'a str> {
        indices
            .into_iter()
            .map(|index| all[*index].name.as_str())
            .collect()
    }

    /// A stand-in for a real ToClO/Redump set: the tags the collection actually
    /// uses, in the order the server would serve them.
    fn collection() -> Vec<RomEntry> {
        [
            "Sonic The Hedgehog (USA, Europe)",
            "Sonic The Hedgehog 2 (World)",
            "Sonic 3D Blast (Japan)",
            "Streets of Rage (Europe)",
            "Toy Story (USA) (Rev A)",
        ]
        .into_iter()
        .map(entry)
        .collect()
    }

    #[test]
    fn an_empty_query_and_the_all_region_keep_everything_in_order() {
        let all = collection();
        let kept = filter_roms(&all, "", RegionFilter::All);
        assert_eq!(kept, vec![0, 1, 2, 3, 4]);
    }

    /// The ticket's own case: every word has to match, in any order, and the
    /// match is a substring of the name rather than a whole word.
    #[test]
    fn every_word_of_the_query_has_to_match() {
        let all = collection();
        let kept = filter_roms(&all, "sonic 2", RegionFilter::All);
        assert_eq!(names(&all, &kept), ["Sonic The Hedgehog 2 (World)"]);
        let kept = filter_roms(&all, "2 sonic", RegionFilter::All);
        assert_eq!(names(&all, &kept), ["Sonic The Hedgehog 2 (World)"]);
    }

    #[test]
    fn the_query_is_case_insensitive() {
        let all = collection();
        let kept = filter_roms(&all, "SONIC THE", RegionFilter::All);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn a_query_no_name_matches_is_empty() {
        let all = collection();
        assert!(filter_roms(&all, "sonic 5", RegionFilter::All).is_empty());
    }

    #[test]
    fn surrounding_whitespace_in_the_query_is_ignored() {
        let all = collection();
        assert_eq!(
            filter_roms(&all, "  sonic   2  ", RegionFilter::All),
            filter_roms(&all, "sonic 2", RegionFilter::All)
        );
    }

    /// The region chips read the same parentheses as [`parse_tags`], so `(USA,
    /// Europe)` is two tags and counts for both chips, and `(World)` counts for
    /// all three.
    #[test]
    fn region_chips_use_the_parenthesised_tags() {
        let all = collection();
        let usa = filter_roms(&all, "", RegionFilter::Usa);
        assert_eq!(
            names(&all, &usa),
            [
                "Sonic The Hedgehog (USA, Europe)",
                "Sonic The Hedgehog 2 (World)",
                "Toy Story (USA) (Rev A)",
            ]
        );
        let europe = filter_roms(&all, "", RegionFilter::Europe);
        assert_eq!(
            names(&all, &europe),
            [
                "Sonic The Hedgehog (USA, Europe)",
                "Sonic The Hedgehog 2 (World)",
                "Streets of Rage (Europe)",
            ]
        );
        let japan = filter_roms(&all, "", RegionFilter::Japan);
        assert_eq!(
            names(&all, &japan),
            ["Sonic The Hedgehog 2 (World)", "Sonic 3D Blast (Japan)",]
        );
    }

    /// A region that names no tag at all is a game the chips cannot place, so it
    /// belongs to no region rather than to all of them - except `World`, which is
    /// a region a dump really carries.
    #[test]
    fn an_untagged_entry_is_in_no_region() {
        let all = [entry("Air Buster"), entry("Air Buster (World)")];
        assert_eq!(filter_roms(&all, "air", RegionFilter::Usa), vec![1]);
        assert_eq!(filter_roms(&all, "air", RegionFilter::All), vec![0, 1]);
    }

    #[test]
    fn a_query_and_a_region_narrow_together() {
        let all = collection();
        let kept = filter_roms(&all, "sonic", RegionFilter::Japan);
        assert_eq!(
            names(&all, &kept),
            ["Sonic The Hedgehog 2 (World)", "Sonic 3D Blast (Japan)",]
        );
    }

    #[test]
    fn parse_tags_splits_a_title_from_its_groups() {
        let (title, tags) = parse_tags("Sonic The Hedgehog 2 (World) (Rev A)");
        assert_eq!(title, "Sonic The Hedgehog 2");
        assert_eq!(tags, ["World", "Rev A"]);
    }

    #[test]
    fn a_comma_inside_a_group_is_two_tags() {
        let (title, tags) = parse_tags("Sonic The Hedgehog (USA, Europe)");
        assert_eq!(title, "Sonic The Hedgehog");
        assert_eq!(tags, ["USA", "Europe"]);
    }

    #[test]
    fn a_name_with_no_group_is_all_title() {
        let (title, tags) = parse_tags("Air Buster");
        assert_eq!(title, "Air Buster");
        assert!(tags.is_empty());
    }

    /// Names in the wild are not always well formed, and a row that panicked or
    /// lost its title is worse than one that kept the parenthesis.
    #[test]
    fn parse_tags_survives_names_that_are_not_well_formed() {
        let cases = [
            "Unbalanced (USA",
            "Unbalanced USA)",
            "Empty ()",
            "(Only a tag)",
            "Nested (USA (Europe))",
        ];
        for name in cases {
            let (title, tags) = parse_tags(name);
            assert!(!title.is_empty(), "{name:?} lost its title entirely");
            assert!(
                !title.contains("()"),
                "{name:?} still has an empty group in its title"
            );
            assert!(
                tags.iter().all(|tag| !tag.trim().is_empty()),
                "{name:?} produced an empty tag"
            );
        }
        assert_eq!(parse_tags("Empty ()").1, Vec::<String>::new());
        assert_eq!(parse_tags(""), (String::new(), Vec::new()));
    }

    #[test]
    fn tag_kinds_separate_a_region_from_a_revision_and_a_warning() {
        assert_eq!(tag_kind("USA"), TagKind::Region);
        assert_eq!(tag_kind("World"), TagKind::Region);
        assert_eq!(tag_kind("Rev A"), TagKind::Revision);
        assert_eq!(tag_kind("Beta"), TagKind::Status);
        assert_eq!(tag_kind("Proto"), TagKind::Status);
        assert_eq!(tag_kind("En"), TagKind::Other);
        assert!(tag_kind("Beta").is_muted());
        assert!(!tag_kind("USA").is_muted());
    }

    #[test]
    fn visible_window_at_the_top_starts_at_the_first_row() {
        let window = visible_window(1000, 0.0, 40.0, 400.0, 3);
        assert_eq!(window, 0..16);
    }

    #[test]
    fn visible_window_in_the_middle_is_centred_on_the_scroll() {
        // Scrolled to row 100 of 40px: overscan reaches back to 97 and the
        // viewport holds 10, so 3 + 10 + 3 = 97..113.
        assert_eq!(visible_window(1000, 4000.0, 40.0, 400.0, 3), 97..113);
    }

    #[test]
    fn visible_window_at_the_end_stops_at_the_last_row() {
        // The last row of ten 40px rows, in a 400px viewport.
        assert_eq!(visible_window(10, 360.0, 40.0, 400.0, 3), 6..10);
        // And a scroll the browser cannot actually reach still ends at the last
        // row rather than past it.
        assert_eq!(visible_window(10, 400.0, 40.0, 400.0, 3), 7..10);
    }

    #[test]
    fn visible_window_of_an_empty_list_is_empty() {
        assert_eq!(visible_window(0, 0.0, 40.0, 400.0, 3), 0..0);
    }

    /// A search narrows the list while the scroll is still down the page: the
    /// browser has not retargeted `scrollTop` yet, and drawing nothing on that one
    /// frame is a blank list rather than a fast one.
    #[test]
    fn visible_window_of_a_stale_scroll_still_draws_a_row() {
        let window = visible_window(10, 100_000.0, 40.0, 400.0, 3);
        assert!(!window.is_empty());
        assert_eq!(window.end, 10);
    }

    #[test]
    fn visible_window_is_never_out_of_bounds() {
        for (total, scroll) in [
            (1usize, 0.0),
            (1, 1000.0),
            (3, 0.0),
            (100, -12.0),
            (2, 79.5),
        ] {
            let window = visible_window(total, scroll, ROW_HEIGHT_PX, 320.0, OVERSCAN);
            assert!(window.end <= total, "{total} {scroll}: {window:?}");
            assert!(window.start <= window.end, "{total} {scroll}: {window:?}");
        }
    }

    /// A row height of nothing, and a NaN from a scroll position the browser has
    /// not measured, are both answers the view must not panic over.
    #[test]
    fn visible_window_refuses_a_row_it_cannot_measure() {
        assert_eq!(visible_window(100, 0.0, 0.0, 400.0, 3), 0..0);
        assert_eq!(visible_window(100, 0.0, f64::NAN, 400.0, 3), 0..0);
        assert_eq!(visible_window(100, f64::NAN, 40.0, 400.0, 3), 0..0);
    }

    /// A row of the window is a row of the *filtered* list: the entry it shows is
    /// `shown[row]` steps into the listing, not the listing's own `row`.
    #[test]
    fn a_row_of_the_window_is_a_row_of_the_filtered_list() {
        let all = collection();
        let shown = filter_roms(&all, "sonic", RegionFilter::Japan);
        assert_eq!(entry_at(&all, &shown, 0).map(|entry| entry.name.as_str()), Some("Sonic The Hedgehog 2 (World)"));
        assert_eq!(entry_at(&all, &shown, 1).map(|entry| entry.name.as_str()), Some("Sonic 3D Blast (Japan)"));
        assert_eq!(entry_at(&all, &shown, 2), None);
        // And past the end of a window the list simply stops.
        assert_eq!(entry_at(&all, &shown, 99), None);
    }

    /// The whole chain one row of the list is drawn from: `filter_roms`, then
    /// `visible_window`, then [`entry_at`]. A row of a searched list shows the entry
    /// that search left at that place, and a row launches the entry it shows - so this
    /// is what keeps "press Enter on `Sonic 2`" from starting the first file in the
    /// directory instead.
    #[test]
    fn a_row_of_a_searched_list_names_the_game_it_launches() {
        let all = collection();
        let cases = [
            ("", RegionFilter::All),
            ("sonic", RegionFilter::All),
            ("sonic 2", RegionFilter::All),
            ("", RegionFilter::Europe),
            ("sonic", RegionFilter::Japan),
            ("nothing matches this", RegionFilter::All),
        ];
        for (query, region) in cases {
            let shown = filter_roms(&all, query, region);
            let window = visible_window(shown.len(), 0.0, ROW_HEIGHT_PX, 400.0, OVERSCAN);
            for row in window {
                let entry = entry_at(&all, &shown, row)
                    .unwrap_or_else(|| panic!("{query:?} {region:?}: row {row} has no entry"));
                // The entry of the row is the entry `shown` names at that place,
                // and it is one the search and the chip both left in.
                assert_eq!(all[shown[row]].id, entry.id, "{query:?} {region:?} row {row}");
                let name = entry.name.to_lowercase();
                assert!(
                    query
                        .to_lowercase()
                        .split_whitespace()
                        .all(|word| name.contains(word)),
                    "{query:?} row {row} shows {entry:?}, which the search would not leave"
                );
                assert!(
                    region.matches(&entry.name),
                    "{region:?} row {row} shows {}, which is not in that region",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn sizes_are_said_in_binary_steps() {
        let cases = [
            (0, "0 B"),
            (999, "999 B"),
            (1024, "1 KB"),
            (512 * 1024, "512 KB"),
            (1024 * 1024, "1 MB"),
            (1572864, "1.5 MB"),
            (8 * 1024 * 1024, "8 MB"),
        ];
        for (bytes, want) in cases {
            assert_eq!(format_size(bytes), want, "{bytes} bytes");
        }
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        let cases = [
            (0, "0"),
            (9, "9"),
            (999, "999"),
            (1000, "1,000"),
            (2775, "2,775"),
            (1234567, "1,234,567"),
        ];
        for (count, want) in cases {
            assert_eq!(format_count(count), want, "{count}");
        }
    }

    /// The shape the server actually sends (M8-03, #157): a list of these, sorted
    /// by name. Deserializing the real document is what keeps the two crates honest
    /// about the four field names.
    #[test]
    fn an_entry_reads_the_servers_json() {
        let json = r#"[{"id":"2f1a","name":"Sonic The Hedgehog 2 (World)","file":"Sonic The Hedgehog 2 (World).md","size":524288}]"#;
        let entries: Vec<RomEntry> = serde_json::from_str(json).expect("the listing parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "2f1a");
        assert_eq!(entries[0].name, "Sonic The Hedgehog 2 (World)");
        assert_eq!(entries[0].file, "Sonic The Hedgehog 2 (World).md");
        assert_eq!(entries[0].size, 524288);
    }
}
