//! Every space in `style.css` is on the 4px scale (M7-19, #152).
//!
//! The scale is the token group at the top of the file: `--space-1` is 4px and
//! every step above it is a multiple of 4. A `gap: 2px` here and a
//! `padding: 6px` there is how a shell ends up with eleven different distances
//! between things that were meant to be three.
//!
//! What counts as a space is therefore a margin, a padding, or a gap - the three
//! ways this stylesheet puts distance between things. Position (`top`, `inset`)
//! and size (`width`, `height`) are not spacing: the 2px that centres a 16px
//! thumb in a 22px toggle track is the shape of a toggle, and a rule that called
//! it a violation would be answered by changing the shape rather than the number.
//!
//! A negative `calc()` of a space token - `margin: var(--space-4) calc(var(--space-4) * -1)`,
//! which is how the drawer's hint bar pulls itself out to the drawer's edges - is
//! on the scale whatever its sign, and so is `0`.

mod support;

use support::{declarations, declarations_in};

/// The properties that put distance between things.
const SPACING: &[&str] = &[
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "gap",
    "row-gap",
    "column-gap",
];

/// The step every space is a multiple of.
const SCALE: f64 = 4.0;

/// Every `(line, text)` of `css` whose spacing is not a multiple of 4px.
fn off_scale(css: &str) -> Vec<(usize, String)> {
    declarations_in(css)
        .iter()
        .filter(|declaration| SPACING.contains(&declaration.property.as_str()))
        .filter_map(|declaration| {
            // Every length of the value, as the number of pixels it names. A
            // declaration may hold more than one - `padding: 2px var(--space-2)`
            // - and a `calc()` inside one still has to land on the scale.
            let offenders = lengths(&declaration.value)
                .into_iter()
                .filter(|pixels| pixels % SCALE != 0.0)
                .collect::<Vec<_>>();
            (!offenders.is_empty()).then(|| {
                (
                    declaration.line,
                    format!(
                        "{}: {} ({})",
                        declaration.property,
                        declaration.value,
                        offenders
                            .iter()
                            .map(|pixels| format!("{pixels}px"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            })
        })
        .collect()
}

/// The pixel lengths of one declaration value.
///
/// Numbers without a unit are not lengths here, and neither is a token: the
/// token's own value is checked where it is written, and a `var()` is not allowed
/// to be off scale in a file where every space token is on it.
fn lengths(value: &str) -> Vec<f64> {
    let mut found = Vec::new();
    let chars: Vec<char> = value.chars().collect();
    let mut at = 0usize;
    while at < chars.len() {
        if !(chars[at].is_ascii_digit() || chars[at] == '-') {
            at += 1;
            continue;
        }
        let start = at;
        if chars[at] == '-' {
            at += 1;
        }
        while at < chars.len() && (chars[at].is_ascii_digit() || chars[at] == '.') {
            at += 1;
        }
        let unit: String = chars[at..].iter().take(2).collect();
        if unit == "px" {
            let text: String = chars[start..at].iter().collect();
            if let Ok(pixels) = text.parse::<f64>() {
                found.push(pixels.abs());
            }
            at += 2;
        }
    }
    found
}

#[test]
fn every_space_is_on_the_four_pixel_scale() {
    let off = off_scale(&support::style_css());
    assert!(
        off.is_empty(),
        "style.css puts a space off the 4px scale: {}",
        off.iter()
            .map(|(line, text)| format!("line {line} ({text})"))
            .collect::<Vec<_>>()
            .join("; ")
    );
}

/// The scan is not vacuous: `4px` passes and `2px` does not, and a mixed value is
/// judged on the part of it that was written out.
#[test]
fn the_scan_finds_a_space_that_is_not_on_the_scale() {
    let css = "
.a { gap: 2px; }
.b { padding: 6px var(--space-2); }
.c { margin: 0; padding: 0 var(--space-3); }
.d { padding: var(--space-4); margin: var(--space-4) calc(var(--space-4) * -1); }
.e { gap: 12px; padding: 16px; }
.f { border-radius: 3px; width: 22px; top: 2px; }
";
    let off = off_scale(css);
    let texts: Vec<&str> = off.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        texts,
        ["gap: 2px (2px)", "padding: 6px var(--space-2) (6px)"]
    );
}

/// `just` the spacing properties are read, and the file's own tokens are the
/// scale: a test that silently matched no declaration would pass anything.
#[test]
fn the_scan_reads_the_real_file_at_all() {
    let spacing = declarations()
        .iter()
        .filter(|declaration| SPACING.contains(&declaration.property.as_str()))
        .count();
    assert!(
        spacing > 20,
        "only {spacing} spacing declarations found in style.css: the scan is broken"
    );
}
