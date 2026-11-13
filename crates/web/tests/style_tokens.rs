//! Every colour in `style.css` comes from a token (M7-19, #152).
//!
//! The theme of M7-02 (#135) is the token block at the top of the file: `:root`
//! for the dark values and `[data-theme="light"]` - plus the
//! `prefers-color-scheme` copy of it - for the light ones. A rule that paints
//! `#5b8cff` instead of `var(--accent)` is invisible to both of those blocks, so
//! it keeps the dark blue after the page turns light, and it is the reason the
//! light theme cannot be trusted without hunting the file for literals.
//!
//! Native test, no browser: it reads the stylesheet as text. Comments are stripped
//! first because this file cites issue numbers in them, and `#152` looks exactly
//! like a short hex grey to anything that does not know it is prose.

use std::path::Path;

/// The stylesheet under test.
fn style_css() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("style.css");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// `css` with every C-style comment gone, replaced by spaces so that line numbers
/// and word boundaries survive.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    let mut in_comment = false;
    while let Some(c) = chars.next() {
        match (in_comment, c) {
            // The end of a comment: both characters go, and the newline that
            // follows it stays where it was so a report names the right line.
            (true, '*') if chars.peek() == Some(&'/') => {
                chars.next();
                in_comment = false;
                out.push(' ');
            }
            // Inside a comment everything but a newline becomes a space: word
            // boundaries survive, and so does the line count.
            (true, '\n') => out.push('\n'),
            (true, _) => out.push(' '),
            (false, '/') if chars.peek() == Some(&'*') => {
                chars.next();
                in_comment = true;
                out.push(' ');
            }
            (false, other) => out.push(other),
        }
    }
    out
}

/// Whether `selector` names a theme block: the place a raw colour is allowed.
fn is_token_selector(selector: &str) -> bool {
    let selector = selector.trim();
    selector.contains(":root") || selector.contains("[data-theme")
}

/// Every `(line, text)` of `css` that is a colour literal outside a token block.
///
/// A one-pass walk of braces and semicolons rather than a line regex, because the
/// value can span lines (`box-shadow:`) and a declaration is what a colour lives
/// in - and because the selector that a `{` closes is what decides whether the
/// body of that block is a theme or a rule.
fn raw_colours(css: &str) -> Vec<(usize, String)> {
    let css = strip_comments(css);
    let mut found = Vec::new();
    // The selector of each block we are inside, innermost last. A `@media` is a
    // block too, and its body is not a rule: `@media (…) { :root { … } }` is
    // inside `@media` and then inside `:root`, and the inner one decides.
    let mut blocks: Vec<String> = Vec::new();
    let mut pending = String::new();
    let mut line = 1usize;
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                line += 1;
                pending.clear();
            }
            '{' => {
                blocks.push(std::mem::take(&mut pending));
            }
            '}' => {
                blocks.pop();
                pending.clear();
            }
            ';' => pending.clear(),
            // A colour literal: `#` followed by three, four, six or eight hex
            // digits and then something that cannot continue the word. Anything
            // longer is not a colour either, and is left alone rather than
            // half-matched.
            '#' => {
                let mut digits = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_hexdigit() {
                        digits.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let is_colour = matches!(digits.len(), 3 | 4 | 6 | 8)
                    && !chars
                        .peek()
                        .is_some_and(|next| next.is_ascii_alphanumeric() || *next == '-');
                if is_colour && !blocks.iter().any(|s| is_token_selector(s)) {
                    found.push((line, format!("#{digits}")));
                }
                pending.clear();
            }
            _ => pending.push(c),
        }
    }
    found
}

#[test]
fn every_colour_in_the_stylesheet_is_a_token() {
    let raw = raw_colours(&style_css());
    assert!(
        raw.is_empty(),
        "style.css paints with a raw colour outside the token blocks: {}",
        raw.iter()
            .map(|(line, value)| format!("line {line} ({value})"))
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
    let values: Vec<&str> = raw.iter().map(|(_, value)| value.as_str()).collect();
    assert_eq!(values, ["#ffffff", "#0b0d12"]);
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
