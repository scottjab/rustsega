//! The top bar's room for the cartridge title at phone widths (#380).
//!
//! #380 measured 44px of title against 156px of icon buttons at 360px. That was true
//! when it was filed, and `aca2510` removed it about twenty hours later by collapsing
//! the bar's action set into a single menu button under 640px. These tests are the
//! regression bar for that: geometry, from the shipped stylesheet, at the widths the
//! ticket named and the boundary where the layout changes hands.
//!
//! **Why an iframe.** The bar changes shape on a *media query*, and
//! `wasm_bindgen_test` gets one window that cannot be resized. A container of the
//! right width is not enough - `@media` reads the viewport, not the container - so
//! the only way to ask the question in CI is a nested browsing context, which has its
//! own viewport and evaluates the stylesheet against that.
//!
//! **Why the markup is written out.** The bar is Leptos (`app.rs`), and mounting the
//! real shell into an iframe is a different, larger piece of work. So this mirrors
//! `app.rs`'s top bar by hand: wordmark, title with region chip, both action halves,
//! with `IconButton`'s real `btn btn--icon` classes and real 20x20 `.icon` content.
//! The content matters: with word-filled buttons instead of icons the bar's minimum
//! content width is ~576px rather than ~316px, which produces confidently wrong
//! numbers. If the top bar's markup changes, this file has to change with it.
//!
//! ## What this cannot see
//!
//! - A 320px or 360px iframe is a real viewport for CSS, but it is not a real device:
//!   no scrollbar of a real browser, no zoom level, no font substitution from a phone,
//!   and no landscape rotation.
//! - Two titles are laid out, one loaded and one empty (`Sonic the Hedgehog`,
//!   `No cartridge loaded`). Long names are covered by rule rather than by example -
//!   `.now-playing` is `flex: 1; min-width: 0`, so its box is decided by the bar and
//!   not by its text, which `an_empty_bar_keeps_the_same_room_as_a_loaded_one`
//!   measures rather than reasons about.
//! - Boxes, not legibility: 124px of room is still an ellipsised name. Whether the
//!   wordmark should give some of its own width back is a design call (#380 left it
//!   open) and no test here settles it.
//!
//! Run it with `just test-web`.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

const STYLE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/style.css"));

/// The icon buttons `app.rs` renders in `.topbar__wide` with a cartridge loaded and
/// the library present: `console_buttons` (pause, reset), the volume toggle, Library,
/// Open ROM, Save states, Fullscreen, Settings.
const WIDE_BUTTONS: [&str; 8] = [
    "Pause",
    "Reset",
    "Unmute",
    "Library",
    "Open ROM",
    "Save states",
    "Fullscreen",
    "Settings",
];

/// The narrowest room the title is allowed, at every width below.
///
/// Set against the layout rule, not against today's measurement. #380's complaint was
/// 44px - one character and an ellipsis - and the squeeze that produces it is the
/// whole action set on a bar with no room, where the title goes to **0px**. A floor of
/// 100px cannot be tripped by restyling: `--space-3` gaps doubling to 24px still leave
/// 104px at 320px, and any redesign that gives the title more only moves away from it.
/// What does trip it is the ≤640px action collapse being widened past a phone or
/// removed, which is exactly the rule being pinned and was verified to fail here.
///
/// 100px is also roughly ten characters at `--text-sm`, so it is a usability floor in
/// its own right rather than a round number.
const MIN_TITLE: f64 = 100.0;

/// A `.btn--icon` as `IconButton` renders one: a fixed `.icon` svg, no text. Text
/// here would change the bar's minimum content width and fake every number below.
fn icon_button(label: &str) -> String {
    format!(
        r#"<button class="btn btn--icon" aria-label="{label}"><svg class="icon" viewBox="0 0 24 24"></svg></button>"#
    )
}

/// The top bar of `app.rs` at `loaded`, written out for layout.
fn topbar_markup(title: &str, loaded: bool) -> String {
    let wide = WIDE_BUTTONS
        .iter()
        .map(|label| icon_button(label))
        .collect::<String>();
    let class = if loaded {
        "now-playing now-playing--loaded"
    } else {
        "now-playing"
    };
    let region = if loaded {
        r#"<span class="now-playing__region">PAL</span>"#
    } else {
        ""
    };
    format!(
        r#"<header class="topbar">
             <span class="wordmark">rustsega<span class="wordmark__dot"></span></span>
             <p class="{class}">{title}{region}</p>
             <nav class="topbar__actions" aria-label="Console">
               <div class="topbar__wide">{wide}</div>
               <div class="topbar__narrow">{}</div>
             </nav>
           </header>"#,
        icon_button("Menu")
    )
}

/// The page laid out in a real window `width` x `height` wide, with the shipped
/// stylesheet and a loaded or empty bar.
struct LaidOut {
    document: web_sys::Document,
    width: u32,
}

impl LaidOut {
    fn new(width: u32, title: &str, loaded: bool) -> Self {
        let outer = web_sys::window()
            .expect("a window")
            .document()
            .expect("a document");
        let frame = outer
            .create_element("iframe")
            .expect("an iframe")
            .dyn_into::<web_sys::HtmlIFrameElement>()
            .expect("an iframe element");
        // An iframe of a fixed pixel width has its own viewport, which is what the
        // media queries in `style.css` are then read against.
        frame
            .set_attribute("style", &format!("width:{width}px;height:740px;border:0"))
            .expect("a width");
        outer
            .body()
            .expect("a body")
            .append_child(&frame)
            .expect("add the frame");
        let document = frame.content_document().expect("a frame document");
        let style = document.create_element("style").expect("a style element");
        style.set_text_content(Some(STYLE));
        document
            .head()
            .expect("a head")
            .append_child(&style)
            .expect("add the shipped stylesheet");
        let body = document.body().expect("a body");
        body.set_attribute("style", "margin:0").expect("no margin");
        body.set_inner_html(&topbar_markup(title, loaded));
        Self { document, width }
    }

    /// The box of the first element matching `selector`.
    fn rect_of(&self, selector: &str) -> web_sys::DomRect {
        self.document
            .query_selector(selector)
            .expect("a valid selector")
            .unwrap_or_else(|| panic!("`{selector}` is not in the bar the harness builds"))
            .get_bounding_client_rect()
    }

    /// Every action button the bar exposes, as `Vec<(label, width, left, right)>`.
    ///
    /// `unchecked_ref` because `instanceof Element` is evaluated in this window's
    /// realm and is false for a node owned by the iframe's document; a `NodeList` is
    /// elements.
    fn actions(&self) -> Vec<(String, f64, f64, f64)> {
        let buttons = self
            .document
            .query_selector_all(".topbar__actions button")
            .expect("a valid selector");
        (0..buttons.length())
            .map(|index| buttons.item(index).expect("a button"))
            .map(|node| {
                let element = node.unchecked_ref::<web_sys::Element>();
                let rect = element.get_bounding_client_rect();
                (
                    element
                        .get_attribute("aria-label")
                        .unwrap_or_else(|| "?".into()),
                    rect.width(),
                    rect.left(),
                    rect.right(),
                )
            })
            .collect()
    }

    /// Whether anything in the frame is wider than the frame.
    fn overflows(&self) -> bool {
        self.document
            .document_element()
            .expect("a document element")
            .scroll_width()
            > self.width as i32
    }
}

/// The loaded title has to keep readable room at phone widths and at the 640px
/// boundary where the action set changes hands (#380).
#[wasm_bindgen_test]
fn a_loaded_title_keeps_room_at_every_phone_width() {
    let mut misses = Vec::new();
    for width in [320u32, 360, 400, 639, 640, 768] {
        let page = LaidOut::new(width, "Sonic the Hedgehog", true);
        let title = page.rect_of(".now-playing");
        if title.width() < MIN_TITLE {
            misses.push(format!(
                "{width}px: title has only {:.0}px (wordmark {:.0}px, actions {:.0}px)",
                title.width(),
                page.rect_of(".wordmark").width(),
                page.rect_of(".topbar__actions").width()
            ));
        }
        if page.overflows() {
            misses.push(format!("{width}px: the bar is wider than the window"));
        }
    }
    assert!(
        misses.is_empty(),
        "the top bar squeezes the cartridge title: {}",
        misses.join("; ")
    );
}

/// Every action the bar shows at those widths has to be on screen and tappable, and
/// the bar may not smuggle a second half of them off the edge (#380).
#[wasm_bindgen_test]
fn every_action_the_bar_shows_is_on_screen_at_every_phone_width() {
    let mut misses = Vec::new();
    for width in [320u32, 360, 400, 639, 640, 768] {
        let page = LaidOut::new(width, "Sonic the Hedgehog", true);
        let actions = page.actions();
        let visible: Vec<_> = actions.iter().filter(|(_, w, _, _)| *w > 0.0).collect();
        // The rule, straight from `style.css`: below 640px `.topbar__wide` is
        // `display: none` and `.topbar__narrow` shows one menu button; from 640px the
        // two swap. A count is the shape of that rule, so it fails when the rule
        // breaks - including a "fix" that buys the title room by dropping a control,
        // which is the move this ticket explicitly forbids.
        let expected = if width < 640 { 1 } else { WIDE_BUTTONS.len() };
        if visible.len() != expected {
            misses.push(format!(
                "{width}px: {} of {} actions visible, expected {expected}",
                visible.len(),
                actions.len()
            ));
        }
        for (label, _, left, right) in visible {
            if *left < 0.0 || *right > width as f64 {
                misses.push(format!(
                    "{width}px: `{label}` runs off the edge at {left:.0}..{right:.0}"
                ));
            }
        }
    }
    assert!(
        misses.is_empty(),
        "top bar actions are not reachable: {}",
        misses.join("; ")
    );
}

/// The empty state, which #380 said was fine, is fine - the bar's room is decided by
/// flex, not by the length of the title, and this says so out loud.
#[wasm_bindgen_test]
fn an_empty_bar_keeps_the_same_room_as_a_loaded_one() {
    let mut misses = Vec::new();
    for width in [320u32, 360, 400] {
        let empty = LaidOut::new(width, "No cartridge loaded", false);
        let loaded = LaidOut::new(width, "Sonic the Hedgehog", true);
        let (a, b) = (
            empty.rect_of(".now-playing").width(),
            loaded.rect_of(".now-playing").width(),
        );
        if (a - b).abs() > 0.5 {
            misses.push(format!("{width}px: empty {a:.0}px vs loaded {b:.0}px"));
        }
        if empty.overflows() {
            misses.push(format!("{width}px: empty bar overflows"));
        }
    }
    assert!(
        misses.is_empty(),
        "the empty state changed the bar's room: {}",
        misses.join("; ")
    );
}
