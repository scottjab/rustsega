//! A mounted [`Toggle`] carries its state as `aria-checked`, measured in a real
//! browser.
//!
//! The switch's state is the `aria-checked` attribute: it is what a screen
//! reader announces, and what `style.css` moves the thumb by
//! (`.toggle[aria-checked="true"]`). It once rendered as a literal
//! `attr:aria-checked` attribute instead, so every switch in the app worked but
//! never looked on - reported from iOS as "the FPS overlay will not toggle".
//! Rendering to a string needs Leptos's `ssr` feature, which this crate does not
//! build with, so this mounts the real component and reads the DOM back.

#![cfg(target_arch = "wasm32")]

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web::ui::toggle::Toggle;

wasm_bindgen_test_configure!(run_in_browser);

/// Mounts a toggle reading `checked` into a fresh element and returns its
/// button, keeping the mount alive for as long as the caller holds it.
fn mount(checked: RwSignal<bool>) -> (web_sys::Element, impl Drop) {
    let document = web_sys::window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&host).unwrap();
    let handle = leptos::mount::mount_to(host.clone().unchecked_into(), move || {
        view! {
            <Toggle
                label="FPS overlay".to_string()
                checked=checked.into()
                on_toggle=Callback::new(move |()| checked.update(|on| *on = !*on))
            />
        }
    });
    let button = host.query_selector("button").unwrap().expect("a button");
    (button, handle)
}

#[wasm_bindgen_test]
fn the_state_is_the_aria_checked_attribute() {
    let checked = RwSignal::new(true);
    let (button, _mounted) = mount(checked);
    assert_eq!(
        button.get_attribute("aria-checked").as_deref(),
        Some("true")
    );
    assert!(
        !button.outer_html().contains("attr:"),
        "{}",
        button.outer_html()
    );
}

/// Lets the browser run one animation frame: Leptos applies a signal's DOM
/// update on its own schedule, not inside the `click()` call.
async fn next_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let callback = Closure::once_into_js(move |_now_ms: f64| {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        });
        web_sys::window()
            .unwrap()
            .request_animation_frame(callback.unchecked_ref())
            .unwrap();
    });
    JsFuture::from(promise).await.unwrap();
}

#[wasm_bindgen_test]
async fn a_click_flips_the_attribute() {
    let checked = RwSignal::new(false);
    let (button, _mounted) = mount(checked);
    assert_eq!(
        button.get_attribute("aria-checked").as_deref(),
        Some("false")
    );
    button.unchecked_ref::<web_sys::HtmlElement>().click();
    next_frame().await;
    assert_eq!(
        button.get_attribute("aria-checked").as_deref(),
        Some("true")
    );
}
