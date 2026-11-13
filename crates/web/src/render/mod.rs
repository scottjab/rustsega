//! The video output of a running console (M7-13, #146).
//!
//! Two drawing paths, one trait: [`canvas2d::Canvas2dRenderer`] is the
//! [`put_image_data`] path the shell has used since M7-04 (#137), and
//! [`webgl::WebGlRenderer`] uploads the frame as a texture and draws a
//! fullscreen quad, which is what makes an optional CRT shader possible.
//! [`choose_renderer`] picks between them from what the browser offers and what
//! the player asked for; [`RendererSetting`] and [`FilterSetting`] are the two
//! settings this module adds to [`crate::settings::Settings`].
//!
//! The pure half of the module - the choice, and [`uv_rect`], and the shader
//! sources of [`shaders`] - is arithmetic and text, so all of it is tested
//! natively. The browser half is only the code that cannot be anywhere else.
//!
//! ## Two sizes of one canvas
//!
//! A canvas has two sizes, and the two paths split them differently:
//!
//! * the **drawing buffer** (`canvas.width`/`height`), which is the bitmap that
//!   is actually written and is what a `put_image_data`/texture draw lands in;
//! * the **CSS box** (`style.width`/`height`), which is how big the picture is
//!   on the page, worked out by [`crate::scale`] and written by
//!   [`crate::screen`] (M7-12, #145).
//!
//! The 2D path keeps them the same - the buffer is the VDP's own 256/320 x
//! 224/240/448/480 pixels and the browser scales that, which is what #137 and
//! #145 shipped. WebGL takes the other side of the trade: the buffer is the CSS
//! box times `devicePixelRatio`, so a shader draws one crisp output pixel per
//! device pixel and [`Renderer::resize`] is what follows a layout change. Neither
//! path may write a size per frame: writing `width` or `height` clears a canvas.
//!
//! The frame's own size stays a third quantity, reported by
//! [`crate::emulator::Controls::frame`] and unchanged by this module - the shell
//! scales from the *frame*, not from the drawing buffer.

pub mod canvas2d;
pub mod shaders;
pub mod webgl;

use serde::Deserialize;
use serde::Serialize;

/// Which drawing path a player wants.
///
/// The settings half of [`choose_renderer`]: what the player asked for, as
/// opposed to what this browser can do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererSetting {
    /// WebGL2 when the browser has it, the 2D canvas when it does not.
    #[default]
    Auto,
    /// WebGL2 whatever the browser says; the 2D canvas if it cannot.
    Webgl,
    /// The 2D canvas: no shaders, no texture, no second opinion.
    Canvas,
}

impl RendererSetting {
    /// Every setting, in the order the settings drawer lists them.
    pub const ALL: [Self; 3] = [Self::Auto, Self::Webgl, Self::Canvas];

    /// The label for this setting in the settings drawer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Webgl => "WebGL",
            Self::Canvas => "Canvas 2D",
        }
    }
}

/// How a frame is filtered on the way to the screen.
///
/// Only the WebGL path has a filter to choose: the 2D path draws the frame
/// unfiltered, which is [`FilterSetting::Sharp`] by another name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterSetting {
    /// The frame as the VDP made it, sampled point for point.
    #[default]
    Sharp,
    /// A gentle CRT: scanlines at the source line rate, a little bloom, a faint
    /// aperture mask, and no curvature.
    Crt,
}

impl FilterSetting {
    /// Every filter, in the order the settings drawer lists them.
    pub const ALL: [Self; 2] = [Self::Sharp, Self::Crt];

    /// The label for this filter in the settings drawer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sharp => "Sharp",
            Self::Crt => "CRT (scanlines)",
        }
    }
}

/// The drawing path a console is using: the answer to [`choose_renderer`], and
/// which of the shell's two canvas elements is the live one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RendererKind {
    /// [`webgl::WebGlRenderer`].
    Webgl,
    /// [`canvas2d::Canvas2dRenderer`] - the path every browser has.
    ///
    /// The default because it is what is on screen before a console starts, and
    /// what a console falls back to when WebGL is refused or lost.
    #[default]
    Canvas,
}

/// The drawing path to use, given what the browser offers and what the player
/// asked for.
///
/// WebGL only when it is there *and* it was not turned off: [`RendererSetting::Canvas`]
/// is an off switch rather than a suggestion, and [`RendererSetting::Webgl`]
/// without WebGL is the canvas path rather than a broken renderer. That is the
/// same rule a lost context follows at run time
/// ([`Renderer::context_lost`]), so "WebGL is not available" and "WebGL went
/// away" end up in the same place.
pub fn choose_renderer(webgl2_available: bool, setting: RendererSetting) -> RendererKind {
    match (setting, webgl2_available) {
        (RendererSetting::Auto | RendererSetting::Webgl, true) => RendererKind::Webgl,
        (RendererSetting::Auto | RendererSetting::Webgl, false) | (RendererSetting::Canvas, _) => {
            RendererKind::Canvas
        }
    }
}

/// [`choose_renderer`] with a context lost earlier in this page load taken into
/// account: `gl_lost` is the latch, set once by a `webglcontextlost` event and
/// never cleared by a setting change.
///
/// A lost context is not the player's doing, so this is not allowed to reach
/// [`RendererSetting`] - a driver hiccup that rewrote the saved preference would
/// leave a player who could have had shaders running on without them forever
/// after. It is a statement about this page load only: WebGL is not going to come
/// back by being asked again, so asking - from the settings drawer or from a
/// second console - gets the 2D path (M7-13, #146).
///
/// Latching is therefore exactly "WebGL is no longer on offer", which is what
/// [`Renderer::context_lost`] says about one context and this says about the rest
/// of the page load.
pub fn choose_renderer_latched(
    webgl2_available: bool,
    gl_lost: bool,
    setting: RendererSetting,
) -> RendererKind {
    choose_renderer(webgl2_available && !gl_lost, setting)
}

/// The part of a `tex_w` x `tex_h` texture that a `w` x `h` frame fills, as
/// `[left, top, right, bottom]` in texture coordinates.
///
/// The WebGL renderer allocates one texture big enough for the largest frame the
/// VDP can make - 320x480, the ceiling of [`vdp::FrameBuffer::pixels`] - and
/// uploads each frame into the top of it, so the sampler needs to know where the
/// picture stops. Up and down the y axis the coordinates are the natural way
/// round for an uploaded image: `top` is `0.0`, the first row written, and
/// `bottom` is `h / tex_h`.
///
/// A frame or a texture of nothing is an empty rectangle rather than a divide by
/// zero, and a frame bigger than the texture - which the VDP cannot make, but a
/// caller should not have to know that - is clipped at `1.0` rather than
/// sampling past the end of the allocation.
pub fn uv_rect(w: u32, h: u32, tex_w: u32, tex_h: u32) -> [f32; 4] {
    if w == 0 || h == 0 || tex_w == 0 || tex_h == 0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let scale = |value: u32, tex: u32| (value as f32 / tex as f32).clamp(0.0, 1.0);
    [0.0, 0.0, scale(w, tex_w), scale(h, tex_h)]
}

/// One console's drawing path.
///
/// The loop hands it a frame and tells it when the picture's box moves; which of
/// `put_image_data` and a shader that becomes is the implementor's business. The
/// two implementations are [`canvas2d::Canvas2dRenderer`] and
/// [`webgl::WebGlRenderer`], and the console holds whichever
/// [`choose_renderer`] settled on as a `Box<dyn Renderer>`.
///
/// Neither method may panic: the draw happens inside the animation-frame closure
/// the console runs in, and a panic there leaves the console borrowed for good
/// (#387). A path that cannot draw says so through [`Renderer::context_lost`]
/// instead.
pub trait Renderer {
    /// Draws one frame: `w` x `h` of `rgba`, RGBA8, top row first.
    ///
    /// `rgba` is the whole frame and nothing else, which for WebGL is the part of
    /// its texture that [`uv_rect`] says is valid.
    fn draw(&mut self, rgba: &[u8], w: u32, h: u32);

    /// The picture's box is now `css_w` x `css_h` CSS pixels at
    /// `dpr` device pixels per CSS pixel - [`crate::screen`]'s answer, from the
    /// same signal that sizes the canvas's CSS box (M7-12, #145).
    ///
    /// The WebGL path sizes its drawing buffer to the answer so a shader writes
    /// one crisp pixel per device pixel. The 2D path ignores it: its buffer *is*
    /// the frame, and the browser does the scaling.
    fn resize(&mut self, css_w: f64, css_h: f64, dpr: f64);

    /// Which shader the WebGL path should draw with (M7-13, #146).
    ///
    /// In the trait rather than in the WebGL renderer's own constructor because
    /// the console keeps one renderer while a player tries the two filters, and
    /// the 2D path has no filter to change - the frame it draws is sharp by
    /// definition.
    fn set_filter(&mut self, filter: FilterSetting) {
        let _ = filter;
    }

    /// Whether the picture is smoothed rather than blocky, which is the
    /// [`crate::settings::Settings::smoothing`] switch of #145 reaching the
    /// sampler. The 2D path needs it not at all: the shell already puts
    /// `image-rendering` on the element for the browser to scale with.
    fn set_smoothing(&mut self, smoothing: bool) {
        let _ = smoothing;
    }

    /// Whether this path has lost the context it draws with, and so cannot draw
    /// again until it is replaced.
    ///
    /// `false` for the 2D path, which has nothing to lose; the WebGL path raises
    /// it from its `webglcontextlost` listener rather than acting there itself,
    /// because switching renderers means dropping the renderer whose event this
    /// is. The loop checks it before each [`Renderer::draw`].
    fn context_lost(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selection table the ticket specifies, every row of it: WebGL is
    /// available or it is not, and the player asked for one of three things.
    #[test]
    fn auto_takes_webgl_only_when_the_browser_has_it() {
        assert_eq!(
            choose_renderer(true, RendererSetting::Auto),
            RendererKind::Webgl
        );
        assert_eq!(
            choose_renderer(false, RendererSetting::Auto),
            RendererKind::Canvas
        );
    }

    /// Asking for WebGL without WebGL is the canvas path with a fallback - never
    /// a renderer that does not exist.
    #[test]
    fn asking_for_webgl_without_it_falls_back_to_the_canvas() {
        assert_eq!(
            choose_renderer(false, RendererSetting::Webgl),
            RendererKind::Canvas
        );
        assert_eq!(
            choose_renderer(true, RendererSetting::Webgl),
            RendererKind::Webgl
        );
    }

    /// The latch table: once a context has been lost, no setting and no available
    /// browser puts the console back on WebGL for the rest of the page load - and
    /// `Canvas` is still `Canvas`, because a latch is not a preference.
    #[test]
    fn a_lost_context_stays_lost_whatever_the_setting_asks_for() {
        for setting in RendererSetting::ALL {
            assert_eq!(
                choose_renderer_latched(true, true, setting),
                RendererKind::Canvas,
                "{setting:?} after a lost context"
            );
        }
        // And with nothing latched the latch changes nothing, which is what lets
        // the console call it unconditionally.
        for setting in RendererSetting::ALL {
            assert_eq!(
                choose_renderer_latched(true, false, setting),
                choose_renderer(true, setting),
                "{setting:?} with WebGL and no latch"
            );
            assert_eq!(
                choose_renderer_latched(false, false, setting),
                choose_renderer(false, setting),
                "{setting:?} with no WebGL and no latch"
            );
        }
    }

    /// Asking for the canvas is the canvas whatever the browser can do, so the
    /// switch is an off switch and not a suggestion.
    #[test]
    fn asking_for_the_canvas_is_the_canvas_even_with_webgl() {
        assert_eq!(
            choose_renderer(true, RendererSetting::Canvas),
            RendererKind::Canvas
        );
        assert_eq!(
            choose_renderer(false, RendererSetting::Canvas),
            RendererKind::Canvas
        );
    }

    /// The sub-rect of a 320x224 frame inside the 320x480 texture the WebGL
    /// renderer allocates once: the ticket's numbers, to 1e-4.
    #[test]
    fn a_320x224_frame_is_the_top_46_percent_of_the_texture() {
        let rect = uv_rect(320, 224, 320, 480);
        for (got, want) in rect.iter().zip([0.0_f32, 0.0, 1.0, 0.4667]) {
            assert!((got - want).abs() < 1e-4, "{rect:?} vs {want}, got {got}");
        }
    }

    /// H32 is 256 wide, so only four fifths of the texture's width is the
    /// picture; 448 of 480 lines is the interlace case.
    #[test]
    fn the_sub_rect_follows_the_frame_and_not_the_texture() {
        let h32 = uv_rect(256, 224, 320, 480);
        assert!((h32[2] - 0.8).abs() < 1e-4, "{h32:?}");
        assert!((h32[3] - 0.4667).abs() < 1e-4, "{h32:?}");

        let interlaced = uv_rect(320, 448, 320, 480);
        assert!((interlaced[2] - 1.0).abs() < 1e-4, "{interlaced:?}");
        assert!((interlaced[3] - 0.9333).abs() < 1e-4, "{interlaced:?}");

        let full = uv_rect(320, 480, 320, 480);
        assert_eq!(full, [0.0, 0.0, 1.0, 1.0]);
    }

    /// A frame of nothing, and a texture of nothing: the answer must be finite
    /// and must not be a divide by zero.
    #[test]
    fn an_empty_frame_or_texture_has_an_empty_rect() {
        assert_eq!(uv_rect(0, 0, 320, 480), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(uv_rect(320, 224, 0, 0), [0.0, 0.0, 0.0, 0.0]);
    }

    /// A frame wider than the allocation cannot be asked for by the VDP, but the
    /// rect is a sampler's instruction and a `1.0`-clipped one is all a caller
    /// can sensibly read.
    #[test]
    fn a_frame_never_reads_past_the_end_of_the_texture() {
        let rect = uv_rect(640, 960, 320, 480);
        assert_eq!(rect, [0.0, 0.0, 1.0, 1.0]);
    }

    /// The trait shape, checked on both implementations without a browser: this
    /// is what makes `Box<dyn Renderer>` a `Canvas2dRenderer` or a
    /// `WebGlRenderer` and nothing else.
    #[test]
    fn both_paths_are_renderers() {
        fn assert_renderer<T: Renderer + ?Sized>() {}
        assert_renderer::<canvas2d::Canvas2dRenderer>();
        assert_renderer::<webgl::WebGlRenderer>();
    }

    /// The labels of the two settings pages, pinned because the selects list the
    /// enums in `ALL` order and a mislabelled row is a setting nobody can find.
    #[test]
    fn the_settings_enums_list_every_choice_in_order() {
        assert_eq!(
            RendererSetting::ALL,
            [
                RendererSetting::Auto,
                RendererSetting::Webgl,
                RendererSetting::Canvas,
            ]
        );
        assert_eq!(
            FilterSetting::ALL,
            [FilterSetting::Sharp, FilterSetting::Crt]
        );
        assert_eq!(RendererSetting::Auto.label(), "Automatic");
        assert_eq!(RendererSetting::Webgl.label(), "WebGL");
        assert_eq!(RendererSetting::Canvas.label(), "Canvas 2D");
        assert_eq!(FilterSetting::Sharp.label(), "Sharp");
        assert!(
            FilterSetting::Crt
                .label()
                .to_ascii_lowercase()
                .contains("crt")
        );
    }
}
