//! Reading `crates/web/style.css` as data, for the native tests of this crate.
//!
//! Every test in this directory wants a different question asked of the one
//! stylesheet - is a colour a literal, does this text contrast with that surface,
//! is this padding on the 4px scale - and all of them want the same two things
//! first: comments gone, and the file as declarations with the selectors that own
//! them. Both are here so the reading is written once.
//!
//! Each test binary includes this module and uses part of it, hence the allow.

#![allow(dead_code)]

use std::path::Path;

/// One declaration, with the selectors that contain it.
#[derive(Debug, Clone)]
pub struct Declaration {
    /// The block stack that this declaration is inside, outermost first:
    /// `["@media (prefers-color-scheme: light)", ":root:not([data-theme])"]`.
    pub selectors: Vec<String>,
    /// The property, lowercased and trimmed: `color`.
    pub property: String,
    /// The value, trimmed, with the trailing `;` gone: `var(--text-muted)`.
    pub value: String,
    /// The line the property starts on, 1-based, in the *real* file.
    pub line: usize,
}

impl Declaration {
    /// Whether this declaration is inside a theme block - the one place a raw
    /// colour is allowed, and where the token values live.
    pub fn is_theme(&self) -> bool {
        self.selectors
            .iter()
            .any(|selector| selector.contains(":root") || selector.contains("[data-theme"))
    }
}

/// The stylesheet under test.
pub fn style_css() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("style.css");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// `css` with every C-style comment gone, replaced by spaces so that line numbers
/// and word boundaries survive.
pub fn strip_comments(css: &str) -> String {
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

/// Every declaration of `css`, in order.
///
/// A one-pass walk of braces, colons and semicolons rather than a line regex,
/// because a declaration can span lines (`box-shadow:`) and because the selector
/// a `{` closes is what decides whether what follows is a theme or a rule. An
/// `@media` is a block of its own, so a `:root` inside one is reported with both.
pub fn declarations_in(css: &str) -> Vec<Declaration> {
    let css = strip_comments(css);
    let mut declarations = Vec::new();
    let mut selectors: Vec<String> = Vec::new();
    // The text being collected: the selector before a `{`, or the property and
    // value between a `;` and the next `;` or `}`.
    let mut pending = String::new();
    let mut line = 1usize;
    // The line `pending` started on, so a report can point at the property rather
    // than at the end of its value.
    let mut start = 1usize;
    for c in css.chars() {
        match c {
            '\n' => {
                line += 1;
                // A declaration cannot end without a `;` or a `}`; a newline in the
                // middle of one is not a reason to forget where it began.
                if pending.trim().is_empty() {
                    start = line;
                }
            }
            '{' => {
                // Trimmed, so a caller compares a selector rather than a
                // selector with the whitespace of the line above it attached.
                selectors.push(std::mem::take(&mut pending).trim().to_string());
                start = line;
            }
            '}' => {
                emit(&mut declarations, &selectors, &pending, start);
                selectors.pop();
                pending.clear();
                start = line;
            }
            ';' => {
                emit(&mut declarations, &selectors, &pending, start);
                pending.clear();
                start = line;
            }
            _ => pending.push(c),
        }
    }
    declarations
}

/// Every declaration of the real `style.css`.
pub fn declarations() -> Vec<Declaration> {
    declarations_in(&style_css())
}

/// Adds `pending` to `declarations` if it reads as `property: value`.
fn emit(out: &mut Vec<Declaration>, selectors: &[String], pending: &str, line: usize) {
    let pending = pending.trim();
    // Outside a block there is nothing to emit, and the `:` of
    // `@media (prefers-color-scheme: light)` is not a property for the same
    // reason: an `@` rule is a selector, not a declaration.
    let Some(last) = selectors.last() else {
        return;
    };
    if last.starts_with('@') {
        return;
    }
    let Some((property, value)) = pending.split_once(':') else {
        return;
    };
    if property.trim().is_empty() || value.trim().is_empty() {
        return;
    }
    out.push(Declaration {
        selectors: selectors.to_vec(),
        property: property.trim().to_string(),
        value: value.trim().to_string(),
        line,
    });
}

/// The `#`-prefixed colour literals of `text`: `#abc`, `#abca`, `#aabbcc`,
/// `#aabbccdd`, and nothing else.
///
/// A word test follows rather than a bare length check, because the shell's
/// comments cite issue numbers - and `#152` is exactly as hex as a grey is.
pub fn colour_literals(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0usize;
    while at < chars.len() {
        if chars[at] != '#' {
            at += 1;
            continue;
        }
        let mut end = at + 1;
        while end < chars.len() && chars[end].is_ascii_hexdigit() {
            end += 1;
        }
        let digits = &chars[at + 1..end];
        let len = digits.len();
        let stops = chars
            .get(end)
            .is_none_or(|next| !next.is_ascii_alphanumeric());
        if matches!(len, 3 | 4 | 6 | 8) && stops {
            found.push(format!("#{}", digits.iter().collect::<String>()));
        }
        at = end;
    }
    found
}
