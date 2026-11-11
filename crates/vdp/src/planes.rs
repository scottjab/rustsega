//! Plane A and plane B line rendering (M4-11, #89).
//!
//! One line of one scroll plane as a layer buffer: a [`u8`] per pixel holding a
//! CRAM index rather than a colour, so the compositing of M4-17 (#95) can still
//! decide which plane a pixel comes from, whether it is in front of the sprites,
//! and whether it is a shadow. [`crate::pattern`] does the two fetches each
//! cell needs - the name table word and the pattern row - and nothing here
//! repeats either.

use crate::pattern::{NameEntry, read_word, tile_row};
use crate::{Vdp, WIDTH_H40};

/// Rows of a plane cell, and so of the pattern a plane cell names.
///
/// 8, always, until interlace mode 2 makes a plane pattern 8x16: genvdp.txt
/// section 12 ("A pattern is an 8x8 or 8x16 (interlace mode 2 only) pixel
/// block"). TODO(M4-20 #98): read [`Vdp::interlace_mode`] and pass 16 there,
/// which [`crate::pattern::tile_addr`] already handles.
const CELL_ROWS: usize = 8;

/// Name table entries of the largest valid plane: genvdp.txt section 13, "The
/// name table size cannot exceed 8192 bytes, so while a 64x64 or 128x32 name
/// table is allowed, a size of 128x128 or 64x128 is invalid" - 8192 bytes is
/// 4096 of the 16-bit words a table is a matrix of.
const MAX_ENTRIES: usize = 4096;

/// Bit 6 of a layer pixel: the priority of the cell it came from, as M4-17
/// (#95) wants it. genvdp.txt section 14 moves a plane's pixels from `B`/`C`
/// to `E'`/`F'` when the entry sets the flag.
const PRIORITY_SHIFT: u32 = 6;

/// Which of the two scroll planes to render (genvdp.txt section 13 "Background
/// Layers": "The VDP manages two background layers, called plane A and plane B").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// Plane A: the front plane of the two, and the one the window replaces
    /// (genvdp.txt section 13: "plane W ... can be thought of a 'replacement'
    /// for plane A"). Its name table base is R2.
    A,
    /// Plane B: the plane behind A (genvdp.txt section 14: "A > B > C" with `B`
    /// the plane B pixels). Its name table base is R4.
    B,
}

impl Plane {
    /// The VRAM address of this plane's name table: R2 for A, R4 for B
    /// (genvdp.txt section 17 "$02" and "$04").
    #[inline]
    fn name_table(self, vdp: &Vdp) -> usize {
        let base = match self {
            Plane::A => vdp.plane_a_base(),
            Plane::B => vdp.plane_b_base(),
        };
        usize::from(base)
    }
}

impl Vdp {
    /// Fill `out` with one line of `plane`: one [`u8`] per pixel, bits 5-0 the
    /// CRAM index (`palette * 16 + colour`, the 6-bit index of CRAM's 64 words,
    /// genvdp.txt section 9) and bit 6 the cell's priority flag.
    ///
    /// Colour 0 of any palette draws nothing (genvdp.txt section 12: "A pixel
    /// within a pattern that uses value zero isn't shown"), and a pixel that
    /// draws nothing is written as a whole zero byte: the transparent test the
    /// compositor of M4-17 (#95) applies, `index & 0x0F == 0`, is then true of
    /// it and of nothing else, and the stale priority bit of a transparent cell
    /// cannot leak into the layer above.
    ///
    /// There is no scrolling yet, so a pixel maps to the plane unchanged: plane x
    /// is `x` and plane y is `line`, a cell is 8 pixels by 8 pixels, and both
    /// axes wrap at the plane size of R16. The fetch runs a cell at a time - eight
    /// pixels, one row of one pattern - and allocates nothing.
    ///
    /// Invalid plane sizes, which genvdp.txt section 13 calls the combinations
    /// over its 8192-byte table (128x128 and 64x128), have no documented
    /// behaviour: the doc only says what they are not, and for the width field
    /// alone adds "If the HSZ bits are set to 10b (invalid), then the first row
    /// of the name table is shown for every line of the display" (section 17
    /// "$10", and `regs::plane_extent` decodes that value as 32 cells). So the
    /// entry index is masked to `MAX_ENTRIES`: every fetch stays inside one
    /// 8192-byte name table, which is all the doc does settle. For every valid
    /// size the mask costs nothing, as the largest index is then 4095.
    ///
    /// The pixel nibble order comes from [`tile_row`] and is INFERRED there,
    /// not documented in genvdp.txt; the plane inherits it.
    ///
    /// What this does not know: the display-enable and left-column-blank bits of
    /// R0/R1, [`Vdp::active_width`] - `out` is all [`WIDTH_H40`] pixels wide in
    /// H32 too, and the columns past the active display are the overscan the
    /// compositor drops - shadow and highlight, the window, the sprites, and any
    /// RGBA. All of that is M4-17 (#95).
    pub fn render_plane_line(&self, plane: Plane, line: u16, out: &mut [u8; WIDTH_H40]) {
        let base = plane.name_table(self);
        let width = usize::from(self.plane_width_cells());
        let height = usize::from(self.plane_height_cells());
        let line = usize::from(line);
        let row_base = ((line / CELL_ROWS) % height) * width;
        let cell_row = line % CELL_ROWS;

        for (column, pixels) in out.chunks_mut(CELL_ROWS).enumerate() {
            let entry_index = (row_base + column % width) & (MAX_ENTRIES - 1);
            let entry = read_word(&self.vram, base + entry_index * 2);
            let cell = NameEntry::decode(entry);
            let indices = tile_row(
                &self.vram, cell.tile, cell_row, cell.vflip, cell.hflip, CELL_ROWS,
            );
            let layer_base = (cell.palette << 4) | (u8::from(cell.priority) << PRIORITY_SHIFT);
            for (pixel, index) in pixels.iter_mut().zip(indices) {
                *pixel = if index == 0 { 0 } else { layer_base | index };
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use crate::VRAM_SIZE;
    use crate::Vdp;
    use crate::pattern::tile_addr;

    /// Plane A table base of the plane tests: R2 = `0x30` is 0xC000.
    pub(crate) const PLANE_A_BASE: usize = 0xC000;
    /// Plane B table base of the plane tests: R4 = `0x07` is 0xE000.
    pub(crate) const PLANE_B_BASE: usize = 0xE000;

    /// A VDP with plane A at 0xC000, plane B at 0xE000 and a 32x32 playfield:
    /// the register set every plane test starts from.
    pub(crate) fn plane_vdp() -> Vdp {
        let mut vdp = Vdp::new(false);
        vdp.write_register(2, 0x30);
        vdp.write_register(4, 0x07);
        vdp.write_register(16, 0x00);
        vdp
    }

    /// Big-endian `word` at byte `addr` of VRAM, which is how
    /// [`crate::pattern::read_word`] reads a table back.
    pub(crate) fn put_word(vdp: &mut Vdp, addr: usize, word: u16) {
        let mask = VRAM_SIZE - 1;
        vdp.vram[addr & mask] = (word >> 8) as u8;
        vdp.vram[(addr + 1) & mask] = word as u8;
    }

    /// Entry `cell` of the name table at `base`: `base + cell * 2`.
    pub(crate) fn put_entry(vdp: &mut Vdp, base: usize, cell: usize, word: u16) {
        put_word(vdp, base + cell * 2, word);
    }

    /// One row of `tile`, filled with the colour index `colour`.
    pub(crate) fn fill_tile_row(vdp: &mut Vdp, tile: u16, row: usize, colour: u8) {
        put_tile_row(vdp, tile, row, &[colour; 8]);
    }

    /// One row of `tile`, set to 8 colour indices: the four bytes of a pattern
    /// row (genvdp.txt section 12) with the high nibble of each byte the left
    /// pixel, the convention of [`crate::pattern::tile_row`] and INFERRED there.
    pub(crate) fn put_tile_row(vdp: &mut Vdp, tile: u16, row: usize, pixels: &[u8; 8]) {
        let mask = VRAM_SIZE - 1;
        let start = tile_addr(tile, 8) + row * 4;
        for (byte, pair) in pixels.chunks(2).enumerate() {
            vdp.vram[(start + byte) & mask] = (pair[0] << 4) | pair[1];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use crate::Vdp;

    /// Bit 6 of a layer pixel: the cell's priority flag, which the compositor of
    /// M4-17 (#95) reads to put the pixel in front of plane A and the sprites
    /// (genvdp.txt section 14, where a plane's entries move from `B`/`C` to
    /// `E'`/`F'` when the bit is set).
    const PRIORITY: u8 = 1 << 6;

    /// The 320-pixel line [`Vdp::render_plane_line`] is called with, ready to fill.
    fn blank_line() -> [u8; 320] {
        [0; 320]
    }

    /// Plane A at 0xC000, 32x32, cell (0,0) naming tile 1 of palette 2 with row
    /// 0 all colour 5: the setup of the ticket's first case. Every other cell is
    /// still the all-zero entry, tile 0 of palette 0, which is all transparent.
    fn plane_a_showing_tile_1() -> Vdp {
        let mut vdp = plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, palette_entry(2, 1));
        fill_tile_row(&mut vdp, 1, 0, 5);
        vdp
    }

    /// The 6-bit layer value of `colour` in `palette`: palette * 16 + colour, the
    /// CRAM index of genvdp.txt section 9 with no priority bit.
    fn cram_index(palette: u8, colour: u8) -> u8 {
        palette * 16 + colour
    }

    /// The name table word naming `tile` in `palette`, priority clear.
    fn palette_entry(palette: u8, tile: u16) -> u16 {
        u16::from(palette) << 13 | tile
    }

    #[test]
    fn plane_a_line_0_renders_the_cell_at_0_0() {
        let vdp = plane_a_showing_tile_1();
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);

        assert_eq!(
            &out[0..8],
            &[cram_index(2, 5); 8],
            "cell (0,0) names tile 1 of palette 2, whose row is all colour 5"
        );
        assert_eq!(out[8], 0, "cell (1,0) is an empty entry: tile 0, colour 0");
        assert_eq!(
            &out[8..256],
            &[0; 248],
            "colour 0 of any palette is transparent, so nothing else is drawn; the cells past 256 are the wrap of the ones before them, which the ticket leaves to its own case"
        );
    }

    #[test]
    fn plane_priority_bit_lands_in_bit_6() {
        let mut vdp = plane_a_showing_tile_1();
        put_entry(&mut vdp, PLANE_A_BASE, 0, palette_entry(2, 1) | 0x8000);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);

        assert_eq!(&out[0..8], &[cram_index(2, 5) | PRIORITY; 8]);
        assert_eq!(
            out[8], 0,
            "a transparent pixel carries no priority either: it draws nothing"
        );
    }

    /// The 6 CRAM-index bits and the 1 priority bit do not overlap even at the
    /// largest palette and colour: palette 3, colour 15 is index 63.
    #[test]
    fn plane_priority_bit_does_not_collide_with_the_cram_index() {
        let mut vdp = plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, 0x6000 | 1);
        fill_tile_row(&mut vdp, 1, 0, 15);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(&out[0..8], &[63; 8], "palette 3 colour 15, priority clear");

        put_entry(&mut vdp, PLANE_A_BASE, 0, 0xE000 | 1);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(&out[0..8], &[63 | PRIORITY; 8]);
    }

    /// Line 9 is the second row of the plane (`9 / 8 == 1`) and the second row of
    /// the pattern (`9 % 8 == 1`), so neither of the two may be pinned to 0.
    #[test]
    fn line_9_reads_cell_row_1_and_tile_row_1() {
        let mut vdp = plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, 1);
        put_entry(&mut vdp, PLANE_A_BASE, 32, 2);
        fill_tile_row(&mut vdp, 1, 0, 1);
        fill_tile_row(&mut vdp, 1, 1, 2);
        fill_tile_row(&mut vdp, 2, 0, 3);
        fill_tile_row(&mut vdp, 2, 1, 4);

        let cases: [(u16, u8); 4] = [(0, 1), (1, 2), (8, 3), (9, 4)];
        for (line, colour) in cases {
            let mut out = blank_line();
            vdp.render_plane_line(Plane::A, line, &mut out);
            assert_eq!(
                &out[0..8],
                &[colour; 8],
                "line {line}: cell row {}, tile row {}",
                line / 8,
                line % 8
            );
        }
    }

    /// A 64-cell-wide plane (R16 = 0x01) indexes entries `base + (cy * 64 + cx) * 2`,
    /// so cell (39, 0) is at `base + 78` and its pixels are the last eight of an
    /// H40 line, and cell (0, 1) is at `base + 128` rather than at the `base + 16`
    /// a 32-wide plane would use (genvdp.txt section 17 "$10", section 13).
    #[test]
    fn a_64_wide_plane_indexes_entries_over_the_whole_row() {
        let mut vdp = plane_vdp();
        vdp.write_register(12, 0x81);
        vdp.write_register(16, 0x01);
        assert_eq!(vdp.plane_width_cells(), 64);
        assert_eq!(vdp.plane_height_cells(), 32);

        put_word(&mut vdp, PLANE_A_BASE + 78, 3);
        fill_tile_row(&mut vdp, 3, 0, 7);
        put_word(&mut vdp, PLANE_A_BASE + 128, 4);
        fill_tile_row(&mut vdp, 4, 0, 8);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            out[319], 7,
            "pixel 319 is the last pixel of the cell at base + 78"
        );
        assert_eq!(&out[312..320], &[7; 8], "cell 39 holds tile 3");
        assert_eq!(
            &out[0..312],
            &[0; 312],
            "the entry at base + 128 is cell (0,1) and is not read on line 0"
        );

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 8, &mut out);
        assert_eq!(&out[0..8], &[8; 8], "base + 128 is cell (0,1)");
    }

    /// Plane B takes its table from R4 and nothing else: the two planes hold
    /// different cells at the same offset and render differently.
    #[test]
    fn plane_b_uses_the_r4_base_and_plane_a_the_r2_base() {
        let mut vdp = plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, 1);
        fill_tile_row(&mut vdp, 1, 0, 1);
        put_entry(&mut vdp, PLANE_B_BASE, 0, 2);
        fill_tile_row(&mut vdp, 2, 0, 2);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(&out[0..8], &[1; 8], "plane A reads its own table");

        let mut out = blank_line();
        vdp.render_plane_line(Plane::B, 0, &mut out);
        assert_eq!(&out[0..8], &[2; 8], "plane B reads its own table");

        put_entry(&mut vdp, PLANE_A_BASE, 0, 0);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::B, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[2; 8],
            "emptying plane A's cell leaves plane B alone"
        );
    }

    /// The flips of a name table entry reach the plane pixels, through the
    /// pattern decode of `pattern` rather than a second implementation of them.
    #[test]
    fn plane_cell_flips_reach_the_pixels() {
        let mut vdp = plane_vdp();
        put_tile_row(&mut vdp, 1, 0, &[0, 1, 2, 3, 4, 5, 6, 7]);
        put_entry(&mut vdp, PLANE_A_BASE, 0, 1 | 0x0800);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(out[0..8], [7, 6, 5, 4, 3, 2, 1, 0], "hflip");

        fill_tile_row(&mut vdp, 1, 0, 1);
        fill_tile_row(&mut vdp, 1, 7, 2);
        put_entry(&mut vdp, PLANE_A_BASE, 0, 1 | 0x1000);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[2; 8],
            "vflip shows the bottom row of the tile"
        );
    }

    /// A 32-cell plane is 256 pixels wide and 256 lines tall while an H40 line
    /// reaches pixel 319 and a V30 frame line 255, so both axes wrap: `cx % W`
    /// and `cy % H` (genvdp.txt section 13: "When the scroll value is larger
    /// than the width of the playfield, the display wraps horizontally").
    #[test]
    fn plane_fetch_wraps_at_the_table_edges() {
        let vdp = plane_a_showing_tile_1();

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            out[256..264],
            out[0..8],
            "cell column 32 of a 32-wide plane is cell column 0"
        );

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 256, &mut out);
        assert_eq!(
            &out[0..8],
            &[cram_index(2, 5); 8],
            "cell row 32 of a 32-tall plane is cell row 0"
        );
    }

    /// An invalid plane size - genvdp.txt section 13 lists 128x128 and 64x128 as
    /// over its 8192-byte table limit - keeps every fetch inside one table here.
    /// See [`Vdp::render_plane_line`]; the wrap itself is undocumented.
    #[test]
    fn an_invalid_plane_size_keeps_every_fetch_inside_one_table() {
        let mut vdp = plane_a_showing_tile_1();
        // One entry past the 4096 an 8192-byte table holds: 0xC000 + 8192, which
        // is plane B's cell (0,0) here and reads as nothing while plane B is not
        // the plane under test.
        put_word(&mut vdp, PLANE_A_BASE + 8192, 5);
        fill_tile_row(&mut vdp, 5, 0, 9);
        vdp.write_register(16, 0x33);
        assert_eq!(vdp.plane_width_cells(), 128);
        assert_eq!(vdp.plane_height_cells(), 128);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 256, &mut out);
        assert_eq!(
            &out[0..8],
            &[cram_index(2, 5); 8],
            "cell (0,32) of a 128-wide plane is entry 4096, masked back to entry 0 and not read past the table"
        );

        // A valid 128-wide row: cell (0,1) is entry 128, not entry 1.
        put_word(&mut vdp, PLANE_A_BASE + 256, 6);
        fill_tile_row(&mut vdp, 6, 0, 6);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 8, &mut out);
        assert_eq!(&out[0..8], &[6; 8], "cell (0,1) of a 128-wide plane");
    }

    /// The plane fetch does not care about the display width: the whole 320-byte
    /// line is written in H32 as well as in H40, and the compositor of M4-17
    /// (#95) drops the columns past [`Vdp::active_width`].
    #[test]
    fn the_plane_line_is_320_pixels_in_h32_too() {
        let vdp = plane_a_showing_tile_1();
        let mut out_h32 = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out_h32);

        let mut h40 = plane_a_showing_tile_1();
        h40.write_register(12, 0x81);
        let mut out_h40 = blank_line();
        h40.render_plane_line(Plane::A, 0, &mut out_h40);

        assert_eq!(out_h32, out_h40, "the plane does not know the width");
        assert_eq!(
            out_h32[256..264],
            out_h32[0..8],
            "and still wraps at cell 32"
        );
    }
}
