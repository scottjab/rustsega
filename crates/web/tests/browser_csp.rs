//! The CSP hash is enforced by a real browser, not just by the server that writes
//! it (#411).
//!
//! The failure this replaces is the reason to write a test like this at all. M8-01
//! (#155) shipped `script-src 'self' 'wasm-unsafe-eval'` and Firefox refused
//! trunk's inline bootstrap; every subresource of the page answered 200, so nothing
//! that looked at responses noticed, while `window.wasmBindings` stayed `undefined`
//! and no canvas was ever mounted. A test that asserts `status == 200` and that the
//! header contains a `sha256-` token reproduces that mistake exactly. So this file
//! asks the only question that matters: **did the inline script run?**
//!
//! ## The three-sided pin
//!
//! One script text - `BOOTSTRAP` - is used by three checks, and no two of them can
//! agree by accident:
//!
//! - here: Firefox runs a script whose hash is in `script-src`, and refuses one
//!   that is not listed, and does not run it when the hash is *almost* right;
//! - `crates/server/tests/csp.rs`: the server's `sha2`/`base64` hasher produces
//!   those same literals, computed there against python;
//! - `crates/server/tests/static_files.rs`: the header the server emits for a page
//!   holding that script is exactly that policy.
//!
//! ## What this cannot see, which is a real gap and not a detail
//!
//! - **The policy arrives as a `<meta http-equiv>`, not a response header**, because
//!   the `wasm-bindgen-test` harness loads its own page and cannot put a header on
//!   it. `<script>` hash matching is the same enforcement path either way and
//!   Firefox honours meta policies - but this is a difference between what is tested
//!   and what ships, stated rather than glossed.
//! - It is a *stand-in* bootstrap, not the real one: `crates/web/dist` is a
//!   gitignored trunk artefact that CI never builds, so the page here compiles an
//!   eight-byte wasm header instead of loading `web-<hash>_bg.wasm`. The extractor
//!   was additionally run against a real `just build` output and produced the
//!   python-verified hash of its bootstrap; that run is described in the PR because
//!   it cannot be a CI test.
//! - `about:srcdoc` inherits this origin, so the frame is read back over the DOM. It
//!   is not a cross-origin probe and does not test that.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The script the policy is written about: mark that it ran, then compile a WebAssembly
/// module - which is the other half of the ticket, since `wasm-unsafe-eval` is kept and
/// is what makes the `.wasm` loadable at all.
const BOOTSTRAP: &str = "document.body.appendChild(document.createElement('i')).id='ran';\
try{new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0]));\
document.body.appendChild(document.createElement('i')).id='wasm'}\
catch(e){document.body.appendChild(document.createElement('i')).id='wasm-'+e.name}";

/// `base64(sha256(BOOTSTRAP))`, computed with python3 and pinned here so that the
/// browser is being tested against a value this crate did not calculate.
const BOOTSTRAP_SHA: &str = "sha256-WjYXdo+j/6Ky7KwdT89atX0ifQft8ykslCoAcFJkg+Q=";

/// A script that is on the page but not in the policy.
const INTRUDER: &str = "document.body.appendChild(document.createElement('i')).id='intruder'";

/// The animation-frame await, shared with `browser_drawer.rs`.
async fn wait_frames(frames: i32) {
    for _ in 0..frames {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let resolve = resolve.clone();
            let callback = Closure::wrap(Box::new(move |_now_ms: f64| {
                let _ = resolve.call0(&JsValue::UNDEFINED);
            }) as Box<dyn FnMut(f64)>);
            let window = web_sys::window().expect("a browser window");
            let _ = window
                .request_animation_frame(callback.as_ref().unchecked_ref())
                .expect("this browser gives animation frames");
            callback.forget();
        });
        JsFuture::from(promise).await.expect("a frame arrives");
    }
}

/// A frame whose document carries `policy` and the given inline scripts, and which
/// has had `frames` animation frames to run them.
async fn page_under(policy: &str, scripts: &[&str]) -> web_sys::Document {
    let document = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document");
    let frame = document
        .create_element("iframe")
        .expect("an iframe")
        .dyn_into::<web_sys::HtmlIFrameElement>()
        .expect("an iframe element");
    let body = scripts
        .iter()
        .map(|script| format!(r#"<script type="module">{script}</script>"#))
        .collect::<String>();
    frame.set_srcdoc(&format!(
        "<!doctype html><meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">\
         <body></body>{body}"
    ));
    document
        .body()
        .expect("a body")
        .append_child(&frame)
        .expect("a frame in the page");
    // Module scripts are deferred, and a srcdoc frame settles with them; twenty
    // frames is generous for three statements and is how #394 waits too.
    wait_frames(20).await;
    frame
        .content_document()
        .expect("the frame has a document to read back")
}

fn marker(inner: &web_sys::Document, id: &str) -> bool {
    inner
        .query_selector(&format!("#{id}"))
        .expect("a valid selector")
        .is_some()
}

/// The policy under test, naming `hash`.
fn policy(hash: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' '{hash}'; style-src 'self' \
         'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'"
    )
}

/// The app boots: an inline module named by hash runs, and compiles WebAssembly
/// while doing it (#411).
#[wasm_bindgen_test]
async fn a_bootstrap_named_by_hash_runs_in_the_browser() {
    let inner = page_under(&policy(BOOTSTRAP_SHA), &[BOOTSTRAP, INTRUDER]).await;

    assert!(
        marker(&inner, "ran"),
        "the hash-pinned bootstrap did not run: the policy is being enforced against \
         it, which is the #155 failure - every request 200 and no app"
    );
    assert!(
        marker(&inner, "wasm"),
        "WebAssembly did not compile under 'wasm-unsafe-eval': the bootstrap sets that          marker itself, so either it did not run or the browser refused to compile a module"
    );
    assert!(
        !marker(&inner, "intruder"),
        "an inline script that is not in the policy ran anyway: 'unsafe-inline' is \
         back, or the policy was not applied to this document at all"
    );
}

/// The negative control, without which the test above proves nothing: a hash that is
/// one character off must not run the script. If this test ever passes while the one
/// above also passes, the browser is not checking the hash.
#[wasm_bindgen_test]
async fn a_hash_that_is_almost_right_does_not_run_it() {
    let mut wrong = BOOTSTRAP_SHA.to_owned();
    let last = wrong.pop().expect("a hash is not empty");
    wrong.push(if last == 'A' { 'B' } else { 'A' });
    assert_ne!(wrong, BOOTSTRAP_SHA);

    let inner = page_under(&policy(&wrong), &[BOOTSTRAP]).await;
    assert!(
        !marker(&inner, "ran"),
        "Firefox ran a script whose hash was wrong ({wrong}) - so nothing here has \
         shown the CSP is being enforced, and the pass above means nothing either"
    );
}
