//! The picture the VDP produces.

use crate::{Vdp, LINES_V30, WIDTH_H40};

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

impl Vdp {
    /// Draw one line of the picture: the backdrop colour, right across it.
    ///
    /// Public and callable on its own, as `docs/ARCHITECTURE.md` puts it
    /// ("Render one line directly. Public for tests; the scheduler uses
    /// advance()"). `timing::advance` calls it once for each line of the active
    /// display as the beam finishes it (M4-06, #84).
    ///
    /// The body is the stub of the ticket: the planes and the sprites are the
    /// rendering tickets (M4-10 #88 onwards) and M4-17 (#95) replaces this
    /// whole function. What it does today is enough for the timing to be
    /// testable and for the frame to be the size the mode says: the row is
    /// written, and `width` and `height` describe it.
    pub fn render_line(&mut self, line: u16) {
        let height = self.active_height();
        let y = usize::from(line);
        if y >= height {
            return;
        }

        let width = self.active_width();
        let colour = self.backdrop_pixel();
        let frame = &mut self.frame_buffer;
        frame.width = width;
        frame.height = height;

        let start = y * width;
        frame.pixels[start..start + width].fill(colour);
    }

    /// The backdrop colour as a frame-buffer pixel: R7's index into CRAM
    /// (genvdp.txt section 17 "$07": "Bits 5-0 of this register select a palette
    /// entry to be used as the backdrop color"), expanded from the 9-bit
    /// `0000 BBB0 GGG0 RRR0` of section 9 to eight bits a channel.
    ///
    /// A plain bit replication, which is all the stub of [`Vdp::render_line`]
    /// needs; the DAC levels and the shadow/highlight halves of the range are
    /// M4-09 (#87).
    pub(crate) fn backdrop_pixel(&self) -> u32 {
        let colour = u32::from(self.cram[usize::from(self.backdrop_index())]);
        // Three bits to eight, by repeating the top bits of the value: 0b101
        // becomes 0b10110100, so black stays black and white stays white.
        let expand = |v: u32| (v << 5) | (v << 2) | (v >> 1);
        let r = expand((colour >> 1) & 0x07);
        let g = expand((colour >> 5) & 0x07);
        let b = expand((colour >> 9) & 0x07);
        0xFF00_0000 | (b << 16) | (g << 8) | r
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
