//! CRAM colour conversion: a 9-bit colour RAM word to an RGBA8888 pixel, at
//! normal, shadow or highlight intensity (M4-09, #87).

/// Colours in the LUT: the 9 bits a CRAM word actually carries, `bbb0 ggg0
/// rrr0` packed to `bbbgggrrr` (genvdp.txt section 9: "This allows for a total
/// of 512 possible colors").
const COLORS: usize = 512;

/// Intensities: the rows of the colour LUT.
const INTENSITIES: usize = 3;

/// How bright the VDP shows a CRAM colour: normal, the shadow half of the
/// range, or the highlight half.
///
/// genvdp.txt section 16 "Shadow / Hilight mode": background and sprite pixels
/// "are shown at half intensity ... if their priority bit" is clear, sprite
/// colour `3Eh` shows the pixel under it "at half intensity" and `3Fh` "at
/// double intensity", and "the backdrop is shown at half intensity" unless a
/// high-priority tile covers its 8x8 block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intensity {
    /// The CRAM colour as written.
    Normal,
    /// Half intensity, the "shadow" of a low-priority pixel.
    Shadow,
    /// The brighter "hilight" of a sprite `3Fh` pixel.
    Highlight,
}

impl Intensity {
    /// Every intensity, in LUT row order.
    pub const ALL: [Self; INTENSITIES] = [Self::Normal, Self::Shadow, Self::Highlight];

    /// Row of [`LEVELS`], and of [`ColorLut`], this intensity is.
    #[inline]
    pub const fn index(self) -> usize {
        match self {
            Self::Normal => 0,
            Self::Shadow => 1,
            Self::Highlight => 2,
        }
    }
}

/// The DAC levels, 8 bits a channel, one table per [`Intensity`]:
/// `LEVELS[Intensity::index()][component]`.
///
/// INFERRED, not documented. genvdp.txt cannot settle any of the three
/// tables: section 9 gives only the CRAM word format (`----bbb-ggg-rrr-`) and
/// never mentions the output DAC at all, and section 16 says of the shadow and
/// highlight colours "It isn't known exactly how the colors are generated in
/// shadow / hilight mode ... Any information on the colors used in shadow /
/// hilight mode would be appreciated". These are the measured values the
/// ticket carries, and the tests below lock them in; only their shape can be
/// checked against genvdp, and it agrees with its one hard observation that
/// "the double intensity palette is actually not much brighter than the normal
/// palette": highlight starts at 130 rather than at double normal.
pub const LEVELS: [[u8; 8]; INTENSITIES] = [
    /* Normal    */ [0, 52, 87, 116, 144, 172, 206, 255],
    /* Shadow    */ [0, 29, 52, 70, 87, 101, 116, 130],
    /* Highlight */ [130, 144, 158, 172, 187, 206, 228, 255],
];

/// The LUT column a CRAM word sits in: its nine colour bits packed to
/// `bbb_ggg_rrr` as `b << 6 | g << 3 | r`.
///
/// Bits outside `0x0EEE` are dropped, as the hardware drops them: CRAM is 9
/// bits wide even though the data port hands over 16 (genvdp.txt section 9).
#[inline]
pub const fn color_index(cram: u16) -> usize {
    let r = (cram >> 1) & 0x07;
    let g = (cram >> 5) & 0x07;
    let b = (cram >> 9) & 0x07;
    (r | (g << 3) | (b << 6)) as usize
}

/// The CRAM word whose [`color_index`] is `index`: the inverse, for walking
/// all 512 colours.
#[inline]
pub const fn color_word(index: usize) -> u16 {
    let index = index as u16;
    let r = (index & 0x07) << 1;
    let g = ((index >> 3) & 0x07) << 5;
    let b = ((index >> 6) & 0x07) << 9;
    r | g | b
}

/// A CRAM word as a frame-buffer pixel: `0xFF000000 | b << 16 | g << 8 | r`,
/// the RGBA8888 order the web frontend hands to a canvas.
///
/// [`ColorLut`] is the same function read out of a table once, and is what
/// rendering uses; this is the definition it is built from.
pub fn color(cram: u16, intensity: Intensity) -> u32 {
    let levels = LEVELS[intensity.index()];
    let red = u32::from(levels[usize::from((cram >> 1) & 0x07)]);
    let green = u32::from(levels[usize::from((cram >> 5) & 0x07)]);
    let blue = u32::from(levels[usize::from((cram >> 9) & 0x07)]);
    0xFF00_0000 | (blue << 16) | (green << 8) | red
}

/// Every CRAM colour at every intensity, computed once.
///
/// Rows are [`Intensity::index`], columns [`color_index`]. `Vdp::new` builds one
/// and nothing rebuilds it, so a pixel costs a lookup and no allocation
/// (docs/ARCHITECTURE.md: "Core crates must not allocate per frame in hot
/// paths. Allocate buffers once.").
#[derive(Debug)]
pub struct ColorLut {
    table: [[u32; COLORS]; INTENSITIES],
}

impl ColorLut {
    /// The table, from [`color`]: 3 x 512 entries, 6 KiB, no allocation after
    /// this returns.
    pub fn new() -> Self {
        let mut table = [[0u32; COLORS]; INTENSITIES];
        for intensity in Intensity::ALL {
            let row = &mut table[intensity.index()];
            for (index, slot) in row.iter_mut().enumerate() {
                *slot = color(color_word(index), intensity);
            }
        }
        Self { table }
    }

    /// The pixel a CRAM word produces at `intensity`.
    #[inline]
    pub fn pixel(&self, cram: u16, intensity: Intensity) -> u32 {
        self.table[intensity.index()][color_index(cram)]
    }
}

impl Default for ColorLut {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x0000` is black and `0x0EEE` is white: the ends of the normal DAC
    /// table land exactly on 0 and 255 a channel.
    #[test]
    fn black_and_white_at_normal_intensity() {
        assert_eq!(color(0x0000, Intensity::Normal), 0xFF00_0000);
        assert_eq!(color(0x0EEE, Intensity::Normal), 0xFFFF_FFFF);
    }

    /// One channel at a time, in the byte the frame buffer wants:
    /// `0xFF000000 | b << 16 | g << 8 | r` (genvdp.txt section 9 gives the
    /// CRAM side, `----bbb-ggg-rrr-`).
    #[test]
    fn each_channel_lands_in_its_own_byte() {
        assert_eq!(color(0x000E, Intensity::Normal), 0xFF00_00FF);
        assert_eq!(color(0x00E0, Intensity::Normal), 0xFF00_FF00);
        assert_eq!(color(0x0E00, Intensity::Normal), 0xFFFF_0000);
    }

    /// The normal table: `0x0002` is red 52, and every step of one channel
    /// walks the table.
    #[test]
    fn normal_uses_the_measured_dac_steps() {
        assert_eq!(color(0x0002, Intensity::Normal), 0xFF00_0034, "red 52");
        for (value, want) in LEVELS[Intensity::Normal.index()].iter().enumerate() {
            let red_only = u16::try_from(value << 1).expect("3-bit red");
            assert_eq!(
                color(red_only, Intensity::Normal),
                0xFF00_0000 | u32::from(*want),
                "red level {value}"
            );
        }
    }

    /// `0x0EEE` at shadow intensity is 130 a channel: shadow tops out at 130,
    /// not at half of the normal table's 255.
    #[test]
    fn shadow_is_the_half_intensity_table() {
        assert_eq!(color(0x0EEE, Intensity::Shadow), 0xFF82_8282);
        for (value, want) in LEVELS[Intensity::Shadow.index()].iter().enumerate() {
            let green_only = u16::try_from(value << 5).expect("3-bit green");
            assert_eq!(
                color(green_only, Intensity::Shadow),
                0xFF00_0000 | (u32::from(*want) << 8),
                "green level {value}"
            );
        }
    }

    /// `0x0000` at highlight intensity is 130 a channel: highlight lifts the
    /// bottom of the range rather than doubling it (genvdp.txt section 16:
    /// "the double intensity palette is actually not much brighter than the
    /// normal palette").
    #[test]
    fn highlight_starts_where_the_range_bottom_is() {
        assert_eq!(color(0x0000, Intensity::Highlight), 0xFF82_8282);
        // Level 0 is 130 at highlight intensity, so a "blue only" word still
        // carries 130 of red and green.
        let base = u32::from(LEVELS[Intensity::Highlight.index()][0]);
        for (value, want) in LEVELS[Intensity::Highlight.index()].iter().enumerate() {
            let blue_only = u16::try_from(value << 9).expect("3-bit blue");
            let blue = u32::from(*want);
            assert_eq!(
                color(blue_only, Intensity::Highlight),
                0xFF00_0000 | (blue << 16) | (base << 8) | base,
                "blue level {value}"
            );
        }
        assert_eq!(color(0x0EEE, Intensity::Highlight), 0xFFFF_FFFF);
    }

    /// CRAM holds 9 bits in a 16-bit word, so everything outside `0x0EEE` is
    /// ignored by the VDP (genvdp.txt section 9).
    #[test]
    fn spare_cram_bits_are_ignored() {
        assert_eq!(
            color(0xF11E, Intensity::Normal),
            color(0x000E, Intensity::Normal)
        );
        assert_eq!(
            color(0xFFFF, Intensity::Highlight),
            color(0x0EEE, Intensity::Highlight)
        );
    }

    /// The LUT is the direct function, for every colour and every intensity.
    #[test]
    fn lut_matches_the_direct_function_for_all_512_colours() {
        let lut = ColorLut::new();
        for index in 0..512 {
            let cram = color_word(index);
            for intensity in Intensity::ALL {
                assert_eq!(
                    lut.pixel(cram, intensity),
                    color(cram, intensity),
                    "cram {cram:#06x} at {intensity:?}"
                );
            }
        }
    }

    /// `color_word` and `color_index` are inverses, which is what lets the test
    /// above cover the whole range through the LUT's own index.
    #[test]
    fn color_index_round_trips_over_the_whole_range() {
        let mut seen = [false; 512];
        for index in 0..512 {
            let word = color_word(index);
            assert_eq!(
                word & !0x0EEE,
                0,
                "color_word {index} only sets CRAM colour bits"
            );
            let seen_here = &mut seen[color_index(word)];
            assert!(!*seen_here, "index {index} is not reached twice");
            *seen_here = true;
        }
        assert!(seen.iter().all(|&hit| hit));
    }
}
