//! How many times one page load will ask a broken driver for its context back
//! (#397).
//!
//! Its own test binary on purpose: the lost-context latch is one flag per page, and
//! `browser_webgl_restore.rs` spends a loss to prove the restore path works. In a
//! binary of its own this test starts from a page that has never lost anything, so
//! each loss can be asserted by number rather than bounded - "the second comes back,
//! the third does not" - which is the promise the budget actually makes.
//!
//! Needs a browser, which is `just test-web`, and is not in CI for the reason
//! #498 is about.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web::render::FilterSetting;
use web::render::Renderer;
use web::render::webgl::WebGlRenderer;
use web::render::webgl::gl_lost;
use web::render::webgl::take_context_restore;

wasm_bindgen_test_configure!(run_in_browser);

/// `webgl::MAX_RESTORES` is private, so the number is restated here: a budget test
/// that reads its own bound from the code under test would not be testing the bound.
const RESTORATIONS_ALLOWED: u32 = 2;

/// Lets the browser run its own frames: the context events are dispatched
/// asynchronously, and this is the clock they were measured against.
async fn frames(count: u32) {
    for _ in 0..count {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let resolve = resolve.clone();
            let callback = Closure::wrap(Box::new(move |_now_ms: f64| {
                let _ = resolve.call0(&JsValue::UNDEFINED);
            }) as Box<dyn FnMut(f64)>);
            let window = web_sys::window().expect("a browser window");
            window
                .request_animation_frame(callback.as_ref().unchecked_ref())
                .expect("this browser gives animation frames");
            callback.forget();
        });
        JsFuture::from(promise).await.expect("a frame arrives");
    }
}

/// Calls `method` on `target`, failing rather than swallowing a throw - a silently
/// un-called `loseContext` would look exactly like a budget that was never spent.
fn call(target: &JsValue, method: &str) {
    let function = js_sys::Reflect::get(target, &JsValue::from_str(method))
        .unwrap_or_else(|error| panic!("`{method}` is a member of WEBGL_lose_context: {error:?}"))
        .dyn_into::<js_sys::Function>()
        .unwrap_or_else(|_| panic!("`{method}` is a function"));
    let _ = function
        .call0(target)
        .unwrap_or_else(|error| panic!("`{method}` threw: {error:?}"));
}

/// One canvas, with a WebGL2 context and the extension that can take it away.
fn page_with_a_renderer() -> Option<(
    web_sys::HtmlCanvasElement,
    web_sys::WebGl2RenderingContext,
    JsValue,
    Box<dyn Renderer>,
)> {
    let document = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document");
    let canvas = document
        .create_element("canvas")
        .expect("create a canvas")
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .expect("a `canvas` element is an HtmlCanvasElement");
    canvas.set_width(64);
    canvas.set_height(64);
    document
        .body()
        .expect("a body")
        .append_child(&canvas)
        .expect("add the canvas to the page");
    // A WebGL2 context and `WEBGL_lose_context` are both things a CI runner may not
    // have - GitHub's headless Firefox answers `None` to the first, which is how
    // #397 found the difference out - so this answers `None` too and the caller
    // reports the skip rather than the failure.
    let renderer: Box<dyn Renderer> = match WebGlRenderer::new(&canvas, FilterSetting::Sharp, false)
    {
        Some(renderer) => Box::new(renderer),
        None => return None,
    };
    let Some(value) = canvas.get_context("webgl2").ok().flatten() else {
        return None;
    };
    let Ok(gl) = value.dyn_into::<web_sys::WebGl2RenderingContext>() else {
        return None;
    };
    let Some(extension) = gl.get_extension("WEBGL_lose_context").ok().flatten() else {
        return None;
    };
    Some((canvas, gl, extension.into(), renderer))
}

/// The budget: two comebacks, then the loss is left permanent (#397).
#[wasm_bindgen_test]
async fn a_driver_that_keeps_losing_its_context_is_left_alone() {
    let Some((_canvas, gl, extension, renderer)) = page_with_a_renderer() else {
        web_sys::console::log_1(&JsValue::from_str(
            "SKIPPED browser_webgl_budget: this browser cannot lose a context, so its \
             budget cannot be driven",
        ));
        return;
    };

    let mut given_back = 0;
    for loss in 1..=RESTORATIONS_ALLOWED + 1 {
        call(&extension, "loseContext");
        frames(10).await;
        assert!(
            gl_lost(),
            "loss {loss} latches the page away from WebGL, however many came before it"
        );
        call(&extension, "restoreContext");
        frames(10).await;

        let came_back = !gl.is_context_lost();
        assert_eq!(
            came_back,
            loss <= RESTORATIONS_ALLOWED,
            "loss {loss} should {} have been offered a context back, and {} one: the \
             page cancels `webglcontextlost` while it has budget and stops after",
            if loss <= RESTORATIONS_ALLOWED {
                ""
            } else {
                "not"
            },
            if came_back { "got" } else { "did not get" },
        );
        if !came_back {
            break;
        }
        assert!(
            !gl_lost(),
            "a restored context on loss {loss} releases the latch, or the shell stays \
             on the 2D path with a working context in hand"
        );
        assert!(
            take_context_restore(),
            "and the draw loop is offered the rebuild on loss {loss}"
        );
        given_back += 1;
    }

    assert_eq!(
        given_back, RESTORATIONS_ALLOWED,
        "one page load should hand back exactly {RESTORATIONS_ALLOWED} contexts"
    );
    assert!(
        gl_lost() && gl.is_context_lost(),
        "the loss after the budget is permanent: the latch is up and the browser has \
         not offered the context back"
    );
    assert!(
        renderer.context_lost(),
        "and the renderer that was drawing when the last loss came still says it lost"
    );
}
