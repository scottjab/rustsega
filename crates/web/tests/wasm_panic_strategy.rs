//! The panic strategy the browser build is designed around (#387).
//!
//! `Emulator`'s console cell is handed over by moving the console out of it rather
//! than by borrowing it, because a wasm panic cannot give a `RefCell` borrow back:
//! with no unwinding there is no `Drop` for a `RefMut` to run, so a borrow taken
//! for a frame that died in the machine is held for the life of the page and every
//! later `borrow_mut` panics with it.
//!
//! That is a fact about the target rather than about this code, and the design
//! rests on it, so it is checked here on the target itself: `just test-web` builds
//! this file for `wasm32-unknown-unknown` and runs it in a browser.
//!
//! It is also why `std::panic::catch_unwind` around `tick` - the other shape #387
//! suggested - is not a fix: there is nothing for it to catch, which a probe of
//! exactly that showed in Firefox before this file was written. `crash.rs` reaches
//! the player through the panic hook instead, and the hook does run; it is the
//! unwinding after it that does not.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

/// wasm is built `panic = "abort"`, so no `catch_unwind` in this crate, nor in
/// wasm-bindgen's own closure shim, can win a borrow back from a panic.
///
/// If this ever fails, the target's default has changed and the console cell of
/// #387 can be revisited - it is a `RefCell` discipline standing in for an ability
/// the target does not have.
#[wasm_bindgen_test]
fn a_wasm_panic_is_not_caught_and_gives_nothing_back() {
    let strategy = if cfg!(panic = "abort") {
        "abort"
    } else if cfg!(panic = "unwind") {
        "unwind"
    } else {
        "neither"
    };
    assert_eq!(
        strategy, "abort",
        "wasm32-unknown-unknown is now built panic={strategy}: catch_unwind may be \
         able to answer #387 on its own, and the console cell of `TakeCell` is \
         standing in for an ability the target does not have"
    );
}
