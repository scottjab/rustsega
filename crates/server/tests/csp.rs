//! The hash of trunk's inline bootstrap, which is what `script-src` now names (#411).
//!
//! Every base64 literal in this file is `base64(sha256(text))` computed with
//! `python3` (`hashlib` + `base64`) and written down by hand next to the text it
//! hashes. That is deliberate: the point of these tests is that the server's hasher
//! agrees with an implementation nobody in this build controls, so computing the
//! expected value with `sha2` here would prove nothing.
//!
//! The first literal is shared with `crates/web/tests/browser_csp.rs`, which proves
//! the other direction - that Firefox 159 really runs a script for this hash and
//! refuses one that is not listed. Neither test alone is enough: this file cannot
//! show a browser accepts the hash, and the browser test cannot show the server
//! computes it.

use server::assets::AssetSource as _;

/// A script in the shape trunk injects: mark that it ran, then compile a wasm
/// module under `wasm-unsafe-eval`.
const BOOTSTRAP: &str = "document.body.appendChild(document.createElement('i')).id='ran';\
try{new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0]));\
document.body.appendChild(document.createElement('i')).id='wasm'}\
catch(e){document.body.appendChild(document.createElement('i')).id='wasm-'+e.name}";

/// `base64(sha256(BOOTSTRAP))`.
const BOOTSTRAP_SHA: &str = "sha256-WjYXdo+j/6Ky7KwdT89atX0ifQft8ykslCoAcFJkg+Q=";

/// A second inline script, for the ordering test.
const SECOND: &str = "window.__second=1";
/// `base64(sha256(SECOND))`.
const SECOND_SHA: &str = "sha256-NfWSX80pY2+XjdiacJ9mWoSugYQeIYKVO+6C/t69nhA=";

/// A script whose text contains `<`, `&&` and the literal characters `&amp;`, so an
/// implementation that entity-decoded the script text would hash something else.
const RAW: &str =
    "\n  const ok = 1 < 2 && '&amp;' !== '';\n  document.title = ok ? 'raw' : 'entity';\n ";
/// `base64(sha256(RAW))`, over those exact bytes, newlines and spaces included.
const RAW_SHA: &str = "sha256-idqjwapKbrf62UkxQ/ffiOMbCrxHR9JYFCJuGov3rpw=";

fn page(scripts: &str) -> Vec<u8> {
    format!("<!doctype html><html><head></head><body>{scripts}</body></html>").into_bytes()
}

#[test]
fn an_inline_module_hashes_to_the_value_a_browser_will_accept() {
    let html = page(&format!(r#"<script type="module">{BOOTSTRAP}</script>"#));
    assert_eq!(server::csp::script_hashes(&html), vec![BOOTSTRAP_SHA]);
}

#[test]
fn every_inline_script_of_the_page_is_hashed_in_document_order() {
    let html = page(&format!(
        r#"<script>{BOOTSTRAP}</script><script type="module">{SECOND}</script>"#
    ));
    assert_eq!(
        server::csp::script_hashes(&html),
        vec![BOOTSTRAP_SHA, SECOND_SHA]
    );
}

#[test]
fn a_script_with_a_src_is_not_an_inline_script() {
    // The `.js` of the wasm bindings is loaded by name, is fetched under
    // `script-src 'self'`, and has no inline text to hash.
    let html = page(r#"<script type="module" src="/web-e3602ed23570787a.js"></script>"#);
    assert!(server::csp::script_hashes(&html).is_empty());
}

#[test]
fn the_hashed_text_is_the_raw_bytes_of_the_script() {
    // `<`, `&` and a literal `&amp;` all survive: a `<script>` element is raw text
    // to an HTML parser, so the browser hashes exactly these bytes.
    let html = page(&format!(r#"<script>{RAW}</script>"#));
    assert_eq!(server::csp::script_hashes(&html), vec![RAW_SHA]);
}

#[test]
fn an_attribute_spelling_or_a_capital_tag_changes_nothing() {
    // trunk writes `type="module"`; a hand-written page may not quote it, and HTML
    // tag names are case-insensitive. None of that moves the hash, which is of the
    // text alone.
    let html = page(&format!(
        "<script type=module>{BOOTSTRAP}</script><SCRIPT>{BOOTSTRAP}</SCRIPT>"
    ));
    assert_eq!(
        server::csp::script_hashes(&html),
        vec![BOOTSTRAP_SHA, BOOTSTRAP_SHA]
    );
}

#[test]
fn a_page_with_no_inline_script_hashes_to_nothing() {
    assert!(server::csp::script_hashes(&page("<p>no scripts</p>")).is_empty());
    // The placeholder page of a binary built without the frontend is such a page.
    assert!(
        server::csp::script_hashes(
            &server::assets::PlaceholderAssets
                .get("index.html")
                .expect("the placeholder page")
                .bytes
        )
        .is_empty()
    );
}

#[test]
fn the_policy_names_wasm_unsafe_eval_and_never_unsafe_inline() {
    let hashes = server::csp::script_hashes(&page(&format!(
        r#"<script type="module">{BOOTSTRAP}</script>"#
    )));
    let policy = server::csp::policy(&hashes);
    let script_src = policy
        .split("; ")
        .find(|directive| directive.starts_with("script-src "))
        .unwrap_or_else(|| panic!("no script-src in {policy}"));
    assert_eq!(
        script_src,
        format!("script-src 'self' 'wasm-unsafe-eval' '{BOOTSTRAP_SHA}'")
    );
    // `wasm-unsafe-eval` stays: it is what lets the `.wasm` compile, and `#411`
    // closes the inline hole, not that. `crates/web/tests/browser_csp.rs` compiles
    // a module under exactly this policy to show both halves hold.
    assert!(
        !policy.contains("'unsafe-inline'") || policy.contains("style-src 'self' 'unsafe-inline'")
    );
}

#[test]
fn no_policy_asks_for_unsafe_inline_anymore() {
    // The whole point of the ticket: the blunt directive is gone from `script-src`
    // whatever the page holds, including a page with nothing to hash.
    for hashes in [vec![], vec![BOOTSTRAP_SHA.to_owned()]] {
        let policy = server::csp::policy(&hashes);
        let script_src = policy
            .split("; ")
            .find(|directive| directive.starts_with("script-src "))
            .expect("a script-src");
        assert!(
            !script_src.contains("'unsafe-inline'"),
            "{script_src} still allows any inline script"
        );
    }
}
