//! The download half of a recorded movie, in a real browser (M7-18, #365).
//!
//! The state machine and the admission checks are native (`tests/movie.rs`);
//! what can only be checked here is what the browser does with the bytes: the
//! Blob a `.rsm` arrives as, and the anchor that hands it to the downloads
//! folder under the name the cartridge ids it by. Both are the same pattern
//! `browser_srm.rs` checks for a `.srm`, split out of the click for exactly
//! this reason.
//!
//! Needs a browser: `just test-web`.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web::movie;

wasm_bindgen_test_configure!(run_in_browser);

/// A `.rsm` is a text download of the right type and name: `text/plain`, not a
/// default-typed Blob some programs read as 0 bytes, and named `.rsm` so it
/// comes back to a file picker or a `--movie` flag as what it is.
#[wasm_bindgen_test]
fn the_download_is_an_rsm_of_the_right_type_and_name() {
    let bytes = "rustsega-movie 1\nrom-hash 0x0\nregion usa\npads 1\n".as_bytes();
    let blob = movie::blob(bytes);
    assert_eq!(blob.type_(), movie::MOVIE_MIME);
    assert_eq!(blob.size() as usize, bytes.len());

    let name = movie::file_name("2b8fdb67ec2b6c6c0e5b6b8e3ba6b8d6c4d3f2a1");
    assert!(name.ends_with(".rsm"));
    let anchor = movie::download_anchor(&name, bytes);
    assert_eq!(anchor.download(), name, "the name the file arrives as");
    assert!(
        anchor.href().starts_with("blob:"),
        "the bytes travel as an object URL"
    );
}
