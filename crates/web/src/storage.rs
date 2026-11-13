//! Persistence: the IndexedDB layer every saved byte of rustsega goes through
//! (M7-14, #147).
//!
//! The database is [`DB_NAME`] at [`DB_VERSION`], and it carries all three of the
//! object stores later tickets need - [`Store::Sram`], [`Store::States`] and
//! [`Store::Roms`] - created together at version 1 so that save states (M7-15)
//! and the ROM library (M7-16) do not need a schema upgrade of their own.
//!
//! Like the rest of the crate this is two halves in one file. The decisions -
//! what a save is keyed by, when a write is worth making - are pure and run
//! natively; [`open`] and [`Db`] are the browser half, over
//! [`rexie`](https://docs.rs/rexie), which is the crate the ticket names and
//! which builds against the workspace's pinned `wasm-bindgen`.
//!
//! Every byte of this is stored under a ROM's [`crate::rom::LoadedRom::id`], the
//! SHA-1 of the image, so two dumps of one game (a release and its `Rev A`) are
//! two different saves. That is deliberate: a save made under `Rev A` is not
//! guaranteed to mean anything to the dump it was revised from.

use std::cell::RefCell;
use std::rc::Rc;

use rexie::ObjectStore;
use rexie::Rexie;
use rexie::TransactionMode;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

/// The name of the IndexedDB database every rustsega save lives in.
pub const DB_NAME: &str = "rustsega";

/// Its schema version: 1, which already holds all three of [`Store`].
///
/// A bump is a `versionchange` that every other tab holding the old version has
/// to release before it applies, so it belongs to a ticket that really needs a
/// schema change rather than to whichever ticket wants to read something.
pub const DB_VERSION: u32 = 1;

/// The three object stores of [`DB_NAME`], all three created now so that save
/// states (M7-15) and the ROM library (M7-16) need no upgrade of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    /// Battery-backed cartridge RAM: one record per ROM, at [`sram_key`].
    Sram,
    /// Save states: any number per ROM, at [`state_key`].
    States,
    /// ROM images, for a library the browser can reopen offline: one record per
    /// ROM, at [`rom_key`].
    Roms,
}

impl Store {
    /// Every store version [`DB_VERSION`] creates, which is what [`open`] asks
    /// IndexedDB for so no ticket has to remember the list.
    pub const ALL: [Self; 3] = [Self::Sram, Self::States, Self::Roms];

    /// The store's name in IndexedDB.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sram => "sram",
            Self::States => "states",
            Self::Roms => "roms",
        }
    }
}

/// The key a cart's battery-backed RAM is stored under: the ROM id on its own,
/// because a cart has exactly one SRAM image.
pub fn sram_key(rom_id: &str) -> String {
    rom_id.to_string()
}

/// The key one save state is stored under: the ROM id and the slot number, so a
/// ROM's states are found together and each slot replaces only itself.
pub fn state_key(rom_id: &str, slot: u32) -> String {
    format!("{rom_id}:{slot}")
}

/// The key a ROM image is stored under: the ROM id on its own, which is what
/// makes the image self-identifying - a cached ROM that does not hash to its own
/// key is not that ROM.
pub fn rom_key(rom_id: &str) -> String {
    rom_id.to_string()
}

/// The hash [`SramWatcher`] compares SRAM images by: 64-bit
/// [FNV-1a](http://www.isthe.com/chongo/tech/comp/fnv/) - the ticket's "cheap
/// hash".
///
/// Cheap in both senses, which is the point: it runs over the whole of a cart's
/// RAM once a second on the frame loop, and it is only ever deciding "did these
/// bytes change", never identity across machines - SHA-1 of [`crate::rom`] is
/// what identity is. So a non-cryptographic hash with no allocation is the right
/// tool, and an 8-byte collision over a two-second window is not a lost save.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The 64-bit FNV-1a offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// The 64-bit FNV-1a prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// How long a change is held back before it is written: the 2 s of the ticket.
pub const SRAM_FLUSH_INTERVAL_MS: f64 = 2_000.0;

/// The thing that decides when a cart's RAM is worth writing out.
///
/// A game can dirty its SRAM on a timer of its own, so comparing on every check
/// is a write per check; the watcher coalesces, holding changes for
/// [`SRAM_FLUSH_INTERVAL_MS`] and then writing the newest bytes it has seen.
///
/// Pure, and given the clock rather than reading one, so that the whole of the
/// policy runs natively. The owner calls [`SramWatcher::should_flush`] about once
/// a second with the cart's current RAM and writes what it says to write - see
/// `crate::emulator`, which is the only owner there is.
#[derive(Clone, Debug)]
pub struct SramWatcher {
    /// The hash of the bytes last handed to storage, `None` until the first
    /// flush. Seeded by [`SramWatcher::new`] with the RAM the cart started with,
    /// so a cart that nobody saved into is never written at all.
    written: Option<u64>,
    /// When [`SramWatcher::written`] was set, on the same clock the caller
    /// passes to [`SramWatcher::should_flush`].
    written_at_ms: f64,
    /// Whether the next changed bytes are due immediately: see [`SramWatcher::force`].
    forced: bool,
}

impl SramWatcher {
    /// A watcher for a cart whose RAM is `initial` right now, at `now_ms`.
    ///
    /// That is the RAM as it stands after the save was loaded into the console -
    /// so "unchanged since the cart was installed" is "nothing to write", which
    /// is what keeps a load from writing its own bytes straight back.
    pub fn new(initial: &[u8], now_ms: f64) -> Self {
        Self {
            written: Some(fnv1a_64(initial)),
            written_at_ms: now_ms,
            forced: false,
        }
    }

    /// Whether `sram` has to be written out now.
    ///
    /// Three ways it is not: `sram` is empty - a cart with no battery-backed RAM
    /// reaches here, and an empty record would blank that ROM's real save - or
    /// its hash is the one last handed over, or it is not yet [`SRAM_FLUSH_INTERVAL_MS`]
    /// since it was and nothing [`SramWatcher::force`]d.
    ///
    /// Saying yes spends the decision: the watcher records these bytes as
    /// written, so the caller is asked again only if they move. A write that then
    /// fails is the caller's to report - [`crate::emulator`] toasts it - and is
    /// not retried until the bytes change again, which is the right amount of
    /// noise for a full disk.
    pub fn should_flush(&mut self, sram: &[u8], now_ms: f64) -> bool {
        if sram.is_empty() {
            return false;
        }
        let hash = fnv1a_64(sram);
        if Some(hash) == self.written {
            return false;
        }
        if !self.forced && now_ms - self.written_at_ms < SRAM_FLUSH_INTERVAL_MS {
            return false;
        }
        self.written = Some(hash);
        self.written_at_ms = now_ms;
        self.forced = false;
        true
    }

    /// Write the next changed bytes without waiting for the window: a ROM switch
    /// and a page that is closing or hidden are each one chance, not a schedule.
    pub fn force(&mut self) {
        self.forced = true;
    }
}

/// What went wrong with storage, in the one sentence a toast shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The database would not open: private mode, a blocked origin, or a version
    /// another tab is holding. Nothing can be persisted from here, and the game
    /// still runs.
    Unavailable,
    /// A record could not be read.
    Read,
    /// A record could not be written: the quota is the practical reason.
    Write,
    /// A record is there but is not the bytes this writes, so a schema changed
    /// under it or something else wrote to the store.
    Corrupt,
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unavailable => write!(
                f,
                "This browser will not let rustsega keep saves, so progress is lost when the tab closes"
            ),
            Self::Read => write!(f, "The saved data could not be read from this browser"),
            Self::Write => write!(
                f,
                "The saved data could not be written to this browser - it may be out of space"
            ),
            Self::Corrupt => write!(f, "The saved data for this game is not readable"),
        }
    }
}

impl std::error::Error for StoreError {}

/// The open `rustsega` database.
///
/// A cheap handle to clone, which is what lets a write be spawned without anyone
/// borrowing the console: see [`crate::emulator`] for the rule this exists to
/// serve, that nothing is borrowed across an `.await`.
#[derive(Clone)]
pub struct Db {
    database: Rc<Rexie>,
}

thread_local! {
    /// The one connection, shared by everyone who asks for it. IndexedDB would
    /// allow more than one, but each is a file handle and a `versionchange` to
    /// wait out, and there is no second thing to want here.
    static CONNECTION: RefCell<Option<Db>> = const { RefCell::new(None) };
}

/// Opens [`DB_NAME`] at [`DB_VERSION`], creating whatever of [`Store::ALL`]
/// is missing.
///
/// Shared: the first caller pays the round trip and everyone after it gets the
/// same handle. `Err` is the whole of the "this browser will not save" case, and
/// the caller's job is one toast and a game that carries on without persistence.
pub async fn open() -> Result<Db, StoreError> {
    if let Some(db) = CONNECTION.with(|connection| connection.borrow().clone()) {
        return Ok(db);
    }
    let mut builder = Rexie::builder(DB_NAME).version(DB_VERSION);
    for store in Store::ALL {
        // No key path: every record is stored against the out-of-line key the
        // helpers above make, which is the only kind of key a ROM id is.
        builder = builder.add_object_store(ObjectStore::new(store.name()));
    }
    let database = builder.build().await.map_err(|_| StoreError::Unavailable)?;
    let db = Db {
        database: Rc::new(database),
    };
    CONNECTION.with(|connection| *connection.borrow_mut() = Some(db.clone()));
    Ok(db)
}

impl Db {
    /// The bytes stored at `store` under `key`, `None` when there is no record.
    pub async fn get(&self, store: Store, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let key = JsValue::from(key);
        let transaction = self.transaction(store, TransactionMode::ReadOnly)?;
        let found = transaction
            .store(store.name())
            .map_err(|_| StoreError::Read)?
            .get(key)
            .await
            .map_err(|_| StoreError::Read)?;
        // A record that is not a `Uint8Array` is not ours: reading on would hand
        // the caller garbage to put back over it.
        found
            .map(|value| bytes_of(&value).ok_or(StoreError::Corrupt))
            .transpose()
    }

    /// Stores `bytes` at `store` under `key`, replacing whatever was there.
    ///
    /// Resolves once IndexedDB has committed, so the caller can promise the write
    /// it is about to make.
    pub async fn put(&self, store: Store, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let key = JsValue::from(key);
        let value = JsValue::from(js_sys::Uint8Array::from(bytes));
        let transaction = self.transaction(store, TransactionMode::ReadWrite)?;
        transaction
            .store(store.name())
            .map_err(|_| StoreError::Write)?
            .put(&value, Some(&key))
            .await
            .map_err(|_| StoreError::Write)?;
        transaction.commit().await.map_err(|_| StoreError::Write)
    }

    /// Removes the record at `store` under `key`. Absent is not an error.
    pub async fn delete(&self, store: Store, key: &str) -> Result<(), StoreError> {
        let key = JsValue::from(key);
        let transaction = self.transaction(store, TransactionMode::ReadWrite)?;
        transaction
            .store(store.name())
            .map_err(|_| StoreError::Write)?
            .delete(key)
            .await
            .map_err(|_| StoreError::Write)?;
        transaction.commit().await.map_err(|_| StoreError::Write)
    }

    /// Every key in `store`, for listing what is stored (M7-15, M7-16).
    ///
    /// Keys rustsega writes are always strings, so a key that is not one - from
    /// a foreign write, or a store made by hand in devtools - is skipped rather
    /// than being a failure to read the whole store.
    pub async fn list_keys(&self, store: Store) -> Result<Vec<String>, StoreError> {
        let transaction = self.transaction(store, TransactionMode::ReadOnly)?;
        let keys = transaction
            .store(store.name())
            .map_err(|_| StoreError::Read)?
            .get_all_keys(None, None)
            .await
            .map_err(|_| StoreError::Read)?;
        Ok(keys.iter().filter_map(JsValue::as_string).collect())
    }

    /// A cart's saved RAM, `None` when this ROM has never been saved.
    pub async fn get_sram(&self, rom_id: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.get(Store::Sram, &sram_key(rom_id)).await
    }

    /// Writes a cart's RAM, replacing its previous save.
    pub async fn put_sram(&self, rom_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.put(Store::Sram, &sram_key(rom_id), bytes).await
    }

    /// One transaction over `store` alone: every record this layer owns is in
    /// exactly one store, so no transaction ever needs to span two.
    fn transaction(
        &self,
        store: Store,
        mode: TransactionMode,
    ) -> Result<rexie::Transaction, StoreError> {
        self.database
            .transaction(&[store.name()], mode)
            // A store that is not there means the database is not the one this
            // code made, which is `Corrupt` for the same reason a bad value is.
            .map_err(|_| StoreError::Corrupt)
    }
}

/// The bytes of a stored record, `None` unless it is a `Uint8Array` - the one
/// thing [`Db::put`] writes.
fn bytes_of(value: &JsValue) -> Option<Vec<u8>> {
    value
        .is_instance_of::<js_sys::Uint8Array>()
        .then(|| js_sys::Uint8Array::new(value).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 64-bit FNV-1a vectors of the reference implementation
    /// (the `fnvtest` vectors of the FNV spec): a hash with a wrong offset basis
    /// or a wrong prime looks exactly as much like a hash as one that is right,
    /// so it is pinned by known answers rather than by round-tripping itself.
    #[test]
    fn fnv1a_64_matches_the_reference_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    /// The watcher's whole job is telling two SRAM images apart, and a Genesis
    /// save is 8 KiB of mostly-repeated bytes: the hash has to be sensitive to
    /// one byte of one of them.
    #[test]
    fn fnv1a_64_tells_two_sram_images_apart() {
        let mut saved = vec![0u8; 8 * 1024];
        let before = fnv1a_64(&saved);
        saved[4096] = 0x5a;
        let after = fnv1a_64(&saved);
        assert_ne!(before, after);
        assert_eq!(before, fnv1a_64(&[0u8; 8 * 1024]));
    }

    /// A save state is one of several per ROM, so its key carries the slot;
    /// the `:` is what keeps slot 11 and slot 1 of a ROM whose id ended in 1
    /// apart.
    #[test]
    fn the_states_key_is_the_rom_id_and_the_slot() {
        assert_eq!(state_key("d34db33f", 0), "d34db33f:0");
        assert_eq!(state_key("d34db33f", 9), "d34db33f:9");
        assert_ne!(state_key("d34db33f", 1), state_key("d34db33f:1", 0));
    }

    /// A cart has one SRAM image and a library has one entry per ROM, so both
    /// keys are the ROM id itself - said out loud once, rather than assumed at
    /// every call site.
    #[test]
    fn the_sram_and_rom_keys_are_the_rom_id() {
        assert_eq!(sram_key("d34db33f"), "d34db33f");
        assert_eq!(rom_key("d34db33f"), "d34db33f");
    }

    /// The schema the ticket names, spelled exactly: a store whose name drifted
    /// is a save that cannot be found again.
    #[test]
    fn the_database_and_its_three_stores_are_named_as_the_ticket_declares() {
        assert_eq!(DB_NAME, "rustsega");
        assert_eq!(DB_VERSION, 1);
        assert_eq!(Store::Sram.name(), "sram");
        assert_eq!(Store::States.name(), "states");
        assert_eq!(Store::Roms.name(), "roms");
        assert_eq!(Store::ALL, [Store::Sram, Store::States, Store::Roms]);
    }

    /// Bytes that have not moved are not worth a write, however long the game
    /// runs: this is the check that runs once a second for the whole life of a
    /// cartridge.
    #[test]
    fn unchanged_sram_never_flushes() {
        let sram = [1u8, 2, 3, 4];
        let mut watcher = SramWatcher::new(&sram, 0.0);
        assert!(!watcher.should_flush(&sram, 1_000.0));
        assert!(!watcher.should_flush(&sram, SRAM_FLUSH_INTERVAL_MS));
        assert!(!watcher.should_flush(&sram, 10.0 * SRAM_FLUSH_INTERVAL_MS));
        // Not even when the page is being closed.
        watcher.force();
        assert!(!watcher.should_flush(&sram, 10.0 * SRAM_FLUSH_INTERVAL_MS));
    }

    /// A change is written, but not before the window: the 2 s of the ticket is
    /// what keeps a game that is writing SRAM every frame from being a write
    /// storm.
    #[test]
    fn changed_sram_flushes_once_after_two_seconds() {
        let mut watcher = SramWatcher::new(&[0u8; 4], 0.0);
        assert!(!watcher.should_flush(&[1u8; 4], SRAM_FLUSH_INTERVAL_MS - 1.0));
        assert!(watcher.should_flush(&[1u8; 4], SRAM_FLUSH_INTERVAL_MS));
        // And once, because the watcher remembers what it handed to storage.
        assert!(!watcher.should_flush(&[1u8; 4], SRAM_FLUSH_INTERVAL_MS + 1.0));
    }

    /// The coalescing rule itself: three changes inside one window are one write,
    /// at the end of the window, of the newest bytes.
    #[test]
    fn changes_inside_the_window_coalesce_into_one_flush() {
        let mut watcher = SramWatcher::new(&[0u8; 4], 0.0);
        assert!(!watcher.should_flush(&[1u8; 4], 100.0));
        assert!(!watcher.should_flush(&[2u8; 4], 900.0));
        assert!(!watcher.should_flush(&[3u8; 4], 1_500.0));
        assert!(watcher.should_flush(&[3u8; 4], SRAM_FLUSH_INTERVAL_MS));
        // The two earlier versions were never written and are never written: the
        // watcher has no memory of them, only of the bytes it handed over.
        assert!(!watcher.should_flush(&[1u8; 4], SRAM_FLUSH_INTERVAL_MS + 1.0));
        assert!(!watcher.should_flush(&[2u8; 4], SRAM_FLUSH_INTERVAL_MS + 1.0));
        // ... and the newest bytes start a fresh window of their own.
        assert!(!watcher.should_flush(&[4u8; 4], 3_000.0));
        assert!(watcher.should_flush(&[4u8; 4], 4_000.0));
    }

    /// `pagehide` and a ROM switch do not get to wait for the window: they have
    /// one chance to hand the bytes over.
    #[test]
    fn force_flushes_immediately() {
        let mut watcher = SramWatcher::new(&[0u8; 4], 0.0);
        assert!(!watcher.should_flush(&[1u8; 4], 10.0));
        watcher.force();
        assert!(watcher.should_flush(&[1u8; 4], 20.0));
        // The force is spent: the window is what governs the next change.
        assert!(!watcher.should_flush(&[2u8; 4], 30.0));
    }

    /// A cart with no battery-backed RAM reports no SRAM at all, and its owner
    /// hands this watcher an empty slice. An empty write is not a no-op: it is a
    /// record that would blank that ROM's real save next time it is loaded, so
    /// nothing coming out of an empty cart is ever written, forced or not.
    #[test]
    fn a_cart_with_no_save_ram_never_flushes() {
        let mut watcher = SramWatcher::new(&[], 0.0);
        assert!(!watcher.should_flush(&[], 10_000.0));
        watcher.force();
        assert!(!watcher.should_flush(&[], 10_000.0));
    }
}
