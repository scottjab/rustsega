//! The window plane (M4-14 #92): what plane A becomes inside an area.
//!
//! genvdp.txt section 13 "Window": "The window plane operates differently from
//! plane A or B. It can be thought of a 'replacement' for plane A which is used
//! under certain conditions. That said, plane A cannot be displayed in any area
//! where plane W is located, it is impossible for them to overlap." The window is
//! therefore not a third layer for the compositor of M4-17 (#95) to sort out but a
//! second source for the plane A pixels of a line, and
//! [`Vdp::render_plane_a_line_with_window`] is the one call that hands over
//! whichever source each pixel came from.
//!
//! The two fetches of a cell are [`crate::pattern`]'s, through the same
//! [`Vdp::fill_cell_run`] the two scroll planes end with, so the entry decode, the
//! flip, the palette and the colour-0 transparency of genvdp.txt section 13 are
//! written once for the three name tables.
//!
//! TODO(#289): four readings here go beyond what genvdp.txt settles - that R17 and
//! R18 are an intersection ([`Vdp::window_line`]), that the window has to be shown
//! for its bug to bite ([`Vdp::window_edge_bug`]), that the bug moves the name
//! table fetch of a column and not its VSRAM word ([`Vdp::render_window_run`]'s
//! caller), and what a window fetch reads past the end of its own table - and each
//! is pinned by a test here so that a correction cannot be quiet.

use crate::Vdp;
use crate::WIDTH_H40;
use crate::planes::{CELL_COLUMN, CELL_ROWS, Plane};

/// Cells in a row of the window name table, by display width: genvdp.txt section
/// 13, "The name table for plane W is 32x32 in 32-cell mode, and 64x32 in
/// 40-cell mode. This size is fixed and is entirely dependant on the display
/// width."
const TABLE_CELLS_H32: usize = 32;
const TABLE_CELLS_H40: usize = 64;

/// Rows of the window name table, in both widths and in both V30 modes: the
/// `x32` of the sentence quoted on [`TABLE_CELLS_H32`].
const TABLE_ROWS: usize = 32;

impl Vdp {
    /// Cells in a row of the window name table: the fixed width of genvdp.txt
    /// section 13 quoted on [`TABLE_CELLS_H32`], which is the only rule the table
    /// has - unlike plane A and plane B it has no size register, and R16 does not
    /// reach it.
    #[inline]
    fn window_table_cells(&self) -> usize {
        if self.h40() {
            TABLE_CELLS_H40
        } else {
            TABLE_CELLS_H32
        }
    }

    /// The pixel x of the R17 split: "The WHP field defines the horizontal range
    /// of the window, in units of two cells. (16 pixels)" (genvdp.txt section 17
    /// "$11"). It is where the window ends when the window is on the left and
    /// where it starts when it is on the right.
    #[inline]
    fn window_split_x(&self) -> usize {
        usize::from(self.window_h_position()) * CELL_COLUMN
    }

    /// The line of the R18 split: "The WVP field defines the vertical range of the
    /// window, in units of eight lines." (genvdp.txt section 17 "$12").
    #[inline]
    fn window_split_line(&self) -> usize {
        usize::from(self.window_v_position()) * CELL_ROWS
    }

    /// The screen columns of a line the window is shown on that the window covers:
    /// "When RIGT=0, the window is shown from column zero to the column specified
    /// by the WHP field ... When RIGHT=1, the window is shown from the column
    /// specified in the WHP field up to the last column in the display" (genvdp.txt
    /// section 17 "$11").
    ///
    /// The two settings the section singles out fall out of it rather than needing
    /// a case of their own: WHP = 0 with RIGT clear is `0..0`, "no window", and WHP
    /// = 0 with RIGT set is the whole line, "it means the window is shown from
    /// column zero up to the last column in the display".
    #[inline]
    fn window_columns(&self) -> core::ops::Range<usize> {
        let split = self.window_split_x().min(WIDTH_H40);
        if self.window_right() {
            split..WIDTH_H40
        } else {
            0..split
        }
    }

    /// True for the lines R18 puts inside the window: "When DOWN=0, the window is
    /// shown from line zero to the line specified by the WVP field ... When DOWN=1,
    /// the window is shown from the line specified in the WVP field up to the last
    /// line in the display" (genvdp.txt section 17 "$12"), WVP counting eight lines
    /// apiece so that WVP = 0 with DOWN clear is no window at all - "Setting the
    /// WVP field to zero disables the window" - and WVP = 0 with DOWN set is every
    /// line of the display.
    ///
    /// This is the vertical half of an area, not an alternative to [`Vdp::window_columns`]:
    /// section 13 says "Registers 17 and 18 define an area which the window is
    /// restricted to", and "Setting the WVP field to zero disables the window" is
    /// only true of an intersection. Two lines of the two register texts - "This
    /// register will affect the window shown on the current line, if the current
    /// line does not fall into the vertical range specified by register $12"
    /// ("$11") and its twin in "$12" - read on their own would make the two
    /// registers alternatives, which would both contradict the sentence above and
    /// make the standard use of the pair, a full-width status bar down one side of
    /// the screen, impossible. They are read here as saying that R17 is consulted
    /// per line and only on the lines R18 is showing the window on.
    #[inline]
    fn window_line(&self, line: usize) -> bool {
        let split = self.window_split_line();
        if self.window_down() {
            line >= split
        } else {
            line < split
        }
    }

    /// Plane A of `line` with the window over it, in one buffer: genvdp.txt
    /// section 13, "plane A cannot be displayed in any area where plane W is
    /// located", so the compositor of M4-17 (#95) gets one 6-bit CRAM index plus
    /// priority bit per pixel and does not have to know which of the two it came
    /// from.
    ///
    /// Plane A is rendered first and the window painted over the columns R17 and
    /// R18 give it, because the columns the window leaves alone are exactly the
    /// plane A line of [`Vdp::render_plane_line`] - the window is scrolled by
    /// neither hscroll nor vscroll, and the plane is not scrolled by the window.
    pub fn render_plane_a_line_with_window(&self, line: u16, out: &mut [u8; WIDTH_H40]) {
        self.render_plane_line(Plane::A, line, out);
        let line = usize::from(line);
        if !self.window_line(line) {
            return;
        }
        let columns = self.window_columns();
        self.render_window_run(line, columns.clone(), out);
        if !self.window_right() {
            self.window_edge_bug(line, columns.end, out);
        }
    }

    /// Fill `out[columns]` with the window plane of `line`.
    ///
    /// The window is never scrolled: genvdp.txt section 13 "Horizontal Scrolling"
    /// gives the scroll table to "every line of both planes A and B" and section 10
    /// the VSRAM words to the planes, while plane W "operates differently from
    /// plane A or B". So this is the plain walk of a name table, entry
    /// `base + (cy * width + cx) * 2` for the fixed `width` of
    /// [`Vdp::window_table_cells`], eight pixels at a time and allocating nothing.
    ///
    /// Both axes wrap at that fixed size, mirroring the way
    /// [`Vdp::render_plane_line`] wraps at the plane size of R16 so that no fetch
    /// leaves the table. Nothing documents what the VDP reads outside a window
    /// table, and no display reaches the edge of one: the tallest screen is 240
    /// lines of 30 cell rows, and only the columns past [`Vdp::active_width`] in an
    /// H32 line pass cell 31 of a 32-cell row - the overscan the compositor drops.
    fn render_window_run(&self, line: usize, columns: core::ops::Range<usize>, out: &mut [u8]) {
        let base = usize::from(self.window_base());
        let width = self.window_table_cells();
        let row = (line / CELL_ROWS) % TABLE_ROWS;
        let tile_row = line % CELL_ROWS;

        let mut x = columns.start;
        while x < columns.end {
            let offset = x % CELL_ROWS;
            let cell = (x / CELL_ROWS) % width;
            let run = (CELL_ROWS - offset).min(columns.end - x);
            self.fill_cell_run(
                base,
                row * width + cell,
                tile_row,
                offset,
                &mut out[x..x + run],
            );
            x += run;
        }
    }

    /// The window bug of genvdp.txt section 17 "$11", in the words of that section:
    ///
    /// > There is a bug in the window processing. This occurs when the window is
    /// > showing partially on the left side of the screen, specifically when the
    /// > VDP is drawing the 2-cell column from plane A that immediately proceeds
    /// > the last column the window was drawn on. (i.e. WHP+1)
    /// >
    /// > If the lower four bits of the horizontal scroll value for the current
    /// > scan line are zero, then the name table attribute data for the current
    /// > column are fetched correctly. If the lower four bits of the horizontal
    /// > scroll value for the current scan line are nonzero, the name table
    /// > attribute data are fetched from next column. (WHP+2)
    /// >
    /// > In effect, you'll have N columns of the window plane, 1 column that has
    /// > identical patterns as the next column, then the remainder of the display
    /// > is drawn correctly.
    ///
    /// So the first 16 pixel column of plane A after a window on the left is drawn
    /// as a copy of the column after it, which is the plane A fetch of `edge` with
    /// [`CELL_COLUMN`] added to its plane x: the sub-cell offset of the scroll is
    /// kept, which is why the section calls the result "identical patterns" rather
    /// than shifted ones.
    ///
    /// `edge` is the window split of a window on the left, so 0 is "no window on the
    /// left" and [`WIDTH_H40`] is "no plane A column after the window"; either way
    /// there is nothing to fetch wrong. Neither is the bug applied to a line the
    /// window is not shown on - the section conditions it on the window "showing"
    /// - nor to a window on the right, which has no plane A column after it.
    fn window_edge_bug(&self, line: usize, edge: usize, out: &mut [u8]) {
        if edge == 0 || edge >= WIDTH_H40 {
            return;
        }
        if self.line_hscroll(Plane::A, line) & (CELL_COLUMN - 1) == 0 {
            return;
        }
        let columns = edge..(edge + CELL_COLUMN).min(WIDTH_H40);
        self.render_plane_run(Plane::A, line, columns, CELL_COLUMN, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planes::testutil::*;

    /// Window name table base of these tests: R3 = 0x28 is 0xA000, clear of the
    /// plane A and B tables (0xC000, 0xE000), of the scroll table (0xFC00) and of
    /// the patterns every fill writes at the bottom of VRAM.
    const WINDOW_BASE: usize = 0xA000;
    /// R3 pointing the window table at [`WINDOW_BASE`]: genvdp.txt section 17
    /// "$03", "Bits 5-1 of this register correspond to bits A15-A11 of the name
    /// table address for the window", 2 KiB per step. Bit 1 is clear so that H40,
    /// which forces A11 to zero, reads the same table.
    const WINDOW_REG: u8 = 0x28;

    /// Plane A of [`window_vdp`] is solid [`PLANE_COLOUR`] in tile [`PLANE_TILE`],
    /// and the window solid [`WINDOW_COLOUR`] in tile [`WINDOW_TILE`], so the
    /// colour of a pixel says which of the two planes drew it. Tile 40 is outside
    /// the 1..=32 [`fill_columns`] and [`fill_rows`] use, so the window of a test
    /// keeps its colour while plane A is refilled with one of those.
    const PLANE_TILE: u16 = 1;
    const PLANE_COLOUR: u8 = 9;
    const WINDOW_TILE: u16 = 40;
    const WINDOW_COLOUR: u8 = 6;

    /// Cells per row of the window table: genvdp.txt section 13, "The name table
    /// for plane W is 32x32 in 32-cell mode, and 64x32 in 40-cell mode. This size
    /// is fixed and is entirely dependant on the display width."
    const H32_CELLS: usize = 32;
    const H40_CELLS: usize = 64;
    /// Cell rows of the window table, in both widths and in both V30 modes.
    const WINDOW_ROWS: usize = 32;

    /// Bit 6 of a layer pixel: the priority flag, which genvdp.txt section 13
    /// "Window" gives the window too: "In terms of priority and intensity
    /// calculation for shadow / hilight mode, plane W is treated _exactly_ the
    /// same as plane A."
    const PRIORITY: u8 = 1 << 6;

    /// The 320-pixel line every test here renders into.
    fn blank_line() -> [u8; WIDTH_H40] {
        [0; WIDTH_H40]
    }

    /// Every cell of the `width` x [`WINDOW_ROWS`] table at `base` naming `tile`,
    /// all 8 rows of it `colour`.
    fn fill_solid(vdp: &mut Vdp, base: usize, width: usize, tile: u16, colour: u8) {
        for cell in 0..width * WINDOW_ROWS {
            put_entry(vdp, base, cell, tile);
        }
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(vdp, tile, row, colour);
        }
    }

    /// An H32 VDP whose plane A is one flat colour and whose window another, with
    /// R17 = `r17` and R18 = `r18`.
    fn window_vdp(r17: u8, r18: u8) -> Vdp {
        let mut vdp = plane_vdp();
        vdp.write_register(3, WINDOW_REG);
        vdp.write_register(17, r17);
        vdp.write_register(18, r18);
        fill_solid(
            &mut vdp,
            PLANE_A_BASE,
            PLANE_COLUMNS,
            PLANE_TILE,
            PLANE_COLOUR,
        );
        fill_solid(&mut vdp, WINDOW_BASE, H32_CELLS, WINDOW_TILE, WINDOW_COLOUR);
        vdp
    }

    /// [`window_vdp`] in H40, with the window table laid out over its 64 cells.
    fn h40_window_vdp(r17: u8, r18: u8) -> Vdp {
        let mut vdp = window_vdp(r17, r18);
        vdp.write_register(12, 0x81);
        fill_solid(&mut vdp, WINDOW_BASE, H40_CELLS, WINDOW_TILE, WINDOW_COLOUR);
        vdp
    }

    /// The line [`Vdp::render_plane_line`] gives for plane A alone, which is what
    /// a window test compares the part of a line the window is not on against.
    fn plane_a_line(vdp: &Vdp, line: u16) -> [u8; WIDTH_H40] {
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, line, &mut out);
        out
    }

    /// Assert that `out` is the window colour over `window` and the plane A colour
    /// over the rest of the line.
    fn assert_window_line(out: &[u8; WIDTH_H40], window: core::ops::Range<usize>) {
        for (x, pixel) in out.iter().enumerate() {
            let want = if window.contains(&x) {
                WINDOW_COLOUR
            } else {
                PLANE_COLOUR
            };
            assert_eq!(
                *pixel,
                want,
                "pixel {x} is {}",
                if want == WINDOW_COLOUR {
                    "window"
                } else {
                    "plane A"
                }
            );
        }
    }

    #[test]
    fn no_window_when_r17_and_r18_are_zero() {
        let vdp = window_vdp(0x00, 0x00);
        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_eq!(
            out,
            plane_a_line(&vdp, 0),
            "WHP = 0 with RIGT = 0 and WVP = 0 with DOWN = 0: genvdp.txt section 17 \"$11\" and \"$12\" both call that no window"
        );

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(200, &mut out);
        assert_eq!(out, plane_a_line(&vdp, 200));
    }

    /// genvdp.txt section 17 "$12": WVP counts units of eight lines, "2 = lines
    /// 0-$F", so with DOWN clear the window is lines 0..15 and line 16 is plane A
    /// again. R17 = 0x80 is WHP = 0 with RIGT = 1, which that section calls the
    /// whole width: "it means the window is shown from column zero up to the last
    /// column in the display".
    #[test]
    fn r18_0x02_is_the_window_for_the_first_16_lines() {
        let vdp = window_vdp(0x80, 0x02);
        let mut out = blank_line();
        for line in 0..16 {
            vdp.render_plane_a_line_with_window(line, &mut out);
            assert_eq!(
                out, [WINDOW_COLOUR; WIDTH_H40],
                "line {line} is all window: WVP = 2 is lines 0..16"
            );
        }

        vdp.render_plane_a_line_with_window(16, &mut out);
        assert_eq!(
            out,
            plane_a_line(&vdp, 16),
            "line 16 is the first line outside the area, so plane A shows again"
        );
    }

    /// The DOWN half of the same field: WVP = 2 with DOWN set is "the line
    /// specified in the WVP field up to the last line in the display", so lines
    /// 0..15 stay plane A and line 16 is the window.
    #[test]
    fn r18_0x82_is_the_window_from_line_16_down() {
        let vdp = window_vdp(0x80, 0x82);
        let mut out = blank_line();
        for line in [0, 8, 15] {
            vdp.render_plane_a_line_with_window(line, &mut out);
            assert_eq!(
                out,
                plane_a_line(&vdp, line),
                "line {line} is above the split, so plane A"
            );
        }

        for line in [16, 17, 200] {
            vdp.render_plane_a_line_with_window(line, &mut out);
            assert_eq!(out, [WINDOW_COLOUR; WIDTH_H40], "line {line} is window");
        }
    }

    /// The RIGT half of genvdp.txt section 17 "$11": "When RIGT=0, the window is
    /// shown from column zero to the column specified by the WHP field ... if
    /// RIGT=0 and WHP=4, the window is displayed on columns zero up to (and
    /// including) column seven", 16 pixels for a WHP of 2. R18 = 0x80 is DOWN with
    /// WVP = 0, which that section says is every line of the display.
    #[test]
    fn r17_0x02_is_the_window_for_the_first_32_pixels() {
        let vdp = window_vdp(0x02, 0x80);
        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_window_line(&out, 0..32);

        vdp.render_plane_a_line_with_window(100, &mut out);
        assert_window_line(&out, 0..32);
    }

    /// The other side: "When RIGHT=1, the window is shown from the column
    /// specified in the WHP field up to the last column in the display".
    #[test]
    fn r17_0x82_leaves_the_first_32_pixels_to_plane_a() {
        let vdp = window_vdp(0x82, 0x80);
        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_window_line(&out, 32..WIDTH_H40);

        vdp.render_plane_a_line_with_window(100, &mut out);
        assert_window_line(&out, 32..WIDTH_H40);
    }

    /// WHP = 0 means no window only with RIGT clear; with RIGT set it is the whole
    /// width, and WVP = 0 with DOWN set is the whole height, so the pair is the
    /// whole screen (genvdp.txt section 17 "$11" and "$12", each "Having WxP set
    /// to zero and ... =1 is a legal setting; it means the entire line/screen is
    /// taken up by the window plane").
    #[test]
    fn a_full_width_and_full_height_window_replaces_plane_a_everywhere() {
        let vdp = window_vdp(0x80, 0x80);
        let mut out = blank_line();
        for line in [0, 100, 239] {
            vdp.render_plane_a_line_with_window(line, &mut out);
            assert_eq!(out, [WINDOW_COLOUR; WIDTH_H40], "line {line}");
        }
    }

    /// genvdp.txt section 13 "Window": "Registers 17 and 18 define an area which
    /// the window is restricted to", and section 17 "$12": "Setting the WVP field
    /// to zero disables the window". Both are read here as an intersection, so a
    /// line R18 keeps out is plane A even where R17 covers it.
    #[test]
    fn a_line_outside_the_r18_area_is_plane_a_wherever_r17_reaches() {
        let vdp = window_vdp(0x02, 0x00);
        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_eq!(
            out,
            plane_a_line(&vdp, 0),
            "R17 = 0x02 alone does not make a window: WVP = 0 disables it"
        );
    }

    /// The window table is 32 cells per row in H32 (genvdp.txt section 13), so
    /// cell (1,1) is entry 32 + 1 and not the 64 + 1 of H40.
    #[test]
    fn h32_window_entries_are_indexed_over_32_cells() {
        let mut vdp = window_vdp(0x80, 0x80);
        put_entry(&mut vdp, WINDOW_BASE, 1, 3);
        put_entry(&mut vdp, WINDOW_BASE, H32_CELLS + 1, 4);
        put_entry(&mut vdp, WINDOW_BASE, H40_CELLS + 1, 5);
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 3, row, 11);
            fill_tile_row(&mut vdp, 4, row, 12);
            fill_tile_row(&mut vdp, 5, row, 13);
        }

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_eq!(&out[8..16], &[11; 8], "cell (1,0) is entry 1");

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(8, &mut out);
        assert_eq!(
            &out[8..16],
            &[12; 8],
            "cell (1,1) of a 32-cell row is entry 32 + 1"
        );
    }

    /// The same table in H40 is 64 cells per row, so cell (1,1) moves to
    /// `base + (64 + 1) * 2` and the 32-wide index of it is not read at all.
    #[test]
    fn h40_window_entries_are_indexed_over_64_cells() {
        let mut vdp = h40_window_vdp(0x80, 0x80);
        put_entry(&mut vdp, WINDOW_BASE, 1, 3);
        put_entry(&mut vdp, WINDOW_BASE, H32_CELLS + 1, 4);
        put_entry(&mut vdp, WINDOW_BASE, H40_CELLS + 1, 5);
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 3, row, 11);
            fill_tile_row(&mut vdp, 4, row, 12);
            fill_tile_row(&mut vdp, 5, row, 13);
        }

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_eq!(&out[8..16], &[11; 8], "cell (1,0) is still entry 1");

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(8, &mut out);
        assert_eq!(
            &out[8..16],
            &[13; 8],
            "cell (1,1) of a 40-cell row is entry 64 + 1, and the H32 index of it at 32 + 1 is not read"
        );
    }

    /// The window is not scrolled. genvdp.txt section 13 "Horizontal Scrolling"
    /// gives the scroll table to "every line of both planes A and B" and section
    /// 10 the VSRAM words to the planes, and plane W "operates differently from
    /// plane A or B" - so a scroll value moves plane A under a window and leaves
    /// the window where it is.
    #[test]
    fn the_window_ignores_the_horizontal_scroll() {
        let mut vdp = window_vdp(0x02, 0x80);
        fill_columns(&mut vdp, PLANE_A_BASE);
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, 0x00);
        let mut plain = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut plain);

        put_scroll(&mut vdp, 0, 16, 0);
        let mut scrolled = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut scrolled);
        assert_eq!(
            &scrolled[0..32],
            &[WINDOW_COLOUR; 32],
            "the window pixels did not move with the scroll"
        );
        assert_eq!(
            &scrolled[32..],
            &plane_a_line(&vdp, 0)[32..],
            "and the rest of the line is plane A scrolled"
        );
        assert_ne!(
            &plain[32..48],
            &plane_a_line(&vdp, 0)[32..48],
            "the scroll really is live on plane A, or the two asserts above would agree by doing nothing"
        );
    }

    /// The vertical half of the same rule, on a plane A filled by [`fill_rows`] so
    /// that a vscroll moves it.
    #[test]
    fn the_window_ignores_the_vertical_scroll() {
        let mut vdp = window_vdp(0x02, 0x80);
        fill_rows(&mut vdp, PLANE_A_BASE);
        let mut before = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut before);

        vdp.vsram[0] = 8;
        let mut after = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut after);
        assert_eq!(
            &after[0..32],
            &[WINDOW_COLOUR; 32],
            "the window does not read VSRAM"
        );
        assert_ne!(
            &before[32..],
            &after[32..],
            "plane A moved by the vertical scroll the window ignored"
        );
    }

    /// genvdp.txt section 13 "Window": "In terms of priority and intensity
    /// calculation for shadow / hilight mode, plane W is treated _exactly_ the
    /// same as plane A", so the flag of a window entry lands in bit 6 of its
    /// pixels just as a plane A one does.
    #[test]
    fn a_window_entry_keeps_its_priority_bit() {
        let mut vdp = window_vdp(0x80, 0x80);
        put_entry(&mut vdp, WINDOW_BASE, 0, 0x8000 | WINDOW_TILE);
        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_eq!(&out[0..8], &[WINDOW_COLOUR | PRIORITY; 8]);
        assert_eq!(
            &out[8..16],
            &[WINDOW_COLOUR; 8],
            "only the entry that was rewritten carries it"
        );
    }

    /// The window bug of genvdp.txt section 17 "$11", in the words of that
    /// section: it "occurs when the window is showing partially on the left side
    /// of the screen, specifically when the VDP is drawing the 2-cell column from
    /// plane A that immediately proceeds the last column the window was drawn on.
    /// (i.e. WHP+1) ... If the lower four bits of the horizontal scroll value for
    /// the current scan line are nonzero, the name table attribute data are
    /// fetched from next column. (WHP+2) In effect, you'll have N columns of the
    /// window plane, 1 column that has identical patterns as the next column, then
    /// the remainder of the display is drawn correctly."
    #[test]
    fn the_plane_a_column_after_a_left_window_is_a_copy_of_the_next_one() {
        let mut vdp = window_vdp(0x02, 0x80);
        fill_columns(&mut vdp, PLANE_A_BASE);
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, 0x00);
        put_scroll(&mut vdp, 0, 3, 0);

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        let plane = plane_a_line(&vdp, 0);
        assert_eq!(&out[0..32], &[WINDOW_COLOUR; 32], "N columns of window");
        assert_eq!(
            &out[32..48],
            &plane[48..64],
            "the 16 pixels after the window edge are the 16 pixels of the next 2-cell column"
        );
        assert_ne!(
            &out[32..48],
            &plane[32..48],
            "and that is not what a correct fetch of the edge column gives"
        );
        assert_eq!(
            &out[48..],
            &plane[48..],
            "then the remainder of the display is drawn correctly"
        );
    }

    /// The other half of that condition: "If the lower four bits of the horizontal
    /// scroll value for the current scan line are zero, then the name table
    /// attribute data for the current column are fetched correctly."
    #[test]
    fn the_window_edge_column_is_fetched_correctly_at_a_16_byte_hscroll() {
        let mut vdp = window_vdp(0x02, 0x80);
        fill_columns(&mut vdp, PLANE_A_BASE);
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, 0x00);
        put_scroll(&mut vdp, 0, 16, 0);

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        let plane = plane_a_line(&vdp, 0);
        assert_eq!(&out[0..32], &[WINDOW_COLOUR; 32]);
        assert_eq!(&out[32..], &plane[32..]);
    }

    /// The bug is a bug of the window being on the left, and of the window being
    /// shown at all: with RIGT set plane A is the part before the split, and genvdp.txt section 17
    /// "$11" describes the bug only "when the window is showing partially on the
    /// left side of the screen".
    #[test]
    fn a_right_hand_window_has_no_edge_bug() {
        let mut vdp = window_vdp(0x82, 0x80);
        fill_columns(&mut vdp, PLANE_A_BASE);
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, 0x00);
        put_scroll(&mut vdp, 0, 3, 0);

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        let plane = plane_a_line(&vdp, 0);
        assert_eq!(
            &out[0..32],
            &plane[0..32],
            "plane A up to the split is fetched normally, so the bug is not a general effect of a nonzero low nibble"
        );
        assert_eq!(&out[32..], &[WINDOW_COLOUR; WIDTH_H40 - 32]);
    }

    /// And on a line the window is not shown on there is no window edge to fetch
    /// across: the section conditions the bug on the window "showing", so a
    /// disabled R18 leaves the whole line to plane A even with the R17 and hscroll
    /// conditions of the bug met.
    #[test]
    fn a_line_with_no_window_has_no_edge_bug() {
        let mut vdp = window_vdp(0x02, 0x00);
        fill_columns(&mut vdp, PLANE_A_BASE);
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, 0x00);
        put_scroll(&mut vdp, 0, 3, 0);

        let mut out = blank_line();
        vdp.render_plane_a_line_with_window(0, &mut out);
        assert_eq!(out, plane_a_line(&vdp, 0));
    }
}
