//! Every `IconButton` in the shell has an accessible name (M7-19, #152).
//!
//! [`web::ui::button::IconButton`] draws a glyph and nothing else, so `label` is
//! the only thing a screen reader has to say what the button does, and the only
//! thing `title` can offer a sighted pointer. It is a required prop for exactly
//! that reason - but a required prop is still satisfiable by
//! `label=String::new()`, and a call site can be edited away as easily as it was
//! written.
//!
//! So this is the grep-style check the ticket asks for: read every `.rs` file
//! under `src/`, take each `<IconButton …>` tag, and insist on a `label` that is
//! either a non-empty string literal or an expression.

use std::path::{Path, PathBuf};

/// Every `.rs` file under `src/`, sorted, so a report is reproducible.
fn sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{} must be readable: {e}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|e| panic!("{}: unreadable entry: {e}", dir.display()))
                .path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no .rs files found under {}: the walk is broken",
        root.display()
    );
    files
}

/// The `<IconButton …>` tags of `source`, as `(line, tag text)`.
///
/// A tag runs from `<IconButton` to the first `/>` that closes it. Attribute
/// values are allowed to contain `{`, `}` and `>` - an `on_click` is a
/// `Callback::new(move |_| …)` whose body is braces - so the only thing worth
/// matching on is the self-closing slash, which no attribute value in this crate
/// contains.
fn icon_button_tags(source: &str) -> Vec<(usize, String)> {
    let mut tags = Vec::new();
    let mut rest = source;
    let mut line = 1usize;
    while let Some(at) = rest.find("<IconButton") {
        line += rest[..at].matches('\n').count();
        let after = &rest[at + "<IconButton".len()..];
        let end = after
            .find("/>")
            .unwrap_or_else(|| panic!("an <IconButton tag opened on line {line} is never closed"));
        tags.push((line, after[..end].to_string()));
        rest = &after[end + 2..];
    }
    tags
}

/// Whether one `<IconButton …>` attribute list carries a usable `label`.
///
/// A string literal has to have something in it - `label="".to_string()` satisfies
/// the type and leaves the button nameless - and anything else is an expression
/// whose value this test cannot see, so it is taken on trust.
fn has_label(attributes: &str) -> bool {
    let mut rest = attributes;
    while let Some(at) = find_word(rest, "label") {
        let after = rest[at + "label".len()..].trim_start();
        // `label: something` is the struct form, which this component does not use
        // but a caller in another file might.
        let value = match after
            .strip_prefix('=')
            .or_else(|| after.strip_prefix(':'))
        {
            Some(value) => value.trim_start(),
            None => {
                rest = &rest[at + "label".len()..];
                continue;
            }
        };
        if let Some(literal) = value.strip_prefix('"') {
            if let Some(end) = literal.find('"') {
                if !literal[..end].trim().is_empty() {
                    return true;
                }
            }
        } else if !value.is_empty() {
            return true;
        }
        rest = &rest[at + "label".len()..];
    }
    false
}

/// The index of the first `word` in `haystack` that is a whole word.
fn find_word(haystack: &str, word: &str) -> Option<usize> {
    haystack.match_indices(word).find_map(|(at, _)| {
        // `-` counts: `aria-label` is not `label`, whatever the substring says.
        let before_ok = !haystack[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
        let after = &haystack[at + word.len()..];
        let after_ok = !after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        (before_ok && after_ok).then_some(at)
    })
}

#[test]
fn every_icon_button_has_a_label() {
    let mut missing = Vec::new();
    let mut checked = 0usize;
    for file in sources() {
        let source = std::fs::read_to_string(&file).expect("source is readable");
        for (line, tag) in icon_button_tags(&source) {
            checked += 1;
            if !has_label(&tag) {
                missing.push(format!("{}:{line}", file.display()));
            }
        }
    }
    assert!(
        checked > 0,
        "no <IconButton> call sites found: the scan is broken"
    );
    assert!(
        missing.is_empty(),
        "IconButton without a label at: {}",
        missing.join(", ")
    );
}

/// The scan is not vacuous, on both halves: a bare tag is reported, and a tag with
/// a label - literal, constant, or computed - is not.
#[test]
fn the_label_scan_finds_a_missing_label() {
    let bare = r#"<IconButton path=icons::CLOSE on_click=Callback::new(move |_| close()) />"#;
    assert!(!has_label(bare));
    assert_eq!(icon_button_tags(bare).len(), 1);

    let empty = r#"<IconButton label="".to_string() path=icons::CLOSE />"#;
    assert!(!has_label(empty));

    let literal = r#"<IconButton label="Close".to_string() path=icons::CLOSE />"#;
    assert!(has_label(literal));

    // A computed label, and one whose attribute list contains the braces of a
    // closure: neither is a bare tag.
    let computed = r#"<IconButton
        label=format!("Remove {title} from the list")
        path=icons::CLOSE
        on_click=Callback::new(move |_| { remove(); })
    />"#;
    assert!(has_label(computed));
    assert_eq!(icon_button_tags(computed).len(), 1);
}

/// Two buttons in one file are two findings, and the line number each is reported
/// against counts the newlines before it rather than restarting at the tag.
#[test]
fn the_scan_reports_every_button_of_a_file() {
    let source = "use leptos::prelude::*;\n\nview! {\n    <IconButton path=A />\n    <IconButton label=\"Go\" path=B />\n};\n";
    let tags = icon_button_tags(source);
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].0, 4);
    assert_eq!(tags[1].0, 5);
    assert!(!has_label(&tags[0].1));
    assert!(has_label(&tags[1].1));
}
