//! A WebGL context lost and given back, in a real browser (#397).
//!
//! Nothing in this file is faked: the renderer is [`WebGlRenderer`] as the shell
//! builds it, the loss is the browser's own `WEBGL_lose_context`, and the assertion
//! is a pixel read back out of the drawing buffer with `read_pixels`.
//! `browser_drawer.rs` (#394) is the same argument in CSS - inject the real thing,
//! read observable state back - and it is worth far more than a structural
//! assertion that `webglcontextrestored` is mentioned somewhere in the source.
//!
//! What it can prove: a real context loss really does latch `gl_lost` and stop the
//! renderer drawing; a real `webglcontextrestored` really clears the latch and
//! offers the draw loop a rebuild; and a renderer built after a restore draws the
//! frame it is given rather than the uninitialised texture it started with. What it
//! cannot: a driver that loses contexts on its own is simulated at the point of
//! failure, even though every event and every piece of state after it is genuinely
//! the browser's. And like every browser test in this crate it is not run by
//! `just check`, so none of this is in CI (#498).
//!
//! Needs a browser, which is `just test-web`.

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

/// The frame every draw here is made of, and the two colours it is filled with.
const FRAME_WIDTH: u32 = 4;
const FRAME_HEIGHT: u32 = 4;
const RED: [u8; 4] = [220, 20, 20, 255];
const GREEN: [u8; 4] = [20, 210, 20, 255];

/// `FRAME_WIDTH` x `FRAME_HEIGHT` of one colour, RGBA8.
fn frame(colour: [u8; 4]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((FRAME_WIDTH * FRAME_HEIGHT * 4) as usize);
    for _ in 0..FRAME_WIDTH * FRAME_HEIGHT {
        pixels.extend_from_slice(&colour);
    }
    pixels
}

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

/// Calls `method` on `target`, and fails rather than ignoring a throw. Worth the
/// ceremony: an `Illegal invocation` swallowed here is indistinguishable from a
/// browser that never lost anything, which is the exact failure a test of this has
/// to be able to tell apart.
fn call(target: &JsValue, method: &str) {
    let function = js_sys::Reflect::get(target, &JsValue::from_str(method))
        .unwrap_or_else(|error| panic!("`{method}` is a member of WEBGL_lose_context: {error:?}"))
        .dyn_into::<js_sys::Function>()
        .unwrap_or_else(|_| panic!("`{method}` is a function"));
    let _ = function
        .call0(target)
        .unwrap_or_else(|error| panic!("`{method}` threw: {error:?}"));
}

/// The canvas's own WebGL2 context: the one [`WebGlRenderer`] draws with, since a
/// canvas belongs to one context for its whole life.
fn context(canvas: &web_sys::HtmlCanvasElement) -> web_sys::WebGl2RenderingContext {
    canvas
        .get_context("webgl2")
        .expect("asking a canvas for its context does not throw")
        .expect("this canvas has a WebGL2 context")
        .dyn_into()
        .expect("a canvas asked for webgl2 gives back a WebGL2 context")
}

/// One pixel out of the middle of the drawing buffer.
fn centre_pixel(gl: &web_sys::WebGl2RenderingContext) -> [u8; 4] {
    let mut pixel = [0u8; 4];
    gl.read_pixels_with_opt_u8_array(
        gl.drawing_buffer_width() / 2,
        gl.drawing_buffer_height() / 2,
        1,
        1,
        web_sys::WebGl2RenderingContext::RGBA,
        web_sys::WebGl2RenderingContext::UNSIGNED_BYTE,
        Some(&mut pixel),
    );
    pixel
}

/// The middle of the picture is `expected`, within a tolerance for a shader and a
/// sampler that are allowed to round.
fn assert_picture(when: &str, gl: &web_sys::WebGl2RenderingContext, expected: &[u8; 4]) {
    let pixel = centre_pixel(gl);
    let tolerance = 8;
    let distance = pixel
        .iter()
        .zip(expected)
        .map(|(got, want)| (*got as i32 - *want as i32).abs())
        .max()
        .unwrap_or(i32::MAX);
    assert!(
        distance <= tolerance,
        "{when}: expected the frame to be {expected:?}, and the middle of the drawing \
         buffer was {pixel:?}"
    );
}

/// A lost context stops drawing and is latched; a restored one is handed back to
/// the draw loop, and a renderer built on it draws (#397).
#[wasm_bindgen_test]
async fn a_restored_context_draws_again() {
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

    let mut renderer = WebGlRenderer::new(&canvas, FilterSetting::Sharp, false)
        .expect("this browser gives a WebGL2 renderer");
    let gl = context(&canvas);
    let Some(extension) = gl.get_extension("WEBGL_lose_context").ok().flatten() else {
        panic!(
            "this browser offers no WEBGL_lose_context, so nothing below could lose a \
             context and the test would assert nothing"
        );
    };

    // The path works before anything is lost: every assertion after this one is
    // about the loss rather than about a renderer that never drew at all.
    renderer.draw(&frame(RED), FRAME_WIDTH, FRAME_HEIGHT);
    assert_picture("before any loss", &gl, &RED);
    assert!(
        !gl.is_context_lost(),
        "nothing has been lost, so the picture above was drawn by the path under test"
    );

    call(&extension, "loseContext");
    frames(10).await;
    assert!(
        gl_lost(),
        "a real loss latches the page away from WebGL, which is what #146 made sticky"
    );
    assert!(
        renderer.context_lost(),
        "and the renderer that lost it says so, rather than drawing nothing quietly"
    );
    assert!(
        gl.is_context_lost(),
        "the browser agrees the context is gone, so the two are not disagreeing"
    );

    // A draw on the dead renderer must not bring the picture back: it is the draw
    // the lost-context check in `Emulator::paint` exists to skip.
    renderer.draw(&frame(GREEN), FRAME_WIDTH, FRAME_HEIGHT);

    call(&extension, "restoreContext");
    frames(10).await;
    assert!(
        !gl.is_context_lost(),
        "the browser has given the context back"
    );
    assert!(
        !gl_lost(),
        "and a restored context is the one thing that clears the latch (#397)"
    );
    assert!(
        take_context_restore(),
        "the draw loop is offered the rebuild it watches for (#397)"
    );

    // What `Emulator::use_renderer` does with that offer, done here in the open:
    // build a whole renderer on the restored context, drop the one that died, and
    // check that the first frame it draws is the frame it was given - not the
    // uninitialised state of a texture that came back empty.
    let mut rebuilt = WebGlRenderer::new(&canvas, FilterSetting::Sharp, false)
        .expect("the restored context gives a renderer");
    drop(renderer);
    rebuilt.draw(&frame(GREEN), FRAME_WIDTH, FRAME_HEIGHT);
    assert_picture("the first frame after a restore", &gl, &GREEN);
}
