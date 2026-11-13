//! The browser end of the test harness (M7-20, #153).
//!
//! Everything else in `crates/web/tests` is native: it reads `style.css` off
//! disk and needs no DOM. This is the opposite -- it compiles only for
//! `wasm32-unknown-unknown` and runs only in a real browser, driven headless by
//! `wasm-bindgen-test-runner` (see `just test-web` and `docs/TESTING.md`).
//!
//! It is deliberately a smoke test of the harness, not of the emulator: two
//! cases that a browser is present and the DOM a later browser test needs is
//! reachable. When either one stops passing, the harness is broken, and the
//! failure should read as "no browser ran" rather than as a web bug.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The `localStorage` key this test owns, cleared before and after it writes.
const STORAGE_KEY: &str = "rustsega.browser_smoke";

/// A `<canvas>` created from script hands back a 2D context.
///
/// The renderer of M7-04 (#137) paints every frame through
/// `CanvasRenderingContext2d::put_image_data`, so "a canvas in a browser yields
/// that context" is the smallest thing that can only be checked in a browser.
#[wasm_bindgen_test]
fn canvas_yields_a_2d_context() {
    let document = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document");
    let canvas = document.create_element("canvas").expect("create a canvas");
    let context = canvas
        .dyn_ref::<web_sys::HtmlCanvasElement>()
        .expect("a `canvas` element is an HtmlCanvasElement")
        .get_context("2d")
        .expect("get_context does not throw")
        .expect("a canvas has a 2D context");
    context
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .expect("the 2D context is a CanvasRenderingContext2d");
}

/// `localStorage` writes and reads back, and a delete really deletes.
///
/// M7-10 (#143) keeps every setting here and M7-06 (#139) the pad bindings, so
/// the store working at all in a browser is a thing no native test can establish.
/// The runner serves the harness page on `http://127.0.0.1` with an ephemeral
/// port, so the store starts empty; this test still clears its own key first and
/// deletes it after, so a failure between the two cannot leak into the next run.
#[wasm_bindgen_test]
fn local_storage_round_trips() {
    let storage = web_sys::window()
        .expect("a browser window")
        .local_storage()
        .expect("localStorage is reachable")
        .expect("this browser offers localStorage");
    storage.remove_item(STORAGE_KEY).expect("clear the key");
    assert_eq!(storage.get_item(STORAGE_KEY).expect("read"), None);

    storage.set_item(STORAGE_KEY, "m7-20").expect("write");
    assert_eq!(
        storage.get_item(STORAGE_KEY).expect("read").as_deref(),
        Some("m7-20")
    );

    storage.remove_item(STORAGE_KEY).expect("delete");
    assert_eq!(storage.get_item(STORAGE_KEY).expect("read"), None);
}
