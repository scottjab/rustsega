//! The picture the VDP produces.

use crate::{LINES_V30, WIDTH_H40};

/// Widest picture the VDP can produce: 40 cells in H40 (R12, genvdp.txt
/// section 17 "$0C").
pub const MAX_WIDTH: usize = WIDTH_H40;
/// Tallest picture the VDP can produce: 240 lines drawn twice, interlace mode 2
/// (R12 LSM, genvdp.txt section 2 "Display Modes").
pub const MAX_HEIGHT: usize = 2 * LINES_V30;

/// The frame the VDP renders into, as RGBA8888 packed into `u32` (`0xAABBGGRR`)
/// so the web frontend can hand it straight to a canvas.
///
/// `pixels` is allocated once, at [`MAX_WIDTH`] x [`MAX_HEIGHT`], and never
/// resized: `width` and `height` say how much of it belongs to the current
/// frame, which is how a game changing mode mid-frame costs no allocation. The
/// row stride is `width`.
///
/// Filling it in is the rendering tickets (M4-10, #88 onwards); nothing before
/// that draws anything, so a fresh VDP hands out a black frame.
#[derive(Debug)]
pub struct FrameBuffer {
    /// Active width in pixels: 256 (H32) or 320 (H40).
    pub width: usize,
    /// Active height in lines: 224, 240, or 480 in interlace mode 2.
    pub height: usize,
    /// The pixels, row-major, `MAX_WIDTH * MAX_HEIGHT` of them.
    pub pixels: Vec<u32>,
}

impl FrameBuffer {
    /// A black frame sized for the largest picture the VDP can produce.
    ///
    /// Starts at the widest mode and the commonest height because `width` and
    /// `height` only describe what the renderer has drawn so far, and the
    /// renderer that sets them is a later ticket (M4-19, #97).
    pub fn new() -> Self {
        Self {
            width: MAX_WIDTH,
            height: crate::LINES_V28,
            pixels: vec![0; MAX_WIDTH * MAX_HEIGHT],
        }
    }

    /// Index of the pixel at `(x, y)` in [`FrameBuffer::pixels`].
    ///
    /// `x` runs over `width`, not `MAX_WIDTH`: the stride of a frame is its own
    /// width.
    #[inline]
    pub fn pixel_index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_allocates_the_whole_picture_once() {
        let fb = FrameBuffer::new();
        assert_eq!(fb.width, 320);
        assert_eq!(fb.height, 224);
        assert_eq!(fb.pixels.len(), 320 * 480);
        assert!(fb.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn pixel_index_walks_rows_at_the_current_width() {
        let mut fb = FrameBuffer::new();
        fb.width = 256;
        assert_eq!(fb.pixel_index(0, 0), 0);
        assert_eq!(fb.pixel_index(255, 0), 255);
        assert_eq!(fb.pixel_index(0, 1), 256);
        assert_eq!(fb.pixel_index(100, 223), 223 * 256 + 100);
        assert!(fb.pixel_index(255, 223) < fb.pixels.len());
    }
}
