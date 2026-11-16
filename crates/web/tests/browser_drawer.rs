//! The drawer's `visibility` transition, measured in a real browser (#394).
//!
//! `crates/web/tests` is otherwise native: it reads `style.css` off disk as text
//! (`support::style_css`), which can say a declaration is present but cannot say
//! what the browser computes from it. This is the opposite - it compiles only for
//! `wasm32-unknown-unknown` and runs in the headless browser `just test-web`
//! drives, the one that `browser_smoke.rs` (M7-20, #153) proves is there.
//!
//! It is the assertion #144 made by hand over the Chrome DevTools Protocol:
//! inject the shipped stylesheet, change the class, read the computed
//! `visibility` back. The stylesheet is injected rather than retyped so that a
//! later edit to `style.css` moves this test with it - if the transition comes
//! back, the test says so.
//!
//! `style.css` puts every transition under `prefers-reduced-motion:
//! no-preference`, so a browser that prefers reduced motion applies none of it
//! and the opening assertion below would pass without measuring anything. Two
//! things guard against that quiet green: the media query is asserted directly,
//! and the closing direction is asserted to *hold* `visible`, which only a live
//! transition can do.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The stylesheet the page really ships with, read from the crate at compile
/// time so this test cannot drift from it.
const STYLE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/style.css"));

/// Enough animation frames to pass one `--duration` of 170ms twice over, at any
/// refresh rate this side of 120Hz.
const FRAMES_PAST_TRANSITION: i32 = 40;

/// Lets the browser run `frames` of its own frames.
///
/// Animation frames rather than a timer because that is the clock a transition
/// advances on, and it is the same await `emulator.rs`'s harness uses - the one
/// that needs no `web-sys` feature beyond `Window`.
async fn wait_frames(frames: i32) {
    for _ in 0..frames {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let resolve = resolve.clone();
            let callback = Closure::wrap(Box::new(move |_now_ms: f64| {
                let _ = resolve.call0(&JsValue::UNDEFINED);
            }) as Box<dyn FnMut(f64)>);
            let window = web_sys::window().expect("a browser window");
            let _ = window
                .request_animation_frame(callback.as_ref().unchecked_ref())
                .expect("this browser gives animation frames");
            // The frame is on its way, so the callback outlives this call.
            callback.forget();
        });
        JsFuture::from(promise).await.expect("a frame arrives");
    }
}

/// The drawer opens with no wait and closes with one (#394).
///
/// `visibility: hidden` is what takes a shut drawer's controls out of the tab
/// order, so it is in the transition list to keep the drawer painted while it
/// slides back off-screen. `step-end` holds the value it is leaving for the whole
/// transition and jumps at the end - which is right on the way out and wrong on
/// the way in, where it left an open drawer computed `hidden` for `--duration`
/// and nothing inside it could take the focus.
#[wasm_bindgen_test]
async fn an_open_drawer_is_visible_the_instant_it_opens() {
    let window = web_sys::window().expect("a browser window");
    let document = window.document().expect("a document");

    assert!(
        window
            .match_media("(prefers-reduced-motion: no-preference)")
            .expect("matchMedia answers")
            .is_some_and(|list| list.matches()),
        "this browser prefers reduced motion, so `style.css` applies no transition \
         and nothing below would have been measured"
    );

    let style = document
        .create_element("style")
        .expect("create a `style` element");
    style.set_text_content(Some(STYLE));
    document
        .head()
        .expect("a head")
        .append_child(&style)
        .expect("add the shipped stylesheet");

    let drawer = document.create_element("div").expect("create a `div`");
    drawer.set_class_name("drawer");
    document
        .body()
        .expect("a body")
        .append_child(&drawer)
        .expect("add the drawer to the page");

    let visibility = || {
        window
            .get_computed_style(&drawer)
            .ok()
            .flatten()
            .expect("the drawer has a computed style")
            .get_property_value("visibility")
            .expect("the drawer declares a visibility")
    };

    assert_eq!(
        visibility(),
        "hidden",
        "a shut drawer hides its controls, which is why `visibility` is not simply off"
    );

    drawer.set_class_name("drawer drawer--open");
    assert_eq!(
        visibility(),
        "visible",
        "opening must flip `visibility` on the frame the drawer opens: a drawer that \
         is still hidden cannot hold the focus, which is #394"
    );

    // And the way out still waits. This is also the control that says a
    // transition is running at all: with none, the drawer would read `hidden`
    // here, and the assertion above would have been worth nothing.
    drawer.set_class_name("drawer");
    assert_eq!(
        visibility(),
        "visible",
        "closing must keep the drawer painted until the slide has finished"
    );

    wait_frames(FRAMES_PAST_TRANSITION).await;
    assert_eq!(
        visibility(),
        "hidden",
        "and hide the drawer once the slide is done"
    );
}
