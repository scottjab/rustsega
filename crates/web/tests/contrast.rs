//! Text on surfaces meets WCAG 2.1 AA in both themes (M7-19, #152).
//!
//! The tool is this test: it reads the token values out of `style.css`, works out
//! what the stylesheet paints as text with (`every color:` declaration), and
//! applies the WCAG 2.1 contrast formula - `(L1 + 0.05) / (L2 + 0.05)` over the
//! relative luminance of sRGB, which is 0.2126 R + 0.7152 G + 0.0722 B with the
//! sRGB transfer function linearised - and holds the result at 4.5:1, the bar for
//! text under 18pt/14pt-bold. The numbers it reports are the numbers in the PR.
//!
//! Two things fall out of doing it that way rather than by eye. The first is the
//! pair list: it is derived from the file, so a rule that starts painting a new
//! colour as text is checked the moment it is written. The second is that the
//! light theme is not the dark theme with the surfaces swapped - a colour that is
//! a text colour in the dark theme has to be a text colour in the light one too,
//! which is what M7-19 found and fixed.
//!
//! Borders, glows and the touch overlay's pressed tint are not text. 1.4.11 asks a
//! different bar of them - 3:1 rather than 4.5:1 - and the two that carry meaning are
//! checked for it below: `--border` on every surface, and the toast kind stripes
//! (#407). `browser_boundary_contrast.rs` is the live-DOM half of the same promise,
//! because a token can be compliant and still not be what an element is painted with.
//! Glows and the pressed tint remain unchecked by anything.

mod support;

use std::collections::BTreeMap;

use support::{declarations, ratio, strip_comments};

/// The four tokens the shell paints behind text.
const SURFACES: [&str; 4] = ["bg", "bg-screen", "surface", "surface-2"];

/// The minimum contrast AA asks of text that is neither large nor bold: 4.5:1.
const AA: f64 = 4.5;

/// One theme: token name (without `--`) to its six-digit hex value.
type Theme = BTreeMap<String, String>;

/// The dark theme, from the `:root` block.
fn dark() -> Theme {
    let mut tokens = Theme::new();
    for declaration in declarations() {
        if declaration.is_theme()
            && declaration.selectors.last().map(String::as_str) == Some(":root")
        {
            insert(&mut tokens, &declaration.property, &declaration.value);
        }
    }
    tokens
}

/// The light theme: `:root`, then every `[data-theme="light"]` and
/// `prefers-color-scheme: light` block on top of it.
fn light() -> Theme {
    let mut tokens = dark();
    for declaration in declarations() {
        if !declaration.is_theme() {
            continue;
        }
        let names_light = declaration
            .selectors
            .iter()
            .any(|selector| selector.contains("[data-theme=\"light\"]"))
            || declaration
                .selectors
                .iter()
                .any(|selector| selector.starts_with("@media (prefers-color-scheme: light"));
        if names_light {
            insert(&mut tokens, &declaration.property, &declaration.value);
        }
    }
    tokens
}

/// Records `value` as `name` when it is a colour, and ignores the tokens that are
/// not (`--focus` is a shadow, and `--space-1` is a length).
fn insert(tokens: &mut Theme, property: &str, value: &str) {
    let Some(name) = property.strip_prefix("--") else {
        return;
    };
    if value.starts_with('#') && value.len() == 7 {
        tokens.insert(name.to_string(), value.to_string());
    }
}

/// Every token the stylesheet paints as `color:`, without the `--`.
///
/// This is the list the contrast rule runs over: a token the file never paints as
/// text is a border or a glow and belongs to 1.4.11, and a token a rule starts
/// painting as text is on this list before the change is reviewed.
fn painted_as_text() -> Vec<String> {
    let mut names: Vec<String> = declarations()
        .iter()
        .filter(|declaration| declaration.property == "color")
        .filter_map(|declaration| {
            declaration
                .value
                .strip_prefix("var(")?
                .strip_suffix(')')?
                .strip_prefix("--")
                .map(|name| name.to_string())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The failures of one theme: `(foreground, background, ratio)` for every text
/// colour that is not 4.5:1 against a surface it can be painted on.
fn failures(theme: &Theme, label: &str) -> Vec<(String, String, f64)> {
    let mut failures = Vec::new();
    for name in painted_as_text() {
        // `--on-accent` is not a text colour on a surface: it is the other half of
        // `--accent`, and its pair is checked on its own below.
        if name == "on-accent" {
            continue;
        }
        let Some(foreground) = theme.get(&name) else {
            failures.push((name.clone(), "MISSING TOKEN".into(), 0.0));
            continue;
        };
        for surface in SURFACES {
            let background = theme
                .get(surface)
                .unwrap_or_else(|| panic!("{} theme has no --{surface}", label));
            let ratio = ratio(foreground, background);
            if ratio < AA {
                failures.push((name.clone(), format!("--{surface}"), ratio));
            }
        }
    }
    failures
}

fn report(failures: &[(String, String, f64)]) -> String {
    failures
        .iter()
        .map(|(name, surface, ratio)| {
            format!("--{name} on {surface} is {ratio:.2}:1, needs {AA}:1")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[test]
fn text_on_every_surface_passes_aa_in_both_themes() {
    for (label, theme) in [("dark", dark()), ("light", light())] {
        let failures = failures(&theme, label);
        assert!(
            failures.is_empty(),
            "{} theme: {}",
            label,
            report(&failures)
        );
    }
}

/// The one pair the surface rule cannot speak for: the primary button, the
/// selected listbox option and a pressed touch control all paint `--on-accent`
/// text on `--accent`.
#[test]
fn text_on_the_accent_passes_aa_in_both_themes() {
    for (label, theme) in [("dark", dark()), ("light", light())] {
        let text = &theme["on-accent"];
        let accent = &theme["accent"];
        let ratio = ratio(text, accent);
        assert!(
            ratio >= AA,
            "{label} theme: --on-accent on --accent is {ratio:.2}:1, needs {AA}:1"
        );
    }
}

/// The contrast function is the WCAG one, not a plausible relative-luminance
/// average: pinned on the values the WebAIM checker gives these pairs.
#[test]
fn the_ratio_is_the_wcag_one() {
    assert!((ratio("#000000", "#ffffff") - 21.0).abs() < 0.01);
    assert!((ratio("#ffffff", "#000000") - 21.0).abs() < 0.01);
    assert!((ratio("#767676", "#ffffff") - 4.54).abs() < 0.01);
    assert!((ratio("#777777", "#ffffff") - 4.48).abs() < 0.01);
    assert!((ratio("#000000", "#000000") - 1.0).abs() < 0.01);
    // The two sides of a pair are interchangeable, and the bar is on the same side
    // of 4.5 that the checker puts it.
    assert!(ratio("#767676", "#ffffff") >= AA);
    assert!(ratio("#777777", "#ffffff") < AA);
}

/// Every token the file paints as text has a value in both themes - which is how
/// a token that was only ever declared in `:root` is noticed at all.
#[test]
fn the_text_colours_are_the_ones_with_values() {
    let theme = light();
    for name in painted_as_text() {
        assert!(theme.contains_key(&name), "--{name} has no light value");
    }
}

/// `strip_comments` is the difference between this test working and it failing on
/// a prose `#152`, so it is pinned here as well.
#[test]
fn comments_are_gone_and_lines_are_not() {
    let css = "/* one\n   two #152 */\n.a { color: var(--text); }\n";
    let stripped = strip_comments(css);
    assert_eq!(stripped.lines().count(), css.lines().count());
    assert!(!stripped.contains("#152"));
    assert_eq!(declarations_count(css), 1);
}

fn declarations_count(css: &str) -> usize {
    support::declarations_in(css).len()
}

// ------------------------------------------------- 1.4.11: boundaries ----

/// The bar WCAG 2.1 asks of a non-text boundary: 3:1, not text's 4.5:1.
const AA_NON_TEXT: f64 = 3.0;

/// The colours a toast's 3px kind stripe is painted with, as `.toast--*` chooses
/// them. The stripe is the only thing that says which kind of message it is, so
/// 1.4.11 asks it to be visible against the toast it is painted on.
const TOAST_KINDS: [&str; 4] = ["accent", "ok", "warn", "danger"];

/// The `--border` of every component in the shell against every surface a boundary
/// can sit on (#407).
///
/// `--border` is the whole boundary: it is used 21 times and `.btn`'s own fill is
/// 1.20:1 against the page, so there is no other cue for where a control ends. The
/// bar is the pair with the least contrast, because a boundary that disappears
/// against one neighbour is a boundary that disappeared.
#[test]
fn borders_meet_1_4_11_on_every_surface_in_both_themes() {
    for (label, theme) in [("dark", dark()), ("light", light())] {
        let border = theme
            .get("border")
            .unwrap_or_else(|| panic!("{label} theme has no --border"));
        let mut misses = Vec::new();
        for surface in SURFACES {
            let background = theme
                .get(surface)
                .unwrap_or_else(|| panic!("{label} theme has no --{surface}"));
            let ratio = ratio(border, background);
            if ratio < AA_NON_TEXT {
                misses.push(format!("--border on --{surface} is {ratio:.2}:1"));
            }
        }
        assert!(
            misses.is_empty(),
            "{label} theme borders below the {AA_NON_TEXT}:1 of 1.4.11: {}",
            misses.join("; ")
        );
    }
}

/// Each toast kind stripe against the toast it is painted on (`--surface-2`, per
/// `.toast { background: var(--surface-2) }`), in both themes (#407).
///
/// This is the test that catches a token the light theme forgot. `.toast--ok` was
/// the dark theme's mint on near-white at 1.55:1, and `contrast.rs` could not see it
/// either way: it checks tokens the file paints as `color:`, and `--ok` is only ever
/// a `border-color`.
#[test]
fn toast_kind_stripes_meet_1_4_11_in_both_themes() {
    for (label, theme) in [("dark", dark()), ("light", light())] {
        let surface = theme
            .get("surface-2")
            .unwrap_or_else(|| panic!("{label} theme has no --surface-2"));
        let mut misses = Vec::new();
        for kind in TOAST_KINDS {
            let Some(colour) = theme.get(kind) else {
                misses.push(format!("--{kind} has no {label} value"));
                continue;
            };
            let ratio = ratio(colour, surface);
            if ratio < AA_NON_TEXT {
                misses.push(format!("--{kind} on --surface-2 is {ratio:.2}:1"));
            }
        }
        assert!(
            misses.is_empty(),
            "{label} theme toast stripes below the {AA_NON_TEXT}:1 of 1.4.11: {}",
            misses.join("; ")
        );
    }
}
