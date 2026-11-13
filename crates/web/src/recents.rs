//! Recent ROMs: what the empty state lists, and how that list changes (M7-16, #149).
//!
//! Two halves, in the shape [`crate::slots`] and [`crate::ui::slot_panel`] set:
//! this module is the decisions - what one card says, what the list does when a
//! ROM is opened, what a damaged record costs - and [`crate::ui::recents_grid`] is
//! the half that draws the cards and answers their two buttons.
//!
//! The bytes and the list are deliberately apart. A ROM image goes in the `roms`
//! object store at [`crate::storage::rom_key`], one record per game, and the list
//! of what is stored goes in `localStorage` at [`STORAGE_KEY`] as a few hundred
//! bytes of JSON. That split is what lets the empty state paint twelve cards
//! without reading twelve ROM images: a card is a title, a region, and a time, and
//! none of that needs the megabytes behind it. The list is in `localStorage`
//! rather than in a `meta` record for the same reason [`crate::settings`] is:
//! reading it is synchronous, so the first frame of the page already has its cards
//! and nothing has to be awaited on the way to a picture.
//!
//! An entry is only ever worth having if its bytes are. So [`Recents::record`]
//! writes the image first and only then lists it, and the two ways a card goes
//! away - the remove button and eviction from the cap - both delete the image.

use leptos::prelude::*;
use serde::Deserialize;
use serde::Serialize;
use wasm_bindgen_futures::spawn_local;

use crate::rom;
use crate::rom::LoadedRom;
use crate::slots;
use crate::storage;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// How many games the empty state remembers: the 12 of the ticket.
///
/// Twelve because a player's working set is a handful of games and the images are
/// megabytes each, so the cap is also the size of the store: 12 images of at most
/// [`rom::MAX_ROM_BYTES`] is the whole of what rustsega keeps in this browser.
pub const MAX_RECENTS: usize = 12;

/// The `localStorage` key the list lives under, versioned like
/// [`crate::settings::STORAGE_KEY`].
pub const STORAGE_KEY: &str = "rustsega.recents.v1";

/// The version this build writes, and the only one [`from_json`] promises
/// field-by-field understanding of.
pub const RECENTS_VERSION: u32 = 1;

/// What a card says when a stored entry carries neither a header title nor a file
/// name, which only a hand-edited record can manage: [`rom::LoadedRom::title` is
/// never empty.
pub const UNKNOWN_TITLE: &str = "Unknown game";

/// The one sentence for a card whose bytes have gone: the list said this game was
/// here and the `roms` store says otherwise, which is a browser that cleared
/// itself, not a bug in the load path.
pub const NO_LONGER_STORED: &str = "That game is no longer stored in this browser - open it again";

/// What one recent game's card shows, and the whole of what is read to draw it.
///
/// `serde` names are part of the file, like [`crate::settings::Settings`]'s: they
/// outlive this build. Every field is `#[serde(default)]` so a record written by
/// an older build keeps the fields it has, and an entry that cannot be read at all
/// is dropped rather than being a failure to read the list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentMeta {
    /// The ROM's [`rom::LoadedRom::id`]: SHA-1 of the image, and the key its bytes
    /// are stored under. An entry without one can be neither launched nor
    /// deleted, so it is not kept.
    #[serde(default)]
    pub id: String,
    /// The header title, as [`rom::load_rom`] worked it out.
    #[serde(default)]
    pub title: String,
    /// The name the file arrived with, kept because a card's load goes back
    /// through [`rom::load_rom`], which wants a file name for a header with no
    /// title in it.
    #[serde(default)]
    pub file_name: String,
    /// The regions the header claims, for the card's badge, or `"Unknown"`.
    #[serde(default)]
    pub region: String,
    /// The size of the stored image in bytes, of the ticket's record shape. The
    /// card does not show it; the record is what says how much is stored.
    #[serde(default)]
    pub size: u64,
    /// When this game was last opened, milliseconds since the epoch on the clock of
    /// [`slots::clock_ms`], so the card's time and a slot's time are on one ruler.
    /// `0` means "not recorded", which shows as no time at all.
    #[serde(default)]
    pub last_played_ms: u64,
}

impl RecentMeta {
    /// The name to put on the card: the header title, the file name when a record
    /// has only that, and [`UNKNOWN_TITLE`] when a hand-edited record has neither.
    pub fn display_title(&self) -> &str {
        if !self.title.is_empty() {
            &self.title
        } else if !self.file_name.is_empty() {
            &self.file_name
        } else {
            UNKNOWN_TITLE
        }
    }
}

/// The card record for a ROM that was just opened, stamped with `now_ms` - the
/// clock of [`slots::clock_ms`], so a recent card and a save state card say the
/// same thing about the same minute.
pub fn meta_of(rom: &LoadedRom, now_ms: f64) -> RecentMeta {
    RecentMeta {
        id: rom.id.clone(),
        title: rom.title.clone(),
        file_name: rom.file_name.clone(),
        region: rom.region.clone(),
        // The image as it was read, which is exactly what goes to IndexedDB.
        size: rom.bytes.len() as u64,
        // A clock that has gone backwards stamps 0, which the card reads as "no
        // time recorded" rather than as 1970.
        last_played_ms: now_ms.max(0.0) as u64,
    }
}

/// `list` with `new` opened: newest first, and the ids whose bytes are no longer
/// listed come back for deletion.
///
/// Three rules, all of them the ticket's:
///
/// * a ROM that was not in the list goes at the front;
/// * a ROM that was moves to the front, once, with `new`'s data - reopening a game
///   is not a second entry for it;
/// * the list is capped at `max`, and everything that fell off it - including
///   `new` itself when `max` is 0 - is returned so its bytes go with it.
///
/// The evicted ids come back once each, in the order they were listed.
pub fn update_recents(
    mut list: Vec<RecentMeta>,
    new: RecentMeta,
    max: usize,
) -> (Vec<RecentMeta>, Vec<String>) {
    // Every id that was in play before the edit, in order, the new one last: what
    // is left out of `list` at the end is what the caller has to delete, and
    // taking the ids first is what keeps this from being a second borrow of the
    // entries it is about to drop.
    let mut before: Vec<String> = list.iter().map(|item| item.id.clone()).collect();
    before.push(new.id.clone());

    list.retain(|item| item.id != new.id);
    list.insert(0, new);
    list.truncate(max);

    let evicted = before
        .iter()
        .filter(|id| !list.iter().any(|item| item.id == **id))
        // Once each: a list that named one game twice - a hand-edited file, or a
        // writer that raced itself - must not delete its bytes twice either.
        .fold(Vec::new(), |mut seen, id| {
            if !seen.contains(id) {
                seen.push(id.clone());
            }
            seen
        });
    (list, evicted)
}

/// The list stored in `text`, **never failing** and never panicking.
///
/// Garbage is no recents, an unreadable entry costs that entry alone, an id that
/// is there twice is one card, and the list is capped at [`MAX_RECENTS`] however
/// many the file named. This runs while the page is still coming up, on the same
/// page that #387 makes unrecoverable, so there is no error to handle.
pub fn from_json(text: &str) -> Vec<RecentMeta> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(items) = value.get("items").and_then(|items| items.as_array()) else {
        return Vec::new();
    };
    let mut list: Vec<RecentMeta> = Vec::new();
    for item in items {
        // One entry that cannot be read costs itself. It cannot be field-salvaged
        // the way a `Settings` can, because the entry is the smallest thing here
        // that means anything: a card with only an id is a game that can be
        // launched, and a card with no id is not a card.
        let Ok(meta) = serde_json::from_value::<RecentMeta>(item.clone()) else {
            continue;
        };
        if meta.id.is_empty() || list.iter().any(|kept| kept.id == meta.id) {
            continue;
        }
        list.push(meta);
        if list.len() == MAX_RECENTS {
            break;
        }
    }
    list
}

pub fn to_json(items: &[RecentMeta]) -> String {
    /// The document: [`RECENTS_VERSION`] and the entries, which is what
    /// [`from_json`] reads back.
    #[derive(Serialize)]
    struct Document<'a> {
        version: u32,
        items: &'a [RecentMeta],
    }

    // A list is data all the way down, so encoding cannot fail; `unwrap_or_default`
    // keeps that promise rather than handing the caller a `Result` it can only
    // ignore, exactly as `Settings::to_json` does.
    serde_json::to_string(&Document {
        version: RECENTS_VERSION,
        items,
    })
    .unwrap_or_default()
}

/// The card's one line of time: what [`slots::relative_time`] says about when this
/// game was last opened, or nothing at all for a record whose time was never
/// recorded - a card reading "19705 days ago" is a card that is lying.
pub fn played_label(now_ms: f64, meta: &RecentMeta) -> String {
    if meta.last_played_ms == 0 {
        return String::new();
    }
    slots::relative_time(now_ms, meta.last_played_ms as f64)
}

/// The shell's handle on the games this browser can relaunch (M7-16, #149).
///
/// `Copy` over signals, like [`crate::emulator::Slots`] and
/// [`crate::ui::slot_panel::SlotCards`]: the app shell writes it when a cartridge
/// opens, the empty state reads it to draw cards, and a spawned storage write has
/// to be able to say that a game is no longer there - and a [`Callback`] can hold
/// neither a `LoadedRom` (megabytes) nor a borrow of the console.
#[derive(Clone, Copy)]
pub struct Recents {
    /// The list, newest first: what the empty state draws its cards from, and the
    /// whole of what is read to draw them. Never longer than [`MAX_RECENTS`].
    pub items: RwSignal<Vec<RecentMeta>>,
    /// Where a storage failure is reported, there being no caller to hand it to.
    toasts: Toasts,
}

impl Recents {
    /// The recents this browser profile holds, read now - synchronously, from
    /// `localStorage` - so the empty state has its cards on the first frame
    /// rather than after an IndexedDB round trip it does not need.
    pub fn new(toasts: Toasts) -> Self {
        Self {
            items: RwSignal::new(load_local()),
            toasts,
        }
    }

    /// `rom` has been opened: it goes to the front of the list, and its bytes go
    /// to the `roms` store so the card can be clicked after a reload.
    ///
    /// The image is written first and the list only afterwards, because a card is
    /// only ever worth having if its bytes are: a `QuotaExceededError` is a toast
    /// and a game that carries on playing, not a list of cards that cannot
    /// launch. The one case it does not write is a ROM this list already names -
    /// a listed entry is a written entry, since the write is what got it listed -
    /// which is what keeps relaunching a game from a card from rewriting a
    /// megabyte image every time it is clicked.
    pub fn record(self, rom: LoadedRom) {
        let meta = meta_of(&rom, slots::clock_ms());
        let LoadedRom { bytes, id, .. } = rom;
        // Read before anything is awaited, from the list as the shell has it.
        let already_stored = self
            .items
            .with_untracked(|items| items.iter().any(|item| item.id == id));
        let toasts = self.toasts;
        spawn_local(async move {
            let db = match storage::open().await {
                Ok(db) => db,
                Err(error) => {
                    // No database means no list worth writing either: the cards
                    // would be promises nothing can keep. Told once, because a
                    // private-mode player gets this answer once per open.
                    toasts.push(ToastKind::Warn, error.to_string());
                    return;
                }
            };
            if !already_stored
                && let Err(error) = db
                    .put(storage::Store::Roms, &storage::rom_key(&id), &bytes)
                    .await
            {
                // `StoreError::Write` is the quota sentence, and it is the whole
                // of what changes for the player: the game is running and this
                // browser will not have kept it for next time.
                toasts.push(ToastKind::Danger, error.to_string());
                return;
            }
            // The list as of now, not as of the `await`: two opens cannot
            // interleave here anyway, because publishing the list is synchronous
            // and only its evictions are spawned.
            let (items, evicted) = update_recents(self.items.get_untracked(), meta, MAX_RECENTS);
            self.publish(items, evicted);
        });
    }

    /// Takes `id` out of the list and deletes the bytes it named.
    pub fn remove(self, id: &str) {
        let id = id.to_string();
        let items: Vec<RecentMeta> = self
            .items
            .get_untracked()
            .into_iter()
            .filter(|item| item.id != id)
            .collect();
        self.publish(items, vec![id]);
    }

    /// The stored image of `id`, put back on the load path.
    ///
    /// `roms` is the cartridge signal the file picker and the drop target write,
    /// and writing it is the whole of what a launch does: [`crate::emulator`] is
    /// what installs the cart's SRAM (#147), chooses the renderer (#146) and takes
    /// the load-is-current token, so a shortcut into a console start would have to
    /// learn all three again and would get none of them in step with the next
    /// ticket that changes one.
    ///
    /// Bytes are copied out of storage and the signal written after the `await`,
    /// never across it (#387).
    pub fn launch(self, roms: RwSignal<Option<LoadedRom>>, id: &str) {
        let id = id.to_string();
        // The name the file arrived with, for a header with no title in it: read
        // now, from the record being launched, rather than after a round trip that
        // may have let the list move.
        let file_name = self.items.with_untracked(|items| {
            items
                .iter()
                .find(|item| item.id == id)
                .map(|item| item.file_name.clone())
                .unwrap_or_default()
        });
        let toasts = self.toasts;
        spawn_local(async move {
            let db = match storage::open().await {
                Ok(db) => db,
                Err(error) => {
                    toasts.push(ToastKind::Warn, error.to_string());
                    return;
                }
            };
            let bytes = match db.get(storage::Store::Roms, &storage::rom_key(&id)).await {
                Ok(Some(bytes)) => bytes,
                // The list named a game the store has not got: a browser that
                // cleared itself, or a hand-edited key. The card is the lie, so
                // the card goes, and the player is told once.
                Ok(None) => {
                    self.remove(&id);
                    toasts.push(ToastKind::Warn, NO_LONGER_STORED);
                    return;
                }
                Err(error) => {
                    toasts.push(ToastKind::Warn, error.to_string());
                    return;
                }
            };
            match rom::load_rom(&file_name, bytes) {
                Ok(rom) => roms.set(Some(rom)),
                // Bytes that were a ROM when they were written and are not one
                // now are a damaged record, and `load_rom` is the judge of that -
                // the same sentence a dropped file gets for the same reason.
                Err(_) => toasts.push(ToastKind::Danger, rom::NOT_A_ROM),
            }
        });
    }

    /// Lists `items` - the signal and `localStorage` - and asks the browser to
    /// forget whatever `evicted` names.
    ///
    /// The list is written synchronously and the bytes go asynchronously, which is
    /// the order that loses nothing: from here on those ids are not on any card,
    /// so a delete that has not landed is nobody's truth to contradict.
    fn publish(self, items: Vec<RecentMeta>, evicted: Vec<String>) {
        save_local(&items);
        self.items.set(items);
        if evicted.is_empty() {
            return;
        }
        spawn_local(async move {
            let Ok(db) = storage::open().await else {
                return;
            };
            for id in evicted {
                if let Err(error) = db
                    .delete(storage::Store::Roms, &storage::rom_key(&id))
                    .await
                {
                    // Console-only. The bytes are unreachable either way, and a
                    // toast about a removal that the player can see has happened
                    // would say the opposite of what the card now says.
                    report(&format!(
                        "rustsega: the stored image of {id} could not be deleted: {error}"
                    ));
                }
            }
        });
    }
}

/// The recents this browser profile keeps, or nothing at all.
///
/// Native builds - the ones `cargo test -p web` runs - have no `localStorage`, so
/// they get the same answer as a browser that has never opened a ROM, exactly as
/// [`crate::settings::Settings::load_local`] does.
fn load_local() -> Vec<RecentMeta> {
    #[cfg(target_arch = "wasm32")]
    {
        let text = crate::settings::storage()
            .and_then(|storage| storage.get_item(STORAGE_KEY).ok().flatten());
        match text {
            Some(text) => from_json(&text),
            None => Vec::new(),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    Vec::new()
}

/// Saves the list to this browser profile, ignoring a write the browser refused.
///
/// A full `localStorage` is not worth losing the session over: the game in front
/// of the player is the thing that matters, and the list is still on screen.
#[cfg(target_arch = "wasm32")]
fn save_local(items: &[RecentMeta]) {
    if let Some(storage) = crate::settings::storage() {
        let _ = storage.set_item(STORAGE_KEY, &to_json(items));
    }
}

/// Native builds have no `localStorage` to write to, which is the same fact
/// [`load_local`] states by returning nothing.
#[cfg(not(target_arch = "wasm32"))]
fn save_local(_items: &[RecentMeta]) {}

/// One line in the browser console, for the storage failure the player can do
/// nothing about: the same shape as `render::webgl::report`.
fn report(text: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(text));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listed game, distinguished from its neighbours by id alone.
    fn meta(id: &str) -> RecentMeta {
        RecentMeta {
            id: id.to_string(),
            title: id.to_uppercase(),
            file_name: format!("{id}.md"),
            region: "Europe".to_string(),
            size: 1024,
            last_played_ms: 1,
        }
    }

    /// The ids of a list, for asserting on order without spelling `RecentMeta`.
    fn ids(list: &[RecentMeta]) -> Vec<&str> {
        list.iter().map(|item| item.id.as_str()).collect::<Vec<_>>()
    }

    /// A game nobody has opened before is the newest thing in the list.
    #[test]
    fn a_new_rom_goes_at_the_front() {
        let (list, evicted) =
            update_recents(vec![meta("sonic"), meta("ctrail")], meta("gunchil"), 12);
        assert_eq!(ids(&list), ["gunchil", "sonic", "ctrail"]);
        assert!(evicted.is_empty());
    }

    /// Reopening a game moves it, it does not add it: three plays of the same two
    /// carts are two cards, and the one just played is the first of them.
    #[test]
    fn reopening_a_rom_moves_it_to_the_front_without_duplicating() {
        let (list, evicted) = update_recents(
            vec![meta("sonic"), meta("ctrail"), meta("gunchil")],
            meta("ctrail"),
            12,
        );
        assert_eq!(ids(&list), ["ctrail", "sonic", "gunchil"]);
        assert_eq!(list.len(), 3);
        assert!(evicted.is_empty());
    }

    /// The move takes the new record's data with it, not the one that was there:
    /// `last_played_ms` is the thing a card shows, and it has to be the time of
    /// this opening rather than of the last one.
    #[test]
    fn a_moved_entry_carries_the_new_record_not_the_old_one() {
        let mut again = meta("sonic");
        again.last_played_ms = 9_999;
        let (list, _evicted) = update_recents(vec![meta("ctrail"), meta("sonic")], again, 12);
        assert_eq!(list[0].last_played_ms, 9_999);
        assert_eq!(ids(&list), ["sonic", "ctrail"]);
    }

    /// The cap is what bounds the store, so the game that falls off it has to be
    /// the one whose bytes are deleted, and it has to be named for that to
    /// happen - a cap that quietly drops the oldest card leaves a ROM image in
    /// IndexedDB for ever.
    #[test]
    fn the_list_is_capped_and_the_overflow_comes_back_for_deletion() {
        let before = vec![
            meta("one"),
            meta("two"),
            meta("three"),
            meta("four"),
            meta("five"),
        ];
        let (list, evicted) = update_recents(before, meta("six"), 4);
        assert_eq!(ids(&list), ["six", "one", "two", "three"]);
        assert_eq!(evicted, ["four", "five"]);
    }

    /// The cap at the size the shell uses, from a full list: exactly one eviction,
    /// and it is the oldest.
    #[test]
    fn a_full_list_evicts_exactly_the_oldest() {
        let before: Vec<RecentMeta> = (0..MAX_RECENTS).map(|n| meta(&n.to_string())).collect();
        let (list, evicted) = update_recents(before, meta("new"), MAX_RECENTS);
        assert_eq!(list.len(), MAX_RECENTS);
        assert_eq!(list[0].id, "new");
        assert_eq!(evicted, [(MAX_RECENTS - 1).to_string()]);
    }

    /// A cap of one is a single card: the game just opened, and the previous one
    /// handed back for deletion.
    #[test]
    fn a_cap_of_one_keeps_only_the_newest() {
        let (list, evicted) =
            update_recents(vec![meta("sonic"), meta("ctrail")], meta("gunchil"), 1);
        assert_eq!(ids(&list), ["gunchil"]);
        assert_eq!(evicted, ["sonic", "ctrail"]);
    }

    /// A cap of zero stores nothing at all - and the game that did not fit comes
    /// back for deletion too, because the caller has already written its bytes by
    /// the time this runs and nothing else will ever offer them again.
    #[test]
    fn a_cap_of_zero_keeps_nothing_and_asks_for_the_new_id_too() {
        let (list, evicted) = update_recents(vec![meta("sonic")], meta("gunchil"), 0);
        assert!(list.is_empty());
        assert_eq!(evicted, ["sonic", "gunchil"]);

        let (list, evicted) = update_recents(Vec::new(), meta("gunchil"), 0);
        assert!(list.is_empty());
        assert_eq!(evicted, ["gunchil"]);
    }

    /// An empty list is the ordinary first case, not a special one.
    #[test]
    fn an_empty_list_starts_with_one_and_evicts_nothing() {
        let (list, evicted) = update_recents(Vec::new(), meta("sonic"), 12);
        assert_eq!(ids(&list), ["sonic"]);
        assert!(evicted.is_empty());
    }

    /// A list longer than its cap - a cap lowered by a build, or a list written by
    /// hand - is trimmed to it, and everything past the edge is offered for
    /// deletion once each.
    #[test]
    fn a_list_already_over_the_cap_is_trimmed_to_it() {
        let before = vec![meta("one"), meta("two"), meta("three"), meta("four")];
        let (list, evicted) = update_recents(before, meta("one"), 2);
        assert_eq!(ids(&list), ["one", "two"]);
        assert_eq!(evicted, ["three", "four"]);
    }

    /// Every shape of nonsense reads as "no recents" rather than as a failure, a
    /// panic, or half a list: this is the boot path of a page that cannot afford
    /// any of the three (#387).
    #[test]
    fn recents_of_garbage_are_no_recents() {
        for text in [
            "",
            "   ",
            "not json at all",
            "[]",
            "null",
            "\"sonic\"",
            "{}",
            "{\"items\":3}",
            "{\"items\":{}}",
            "{\"items\":[3,\"x\",null]}",
        ] {
            assert_eq!(from_json(text), Vec::new(), "for {text:?}");
        }
    }

    /// One unreadable entry costs itself and nothing else: the good cards either
    /// side of it are still listed, in order.
    #[test]
    fn one_bad_entry_costs_only_itself() {
        let text = r#"{"version":1,"items":[
            {"id":"sonic","title":"SONIC","file_name":"sonic.md","region":"Europe","size":1024,"last_played_ms":7},
            {"title":"NO ID"},
            {"id":"ctrail","size":"not a number"},
            42,
            {"id":"gunchil"}
        ]}"#;
        let list = from_json(text);
        assert_eq!(ids(&list), ["sonic", "gunchil"]);
        let sonic = &list[0];
        assert_eq!(sonic.title, "SONIC");
        assert_eq!(sonic.file_name, "sonic.md");
        assert_eq!(sonic.region, "Europe");
        assert_eq!(sonic.size, 1024);
        assert_eq!(sonic.last_played_ms, 7);
        // A record that made it in with only its id still renders, and not as a
        // forty character hex string: the card says it does not know the name.
        assert_eq!(list[1].display_title(), UNKNOWN_TITLE);
    }

    /// A field of the wrong shape inside an entry is not a whole list to throw
    /// away, but it is that entry: what is left has to be launchable, and a title
    /// or a region of the wrong type still describes a game that is stored.
    #[test]
    fn a_read_entry_keeps_the_fields_an_older_build_wrote() {
        let text = r#"{"items":[{"id":"sonic","title":"SONIC THE HEDGEHOG"}]}"#;
        let list = from_json(text);
        assert_eq!(
            list,
            vec![RecentMeta {
                id: "sonic".to_string(),
                title: "SONIC THE HEDGEHOG".to_string(),
                ..RecentMeta::default()
            }]
        );
    }

    /// What is written is what is read, and a card needs all six fields back to
    /// launch the game it is showing.
    #[test]
    fn the_stored_list_round_trips() {
        let items = vec![meta("sonic"), meta("ctrail")];
        let text = to_json(&items);
        assert_eq!(from_json(&text), items);
        assert!(text.contains(&format!("\"version\":{RECENTS_VERSION}")));
    }

    /// A hand-written list of fifteen is read as twelve, and one id written twice
    /// is one card: two cards for one ROM is two cards that disagree about
    /// whether the game is stored.
    #[test]
    fn a_read_is_capped_and_never_lists_one_rom_twice() {
        let mut entries: Vec<String> = (0..15).map(|n| format!(r#"{{"id":"{n}"}}"#)).collect();
        entries.push(r#"{"id":"0"}"#.to_string());
        let text = format!(
            r#"{{"version":{RECENTS_VERSION},"items":[{}]}}"#,
            entries.join(",")
        );
        let list = from_json(&text);
        assert_eq!(list.len(), MAX_RECENTS);
        let mut found = ids(&list);
        found.sort_unstable();
        let mut unique = found.clone();
        unique.dedup();
        assert_eq!(found, unique);
    }

    /// A card's name: the header title, or the file name for a record that has
    /// only that, or something rather than a blank card.
    #[test]
    fn a_card_is_named_by_its_title_then_its_file_name() {
        assert_eq!(meta("sonic").display_title(), "SONIC");
        let only_file = RecentMeta {
            id: "x".to_string(),
            file_name: "homebrew.md".to_string(),
            ..RecentMeta::default()
        };
        assert_eq!(only_file.display_title(), "homebrew.md");
        assert_eq!(RecentMeta::default().display_title(), UNKNOWN_TITLE);
    }

    /// A card is only any use if it can find the game again: the id is the ROM's
    /// own SHA-1, the size is the image's, and the rest is what the card shows.
    #[test]
    fn the_record_of_an_opened_rom_is_its_identity_and_its_size() {
        let rom = loaded_rom("SONIC THE HEDGEHOG", "sonic.md");
        let entry = meta_of(&rom, 1_700_000_000_000.0);
        assert_eq!(entry.id, rom.id);
        assert_eq!(entry.title, "SONIC THE HEDGEHOG");
        assert_eq!(entry.file_name, "sonic.md");
        assert_eq!(entry.region, "Unknown");
        assert_eq!(entry.size, rom.bytes.len() as u64);
        assert_eq!(entry.last_played_ms, 1_700_000_000_000);
    }

    /// The 512 byte image of `rom`'s own tests: a header at 0x100 is all
    /// [`rom::load_rom`] asks for, and this way the record is built from a real
    /// [`LoadedRom`] rather than from a second parser.
    fn loaded_rom(title: &str, file_name: &str) -> LoadedRom {
        let mut bytes = vec![0u8; 0x200];
        bytes[0x100..0x200].fill(b' ');
        bytes[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        bytes[0x150..0x150 + title.len()].copy_from_slice(title.as_bytes());
        rom::load_rom(file_name, bytes).expect("a header is a ROM")
    }

    /// The time line of a card is the shared relative-time formatter, on the same
    /// clock a save state's card uses.
    #[test]
    fn the_last_played_line_is_the_shared_relative_time() {
        let minute = 60_000.0;
        let mut entry = meta("sonic");
        entry.last_played_ms = 1_000;
        assert_eq!(played_label(1_000.0 + 5.0 * minute, &entry), "5 min ago");
    }

    /// A record whose time was never recorded says nothing rather than quoting
    /// 1970 at the player.
    #[test]
    fn an_unrecorded_last_played_says_nothing() {
        let unknown = RecentMeta {
            last_played_ms: 0,
            ..meta("sonic")
        };
        assert_eq!(played_label(1_700_000_000_000.0, &unknown), "");
    }
}
