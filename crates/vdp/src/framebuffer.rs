//! The picture the VDP produces.

use crate::color::Intensity;
use crate::{LINES_V30, Vdp, WIDTH_H40};

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
/// row stride is `width`, so it belongs to the frame as a whole rather than to
/// the line that set it: [`Vdp::render_line`] takes it from the mode of line 0
/// and keeps it to the end of the frame, and a line of a narrower mode draws
/// what it has and takes the backdrop for the rest of the row (M4-19, #97).
///
/// [`Vdp::render_line`] fills it a line at a time, from the backdrop up through
/// the planes and the sprites (M4-17, #95); a VDP that has not drawn a frame
/// still hands out a black one.
#[derive(Debug)]
pub struct FrameBuffer {
    /// Active width in pixels: 256 (H32) or 320 (H40).
    pub width: usize,
    /// Active height in lines: 224, 240, or the 448 and 480 of interlace mode 2,
    /// where the two fields of a frame share these rows two lines at a time
    /// ([`Vdp::frame_height`], M4-20 #98).
    pub height: usize,
    /// The pixels, row-major, `MAX_WIDTH * MAX_HEIGHT` of them.
    pub pixels: Vec<u32>,
}

impl FrameBuffer {
    /// A black frame sized for the largest picture the VDP can produce.
    ///
    /// Starts at the widest mode and the commonest height because `width` and
    /// `height` only describe the frame that has been drawn so far, and the line
    /// that decides them is the first of a frame rather than this constructor
    /// ([`Vdp::render_line`], M4-19 #97).
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
    /// The backdrop colour as a frame-buffer pixel: R7's index into CRAM
    /// (genvdp.txt section 17 "$07": "Bits 5-0 of this register select a palette
    /// entry to be used as the backdrop color") through the colour LUT, so the
    /// 9-bit `0000 BBB0 GGG0 RRR0` of section 9 becomes a pixel at the DAC
    /// levels of [`crate::color::color`] (M4-09, #87).
    ///
    /// At [`Intensity::Normal`], which is what a display switched off by R1 bit 6
    /// paints with and what the overscan of a live one does too: genvdp.txt
    /// section 16 shows the backdrop at half intensity "while the overscan region
    /// outside of the display area is always shown at normal intensity", and the
    /// pick between the two per pixel needs the plane priority data, so it is
    /// [`Vdp::render_line`] and its compositor that make it (M4-18, #96).
    pub(crate) fn backdrop_pixel(&self) -> u32 {
        let colour = self.cram[usize::from(self.backdrop_index())];
        self.color_lut.pixel(colour, Intensity::Normal)
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

    /// The backdrop is converted by the DAC levels of `color`, not by the
    /// three-bit replication the M4-06 stub used (M4-09, #87): `0x0002` is red
    /// 52 (`0x34`), where the replication gave 36 (`0x24`).
    #[test]
    fn backdrop_pixel_uses_the_colour_conversion() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(7, 0x09);
        for (cram, want) in [
            (0x0002_u16, 0xFF00_0034_u32),
            (0x0000, 0xFF00_0000),
            (0x0EEE, 0xFFFF_FFFF),
            (0x0E00, 0xFFFF_0000),
        ] {
            vdp.cram[9] = cram;
            assert_eq!(vdp.backdrop_pixel(), want, "CRAM entry 9 = {cram:#06x}");
        }
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
