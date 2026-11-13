//! The WebGL2 path: one texture, one fullscreen quad, one shader of two
//! (M7-13, #146).
//!
//! The frame is uploaded into a texture allocated once at the size of the
//! largest frame the VDP can make, and the quad that covers the screen samples
//! the part of it the current frame fills - [`crate::render::uv_rect`], the one
//! number this path could not live without. Everything else in this file is
//! either setup that happens once or the few calls that happen per frame.
//!
//! ## Why nothing here may panic
//!
//! The draw happens inside the console's animation-frame closure, and a panic in
//! there leaves the console borrowed for good (#387). So every GL call that can
//! fail is a `Result` or an `Option` that is answered here, a shader that will
//! not compile is a `console.error` and a fallback rather than an exception, and
//! a context the driver takes away is a flag the loop reads
//! ([`Renderer::context_lost`]) rather than a recovery attempted from inside its
//! own event listener.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::HtmlCanvasElement;
use web_sys::WebGl2RenderingContext;
use web_sys::WebGlBuffer;
use web_sys::WebGlProgram;
use web_sys::WebGlShader;
use web_sys::WebGlTexture;
use web_sys::WebGlUniformLocation;
use web_sys::WebGlVertexArrayObject;

use super::FilterSetting;
use super::Renderer;
use super::shaders;
use super::uv_rect;

/// The texture every frame is uploaded into: the largest frame the VDP can make,
/// which is the capacity of [`vdp::FrameBuffer::pixels`] (docs/ARCHITECTURE.md,
/// "vdp"). Allocated once, in [`WebGlRenderer::new`], and never reallocated -
/// a frame is a `tex_sub_image_2d` into the top of it, sized by
/// [`crate::render::uv_rect`].
const TEXTURE_WIDTH: u32 = 320;
const TEXTURE_HEIGHT: u32 = 480;

/// The canvas event that says the driver has taken the context away.
const CONTEXT_LOST: &str = "webglcontextlost";

/// The longest edge a drawing buffer is allowed to be.
///
/// A canvas the browser refuses to size is a black screen with no error, so the
/// product of the CSS box and `devicePixelRatio` is capped rather than trusted -
/// and 8192 is past any window a real player has while staying inside the
/// texture limit every driver this was tested with reports.
const MAX_SURFACE: u32 = 8192;

thread_local! {
    /// [`webgl2_supported`]'s answer, once per page: see the reason there.
    static WEBGL2: Cell<Option<bool>> = const { Cell::new(None) };
    /// Whether a WebGL context has been lost during this page load: [`gl_lost`].
    static GL_LOST: Cell<bool> = const { Cell::new(false) };
}

/// Whether WebGL has gone away for good during this page load.
///
/// `true` from the first `webglcontextlost` onwards, and the shell reads it where
/// it would otherwise ask [`webgl2_supported`] - so a settings change, or a second
/// console, cannot put the player back on a driver that has already taken a context
/// away (#146).
///
/// Deliberately *not* cleared by `webglcontextrestored`. A restored context means a
/// renderer built against the old one is holding dead handles, and the shell would
/// have to rebuild one mid-frame to use it; the loss is rare, the cost of staying
/// on the 2D path until the page is reloaded is a session without shaders, and a
/// driver that loses a context once is not obviously the one to hand a second
/// attempt to. Recovery is tracked as its own issue rather than half-done here.
pub fn gl_lost() -> bool {
    GL_LOST.with(|latched| latched.get())
}

/// Latches a lost context, and says so once: the loss is one event, and the draw
/// path runs it sixty times a second.
fn latch_gl_lost() {
    GL_LOST.with(|latched| {
        if !latched.replace(true) {
            report("WebGL context lost: using the canvas path for the rest of this page");
        }
    });
}

/// Whether this browser will give up a WebGL2 context at all.
///
/// Asked of a canvas of its own, not of the shell's: a canvas belongs to one
/// context type for the rest of its life, so asking the shell's canvas would
/// decide the question the fallback still needs to ask.
///
/// The answer is cached for the page. A context costs a driver round trip, and
/// nothing that can happen inside one page session changes whether the browser
/// has WebGL2 - so this is asked once, and every later setting change reads the
/// same answer.
pub fn webgl2_supported() -> bool {
    WEBGL2.with(|cached| match cached.get() {
        Some(known) => known,
        None => {
            let available = probe_webgl2();
            cached.set(Some(available));
            available
        }
    })
}

/// The probe [`webgl2_supported`] caches.
fn probe_webgl2() -> bool {
    let canvas = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.create_element("canvas").ok())
        .and_then(|canvas| canvas.dyn_into::<HtmlCanvasElement>().ok());
    let Some(canvas) = canvas else {
        return false;
    };
    canvas.get_context("webgl2").ok().flatten().is_some()
}

/// One compiled and linked program: the shared vertex shader of
/// [`shaders::VERTEX`] and one of the two fragment shaders.
struct Program {
    program: WebGlProgram,
    /// The frame's sub-rect, as the vertex shader wants it. `None` is not a
    /// failure - a compiler that can prove a uniform unused is allowed to drop
    /// it - and every use of these is an `Option`, so a dropped uniform is
    /// simply a value the shader never needed.
    uv: Option<WebGlUniformLocation>,
    /// The frame's own size in pixels, which only [`shaders::CRT`] asks for.
    source: Option<WebGlUniformLocation>,
}

impl Program {
    /// Compiles `fragment` and links it to the shared vertex shader.
    ///
    /// `name` only ever appears in the log line, and it is the whole reason the
    /// failure is diagnosable: a shader that will not compile is otherwise a
    /// silent black rectangle.
    fn new(gl: &WebGl2RenderingContext, name: &str, fragment: &str) -> Option<Self> {
        let vertex = compile(
            gl,
            "vertex",
            shaders::VERTEX,
            WebGl2RenderingContext::VERTEX_SHADER,
        )?;
        let fragment = compile(gl, name, fragment, WebGl2RenderingContext::FRAGMENT_SHADER)?;
        let program = gl.create_program()?;
        gl.attach_shader(&program, &vertex);
        gl.attach_shader(&program, &fragment);
        gl.link_program(&program);
        if !gl
            .get_program_parameter(&program, WebGl2RenderingContext::LINK_STATUS)
            .as_bool()
            .unwrap_or(false)
        {
            report(&format!(
                "{name}: shader program would not link: {}",
                gl.get_program_info_log(&program).unwrap_or_default()
            ));
            gl.delete_program(Some(&program));
            return None;
        }
        // Both shaders are in the program now, which is all they were ever for.
        gl.detach_shader(&program, &vertex);
        gl.detach_shader(&program, &fragment);
        gl.delete_shader(Some(&vertex));
        gl.delete_shader(Some(&fragment));
        Some(Self {
            uv: gl.get_uniform_location(&program, "u_uv"),
            source: gl.get_uniform_location(&program, "u_source"),
            program,
        })
    }
}

/// Compiles one shader stage, logging and forgetting it if the driver refuses.
fn compile(
    gl: &WebGl2RenderingContext,
    stage: &str,
    source: &str,
    kind: u32,
) -> Option<WebGlShader> {
    let shader = gl.create_shader(kind)?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);
    if gl
        .get_shader_parameter(&shader, WebGl2RenderingContext::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        return Some(shader);
    }
    report(&format!(
        "{stage} shader would not compile: {}",
        gl.get_shader_info_log(&shader).unwrap_or_default()
    ));
    gl.delete_shader(Some(&shader));
    None
}

/// One line in the browser console, at the level that says "this build wanted to
/// do something else, and could not".
fn report(text: &str) {
    web_sys::console::error_1(&JsValue::from_str(text));
}

/// The four corners of a fullscreen triangle strip, as `(x, y)` clip
/// coordinates.
const CORNERS: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];

/// The corners as the buffer wants them: little endian, which is what both wasm
/// and every GPU this ships to use for `FLOAT` attribute data.
fn corner_bytes() -> [u8; CORNERS.len() * 4] {
    let mut bytes = [0u8; CORNERS.len() * 4];
    for (index, corner) in CORNERS.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&corner.to_le_bytes());
    }
    bytes
}

/// The WebGL2 drawing path.
///
/// Built by [`crate::emulator`] when [`super::choose_renderer`] says WebGL; the
/// constructor answers `None` - and the console falls back - when the browser
/// will not cooperate.
pub struct WebGlRenderer {
    canvas: HtmlCanvasElement,
    gl: WebGl2RenderingContext,
    /// The two filters, both already compiled. Compiling both at start costs one
    /// link each and buys a filter switch that does not happen on the draw path.
    sharp: Program,
    crt: Program,
    texture: WebGlTexture,
    /// The quad, in a VAO of its own so its attribute state is not the context's
    /// default one.
    quad: WebGlVertexArrayObject,
    /// The corners, uploaded once. Nothing else uses them, and the VAO names the
    /// buffer for as long as this renderer draws, so the handle is kept.
    _corners: WebGlBuffer,
    filter: FilterSetting,
    /// The drawing buffer's size in device pixels, which follows the CSS box
    /// times `devicePixelRatio` (see [`Renderer::resize`]).
    surface: (u32, u32),
    /// Raised by the [`CONTEXT_LOST`] listener and read by the loop. A `Cell` in
    /// an `Rc` because the listener outlives this struct's borrow, not the struct
    /// itself.
    lost: Rc<Cell<bool>>,
    /// The listener JS is holding. Taken off in [`Self::drop`], because the
    /// canvas outlives the renderer that attached it.
    on_lost: Closure<dyn FnMut(web_sys::Event)>,
    /// Whether a GL error has already been reported. Drivers are chatty after the
    /// first one, and this is inside the frame loop.
    reported: Cell<bool>,
}

impl WebGlRenderer {
    /// A WebGL2 renderer for `canvas`, or `None` - after saying why in the
    /// console - if this browser will not have one: no context, a shader that
    /// will not compile, a program that will not link.
    pub fn new(canvas: &HtmlCanvasElement, filter: FilterSetting, smoothing: bool) -> Option<Self> {
        let gl = canvas
            .get_context("webgl2")
            .ok()
            .flatten()
            .and_then(|context| context.dyn_into::<WebGl2RenderingContext>().ok())?;

        let sharp = Program::new(&gl, "sharp", shaders::SHARP)?;
        let crt = Program::new(&gl, "crt", shaders::CRT)?;

        let quad = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(&quad));
        let corners = gl.create_buffer()?;
        gl.bind_buffer(WebGl2RenderingContext::ARRAY_BUFFER, Some(&corners));
        gl.buffer_data_with_u8_array(
            WebGl2RenderingContext::ARRAY_BUFFER,
            &corner_bytes(),
            WebGl2RenderingContext::STATIC_DRAW,
        );
        // -1 is what WebGL answers an attribute that is not in the program with,
        // and a program without its one attribute is a quad that is not drawn.
        let position = gl.get_attrib_location(&sharp.program, "a_position");
        if position < 0 {
            report("the sharp shader has no a_position; WebGL is not being used");
            return None;
        }
        gl.enable_vertex_attrib_array(position as u32);
        gl.vertex_attrib_pointer_with_i32(
            position as u32,
            2,
            WebGl2RenderingContext::FLOAT,
            false,
            0,
            0,
        );
        gl.bind_vertex_array(None);

        let texture = gl.create_texture()?;
        gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&texture));
        // A 320-wide RGBA row is 1280 bytes, so the default 4 byte alignment
        // would do no harm today - and a 256-wide frame inside it would do
        // plenty. Say 1 and never think about it again.
        gl.pixel_storei(WebGl2RenderingContext::UNPACK_ALIGNMENT, 1);
        if gl
            .tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
                WebGl2RenderingContext::TEXTURE_2D,
                0,
                WebGl2RenderingContext::RGBA8 as i32,
                TEXTURE_WIDTH as i32,
                TEXTURE_HEIGHT as i32,
                0,
                WebGl2RenderingContext::RGBA,
                WebGl2RenderingContext::UNSIGNED_BYTE,
                None,
            )
            .is_err()
        {
            report("the frame texture could not be allocated; WebGL is not being used");
            return None;
        }
        gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_WRAP_S,
            WebGl2RenderingContext::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_WRAP_T,
            WebGl2RenderingContext::CLAMP_TO_EDGE as i32,
        );

        let lost = Rc::new(Cell::new(false));
        let listener = Rc::clone(&lost);
        // The listener only raises a flag. Switching renderers here would mean
        // dropping this renderer - and this closure - while JS is inside its own
        // callback, and the loop is about to read the flag anyway.
        let on_lost = Closure::wrap(Box::new(move |_: web_sys::Event| {
            listener.set(true);
            // And for every renderer this page builds from now on, so the loss
            // outlives this one: switching to the 2D path drops `self`, and with
            // it the flag a later setting change would have had to read.
            latch_gl_lost();
        }) as Box<dyn FnMut(web_sys::Event)>);
        if canvas
            .add_event_listener_with_callback(CONTEXT_LOST, on_lost.as_ref().unchecked_ref())
            .is_err()
        {
            // Not fatal: a listener the browser would not take only means a lost
            // context is found out by the draws failing rather than by the event.
            report("the WebGL context loss listener could not be attached");
        }

        let mut renderer = Self {
            canvas: canvas.clone(),
            gl,
            sharp,
            crt,
            texture,
            quad,
            _corners: corners,
            filter,
            surface: (canvas.width(), canvas.height()),
            lost,
            on_lost,
            reported: Cell::new(false),
        };
        renderer.set_smoothing(smoothing);
        Some(renderer)
    }

    /// One line, once, about a GL error: after the first one a driver will say
    /// something on every call for the rest of the session, and this is the frame
    /// loop.
    fn check_error(&self) {
        let error = self.gl.get_error();
        if error != WebGl2RenderingContext::NO_ERROR && !self.reported.replace(true) {
            report(&format!(
                "WebGL reported error {error} while drawing the screen"
            ));
        }
    }
}

impl Renderer for WebGlRenderer {
    /// Uploads the frame and draws the quad.
    ///
    /// Four GL calls against a texture that was allocated once: the upload, then
    /// the program the filter asks for, then the strip.
    fn draw(&mut self, rgba: &[u8], w: u32, h: u32) {
        if self.lost.get() || w == 0 || h == 0 {
            return;
        }
        let width = w as i32;
        let height = h as i32;
        if width > TEXTURE_WIDTH as i32 || height > TEXTURE_HEIGHT as i32 {
            // Not something the VDP can ask for (docs/ARCHITECTURE.md, "vdp"),
            // and an upload bigger than the texture is an error rather than a
            // picture.
            if !self.reported.replace(true) {
                report(&format!("a {w}x{h} frame does not fit the frame texture"));
            }
            return;
        }

        let gl = &self.gl;
        gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&self.texture));
        if gl
            .tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_opt_u8_array(
                WebGl2RenderingContext::TEXTURE_2D,
                0,
                0,
                0,
                width,
                height,
                WebGl2RenderingContext::RGBA,
                WebGl2RenderingContext::UNSIGNED_BYTE,
                Some(rgba),
            )
            .is_err()
        {
            self.check_error();
            return;
        }

        gl.viewport(0, 0, self.surface.0 as i32, self.surface.1 as i32);
        let program = if self.filter == FilterSetting::Crt {
            &self.crt
        } else {
            &self.sharp
        };
        gl.use_program(Some(&program.program));
        let rect = uv_rect(w, h, TEXTURE_WIDTH, TEXTURE_HEIGHT);
        gl.uniform4f(program.uv.as_ref(), rect[0], rect[1], rect[2], rect[3]);
        gl.uniform2f(program.source.as_ref(), w as f32, h as f32);
        gl.bind_vertex_array(Some(&self.quad));
        gl.draw_arrays(WebGl2RenderingContext::TRIANGLE_STRIP, 0, 4);
        self.check_error();
    }

    /// The drawing buffer follows the picture's CSS box times `devicePixelRatio`,
    /// which is what makes a scanline in [`shaders::CRT`] one output pixel thick
    /// rather than one CSS pixel thick (M7-13, #146).
    fn resize(&mut self, css_w: f64, css_h: f64, dpr: f64) {
        let surface = (surface_side(css_w, dpr), surface_side(css_h, dpr));
        if surface == self.surface || surface.0 == 0 || surface.1 == 0 {
            return;
        }
        // Writing either attribute clears the canvas, so it is written when the
        // picture really moved and not once per frame - and the console puts the
        // last frame back after a resize it asked for (`Emulator::resize_surface`).
        self.canvas.set_width(surface.0);
        self.canvas.set_height(surface.1);
        self.surface = surface;
    }

    fn set_filter(&mut self, filter: FilterSetting) {
        self.filter = filter;
    }

    /// `Smoothing` is the sampler's, not the shader's: point sampling is the
    /// crisp picture, linear is the soft one, and both fragment shaders sample
    /// through the one texture's parameters.
    fn set_smoothing(&mut self, smoothing: bool) {
        let filter = if smoothing {
            WebGl2RenderingContext::LINEAR
        } else {
            WebGl2RenderingContext::NEAREST
        };
        self.gl
            .bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&self.texture));
        self.gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_MIN_FILTER,
            filter as i32,
        );
        self.gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_MAG_FILTER,
            filter as i32,
        );
    }

    fn context_lost(&self) -> bool {
        self.lost.get()
    }
}

impl Drop for WebGlRenderer {
    fn drop(&mut self) {
        // The canvas outlives a renderer that was drawing to it, and it is
        // holding this closure: dropping the closure without taking the listener
        // off first would leave a call to nothing.
        let _ = self.canvas.remove_event_listener_with_callback(
            CONTEXT_LOST,
            self.on_lost.as_ref().unchecked_ref(),
        );
    }
}

/// One edge of the drawing buffer: `css` pixels at `dpr`, at least one and never
/// more than [`MAX_SURFACE`]. A size that is not a number - which is what a
/// browser with no layout to report can hand back - is `0`, and `0` means "do not
/// resize at all".
fn surface_side(css: f64, dpr: f64) -> u32 {
    let device = css * dpr.max(1.0);
    if !device.is_finite() || device <= 0.0 {
        return 0;
    }
    (device.round() as u32).clamp(1, MAX_SURFACE)
}
