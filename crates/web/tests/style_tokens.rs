//! Every colour in `style.css` comes from a token (M7-19, #152).
//!
//! The theme of M7-02 (#135) is the token block at the top of the file: `:root`
//! for the dark values and `[data-theme="light"]` - plus the
//! `prefers-color-scheme` copy of it - for the light ones. A rule that paints
//! `#5b8cff` instead of `var(--accent)` is invisible to both of those blocks, so
//! it keeps the dark blue after the page turns light, and it is the reason the
//! light theme cannot be trusted without hunting the file for literals.
//!
//! Native test, no browser: it reads the stylesheet as declarations (see
//! [`support`]) rather than as text, because a colour lives in a declaration and
//! the selector that owns the declaration is what decides whether it is legal.
//! Comments are gone before any of it happens, because this file cites issue
//! numbers in them and `#152` looks exactly like a short hex grey to anything that
//! does not know it is prose.

mod support;

use support::{colour_literals, declarations, declarations_in, style_css};

/// Every `(line, text)` of `css` that is a colour literal outside a theme block.
fn raw_colours(css: &str) -> Vec<(usize, String)> {
    declarations_in(css)
        .iter()
        .filter(|declaration| !declaration.is_theme())
        .flat_map(|declaration| {
            colour_literals(&declaration.value)
                .into_iter()
                .map(move |colour| (declaration.line, colour))
        })
        .collect()
}

#[test]
fn every_colour_in_the_stylesheet_is_a_token() {
    let raw = raw_colours(&style_css());
    assert!(
        raw.is_empty(),
        "style.css paints with a raw colour outside the token blocks: {}",
        raw.iter()
            .map(|(line, colour)| format!("line {line} ({colour})"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// The scan is not vacuous: the same rule that passes the real file fails on a
/// stylesheet that paints with a literal. Without this, a broken walk looks
/// exactly like a clean one.
#[test]
fn the_token_scan_finds_a_raw_colour() {
    let css = "
:root { --accent: #5b8cff; }
:root[data-theme=\"light\"] { --accent: #0055aa; }
@media (prefers-color-scheme: light) {
  :root:not([data-theme]) { --accent: #0055aa; }
}
.card {
  color: #ffffff;
  /* a comment naming #152 is prose, not a colour */
  background: #0b0d12;
}
";
    let raw = raw_colours(css);
    let values: Vec<&str> = raw.iter().map(|(_, colour)| colour.as_str()).collect();
    assert_eq!(values, ["#ffffff", "#0b0d12"]);
}

/// The light theme is written twice: once for `data-theme="light"`, and once for a
/// `data-theme`-less page that prefers light - `Theme::System` writes no attribute
/// at all, so both are load-bearing and the file says to keep them in step.
/// A token set in one and not the other is an OS-following page that is exactly one
/// colour out of theme, which is the kind of thing nobody notices until a screenshot
/// of a Mac disagrees with a screenshot of a Windows box.
#[test]
fn the_two_light_blocks_set_the_same_tokens() {
    let mut prefers: Vec<String> = Vec::new();
    let mut explicit: Vec<String> = Vec::new();
    for declaration in declarations() {
        let Some(innermost) = declaration.selectors.last() else {
            continue;
        };
        if innermost == ":root:not([data-theme])"
            && declaration
                .selectors
                .iter()
                .any(|selector| selector.starts_with("@media (prefers-color-scheme: light"))
        {
            prefers.push(declaration.property.clone());
        } else if innermost.contains("[data-theme=\"light\"]") {
            explicit.push(declaration.property.clone());
        }
    }
    prefers.sort();
    prefers.dedup();
    explicit.sort();
    explicit.dedup();
    assert!(
        !explicit.is_empty(),
        "no [data-theme=\"light\"] block found: the scan is broken"
    );
    assert_eq!(
        prefers, explicit,
        "the prefers-color-scheme block and the data-theme=\"light\" block differ"
    );
}

/// A colour written as a `var()` - including inside `color-mix`, which is how the
/// shell tints its surfaces - is not a literal and stays out of the report.
#[test]
fn the_token_scan_ignores_variants_of_a_token() {
    let css = "
.card {
  color: var(--text);
  background: color-mix(in srgb, var(--danger) 14%, transparent);
  box-shadow: 0 12px 32px rgb(0 0 0 / 45%);
}
";
    assert!(raw_colours(css).is_empty(), "{:?}", raw_colours(css));
}
