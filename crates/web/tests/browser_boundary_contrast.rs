//! The shipped stylesheet's component boundaries, measured in a live DOM (#407).
//!
//! `tests/contrast.rs` asks the stylesheet what its tokens are. This asks the
//! browser what it paints: `style.css` is injected, a real `.btn` and a real
//! `.toast--ok` are built, and `getComputedStyle` is read back and put through the
//! WCAG 1.4.11 bar of 3:1. The difference matters because a token can be compliant
//! and still not be what an element ends up bordered with - an `!important` later in
//! the file, a rule that overrides `border-color` for one component - and that is a
//! class of miss the file test cannot see. Same argument #394 made for CSS
//! transitions and #397 for GL: read the real thing back.
//!
//! What it cannot see, said plainly:
//!
//! - it measures the elements it builds. Twenty-one rules use `--border`; these are
//!   two of them, chosen because they are a control and a kind indicator.
//! - the outside neighbour is `body`, which paints `--bg`. A component nested on a
//!   card has a different neighbour and is not exercised here.
//! - nothing is compared against a screenshot: overlap, opacity and `box-shadow`
//!   are not measured, so this is "the colour the browser computed is 3:1", not
//!   "a person sees the line".
//!
//! Needs a browser, which is `just test-web`.

#![cfg(target_arch = "wasm32")]

mod support;

use support::parse_rgb;
use support::ratio_rgb;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The stylesheet the page really ships with, so this cannot drift from it.
const STYLE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/style.css"));

/// WCAG 2.1 1.4.11's bar for the visual boundary of a component.
const AA_NON_TEXT: f64 = 3.0;

/// The four toast kinds, as the stylesheet names them.
const TOAST_KINDS: [&str; 4] = ["info", "ok", "warn", "danger"];

/// Both themes, by the attribute the shell writes on `<html>`.
///
/// `"dark"` is not a rule in `style.css` - dark is plain `:root` - which is exactly
/// why it works as a setting here: `:root:not([data-theme])` stops matching, so the
/// operating system's `prefers-color-scheme` cannot quietly decide the case being
/// measured.
const THEMES: [&str; 2] = ["dark", "light"];

/// Installs the shipped stylesheet once per page.
fn install_stylesheet() {
    let document = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document");
    if document
        .query_selector("style[data-rustsega]")
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let style = document
        .create_element("style")
        .expect("create a `style` element");
    style.set_attribute("data-rustsega", "").expect("mark it");
    style.set_text_content(Some(STYLE));
    document
        .head()
        .expect("a head")
        .append_child(&style)
        .expect("add the shipped stylesheet");
}

fn set_theme(theme: &str) {
    let root = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document")
        .document_element()
        .expect("a document element");
    root.set_attribute("data-theme", theme)
        .expect("set data-theme");
}

/// An element of `class` in the page, whose colour is being asked about.
fn page_element(tag: &str, class: &str) -> web_sys::HtmlElement {
    let document = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document");
    let element = document
        .create_element(tag)
        .expect("create an element")
        .dyn_into::<web_sys::HtmlElement>()
        .expect("a real element");
    element.set_class_name(class);
    document
        .body()
        .expect("a body")
        .append_child(&element)
        .expect("add it to the page");
    element
}

/// One computed colour of `element`, as three channels.
fn computed(element: &web_sys::Element, property: &str) -> [u8; 3] {
    let value = web_sys::window()
        .expect("a browser window")
        .get_computed_style(element)
        .ok()
        .flatten()
        .expect("the element has a computed style")
        .get_property_value(property)
        .expect("the property is readable");
    parse_rgb(&value).unwrap_or_else(|| panic!("`{property}` computed to {value:?}"))
}

fn page_background() -> [u8; 3] {
    let body = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document")
        .body()
        .expect("a body")
        .dyn_into::<web_sys::Element>()
        .expect("a body element");
    computed(&body, "background-color")
}

/// A control's own boundary has to be visible on both sides of it (#407).
#[wasm_bindgen_test]
fn a_button_border_is_visible_on_both_sides_in_both_themes() {
    install_stylesheet();
    let mut misses = Vec::new();
    for theme in THEMES {
        set_theme(theme);
        let button = page_element("button", "btn");
        let border = computed(&button, "border-top-color");
        let fill = computed(&button, "background-color");
        let page = page_background();
        // The two sides are named rather than min'd so a failure says which one went.
        for (side, neighbour) in [("its own fill", fill), ("the page behind it", page)] {
            let ratio = ratio_rgb(border, neighbour);
            if ratio < AA_NON_TEXT {
                misses.push(format!(
                    "{theme}: .btn border {border:?} on {side} {neighbour:?} is {ratio:.2}:1"
                ));
            }
        }
    }
    assert!(
        misses.is_empty(),
        "component borders below the {AA_NON_TEXT}:1 of WCAG 1.4.11: {}",
        misses.join("; ")
    );
}

/// A toast's kind stripe has to be visible on the toast it is painted on (#407).
#[wasm_bindgen_test]
fn every_toast_kind_stripe_is_visible_on_its_toast_in_both_themes() {
    install_stylesheet();
    let mut misses = Vec::new();
    for theme in THEMES {
        set_theme(theme);
        for kind in TOAST_KINDS {
            let toast = page_element("div", &format!("toast toast--{kind}"));
            let stripe = computed(&toast, "border-left-color");
            let surface = computed(&toast, "background-color");
            let ratio = ratio_rgb(stripe, surface);
            if ratio < AA_NON_TEXT {
                misses.push(format!(
                    "{theme}: .toast--{kind} stripe {stripe:?} on {surface:?} is {ratio:.2}:1"
                ));
            }
        }
    }
    assert!(
        misses.is_empty(),
        "toast kind stripes below the {AA_NON_TEXT}:1 of WCAG 1.4.11: {}",
        misses.join("; ")
    );
}
