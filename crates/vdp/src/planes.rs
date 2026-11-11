//! Plane A and plane B line rendering (M4-11 #89, and its horizontal scroll of
//! M4-12 #90).
//!
//! One line of one scroll plane as a layer buffer: a [`u8`] per pixel holding a
//! CRAM index rather than a colour, so the compositing of M4-17 (#95) can still
//! decide which plane a pixel comes from, whether it is in front of the sprites,
//! and whether it is a shadow. [`crate::pattern`] does the two fetches each
//! cell needs - the name table word and the pattern row - and nothing here
//! repeats either.

use crate::pattern::{NameEntry, read_word, tile_row};
use crate::{HScrollMode, InterlaceMode, Vdp, WIDTH_H40};

/// Pixels across a plane cell: 8, in every mode.
///
/// genvdp.txt section 12 doubles the height of a pattern for interlace mode 2
/// ("A pattern is an 8x8 or 8x16 (interlace mode 2 only) pixel block") and never
/// its width, and R16 counts the columns of a name table in cells of this width
/// whatever the mode.
pub(crate) const CELL_WIDTH: usize = 8;

/// Rows of a plane cell, and so of the pattern a cell names, of the two modes
/// that are not interlace mode 2.
///
/// The pair of [`CELL_ROWS_MODE2`]; [`Vdp::cell_height`] is the side that reads
/// [`Vdp::interlace_mode`] and picks between them, and every fetch takes the
/// height from it rather than from either constant.
pub(crate) const CELL_ROWS: usize = 8;
/// Rows of a plane cell of interlace mode 2: double [`CELL_ROWS`], for the 8x16
/// pattern of genvdp.txt section 12 that [`crate::pattern::tile_addr`] addresses
/// by dropping bit 0 of the name.
pub(crate) const CELL_ROWS_MODE2: usize = 16;

/// Name table entries of the largest valid plane: genvdp.txt section 13, "The
/// name table size cannot exceed 8192 bytes, so while a 64x64 or 128x32 name
/// table is allowed, a size of 128x128 or 64x128 is invalid" - 8192 bytes is
/// 4096 of the 16-bit words a table is a matrix of.
const MAX_ENTRIES: usize = 4096;

/// Bit 6 of a layer pixel: the priority of the cell it came from, as M4-17
/// (#95) wants it. genvdp.txt section 14 moves a plane's pixels from `B`/`C`
/// to `E'`/`F'` when the entry sets the flag.
pub(crate) const PRIORITY_SHIFT: u32 = 6;

/// Bits of a horizontal scroll table entry that the VDP reads: genvdp.txt
/// section 13 "Horizontal Scrolling", "x = Horizontal scroll value (0-3FFh)"
/// and "Bits D15 through D10 are ignored by the VDP".
const HSCROLL_MASK: u16 = 0x3FF;

/// Bytes between one horizontal scroll entry and the next: the 16-bit word of
/// plane A and the 16-bit word of plane B, which that section describes as
/// "Scroll values for planes A and B are stored in an interleaved fashion".
const HSCROLL_ENTRY: usize = 4;

/// Bits of a VSRAM word that the plane fetch reads: genvdp.txt section 10,
/// "y = Vertical scroll factor (0-3FFh)", which is the scroll of every mode but
/// interlace mode 2.
const VSCROLL_MASK: u16 = 0x3FF;

/// The 11 bits of a vertical scroll value in interlace mode 2, where the value
/// counts the lines of the 448-line picture rather than the 224 of the beam
/// (ticket #98; genvdp.txt section 10 stops at the 10 and SpritesMind "Interlace
/// mode (LSMx=3)" gives the 11th). Bit 10 is all that differs from the mask
/// above, and it only shows on a plane of more than 512 lines - 128 cells of the
/// 16-line cells of the mode - which is what
/// `interlace_mode_2_vertical_scroll_is_eleven_bits_wide` sets up.
const VSCROLL_MASK_MODE2: u16 = 0x7FF;

/// Width in pixels of the column one VSRAM word scrolls in 2-cell column mode:
/// genvdp.txt section 17 "$0B" bit 2 "selects between full screen vertical
/// scrolling when clear, and 2-cell column based vertical scrolling when set",
/// and a cell is [`CELL_WIDTH`] pixels wide.
///
/// This is also the unit of the R17 window split and of the window bug of
/// section 17 "$11", which both speak of "2-cell columns" of 16 pixels: the same
/// granularity the VDP walks the playfield with.
pub(crate) const CELL_COLUMN: usize = 2 * CELL_WIDTH;

/// The two VSRAM words the partial 2-cell column at the left edge of an H40 line
/// is fetched with: see [`Vdp::left_column_vscroll`], which is as uncertain as
/// this list of numbers is.
const LEFT_COLUMN_WORDS: [usize; 2] = [19, 39];

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

    /// Which of the two words of a horizontal scroll entry this plane reads.
    ///
    /// genvdp.txt section 13 "Horizontal Scrolling": "Entry #0 of the table is
    /// for plane A, entry #1 is for plane B, and this repeats for the entire
    /// length of the table."
    #[inline]
    fn scroll_word(self) -> usize {
        match self {
            Plane::A => 0,
            Plane::B => 2,
        }
    }

    /// Which of the two VSRAM words of a column this plane reads: A the first,
    /// B the second, which is also `vsram[0]` and `vsram[1]` in full screen mode.
    ///
    /// INFERRED, not documented. genvdp.txt section 10 counts the 40 words and
    /// section 17 "$0B" names the mode bit, but nothing says which word belongs
    /// to which plane; this follows the only interleave the doc does give, the
    /// horizontal one of section 13 quoted by [`Plane::scroll_word`].
    #[inline]
    fn scroll_vsram_word(self) -> usize {
        match self {
            Plane::A => 0,
            Plane::B => 1,
        }
    }
}

impl Vdp {
    /// Rows of a plane, window or sprite cell, and of the pattern one names: 8,
    /// or 16 in interlace mode 2 (genvdp.txt section 12: "A pattern is an 8x8 or
    /// 8x16 (interlace mode 2 only) pixel block", M4-20 #98).
    ///
    /// Only the height of a cell moves: [`CELL_WIDTH`] is the width in every
    /// mode, so the `8` of R16's cell counts and of the 2-cell column of a
    /// scroll value stays 8 while the cell row is walked twice as slowly.
    #[inline]
    pub(crate) fn cell_height(&self) -> usize {
        match self.interlace_mode() {
            InterlaceMode::Mode2 => CELL_ROWS_MODE2,
            InterlaceMode::Off | InterlaceMode::Mode1 => CELL_ROWS,
        }
    }

    /// The horizontal scroll value of `plane` on `line`: the 10-bit value of the
    /// table entry that R11's mode says this line reads, at
    /// [`Vdp::hscroll_base`] plus `index * HSCROLL_ENTRY` plus this plane's word.
    ///
    /// genvdp.txt section 13 "Horizontal Scrolling" gives the walk of the table
    /// as the `get_scroll` pseudocode of that section, over R11 bits 1-0
    /// (section 17 "$0B", HSCR/LSCR):
    ///
    /// - `00` full screen: one entry for the whole frame, `index = 0`;
    /// - `01` invalid, "the first eight lines": `index = line & 7`. The doc says
    ///   of it "This mode is identical to per-line scrolling, however the VDP
    ///   will only read the first sixteen entries in the scroll table for every
    ///   line of the display", and sixteen 16-bit words are exactly the eight
    ///   lines of two words that `line & 7` addresses, so its prose and its
    ///   pseudocode agree;
    /// - `10` per cell: `index = line & !7`, one entry per cell row;
    /// - `11` per line: `index = line`.
    ///
    /// `line` is the display line of [`Vdp::display_line`], so in interlace mode
    /// 2 the per-line walk reads one entry per line of the picture and the cell
    /// walk one per 16 of them. INFERRED - section 13 says `line` and no more -
    /// pinned by `interlace_mode_2_walks_the_horizontal_scroll_table_by_display_line`,
    /// and TODO(M6-06 #125) to confirm it on hardware with a scroll test ROM.
    #[inline]
    pub(crate) fn line_hscroll(&self, plane: Plane, line: usize) -> usize {
        let cell_rows = self.cell_height();
        let index = match self.hscroll_mode() {
            HScrollMode::Full => 0,
            HScrollMode::EightLines => line & (cell_rows - 1),
            HScrollMode::PerCell => line & !(cell_rows - 1),
            HScrollMode::PerLine => line,
        };
        let addr = usize::from(self.hscroll_base()) + index * HSCROLL_ENTRY + plane.scroll_word();
        usize::from(read_word(&self.vram, addr) & HSCROLL_MASK)
    }

    /// The vertical scroll value of `plane` for the 16 pixel column `column`.
    ///
    /// genvdp.txt section 17 "$0B" bit 2 gives the two modes, "full screen
    /// vertical scrolling" and "2-cell column based vertical scrolling", and
    /// section 10 gives each value as 10 bits of VSRAM:
    ///
    /// - full screen: `vsram[0]` for plane A and `vsram[1]` for plane B, so
    ///   `column` is not read and one value moves the whole plane;
    /// - 2-cell: `vsram[2 * column]` for plane A and `vsram[2 * column + 1]` for
    ///   plane B, which is what makes the 40 words of section 10 exactly the 20
    ///   [`CELL_COLUMN`] columns of an H40 line times the two planes. Nothing
    ///   clamps `column` because the line of [`WIDTH_H40`] pixels is 20 columns
    ///   wide, so the largest index is `2 * 19 + 1` = 39.
    ///
    /// Both the walk and the A/B choice are INFERRED: all section 10 says is
    /// that "The VDP has 40x10 bits of on-chip vertical scroll RAM. It is
    /// accessed as 40 16-bit words through the data port", and no other section
    /// addresses the playfield's use of them. They are pinned by the tests of #91
    /// instead of by prose.
    #[inline]
    fn column_vscroll(&self, plane: Plane, column: usize) -> usize {
        let index = if self.vscroll_per_cell_column() {
            column * 2
        } else {
            0
        } + plane.scroll_vsram_word();
        usize::from(self.vsram[index] & self.vscroll_mask())
    }

    /// The width of a vertical scroll value: 11 bits in interlace mode 2, where
    /// it counts the lines of the doubled picture, and the 10 of genvdp.txt
    /// section 10 in the other two modes (#98).
    ///
    /// TODO(M6-06 #125): the note on [`crate::Vdp::vsram`] quotes SpritesMind as
    /// "VSRAM width is 11-bit but bit0 is ignored in interlaced mode 2", which
    /// would drop the half-line step the ticket gives bit 0; a scroll test ROM is
    /// what settles the two.
    #[inline]
    fn vscroll_mask(&self) -> u16 {
        if self.interlace_mode() == InterlaceMode::Mode2 {
            VSCROLL_MASK_MODE2
        } else {
            VSCROLL_MASK
        }
    }

    /// The vertical scroll value of the partially visible 2-cell column at the
    /// left edge of the line: the `hscroll & (CELL_COLUMN - 1)` pixels before the
    /// first whole column, which hardware is said to fetch with a broken value.
    ///
    /// UNCERTAIN, and NOT hardware fact. genvdp.txt does not mention a partial
    /// column at all - it says only that the lower bits of a scroll value are a
    /// pixel offset (section 13) - and says nothing about what VSRAM word that
    /// half-fetched column is read with. The model #91 works to, and which the
    /// `h40_fetches_the_partial_left_column_with_vsram_19_and_39` test pins so
    /// that a change here cannot be quiet:
    ///
    /// - H40: both planes use `vsram[19] & vsram[39]`, the two words of
    ///   [`LEFT_COLUMN_WORDS`];
    /// - H32: 0.
    ///
    /// TODO(M6-06): verify both halves of that with the Nemesis test ROMs, and
    /// correct this function and its tests if hardware says otherwise.
    #[inline]
    fn left_column_vscroll(&self) -> usize {
        if !self.h40() {
            return 0;
        }
        let [first, second] = LEFT_COLUMN_WORDS;
        let mask = self.vscroll_mask();
        usize::from(self.vsram[first] & self.vsram[second] & mask)
    }

    /// Fill `out` with one line of `plane`: one [`u8`] per pixel, bits 5-0 the
    /// CRAM index (`palette * 16 + colour`, the 6-bit index of CRAM's 64 words,
    /// genvdp.txt section 9) and bit 6 the cell's priority flag.
    ///
    /// `line` is a line of the picture, which [`Vdp::display_line`] makes from the
    /// beam line: the same number except in interlace mode 2, where the picture is
    /// 448 lines to the beam's 224 and a cell row is 16 of them (#98).
    ///
    /// Colour 0 of any palette draws nothing (genvdp.txt section 12: "A pixel
    /// within a pattern that uses value zero isn't shown"), and a pixel that
    /// draws nothing is written as a whole zero byte: the transparent test the
    /// compositor of M4-17 (#95) applies, `index & 0x0F == 0`, is then true of
    /// it and of nothing else, and the stale priority bit of a transparent cell
    /// cannot leak into the layer above.
    ///
    /// Horizontal scroll: a pixel of the line comes from plane x
    /// `(x - hscroll) & (width * 8 - 1)` of this plane. `hscroll` is
    /// [`Vdp::line_hscroll`] of this very plane and line, and only its low three
    /// bits are a sub-cell offset, so a scroll of 8 moves the plane exactly one
    /// cell and a scroll of 3 starts the fetch three pixels into the first
    /// visible cell. genvdp.txt section 13 is clear on the wrap ("When the scroll
    /// value is larger than the width of the playfield, the display wraps
    /// horizontally") and on the size of the value, but NOT on its direction: it
    /// says only that "the lower three bits of the scroll value provide a pixel
    /// offset into each column comprised of one pattern". Subtracting the value,
    /// so that a larger one moves the plane right, is what #90 works to and what
    /// the tests here fix in. It is INFERRED, not documented.
    ///
    /// Vertical scroll: plane y is `(line + vscroll) & (height * cell_height - 1)`,
    /// where `vscroll` is [`Vdp::column_vscroll`] for the 16 pixel column the pixel
    /// is in, or [`Vdp::left_column_vscroll`] for the partial column at the left
    /// edge of the line. Adding the value, so that a larger one moves the plane up,
    /// is what #91 works to: like the horizontal scroll its direction is INFERRED,
    /// and unlike it the value comes from VSRAM 10 bits at a time - 11 in interlace
    /// mode 2 - instead of from a VRAM table. A 2-cell column can name any plane
    /// row, so `row_base` and `cell_row` belong to the run being fetched and not to
    /// the line, and one line can change plane row at every [`CELL_COLUMN`] pixels.
    ///
    /// A cell is [`CELL_WIDTH`] pixels across and [`Vdp::cell_height`] lines down -
    /// 8 by 8, or the 8 by 16 of interlace mode 2 (#98) - and both axes wrap at the
    /// plane size of R16. The fetch runs one cell of the plane at a time - eight
    /// pixels, or the remainder of one at the left edge of a line or of a 2-cell
    /// column - and allocates nothing.
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
    /// H32 too, and the columns past the active display of a line are the
    /// overscan its compositor leaves out - shadow and highlight, the window, the
    /// sprites, and any RGBA. All of that is `compose`'s, from M4-17 (#95) on.
    pub fn render_plane_line(&self, plane: Plane, line: u16, out: &mut [u8; WIDTH_H40]) {
        let len = out.len();
        self.render_plane_run(plane, usize::from(line), 0..len, 0, out);
    }

    /// [`Vdp::render_plane_line`] for part of a line only: `range` is the range of
    /// *screen* pixels to fill - the 16 pixel column the window bug re-fetches -
    /// and `plane_x_shift` is added to the plane x of every pixel of it, which is
    /// how a column is fetched from the next 2-cell column over (genvdp.txt
    /// section 17 "$11", modelled by [`crate::window`]).
    ///
    /// The vscroll lookup still keys on the screen pixel: the bug of that section
    /// moves the name table fetch alone, and nothing says it moves the VSRAM read
    /// with it. `out` is the whole line and `range` indexes it.
    pub(crate) fn render_plane_run(
        &self,
        plane: Plane,
        line: usize,
        range: core::ops::Range<usize>,
        plane_x_shift: usize,
        out: &mut [u8],
    ) {
        let base = plane.name_table(self);
        let width = usize::from(self.plane_width_cells());
        let height = usize::from(self.plane_height_cells());
        let cell_rows = self.cell_height();
        let hscroll = self.line_hscroll(plane, line);
        let columns = self.vscroll_per_cell_column();
        // The plane is `width * CELL_WIDTH` pixels wide and `height * cell_rows`
        // lines tall, and every valid size of R16 is a power of two, so masking is
        // wrapping.
        let plane_x_mask = width * CELL_WIDTH - 1;
        let plane_y_mask = height * cell_rows - 1;
        // Width of the partial column at the left edge: 0 when there are no 2-cell
        // columns, or when the horizontal scroll lands on a column boundary and
        // the whole of the first column is visible.
        let left_partial = if columns {
            hscroll & (CELL_COLUMN - 1)
        } else {
            0
        };

        let mut x = range.start;
        let end = range.end;
        while x < end {
            let partial = x < left_partial;
            let vscroll = if partial {
                self.left_column_vscroll()
            } else {
                self.column_vscroll(plane, x / CELL_COLUMN)
            };
            // The first x where `vscroll` no longer applies: a fetch must not run
            // into the next column, which may name another plane row.
            let ends_at = if partial {
                left_partial
            } else if columns {
                x + CELL_COLUMN - x % CELL_COLUMN
            } else {
                end
            };
            let plane_y = line.wrapping_add(vscroll) & plane_y_mask;
            let row_base = (plane_y / cell_rows) * width;
            let cell_row = plane_y % cell_rows;

            let plane_x = (x + plane_x_shift).wrapping_sub(hscroll) & plane_x_mask;
            let offset = plane_x % CELL_WIDTH;
            let run = (CELL_WIDTH - offset).min(ends_at - x).min(end - x);
            let entry_index = (row_base + plane_x / CELL_WIDTH) & (MAX_ENTRIES - 1);
            self.fill_cell_run(base, entry_index, cell_row, offset, &mut out[x..x + run]);
            x += run;
        }
    }

    /// Fill `pixels` with the layer values of the cell that the entry at
    /// `base + entry_index * 2` names: the fetch every reader of a name table -
    /// the two scroll planes and the window alike - ends with. `row` is the row of
    /// the pattern to draw, `0..Vdp::cell_height()` of them, and `offset` how many
    /// pixels of it to skip, so `pixels.len()` never exceeds [`CELL_WIDTH`] -
    /// `offset`.
    ///
    /// A pixel of colour 0 draws nothing (genvdp.txt section 12: "A pixel within
    /// a pattern that uses value zero isn't shown") and is written as a whole zero
    /// byte; see [`Vdp::render_plane_line`] for why that is a byte and not a flag.
    pub(crate) fn fill_cell_run(
        &self,
        base: usize,
        entry_index: usize,
        row: usize,
        offset: usize,
        pixels: &mut [u8],
    ) {
        let entry = NameEntry::decode(read_word(&self.vram, base + entry_index * 2));
        let indices = tile_row(
            &self.vram,
            entry.tile,
            row,
            entry.vflip,
            entry.hflip,
            self.cell_height(),
        );
        let layer_base = (entry.palette << 4) | (u8::from(entry.priority) << PRIORITY_SHIFT);
        for (pixel, index) in pixels.iter_mut().zip(indices[offset..].iter().copied()) {
            *pixel = if index == 0 { 0 } else { layer_base | index };
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
    /// Horizontal scroll table base of the scroll tests: R13 = `0x3F` is 0xFC00,
    /// clear of the pattern and name tables the plane tests use.
    pub(crate) const HSCROLL_BASE: usize = 0xFC00;
    /// Cell columns and cell rows of the 32x32 playfield of [`plane_vdp`].
    pub(crate) const PLANE_COLUMNS: usize = 32;
    pub(crate) const PLANE_ROWS: usize = 32;
    /// Rows of a plane pattern, so the number of [`fill_tile_row`] calls that
    /// make one solid tile.
    pub(crate) const CELL_TILE_ROWS: usize = 8;

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

    /// A [`plane_vdp`] with the horizontal scroll table at [`HSCROLL_BASE`] and
    /// R11 bits 1-0 (the scroll mode) set to `mode`.
    pub(crate) fn scroll_vdp(mode: u8) -> Vdp {
        let mut vdp = plane_vdp();
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, mode);
        vdp
    }

    /// The horizontal scroll entry of `index`: plane A at `base + index * 4`,
    /// plane B at the next word (genvdp.txt section 13 "Horizontal Scrolling":
    /// "Scroll values for planes A and B are stored in an interleaved fashion").
    pub(crate) fn put_scroll(vdp: &mut Vdp, index: usize, plane_a: u16, plane_b: u16) {
        put_word(vdp, HSCROLL_BASE + index * 4, plane_a);
        put_word(vdp, HSCROLL_BASE + index * 4 + 2, plane_b);
    }

    /// The whole 32x32 plane of the table at `base` as one distinct colour per
    /// cell column: every cell of column `column` names tile `column + 1`, and
    /// that tile is solid [`column_colour`] in all 8 of its rows. Filled over all
    /// 32 cell rows, so any display line of the plane reads a cell and the
    /// colour of a pixel says which column of the plane it came from.
    pub(crate) fn fill_columns(vdp: &mut Vdp, base: usize) {
        for column in 0..PLANE_COLUMNS {
            let tile = (column + 1) as u16;
            for row in 0..PLANE_ROWS {
                put_entry(vdp, base, row * PLANE_COLUMNS + column, tile);
            }
            for row in 0..CELL_TILE_ROWS {
                fill_tile_row(vdp, tile, row, column_colour(column));
            }
        }
    }

    /// The colour [`fill_columns`] gives cell column `column`: 1..=15, so no
    /// column is transparent and no two neighbouring columns match. The plane is
    /// 32 columns and 15 does not divide 32, so the wrap at column 32/0 differs
    /// too (`column_colour(31)` is 2, `column_colour(0)` is 1).
    pub(crate) fn column_colour(column: usize) -> u8 {
        (column % 15 + 1) as u8
    }

    /// The whole 32x32 plane of the table at `base` as one distinct colour per
    /// cell row: every cell of row `row` names tile `row + 1`, and that tile is
    /// solid [`row_colour`] in all 8 of its rows. Filled over all 32 cell
    /// columns, so any display line of the plane reads a cell and the colour of a
    /// pixel says which cell row of the plane it came from - which is what a
    /// vertical scroll test needs to see.
    ///
    /// This uses the same 32 pattern tiles as [`fill_columns`], so one VDP is
    /// filled with one of the two and never both.
    pub(crate) fn fill_rows(vdp: &mut Vdp, base: usize) {
        for row in 0..PLANE_ROWS {
            let tile = (row + 1) as u16;
            for column in 0..PLANE_COLUMNS {
                put_entry(vdp, base, row * PLANE_COLUMNS + column, tile);
            }
            for tile_row in 0..CELL_TILE_ROWS {
                fill_tile_row(vdp, tile, tile_row, row_colour(row));
            }
        }
    }

    /// The colour [`fill_rows`] gives cell row `row`: 1..=15, so no row is
    /// transparent. 15 does not divide 32, so neighbouring rows never match and
    /// the wrap at cell row 32/0 differs too.
    pub(crate) fn row_colour(row: usize) -> u8 {
        (row % 15 + 1) as u8
    }

    /// One row of `tile`, filled with the colour index `colour`.
    pub(crate) fn fill_tile_row(vdp: &mut Vdp, tile: u16, row: usize, colour: u8) {
        put_tile_row(vdp, tile, row, &[colour; 8]);
    }

    /// [`fill_tile_row`] for a pattern of `height` rows: the 8x16 cells of
    /// interlace mode 2 (genvdp.txt section 12) hold their rows at the stride
    /// [`tile_addr`] gives from the height, which is the one difference from the
    /// 8x8 pattern of [`put_tile_row`].
    pub(crate) fn fill_tile_row_of_height(
        vdp: &mut Vdp,
        tile: u16,
        row: usize,
        colour: u8,
        height: usize,
    ) {
        put_tile_row_of_height(vdp, tile, row, height, &[colour; 8]);
    }

    /// One row of `tile`, set to 8 colour indices: the four bytes of a pattern
    /// row (genvdp.txt section 12) with the high nibble of each byte the left
    /// pixel, the convention of [`crate::pattern::tile_row`] and INFERRED there.
    pub(crate) fn put_tile_row(vdp: &mut Vdp, tile: u16, row: usize, pixels: &[u8; 8]) {
        put_tile_row_of_height(vdp, tile, row, CELL_TILE_ROWS, pixels);
    }

    /// [`put_tile_row`] for a pattern of `height` rows, the address of which
    /// [`tile_addr`] decides.
    pub(crate) fn put_tile_row_of_height(
        vdp: &mut Vdp,
        tile: u16,
        row: usize,
        height: usize,
        pixels: &[u8; 8],
    ) {
        let mask = VRAM_SIZE - 1;
        let start = tile_addr(tile, height) + row * 4;
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

    /// The 8 pixels `out` must hold at screen x 0 when the fetch starts at plane
    /// x `plane_x`, on the 32-column plane of [`fill_columns`]: the plane wraps
    /// at its own width of 256 pixels, so plane x 256 is plane x 0.
    fn expect_from(plane_x: usize) -> [u8; 8] {
        std::array::from_fn(|i| column_colour(((plane_x + i) % 256) / 8))
    }

    /// The scroll tests all read a plane whose first cell row is one colour per
    /// cell column, and assert where the scroll puts plane x 0.
    fn scrolled_plane(mode: u8) -> Vdp {
        let mut vdp = scroll_vdp(mode);
        fill_columns(&mut vdp, PLANE_A_BASE);
        vdp
    }

    /// The whole line [`fill_columns`] draws when the fetch is scrolled by
    /// `scroll`: screen x shows plane x `(x - scroll) & 255`, the rule of
    /// [`Vdp::render_plane_line`] and of genvdp.txt section 13 "When the scroll
    /// value is larger than the width of the playfield, the display wraps
    /// horizontally".
    fn expected_line(scroll: usize) -> [u8; 320] {
        std::array::from_fn(|x| column_colour((x + 256 - scroll) % 256 / 8))
    }

    /// The scroll value `out` was drawn with, told apart over the whole line:
    /// [`column_colour`] repeats every 15 columns, so 8 pixels of one colour
    /// would also be cell 15 or cell 30.
    fn scroll_of(out: &[u8; 320]) -> usize {
        (0..256)
            .find(|&scroll| out == &expected_line(scroll))
            .expect("the line is some scroll of the fill_columns plane")
    }

    /// Full-screen mode (`00`) reads one entry, and a scroll of 8 is a whole
    /// cell: what was at plane x 0 is at screen x 8, and the 8 pixels before it
    /// are the end of the plane (genvdp.txt section 13 "Horizontal Scrolling":
    /// "The upper seven bits provide an offset into each column of the name
    /// table" and "When the scroll value is larger than the width of the
    /// playfield, the display wraps horizontally").
    #[test]
    fn full_scroll_of_8_moves_plane_a_one_cell_right() {
        let mut vdp = scrolled_plane(0b00);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            scroll_of(&out),
            0,
            "an all-zero entry leaves the plane where it was"
        );

        put_scroll(&mut vdp, 0, 8, 0);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[8..16],
            &[column_colour(0); 8],
            "screen pixel 8 shows what was at plane x 0"
        );
        assert_eq!(
            &out[0..8],
            &[column_colour(31); 8],
            "and screen pixel 0 shows the end of the plane, one cell before its start"
        );
        assert_eq!(out, expected_line(8), "the whole line is shifted");
    }

    /// A scroll of 3 is not a whole cell, so the fetch starts mid-tile: 3 pixels
    /// of the last cell of the plane, then the 5 remaining pixels of cell 0,
    /// then 3 pixels of cell 1 at screen x 8 (genvdp.txt section 13: "The lower
    /// three bits of the scroll value provide a pixel offset into each column
    /// comprised of one pattern").
    #[test]
    fn full_scroll_of_3_fetches_plane_a_mid_cell() {
        let mut vdp = scrolled_plane(0b00);
        put_scroll(&mut vdp, 0, 3, 0);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[3..11],
            &expect_from(0),
            "screen pixel 3 shows plane x 0, and the 8 pixels of cell 0 run over a cell boundary on the screen"
        );
        assert_eq!(
            &out[0..3],
            &[column_colour(31); 3],
            "the 3 pixels before it are the last 3 of the plane"
        );
        assert_eq!(
            out,
            expected_line(3),
            "and every cell of the line is fetched 3 pixels late"
        );
    }

    /// 0x3FF is -1 of the 10-bit scroll value, so screen x 0 shows plane x 1: a
    /// 32-column plane wraps at its own 256 pixels, not at 1024, and the top 6
    /// bits of the entry word are ignored (genvdp.txt section 13: "x = Horizontal
    /// scroll value (0-3FFh)", "Bits D15 through D10 are ignored by the VDP").
    #[test]
    fn full_scroll_of_0x3ff_shows_plane_x_1_at_screen_x_0() {
        let mut vdp = scrolled_plane(0b00);
        put_scroll(&mut vdp, 0, 0x3FF, 0);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &expect_from(1),
            "plane x 1 at screen x 0: 7 pixels of cell 0, then cell 1 one pixel early"
        );

        // Bits 15-10 of the word are ignored, so this is the same scroll value.
        put_scroll(&mut vdp, 0, 0xFC00 | 0x3FF, 0);
        let mut same = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut same);
        assert_eq!(same, out, "only the low 10 bits of the entry are read");
    }

    /// Per-line mode (`11`) reads `base + line * 4`, so line 5 and line 6 move
    /// differently (genvdp.txt section 13 "Horizontal Scrolling", the `case 0x03`
    /// of its `get_scroll` pseudocode).
    #[test]
    fn per_line_mode_reads_the_entry_of_its_own_line() {
        let mut vdp = scrolled_plane(0b11);
        put_scroll(&mut vdp, 5, 8, 0);
        put_scroll(&mut vdp, 6, 16, 0);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 5, &mut out);
        assert_eq!(scroll_of(&out), 8, "line 5 uses the entry of index 5");

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 6, &mut out);
        assert_eq!(
            scroll_of(&out),
            16,
            "line 6 uses the different entry of index 6, not index 5"
        );
    }

    /// Per-cell mode (`10`) reads `base + (line & !7) * 4`, so every line of one
    /// cell row shares a scroll value: lines 8..15 all use the entry of index 8
    /// (genvdp.txt section 13, the `case 0x02` of its pseudocode, "Every row").
    #[test]
    fn per_cell_mode_shares_one_entry_over_eight_lines() {
        let mut vdp = scrolled_plane(0b10);
        put_scroll(&mut vdp, 1, 3, 0);
        put_scroll(&mut vdp, 8, 8, 0);

        for line in 8..16 {
            let mut out = blank_line();
            vdp.render_plane_line(Plane::A, line, &mut out);
            assert_eq!(
                scroll_of(&out),
                8,
                "line {line} uses the entry of index 8, not the one of index {} that a numbered read would pick",
                line / 8
            );
        }

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 7, &mut out);
        assert_eq!(
            scroll_of(&out),
            0,
            "the cell row above still uses its own entry, index 0"
        );
    }

    /// The invalid mode `01` is per-line scrolling over the first eight entries
    /// only, which is `base + (line & 7) * 4` (genvdp.txt section 13: "This mode
    /// is identical to per-line scrolling, however the VDP will only read the
    /// first sixteen entries in the scroll table for every line of the display"
    /// - sixteen 16-bit words are the 8 lines of 2 words this mode addresses).
    #[test]
    fn invalid_mode_01_repeats_the_first_eight_entries() {
        let mut vdp = scrolled_plane(0b01);
        for index in 0..8 {
            put_scroll(&mut vdp, index, (8 * (index + 1)) as u16, 0);
        }
        // The entry a per-line read would take for lines 8..16, and a read that
        // mistook this mode for per-cell would take for lines 8..15.
        put_scroll(&mut vdp, 8, 96, 0);

        for line in 0..16u16 {
            let mut out = blank_line();
            vdp.render_plane_line(Plane::A, line, &mut out);
            assert_eq!(
                scroll_of(&out),
                8 * (usize::from(line) % 8 + 1),
                "line {line} uses the entry of index {}",
                line % 8
            );
        }
    }

    /// Plane B takes the second word of the entry, plane A the first, and each
    /// plane moves by its own value while reading the same table
    /// (genvdp.txt section 13: "Entry #0 of the table is for plane A, entry #1 is
    /// for plane B, and this repeats for the entire length of the table").
    #[test]
    fn plane_b_uses_the_second_word_of_the_scroll_entry() {
        let mut vdp = scroll_vdp(0b00);
        fill_columns(&mut vdp, PLANE_A_BASE);
        fill_columns(&mut vdp, PLANE_B_BASE);
        put_scroll(&mut vdp, 0, 0, 8);

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            scroll_of(&out),
            0,
            "plane A reads the first word of the entry, which is zero here"
        );

        let mut out = blank_line();
        vdp.render_plane_line(Plane::B, 0, &mut out);
        assert_eq!(
            scroll_of(&out),
            8,
            "plane B reads the second word of the entry, which is 8"
        );
    }

    /// A [`plane_vdp`] with the horizontal scroll table at [`HSCROLL_BASE`] and
    /// `r11` in R11, and plane A filled by [`fill_rows`] so that the colour of a
    /// pixel says which cell row of the plane it came from.
    fn vscrolled_plane(r11: u8) -> Vdp {
        let mut vdp = scroll_vdp(r11);
        fill_rows(&mut vdp, PLANE_A_BASE);
        vdp
    }

    /// Plane A with cell (0,0) naming tile 1 (all 8 rows colour 1) and cell
    /// (0,1) naming tile 2 (row 0 colour 2, every other row colour 3): the top
    /// row of the second cell has a colour of its own, so a line that shows it
    /// cannot be confused with the cell row above, the cell row below, or the
    /// same cell read at the wrong tile row.
    fn plane_a_two_cell_rows() -> Vdp {
        let mut vdp = plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, 1);
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 1, row, 1);
        }
        put_entry(&mut vdp, PLANE_A_BASE, PLANE_COLUMNS, 2);
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 2, row, if row == 0 { 2 } else { 3 });
        }
        vdp
    }

    /// The 32 cell columns of cell row 0 naming tile 1, whose 8 rows are the 8
    /// different colours 1..=8: the colour of a pixel then says which row of the
    /// tile it was fetched from, so a vscroll of 3 shows colour 4 and a vscroll
    /// of 0 colour 1. Every other cell row is the empty entry, drawing nothing,
    /// so a vscroll that lands on another cell row shows colour 0.
    fn plane_a_row_gradient() -> Vdp {
        let mut vdp = plane_vdp();
        for column in 0..PLANE_COLUMNS {
            put_entry(&mut vdp, PLANE_A_BASE, column, 1);
        }
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 1, row, (row + 1) as u8);
        }
        vdp
    }

    /// R11 bit 2 clear is "full screen vertical scrolling" (genvdp.txt section
    /// 17 "$0B") and section 10 gives the value as "y = Vertical scroll factor
    /// (0-3FFh)": one value for the whole plane, `vsram[0]` for plane A. A value
    /// of 8 is one whole cell row, so line 0 shows what was on line 8 - plane y
    /// `line + vscroll`, the direction #91 works to; like the horizontal one of
    /// [`Vdp::render_plane_line`] it is INFERRED, not documented.
    #[test]
    fn full_vscroll_of_8_shows_plane_row_8_on_line_0() {
        let mut vdp = plane_a_two_cell_rows();

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[1; 8],
            "with no vertical scroll line 0 still shows the top cell row"
        );

        vdp.vsram[0] = 8;
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[2; 8],
            "vscroll 8: line 0 shows plane y 8, which is tile row 0 of cell row 1"
        );

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 1, &mut out);
        assert_eq!(
            &out[0..8],
            &[3; 8],
            "line 1 is the row below it, so the scroll moved plane y and not the tile row"
        );
    }

    /// A 32-cell plane is 256 lines tall, so the 10-bit vscroll of 0x3FF is -1
    /// and line 0 shows plane y 255: the plane wraps at its own height, exactly
    /// as it wraps at its own width (genvdp.txt section 13 says the wrap for the
    /// horizontal case; the vertical one is INFERRED from it and from R16).
    #[test]
    fn vscroll_of_0x3ff_wraps_a_32_cell_plane_to_plane_y_255() {
        let mut vdp = plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 31 * PLANE_COLUMNS, 1);
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 1, row, if row == 7 { 4 } else { 5 });
        }

        vdp.vsram[0] = 0x3FF;
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[4; 8],
            "plane y 255 of a 256-line plane: cell row 31, tile row 7"
        );

        // The 11th bit of a VSRAM word belongs to interlace mode 2 alone (see
        // the note on [`crate::Vdp::vsram`), so it is not part of the scroll.
        vdp.vsram[0] = 0x400 | 0x3FF;
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[4; 8],
            "only the 10 scroll bits of genvdp.txt section 10 are read outside interlace mode 2"
        );
    }

    /// Full mode reads `vsram[0]` for plane A and `vsram[1]` for plane B: the
    /// same A/B interleave genvdp.txt section 13 records for the horizontal
    /// table ("Entry #0 of the table is for plane A, entry #1 is for plane B"),
    /// which is INFERRED for VSRAM as section 10 gives the words but not who
    /// reads them.
    #[test]
    fn plane_b_takes_its_vertical_scroll_from_vsram_1() {
        let mut vdp = plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, 1);
        put_entry(&mut vdp, PLANE_A_BASE, PLANE_COLUMNS, 2);
        fill_tile_row(&mut vdp, 1, 0, 1);
        fill_tile_row(&mut vdp, 2, 0, 2);
        put_entry(&mut vdp, PLANE_B_BASE, 0, 3);
        put_entry(&mut vdp, PLANE_B_BASE, PLANE_COLUMNS, 4);
        fill_tile_row(&mut vdp, 3, 0, 5);
        fill_tile_row(&mut vdp, 4, 0, 6);

        vdp.vsram[1] = 8;
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[1; 8],
            "plane A reads vsram[0], which is still 0, so it has not moved"
        );

        let mut out = blank_line();
        vdp.render_plane_line(Plane::B, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[6; 8],
            "plane B reads vsram[1] = 8, so its line 0 is the second cell row"
        );

        let mut out = blank_line();
        vdp.render_plane_line(Plane::B, 8, &mut out);
        assert_eq!(
            out[0], 0,
            "line 8 is then the third cell row, which is empty: the scroll is added once, not instead of the line"
        );
    }

    /// R11 bit 2 set is "2-cell column based vertical scrolling" (genvdp.txt
    /// section 17 "$0B"): one VSRAM word per 16 pixel column of the line, plane A
    /// at `vsram[2 * column]` and plane B at `vsram[2 * column + 1]`.
    ///
    /// genvdp.txt says none of that - section 10 counts 40 words, section 17
    /// names the bit, and nothing between them describes how the words are laid
    /// out. The `2 * column` walk and the A/B interleave are INFERRED from the
    /// two facts and from the horizontal table's "Scroll values for planes A and
    /// B are stored in an interleaved fashion"; 40 words is exactly the 20
    /// columns of an H40 line times the two planes.
    #[test]
    fn two_cell_mode_gives_each_16_pixel_column_its_own_vscroll() {
        let mut vdp = vscrolled_plane(0x04);
        vdp.vsram[0] = 0;
        vdp.vsram[2] = 8;
        vdp.vsram[4] = 16;
        // The words of plane B, none of which may move plane A.
        vdp.vsram[1] = 24;
        vdp.vsram[3] = 40;
        vdp.vsram[5] = 56;

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..16],
            &[row_colour(0); 16],
            "column 0 reads vsram[0] = 0, and 8 pixels is not a column"
        );
        assert_eq!(
            &out[16..32],
            &[row_colour(1); 16],
            "column 1 reads vsram[2] = 8: a whole cell row down, from pixel 16"
        );
        assert_eq!(
            &out[32..48],
            &[row_colour(2); 16],
            "column 2 reads vsram[4] = 16, neither plane B's vsram[3] nor the column before it"
        );
        assert_eq!(
            &out[48..],
            &[row_colour(0); 272],
            "every column past vsram[4] still has a scroll of 0"
        );
    }

    /// The 16 pixel column is the *screen* column and hscroll only moves the
    /// fetch: with an hscroll of 16 the split between `vsram[0]` and `vsram[2]`
    /// is still at pixel 16, where the screen column changes, not at pixel 32
    /// where the plane's own 16 pixel column changes. INFERRED - genvdp.txt never
    /// says which x picks the VSRAM word - and pinned here so that a change of
    /// model is loud.
    ///
    /// TODO(M6-06): verify against the Nemesis scroll tests, together with the
    /// direction of the scroll itself.
    #[test]
    fn two_cell_columns_are_screen_columns_under_hscroll() {
        let mut vdp = vscrolled_plane(0x04);
        put_scroll(&mut vdp, 0, 16, 0);
        vdp.vsram[0] = 0;
        vdp.vsram[2] = 8;

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..16],
            &[row_colour(0); 16],
            "pixels 0..16 are screen column 0, which is vsram[0], even though the fetch of pixel 0 is 16 pixels into the plane"
        );
        assert_eq!(
            &out[16..32],
            &[row_colour(1); 16],
            "and the vscroll changes at pixel 16, not where plane x crosses a 16 pixel boundary"
        );

        // An hscroll of 4 puts the plane cells astride the 16 pixel boundary, so
        // the fetch of a cell has to stop at the column edge: the plane row of
        // the two columns is all that changes there.
        put_scroll(&mut vdp, 0, 4, 0);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..16],
            &[row_colour(0); 16],
            "the 4 partial pixels and the 12 that end screen column 0 both read vsram[0]"
        );
        assert_eq!(
            &out[16..32],
            &[row_colour(1); 16],
            "column 1 still starts at pixel 16, a third of the way into a plane cell"
        );
    }

    /// The left column quirk of the ticket, which is a model of undocumented
    /// hardware and not a fact: see [`Vdp::left_column_vscroll`]. H40, 2-cell
    /// mode, an hscroll of 4 so that only 4 pixels of the first column are
    /// visible, and `vsram[19] = 0x0F` and `vsram[39] = 0xF3`.
    #[test]
    fn h40_fetches_the_partial_left_column_with_vsram_19_and_39() {
        let mut vdp = plane_a_row_gradient();
        vdp.write_register(12, 0x81);
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, 0x04);
        put_scroll(&mut vdp, 0, 4, 0);
        vdp.vsram[19] = 0x0F;
        vdp.vsram[39] = 0xF3;

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..4],
            &[4; 4],
            "the first 4 pixels - 4 and 15 is 4 - are at plane y 0x0F & 0xF3 = 3, which is tile row 3"
        );
        assert_eq!(
            &out[4..16],
            &[1; 12],
            "from pixel 4 the column's own value, vsram[0] = 0, applies again"
        );
        assert_eq!(
            &out[16..],
            &[1; 304],
            "and no other column of plane A is affected: the odd VSRAM words belong to plane B"
        );

        put_scroll(&mut vdp, 0, 16, 0);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[1; 8],
            "an hscroll that is a multiple of 16 has no partial column, so vsram[19] and vsram[39] are not read"
        );
    }

    /// The other half of that model: an H32 display fetches its partial left
    /// column with a vscroll of 0 rather than with the two words, and full screen
    /// mode has no 2-cell column to fetch wrong at all.
    #[test]
    fn h32_and_full_mode_fetch_the_partial_left_column_with_a_scroll_of_0() {
        let mut vdp = plane_a_row_gradient();
        vdp.write_register(13, 0x3F);
        vdp.write_register(11, 0x04);
        put_scroll(&mut vdp, 0, 4, 0);
        vdp.vsram[19] = 0x0F;
        vdp.vsram[39] = 0xF3;

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..4],
            &[1; 4],
            "H32: the partial column is at plane y 0, not at the H40 value of 3"
        );

        vdp.write_register(11, 0x00);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..4],
            &[1; 4],
            "full screen mode has no 2-cell column, so the two words are not read there either"
        );

        // H40, where the two words do say something, and full screen mode, where
        // there is no partial column to read them with.
        vdp.write_register(12, 0x81);
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..4],
            &[1; 4],
            "H40 in full screen mode: vsram[19] & vsram[39] is 3, and plane y stays 0"
        );
    }

    // ---------------------------------------------------------------------
    // Interlace mode 2 (M4-20 #98): the 8x16 cell and the 11-bit vscroll.
    // ---------------------------------------------------------------------

    /// A plane pattern row of interlace mode 2, filled with `colour`.
    const MODE2_ROWS: usize = 16;
    /// A colour of [`crate::pattern`] patterns: 7, since colour 0 is transparent
    /// and every other index of a palette line would do as well.
    const COLOUR: u8 = 7;

    /// [`plane_vdp`] switched to interlace mode 2, R12 LSM = `11` (genvdp.txt
    /// section 17 "$0C"): the mode whose plane cells are 8x16 and whose vertical
    /// scroll counts the 448 lines of the picture rather than the 224 of the beam.
    fn interlaced_plane_vdp() -> Vdp {
        let mut vdp = plane_vdp();
        vdp.write_register(12, 0x06);
        vdp
    }

    /// `tile` as a 16-row pattern, one [`row_colour`] per row: every row of an
    /// interlace mode 2 cell a different colour, so the row a pixel came from is
    /// readable from the pixel itself.
    fn fill_cell_16(vdp: &mut Vdp, tile: u16) {
        for row in 0..MODE2_ROWS {
            fill_tile_row_of_height(vdp, tile, row, row_colour(row), MODE2_ROWS);
        }
    }

    /// Interlace mode 2: a plane cell is 8x16, so a plane row takes 16 display
    /// lines and the pattern it names is 64 bytes at `(name & !1) * 32`
    /// (genvdp.txt sections 12 and 13). Cell (0,0) names tile 3 and its rows are
    /// written at 64, the address of tile 2, so a fetch that used `name * 32` or
    /// `tile * 64` reads nothing here; the cell row under it is the empty entry,
    /// so a fetch that kept the 8-line cell reads nothing either.
    #[test]
    fn interlace_mode_2_reads_an_8x16_cell_from_the_even_name() {
        let mut vdp = interlaced_plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, palette_entry(2, 3));
        fill_cell_16(&mut vdp, 3);

        for line in 0..MODE2_ROWS {
            let mut out = blank_line();
            vdp.render_plane_line(Plane::A, line as u16, &mut out);
            assert_eq!(
                &out[0..8],
                &[cram_index(2, row_colour(line)); 8],
                "display line {line} of cell (0,0) is pattern row {line} of 16"
            );
        }

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, MODE2_ROWS as u16, &mut out);
        assert_eq!(
            &out[0..8],
            &[0; 8],
            "line 16 is the next cell row, which is the empty entry"
        );
    }

    /// Interlace mode 2 vertical scroll counts the lines of the picture, so a
    /// vscroll of 1 moves the plane by one row of a 16-line cell and a vscroll of
    /// 16 by a whole cell (ticket #98: "Vertical scroll is 11 bits and in half
    /// lines"). Bit 0 of the value therefore moves the plane like any other bit,
    /// which is what the SpritesMind note that "bit0 is ignored in interlaced mode
    /// 2" would have done differently; the ticket decides it.
    #[test]
    fn interlace_mode_2_vertical_scroll_counts_the_lines_of_the_picture() {
        let mut vdp = interlaced_plane_vdp();
        put_entry(&mut vdp, PLANE_A_BASE, 0, palette_entry(2, 3));
        fill_cell_16(&mut vdp, 3);

        vdp.vsram[0] = 1;
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[cram_index(2, row_colour(1)); 8],
            "vscroll 1 shows pattern row 1 of the cell at plane y 0"
        );

        vdp.vsram[0] = MODE2_ROWS as u16;
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[0; 8],
            "16 of them is the cell row under it, the empty entry"
        );
    }

    /// The 11th bit of genvdp.txt section 10's "40x10 bits of on-chip vertical
    /// scroll RAM" belongs to interlace mode 2 alone (see the note on
    /// [`crate::Vdp::vsram`]). A 128-cell plane of 16-line cells is 2048 lines
    /// tall, so a vscroll of `0x400` is 64 whole cell rows and reaches the one
    /// cell this plane names.
    #[test]
    fn interlace_mode_2_vertical_scroll_is_eleven_bits_wide() {
        let mut vdp = interlaced_plane_vdp();
        vdp.write_register(16, 0x30); // 32 cells wide, 128 cells tall
        put_entry(
            &mut vdp,
            PLANE_A_BASE,
            64 * PLANE_COLUMNS,
            palette_entry(2, 3),
        );
        for row in 0..MODE2_ROWS {
            fill_tile_row_of_height(&mut vdp, 3, row, COLOUR, MODE2_ROWS);
        }

        vdp.vsram[0] = 0x400;
        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[0..8],
            &[cram_index(2, COLOUR); 8],
            "plane y 1024 of a 2048-line plane is cell row 64, the only cell that draws"
        );
    }

    /// INFERRED, and pinned so that a change of reading cannot be quiet: the
    /// horizontal scroll table is walked with the display line, the same doubled
    /// number the name table fetch and the vscroll use, so the two display lines
    /// of one beam line read two different entries of a per-line table. Nothing in
    /// genvdp.txt addresses which line the scroll table counts - section 13 gives
    /// the walk as `index = line` and stops there.
    #[test]
    fn interlace_mode_2_walks_the_horizontal_scroll_table_by_display_line() {
        let mut vdp = interlaced_plane_vdp();
        vdp.write_register(11, 0x03); // per-line horizontal scroll
        vdp.write_register(13, 0x3F); // the table of `put_scroll`, at HSCROLL_BASE
        put_scroll(&mut vdp, 0, 8, 0);
        put_scroll(&mut vdp, 1, 0, 0);

        // One colour per cell column, as `fill_columns` does it but at the 8x16
        // address of interlace mode 2, and naming the even tiles so that no two
        // columns share a pattern once bit 0 of the name is dropped.
        for column in 0..PLANE_COLUMNS {
            let tile = 2 * (column as u16 + 1);
            for row in 0..PLANE_ROWS {
                put_entry(&mut vdp, PLANE_A_BASE, row * PLANE_COLUMNS + column, tile);
            }
            for row in 0..MODE2_ROWS {
                fill_tile_row_of_height(&mut vdp, tile, row, column_colour(column), MODE2_ROWS);
            }
        }

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 0, &mut out);
        assert_eq!(
            &out[..8],
            &[column_colour(1); 8],
            "display line 0 reads scroll entry 0, which scrolls plane A by 8"
        );

        let mut out = blank_line();
        vdp.render_plane_line(Plane::A, 1, &mut out);
        assert_eq!(
            &out[..8],
            &[column_colour(0); 8],
            "and display line 1 reads scroll entry 1, which does not"
        );
    }
}
