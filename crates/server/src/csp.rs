//! The document policy of the served page (#411).
//!
//! `script-src` used to end in `'unsafe-inline'`, which permits every inline
//! script on the page including one injected later by anything that can write into
//! `index.html`. The only reason it was there at all was trunk's bootstrap: the
//! `<script type="module">` trunk injects into `dist/index.html` to call the wasm
//! loader, which M8-01 (#155) found Firefox refuses to run under
//! `script-src 'self' 'wasm-unsafe-eval'`. Every subresource of that page returns
//! 200 - the `.js` and the `.wasm` both fetch fine - while `window.wasmBindings`
//! stays `undefined` and no canvas is ever mounted, which is why the policy is
//! pinned in tests at the level of the script running rather than the response
//! arriving.
//!
//! Naming that one script by hash closes the hole without reopening it: the browser
//! runs exactly the inline scripts whose SHA-256 it was given and nothing else. So
//! the hash is read out of the very `index.html` this server is holding, which means
//! it cannot drift from what is served, and cannot be hardcoded either: trunk
//! regenerates the bootstrap - its text names the hashed `.js` and `.wasm` - so a
//! pasted base64 literal would silently become wrong the next time anyone runs
//! `just build`, and "silently wrong" here is the dead-app-with-200s again.
//! `crates/server/tests/csp.rs` pins the hashing itself against values computed by
//! another tool, and `crates/web/tests/browser_csp.rs` pins the same fixture against
//! a browser that really runs it.

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use crate::assets::AssetSource;

/// The policy of a page whose inline scripts are named by `hashes`.
///
/// `style-src` keeps `'unsafe-inline'`: Leptos writes `style=` attributes, and an
/// attribute is inline style as far as CSP is concerned, so removing it would blank
/// the layout the same way #155 found. Only the script hole is being closed.
///
/// `wasm-unsafe-eval` stays for the same reason and is *not* the wide-open door it
/// sounds like: it permits compiling the WebAssembly the page fetches under `'self'`
/// and nothing in JavaScript. `crates/web/tests/browser_csp.rs` compiles a module
/// under exactly this policy to show that it is still needed.
pub fn policy(hashes: &[String]) -> String {
    let named: String = hashes.iter().map(|hash| format!(" '{hash}'")).collect();
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{named}; style-src 'self' \
         'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'"
    )
}

/// The policy for `assets`, read from the `index.html` it will actually serve.
///
/// Called once when [`crate::AppState`] is built, not per request. A page with no
/// inline script - the placeholder of a binary built without `embed-web` - gets the
/// policy with nothing added to `script-src`, which is narrower than any page that
/// has one.
pub fn for_assets(assets: &dyn AssetSource) -> String {
    let html = assets
        .get("index.html")
        .map(|asset| asset.bytes)
        .unwrap_or_default();
    policy(&script_hashes(&html))
}

/// `'sha256-<base64>'` for every inline `<script>` of `html`, in document order.
///
/// The scan follows how an HTML tokenizer treats `<script>`: the element's content
/// is *raw text*, so it ends at the first `</script` and nothing in it is
/// entity-decoded - which is what makes "hash the bytes between the tags" the same
/// thing the browser hashes. A tag with `src` has no content to hash.
pub fn script_hashes(html: &[u8]) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut at = 0;
    while let Some(open) = find_case_insensitive(html, b"<script", at) {
        let tag_end = match tag_end(html, open + b"<script".len()) {
            Some(end) => end,
            // A document whose last script tag never closes: nothing further can be
            // true of it, and guessing would hash something the browser will not run.
            None => break,
        };
        let tag = &html[open + b"<script".len()..tag_end];
        let content_starts = tag_end + 1;
        let close = match find_case_insensitive(html, b"</script", content_starts) {
            // Like an unclosed tag: a browser will not run it, so it is not hashed.
            None => break,
            Some(close) => close,
        };
        if !has_src_attribute(tag) {
            hashes.push(hash(&html[content_starts..close]));
        }
        at = close + b"</script".len();
    }
    hashes
}

/// The index of the `>` that ends the tag starting at `from`, or `None`.
///
/// Quotes are tracked, because `<script integrity="a>b">` is one tag: an unquoted
/// scan for `>` would end the tag early and hash half an attribute value's
/// remainder as script text.
fn tag_end(html: &[u8], from: usize) -> Option<usize> {
    let mut quote = None::<u8>;
    let mut at = from;
    while at < html.len() {
        let byte = html[at];
        match quote {
            Some(mark) if byte == mark => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return Some(at),
                _ => {}
            },
        }
        at += 1;
    }
    None
}

/// Whether the attributes of a `<script` tag carry `src`.
///
/// Attribute position matters: `data-src` is not `src`. A name counts as `src` when
/// whitespace precedes it and the next non-space is `=`.
fn has_src_attribute(tag: &[u8]) -> bool {
    let tag = &tag.to_ascii_lowercase();
    let name = b"src";
    let mut at = 0;
    while let Some(found) = find(tag, name, at) {
        let preceded_by_space = found > 0 && tag[found - 1].is_ascii_whitespace();
        let mut value = found + name.len();
        while value < tag.len() && tag[value].is_ascii_whitespace() {
            value += 1;
        }
        if preceded_by_space && value < tag.len() && tag[value] == b'=' {
            return true;
        }
        at = found + name.len();
    }
    false
}

/// `sha256-` and the standard base64 of those bytes, as CSP wants them.
fn hash(script: &[u8]) -> String {
    let digest = Sha256::digest(script);
    format!(
        "sha256-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// The index of the first `needle` at or after `from`, comparing ASCII-insensitively.
fn find_case_insensitive(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    let lower = haystack.to_ascii_lowercase();
    find(&lower, &needle.to_ascii_lowercase(), from)
}

/// The index of the first `needle` at or after `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|at| &haystack[*at..*at + needle.len()] == needle)
}
