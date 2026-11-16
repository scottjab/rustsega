//! Export and import against a real IndexedDB, in a real browser (#398).
//!
//! The ticket's risk is not the byte handling - the `.srm` is the raw image, and
//! `check_import` covers the arithmetic natively (`tests/srm.rs`). The risk is
//! claiming an import worked when storage says otherwise, and the opposite: a
//! **refused** import that quietly wrote something anyway. Both of those are
//! claims about IndexedDB, and IndexedDB is only in a browser. A probe confirmed
//! it opens and round-trips here (`put_sram` then `get_sram` came back byte-exact)
//! before this file was written, so none of it is a unit test in disguise.
//!
//! What it cannot see is said plainly: these tests drive the storage half of an
//! import (validate, then store), which is the half that touches the persistent
//! record. The in-memory half is the single `Genesis::load_sram` call in
//! [`web::emulator::Controls::import_sram`], applied to a running console and read
//! back and compared there. A web test cannot drive that end to end: `Emulator`
//! is private to the crate and there is no SRAM cartridge to load in a browser
//! test: `Emulator::start` is private to its module and building one needs a
//! mounted canvas. What that call does with a wrong-sized blob is covered where the
//! machine is: `crates/genesis/src/cart/sram.rs`'s
//! `load_replaces_contents_and_wrong_length_is_ignored`.
//!
//! Needs a browser: `just test-web`.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web::srm;
use web::storage::Db;

wasm_bindgen_test_configure!(run_in_browser);

/// SRAM of a real cart: 8 KiB of `EE`, distinct from every other fixture here.
fn image(first: u8) -> Vec<u8> {
    vec![first; 8 * 1024]
}

/// A database and a cart of our own, so tests do not read each other's saves.
async fn db_and_rom(rom_id: &str) -> Db {
    let db = web::storage::open()
        .await
        .expect("IndexedDB opens in the browser test harness");
    db.put_sram(rom_id, &image(0xEE))
        .await
        .expect("storing a fixture save");
    db
}

/// What the storage half of `Controls::import_sram` is: the size rule, and only
/// then `put_sram`. Written out here once more than in the app because the point
/// of these two tests is the *order* - and because the app's version needs a
/// running console to reach.
async fn import(
    db: &Db,
    rom_id: &str,
    bytes: &[u8],
    cart_len: usize,
) -> Result<(), srm::ImportError> {
    srm::check_import(bytes.len(), cart_len)?;
    db.put_sram(rom_id, bytes)
        .await
        .expect("a write to a database that just answered a read");
    Ok(())
}

/// The round trip: what went in is what comes back, byte for byte - which is what
/// makes "export gives you the stored bytes" worth asserting at all (#398).
#[wasm_bindgen_test]
async fn a_stored_save_comes_back_byte_for_byte() {
    let rom_id = "rustsega-test-roundtrip";
    let db = db_and_rom(rom_id).await;
    let stored = db
        .get_sram(rom_id)
        .await
        .expect("read back")
        .expect("the fixture was written");
    assert_eq!(
        stored,
        image(0xEE),
        "IndexedDB changed a save it was handed"
    );

    let other = image(0x5A);
    import(&db, rom_id, &other, other.len())
        .await
        .expect("a same-size import is accepted");
    assert_eq!(
        db.get_sram(rom_id)
            .await
            .expect("read back")
            .expect("stored"),
        other,
        "an accepted import did not reach IndexedDB"
    );
}

/// The one that matters most: a save for the wrong cartridge is refused, and
/// **nothing changed** - not a truncation, not a zeroing, not a partial write.
/// This is the state a player who picks the wrong file at 2 a.m. is relying on.
#[wasm_bindgen_test]
async fn a_refused_import_leaves_the_stored_save_exactly_as_it_was() {
    let rom_id = "rustsega-test-refused";
    let db = db_and_rom(rom_id).await;
    let before = db
        .get_sram(rom_id)
        .await
        .expect("read back")
        .expect("the fixture was written");

    let wrong = vec![0x00u8; 32 * 1024];
    let error = import(&db, rom_id, &wrong, 8 * 1024)
        .await
        .expect_err("32 KiB into an 8 KiB cart must be refused");
    assert!(
        matches!(
            error,
            srm::ImportError::WrongSize {
                got: 32768,
                want: 8192
            }
        ),
        "refused for the wrong reason: {error:?}"
    );

    let after = db
        .get_sram(rom_id)
        .await
        .expect("read back")
        .expect("still there");
    assert_eq!(after, before, "a refused import touched IndexedDB");
}

/// The download half: the MIME a browser is handed and the name a player gets.
/// Both are asserted rather than believed because both fail silently - a `Blob`
/// with an empty type and a wrong name are still a download.
#[wasm_bindgen_test]
async fn the_download_is_a_srm_of_the_right_type_and_name() {
    let bytes = image(0x42);
    let blob = srm::blob(&bytes);
    assert_eq!(
        blob.type_(),
        srm::SRAM_MIME,
        "the browser was told what this is"
    );
    assert_eq!(
        blob.size() as usize,
        bytes.len(),
        "the whole image is in it"
    );

    let name = srm::file_name("cafebabe");
    let anchor = srm::download_anchor(&name, &bytes);
    assert_eq!(anchor.download(), name, "the name the file arrives as");
    assert!(
        anchor.href().starts_with("blob:"),
        "an object URL was expected, got {:?}",
        anchor.href()
    );
    web_sys::Url::revoke_object_url(&anchor.href()).expect("revoke what we made");
}
