//! The 2D canvas path (M7-13, #146).
//!
//! This is the drawing code M7-04 (#137) did inline in [`crate::emulator`],
//! moved behind [`Renderer`] so a WebGL path can be swapped in beside it. Nothing
//! about the pixels changes: the frame goes to the canvas at 1:1 through
//! [`web_sys::ImageData`], and the canvas keeps the VDP's own resolution while
//! [`crate::screen`] scales it with CSS.
//!
//! It is also the path every WebGL failure ends up on, so it has to stay
//! available: the canvas it draws on must never have been asked for a WebGL
//! context, because a canvas belongs to one context type for the rest of its
//! life. That is why the shell hands the console two canvas elements rather than
//! one (see [`crate::render`]'s "two sizes of one canvas").

use wasm_bindgen::Clamped;
use wasm_bindgen::JsCast;
use web_sys::CanvasRenderingContext2d;
use web_sys::HtmlCanvasElement;
use web_sys::ImageData;

use super::Renderer;

/// The 2D canvas drawing path: `put_image_data` of the frame, at the frame's own
/// size, every frame.
pub struct Canvas2dRenderer {
    /// The canvas whose `width`/`height` are the drawing buffer. Held because
    /// writing them is the only way to resize that buffer, and because writing
    /// either one clears the canvas - so they are written only when the frame
    /// really changed size, which is what `size` is for.
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    /// The drawing buffer's current size, seeded from the canvas so the first
    /// frame at the size the markup asked for does not clear it.
    size: (u32, u32),
}

impl Canvas2dRenderer {
    /// The 2D path for `canvas`, or `None` if the element will not give up a 2d
    /// context: canvas switched off, or - the interesting case - a canvas that
    /// has already been handed a WebGL context.
    pub fn new(canvas: &HtmlCanvasElement) -> Option<Self> {
        let ctx = canvas
            .get_context("2d")
            .ok()
            .flatten()
            .and_then(|context| context.dyn_into::<CanvasRenderingContext2d>().ok())?;
        Some(Self {
            canvas: canvas.clone(),
            ctx,
            size: (canvas.width(), canvas.height()),
        })
    }
}

impl Renderer for Canvas2dRenderer {
    /// The frame, written straight through: this is the whole 2D path, and it
    /// allocates nothing per frame beyond the [`ImageData`] the browser will not
    /// let us avoid (see [`crate::emulator::frame_to_rgba_bytes`] for the buffer
    /// that is reused to feed it).
    fn draw(&mut self, rgba: &[u8], w: u32, h: u32) {
        if self.size != (w, h) {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
            self.size = (w, h);
        }
        // A frame of nothing at all is not an image, and `ImageData` says so with
        // an error rather than a panic; a canvas that was not drawn on is the
        // whole of the damage.
        let image = ImageData::new_with_u8_clamped_array_and_sh(Clamped(rgba), w, h);
        if let Ok(image) = image {
            let _ = self.ctx.put_image_data(&image, 0.0, 0.0);
        }
    }

    /// Nothing to do: the buffer is the frame, and [`crate::screen`]'s answer is
    /// the CSS box the browser scales that buffer into (#145).
    fn resize(&mut self, _css_w: f64, _css_h: f64, _dpr: f64) {}
}
