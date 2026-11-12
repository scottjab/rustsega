//! Per-line latching of the scroll tables and the mode registers (M4-23, #102).
//!
//! A raster effect written in an HINT handler rewrites the scroll tables and the
//! mode registers halfway along a line, and the VDP cannot follow it: it reads
//! the state a line is drawn with at one moment of the line and works from that
//! reading until the same moment of the next one. This module is that reading.
//! [`Vdp::latch_line_state`] takes it as `timing` walks the beam through the
//! latch point of [`Vdp::line`], and everything under [`Vdp::render_line`] reads
//! it back through [`Vdp::line_reg`], [`Vdp::latched_vsram`] and
//! [`Vdp::latched_hscroll`]. Writes still reach the live chip: the latch is a
//! read-side view of it, not a second copy of state the bus has to keep up.
//!
//! # The latch point
//!
//! The start of the line's active display: [`Vdp::hblank_clear_mclk`], the
//! front-porch point of the line that `timing` already stops the beam at (81
//! master clocks in H40, 100 in H32). One point for all three reads, because the
//! three hardware rules of ticket #102 all land near it:
//!
//! - horizontal scroll: "Horizontal scroll for line N is read from VRAM during
//!   HBlank before line N" (#102, after the plutiedev VDP timing notes), and the
//!   front porch is the last moment of that HBlank;
//! - vertical scroll: latched per line "at a position near the start of the
//!   line", which is where the VSRAM read of [`Vdp::column_vscroll`] sits;
//! - mode registers: a write lands "immediately for the rest of the line" as far
//!   as the bus is concerned, the renderer is per line, and the ticket picks
//!   "register values at the start of the line's active display".
//!
//! The per-line access pattern of the same notes, `Hssss AsaaBsbb ((A~aaBSbb)*3
//! AraaBSbb)*5 ~~ s*23 ~ s*11` for H40 with `s` a scroll fetch, puts scroll
//! fetches in both blanking halves of the line - five pixels after HSYNC and a
//! block of twenty-three before the back porch - so where exactly the value the
//! line ends up with is sampled is INFERRED, and this model says "at the front
//! porch of its own line" rather than the ticket's other candidate, the back
//! porch of the line before. The two differ only for a write in the first 81
//! clocks of an H40 line, or the first 100 of an H32 one.
//!
//! Sprites are latched here too, but one line earlier and in another place: the
//! sprite list of a line is evaluated during the line before, so
//! [`Vdp::search_sprites`] walks the table at this same point and leaves what it
//! read in [`Vdp::sprite_lines`] for the line after this one to draw (#311). It is
//! not part of [`LineState`] because a `LineState` is read by the line it was
//! latched for, at the end of that line, and a sprite search is not read until the
//! end of the line *after* the one it ran on - one box would be refilled by the next
//! line's search before the line the first one belongs to was ever drawn. The cache
//! of the table's first four bytes per entry, `crate::sprites::SpriteCache` (#103),
//! stays chip state and keeps its semantics: the search reads it, keeps what it
//! read, and still leaves the pattern and the x of an entry to the fetch of the line
//! that draws them.
//!
//! TODO(#102): pin the latch point with a scroll test ROM (M6-06), including
//! whether a write inside that first-blanking window reaches the line it is
//! written on; `latch_point` below names the clock so there is one place to
//! change.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::planes::Plane;
use crate::{VSRAM_WORDS, Vdp, regs};

/// The state one line of the picture is drawn from: what [`Vdp::latch_line_state`]
/// reads at the latch point and [`Vdp::render_line`] works from.
///
/// Copy fields throughout, rewritten once a line by [`Vdp::latch_line_state`]:
/// 24 bytes of register file, 80 of VSRAM and the two horizontal scroll values
/// of the line. The box it lives in is allocated by the first latch of a chip
/// and reused by every one after it, so a line costs no allocation
/// (docs/ARCHITECTURE.md: "Core crates must not allocate per frame in hot
/// paths. Allocate buffers once.").
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct LineState {
    /// The beam line this was latched for: [`Vdp::line`] at the time, and how
    /// [`Vdp::begin_line`] knows a `render_line` call is drawing this line
    /// rather than some other one.
    line: u16,
    /// R0 to R23 as they stood at the latch point, which is what every register
    /// getter of the renderer answers from - see [`Vdp::line_reg`].
    regs: [u8; regs::NUM_REGISTERS],
    /// VSRAM as it stood at the latch point - see [`Vdp::latched_vsram`].
    ///
    /// Saved as a sequence: forty words is past the thirty-two entries `serde`
    /// derives, see [`crate::big_array`].
    #[cfg_attr(feature = "serde", serde(with = "crate::big_array"))]
    vsram: [u16; VSRAM_WORDS],
    /// The line's two horizontal scroll values, read out of the table at the
    /// latch point: `hscroll[0]` for plane A and `hscroll[1]` for plane B, the
    /// order genvdp.txt section 13 interleaves the table in ("Entry #0 of the
    /// table is for plane A, entry #1 is for plane B").
    hscroll: [u16; 2],
}

impl LineState {
    /// The horizontal scroll value of `plane` this line was latched with.
    #[inline]
    fn hscroll(&self, plane: Plane) -> usize {
        match plane {
            Plane::A => usize::from(self.hscroll[0]),
            Plane::B => usize::from(self.hscroll[1]),
        }
    }
}

impl Vdp {
    /// The beam has reached the latch point of [`Vdp::line`]: read the scroll
    /// tables and the register file that line will be drawn from, and walk the
    /// sprite table for the line after it.
    ///
    /// Called by `timing` at [`Vdp::hblank_clear_mclk`] of every line, the last
    /// moment of the HBlank that #102's "Horizontal scroll for line N is read
    /// from VRAM during HBlank before line N" puts the scroll read in. Live state
    /// is read here, not the latch: this is what fills it.
    ///
    /// The three reads below are of this line and go into [`LineState`];
    /// [`Vdp::search_sprites`] is of the line after this one and does not, for the
    /// reason on [`Vdp::searched_line`] (#311).
    pub(crate) fn latch_line_state(&mut self) {
        // The sprite walk of the line after this one, at the same moment and for the
        // same reason as the three reads below: the sprite list of a line is
        // evaluated during the line before (#311).
        self.search_sprites();

        let display_line = usize::from(self.display_line(self.line));
        let latch = LineState {
            line: self.line,
            regs: self.regs,
            vsram: self.vsram,
            hscroll: [
                self.hscroll_table(Plane::A, display_line) as u16,
                self.hscroll_table(Plane::B, display_line) as u16,
            ],
        };
        // The first line of a chip allocates the latch and every one after it is
        // written into the same box, so a line costs no allocation.
        if let Some(boxed) = &mut self.line_latch {
            **boxed = latch;
        } else {
            self.line_latch = Some(Box::new(latch));
        }
    }

    /// The renderer is about to draw `line`: read the register file, the VSRAM
    /// and the horizontal scroll of [`Vdp::line_latch`] until [`Vdp::end_line`].
    ///
    /// Only the latch *of `line`* is used. A `render_line` call for a line the
    /// beam never latched - the direct call docs/ARCHITECTURE.md promises tests,
    /// and a beam moved by [`Vdp::set_position`], which runs no line work - draws
    /// live state, which is what it did before #102 latched anything.
    pub(crate) fn begin_line(&mut self, line: u16) {
        self.line_drawing = self
            .line_latch
            .as_ref()
            .is_some_and(|latch| latch.line == line);
    }

    /// The line [`Vdp::begin_line`] started is drawn: the register file and
    /// VSRAM mean the chip again. Paired with it by [`Vdp::render_line`].
    pub(crate) fn end_line(&mut self) {
        self.line_drawing = false;
    }

    /// Register `index` as the code reading it now should see it: the live
    /// register file, or the [`LineState::regs`] of the line being drawn.
    ///
    /// Every register read of the crate goes through here - the writes of
    /// [`Vdp::write_register`] and of `dma` go to [`Vdp::regs`], which is what a
    /// latched read of them would have to be careful about. Nothing runs out of
    /// a data port write or a DMA while a line is being drawn, so the two never
    /// meet and one flag covers the lot.
    #[inline]
    pub(crate) fn line_reg(&self, index: usize) -> u8 {
        match &self.line_latch {
            Some(latch) if self.line_drawing => latch.regs[index],
            _ => self.regs[index],
        }
    }

    /// VSRAM as the code reading it now should see it: [`Vdp::vsram`], or the
    /// [`LineState::vsram`] of the line being drawn. [`Vdp::line_reg`] is the
    /// same rule one register at a time.
    #[inline]
    pub(crate) fn latched_vsram(&self) -> &[u16; VSRAM_WORDS] {
        match &self.line_latch {
            Some(latch) if self.line_drawing => &latch.vsram,
            _ => &self.vsram,
        }
    }

    /// The horizontal scroll of `plane` latched for the line being drawn, or
    /// `None` when there is no latched line to read: [`Vdp::hscroll_table`]
    /// reads the table live in that case, which is what a plane call from
    /// outside the beam wants.
    #[inline]
    pub(crate) fn latched_hscroll(&self, plane: Plane) -> Option<usize> {
        match &self.line_latch {
            Some(latch) if self.line_drawing => Some(latch.hscroll(plane)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Vdp;
    use crate::color::{Intensity, color};
    use crate::hv::MCLKS_PER_LINE;
    use crate::planes::testutil::{
        CELL_TILE_ROWS, PLANE_A_BASE, column_colour, fill_columns, fill_rows, fill_tile_row,
        put_scroll, put_sprite_at, put_word, row_colour, scroll_vdp,
    };
    use crate::timing::HBLANK_CLEAR_MCLK_H32;

    /// Every CRAM entry as its own entry number: the 9 bits of a CRAM word are
    /// `----bbb-ggg-rrr-`, the low bit of the word unused (genvdp.txt section 9 and
    /// [`crate::color::color_index`]), so an entry number shifted up one is six
    /// distinct colour bits and no two entries of these tests share a pixel.
    fn cram_word(entry: usize) -> u16 {
        (entry as u16) << 1 & 0x0EEE
    }

    /// [`cram_word`] in every entry of [`Vdp::cram`], so that a pixel of the
    /// frame says which CRAM entry it came from.
    fn fill_cram(vdp: &mut Vdp) {
        for (entry, word) in vdp.cram.iter_mut().enumerate() {
            *word = cram_word(entry);
        }
    }

    /// The frame pixel a CRAM entry of [`fill_cram`] arrives as.
    fn pixel_of(entry: usize) -> u32 {
        color(cram_word(entry), Intensity::Normal)
    }

    /// Row `line` of the frame buffer, as wide as the frame says.
    fn row(vdp: &Vdp, line: usize) -> Vec<u32> {
        let frame = vdp.frame();
        let start = frame.pixel_index(0, line);
        frame.pixels[start..start + frame.width].to_vec()
    }

    /// The display-on [`scroll_vdp`] of these tests: H32, 32x32 playfields, the
    /// horizontal scroll table at the base [`scroll_vdp`] sets and full screen
    /// scrolling in both axes unless a test says otherwise.
    fn running(mode: u8) -> Vdp {
        let mut vdp = scroll_vdp(mode);
        vdp.write_register(1, 0x40);
        fill_cram(&mut vdp);
        vdp
    }

    /// The row a [`fill_columns`] plane A draws when it is scrolled right by
    /// `shift`: plane x `(pixel - shift)` of a 256 pixel plane, and the colour of
    /// a pixel says which cell column of it the pixel came from.
    fn plane_columns(shift: usize) -> Vec<u32> {
        (0..256)
            .map(|x| {
                let plane_x = (x + 256 - shift) & 255;
                pixel_of(column_colour(plane_x / 8).into())
            })
            .collect()
    }

    /// The single colour a [`fill_rows`] plane A draws on picture line `line`
    /// when it is scrolled down by `shift`: plane y `(line + shift)` of a 256
    /// line plane, and the colour of a pixel says which cell row it came from.
    fn plane_rows(line: usize, shift: usize) -> Vec<u32> {
        let colour = row_colour(((line + shift) & 255) / 8);
        vec![pixel_of(colour.into()); 256]
    }

    /// #102 test 1: "Change hscroll in VRAM in the middle of line 10's active
    /// period: line 10 still uses the old value, line 11 the new one."
    #[test]
    fn a_horizontal_scroll_write_in_mid_line_shows_on_the_next_line() {
        let mut vdp = running(0b00);
        fill_columns(&mut vdp, PLANE_A_BASE);

        // Ten lines and a bit: line 10 is into its active display, so the state
        // it will be drawn from has already been read by the time of the write.
        vdp.advance(10 * MCLKS_PER_LINE);
        vdp.advance(HBLANK_CLEAR_MCLK_H32 + 100);
        assert_eq!((vdp.line, vdp.line_mclk), (10, HBLANK_CLEAR_MCLK_H32 + 100));

        put_scroll(&mut vdp, 0, 24, 0);

        vdp.advance(MCLKS_PER_LINE - vdp.line_mclk);
        assert_eq!((vdp.line, vdp.line_mclk), (11, 0), "line 10 is drawn");
        assert_eq!(row(&vdp, 10), plane_columns(0), "and with the old scroll");

        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(
            row(&vdp, 11),
            plane_columns(24),
            "line 11 reads the table again"
        );
    }

    /// The other half of the same rule: the read of the line is at the start of
    /// its active display, so a write in the front porch of that very line is
    /// still in time for it.
    #[test]
    fn a_horizontal_scroll_write_in_the_blankening_of_a_line_shows_on_that_line() {
        let mut vdp = running(0b00);
        fill_columns(&mut vdp, PLANE_A_BASE);

        vdp.advance(10 * MCLKS_PER_LINE);
        assert_eq!((vdp.line, vdp.line_mclk), (10, 0));
        put_scroll(&mut vdp, 0, 24, 0);

        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(
            row(&vdp, 10),
            plane_columns(24),
            "the line that begins with the write reads the new value"
        );
    }

    /// #102 test 2, one step harder than "between lines": R7 written halfway
    /// across a line is the backdrop of the next one, not of the line it was
    /// written on.
    #[test]
    fn a_mode_register_write_in_mid_line_shows_on_the_next_line() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(1, 0x40);
        fill_cram(&mut vdp);

        vdp.advance(5 * MCLKS_PER_LINE);
        vdp.advance(HBLANK_CLEAR_MCLK_H32 + 100);
        assert_eq!((vdp.line, vdp.line_mclk), (5, HBLANK_CLEAR_MCLK_H32 + 100));

        vdp.write_register(7, 1);

        vdp.advance(MCLKS_PER_LINE - vdp.line_mclk);
        assert_eq!(row(&vdp, 5), vec![pixel_of(0); 256], "R7 of line 5 is 0");

        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(
            row(&vdp, 6),
            vec![pixel_of(1); 256],
            "and R7 of line 6 is 1"
        );
    }

    /// The vertical half of the scroll latch: VSRAM is read for a line at the
    /// same point as the horizontal table, so a `vdp.vsram[0]` write in mid line
    /// moves the plane from the next line on.
    #[test]
    fn a_vertical_scroll_write_in_mid_line_shows_on_the_next_line() {
        let mut vdp = running(0b00);
        fill_rows(&mut vdp, PLANE_A_BASE);

        vdp.advance(10 * MCLKS_PER_LINE);
        vdp.advance(HBLANK_CLEAR_MCLK_H32 + 100);
        vdp.vsram[0] = 8;

        vdp.advance(MCLKS_PER_LINE - vdp.line_mclk);
        assert_eq!(
            row(&vdp, 10),
            plane_rows(10, 0),
            "line 10 keeps the vscroll of its own latch"
        );

        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(row(&vdp, 11), plane_rows(11, 8), "line 11 is scrolled");
    }

    /// A register of the compositor is latched with the rest of the line: R0 bit
    /// 5 (LCB) written halfway across line 10 blanks the left column of line 11
    /// and leaves line 10 alone. This is what [`Vdp::left_column_blank`] already
    /// promises ("what it says is true of the next line drawn").
    #[test]
    fn a_blank_column_switch_in_mid_line_shows_on_the_next_line() {
        let mut vdp = running(0b00);
        fill_columns(&mut vdp, PLANE_A_BASE);

        vdp.advance(10 * MCLKS_PER_LINE);
        vdp.advance(HBLANK_CLEAR_MCLK_H32 + 100);
        vdp.write_register(0, 1 << 5);

        vdp.advance(MCLKS_PER_LINE - vdp.line_mclk);
        assert_eq!(
            row(&vdp, 10),
            plane_columns(0),
            "line 10 does not see the bit"
        );

        vdp.advance(MCLKS_PER_LINE);
        let line = row(&vdp, 11);
        assert!(
            line[..8].iter().all(|&p| p == pixel_of(0)),
            "line 11 opens with the 8 backdrop pixels of LCB"
        );
        assert_eq!(&line[8..], &plane_columns(0)[8..]);
    }

    /// The SAT of the sprite tests below: R5 = `0x78` is 0xF000 in H32 (genvdp.txt
    /// section 17 "$05"), clear of the pattern, plane and scroll tables
    /// [`scroll_vdp`] and [`fill_tile_row`] put to other uses.
    const SPRITE_BASE: usize = 0xF000;
    /// The CRAM index a sprite of these tests draws: palette 3, colour 1, the
    /// `palette * 16 + colour` of genvdp.txt section 9 that a sprite layer byte
    /// carries. [`fill_cram`] gives every entry its own colour, so a sprite pixel
    /// says which entry it came from and colour 2 means the second pattern tile.
    const SPRITE_ENTRY: usize = 3 * 16 + 1;
    /// The word 2 of a sprite of these tests: palette 3, tile 1, flips and
    /// priority clear.
    const SPRITE_PATTERN: u16 = (3 << 13) | 1;

    /// [`running`] with the SAT at [`SPRITE_BASE`] and tile 1 solid in colour 1: the
    /// one-cell, 8x8 sprite of the tests below. The two plane tables are empty, so
    /// a sprite pixel is the only thing a row of the frame can be carrying.
    fn sprite_running() -> Vdp {
        let mut vdp = running(0b00);
        vdp.write_register(5, 0x78);
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 1, row, 1);
        }
        vdp
    }

    /// A row of the frame with nothing on it but the backdrop colour.
    fn backdrop_row() -> Vec<u32> {
        vec![pixel_of(0); 256]
    }

    /// A row of the frame carrying the sprite of these tests: the 8 pixels of
    /// `entry`'s colour at x 20, and backdrop either side of them.
    fn sprite_row(entry: usize) -> Vec<u32> {
        let mut row = backdrop_row();
        row[20..28].fill(pixel_of(entry));
        row
    }

    /// The sprite of picture line `line` at screen x 20, at entry 0 of the list and
    /// ending it.
    fn sprite_on_line(vdp: &mut Vdp, line: u16) {
        put_sprite_at(
            vdp,
            SPRITE_BASE,
            0,
            line + 128,
            128 + 20,
            (1, 1),
            0,
            SPRITE_PATTERN,
        );
    }

    /// #311: "a sprite added to the SAT during line N-1's active display does not
    /// show on line N, shows on N+1". The list of a line is walked during the line
    /// before, at the same point of it that #102 reads the scroll tables and the
    /// mode registers from, so a write that is late for the scroll is exactly as
    /// late for a sprite.
    #[test]
    fn a_sprite_added_in_mid_line_shows_on_the_line_after_the_next() {
        let mut vdp = sprite_running();

        // Line 9 is into its active display, so the walk that decides what line 10
        // draws has been and gone by the time of the write.
        vdp.advance(9 * MCLKS_PER_LINE);
        vdp.advance(HBLANK_CLEAR_MCLK_H32 + 100);
        assert_eq!((vdp.line, vdp.line_mclk), (9, HBLANK_CLEAR_MCLK_H32 + 100));
        sprite_on_line(&mut vdp, 10);

        vdp.advance(MCLKS_PER_LINE - vdp.line_mclk);
        vdp.advance(MCLKS_PER_LINE);
        assert_eq!((vdp.line, vdp.line_mclk), (11, 0), "line 10 is drawn");
        assert_eq!(
            row(&vdp, 10),
            backdrop_row(),
            "and the entry is not in its list"
        );

        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(
            row(&vdp, 11),
            sprite_row(SPRITE_ENTRY),
            "line 11 walks the table again, and finds it"
        );
    }

    /// The other half of the same rule, and the same half the scroll have: the walk
    /// of the line after is at the start of the active display of this one, so a
    /// write in the blankening of line 9 is in time for line 10.
    #[test]
    fn a_sprite_added_in_the_blankening_of_a_line_shows_on_the_next_line() {
        let mut vdp = sprite_running();

        vdp.advance(9 * MCLKS_PER_LINE);
        assert_eq!((vdp.line, vdp.line_mclk), (9, 0));
        sprite_on_line(&mut vdp, 10);

        vdp.advance(MCLKS_PER_LINE);
        vdp.advance(MCLKS_PER_LINE);
        assert_eq!((vdp.line, vdp.line_mclk), (11, 0));
        assert_eq!(
            row(&vdp, 10),
            sprite_row(SPRITE_ENTRY),
            "line 10 is searched after the write"
        );
    }

    /// It is the y of the entry that the walk reads, not only the link that reaches
    /// it: a sprite moved down a line halfway across line 9 is drawn on line 10 all
    /// the same, because line 10 was searched while the sprite was still there.
    #[test]
    fn a_sprite_moved_in_mid_line_is_drawn_where_the_line_before_saw_it() {
        let mut vdp = sprite_running();
        sprite_on_line(&mut vdp, 10);

        vdp.advance(9 * MCLKS_PER_LINE);
        vdp.advance(HBLANK_CLEAR_MCLK_H32 + 100);
        sprite_on_line(&mut vdp, 11);

        vdp.advance(MCLKS_PER_LINE - vdp.line_mclk);
        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(
            row(&vdp, 10),
            sprite_row(SPRITE_ENTRY),
            "line 10 draws the y of its own search, not the y of the moment"
        );

        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(
            row(&vdp, 11),
            sprite_row(SPRITE_ENTRY),
            "and line 11 draws the moved one"
        );
    }

    /// The other side of the #103 split, which #311 leaves where it is: the pattern
    /// is not part of the walk of the line before, it is fetched by the line that
    /// draws the sprite. So a pattern written halfway across line 9 is on line 10,
    /// one line earlier than an entry written at the same moment would be.
    #[test]
    fn a_sprite_pattern_written_in_mid_line_shows_on_the_next_line() {
        let mut vdp = sprite_running();
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(&mut vdp, 2, row, 2);
        }
        sprite_on_line(&mut vdp, 10);

        vdp.advance(9 * MCLKS_PER_LINE);
        vdp.advance(HBLANK_CLEAR_MCLK_H32 + 100);
        // Word 2 of entry 0, the pattern, now naming tile 2: colour 2 is CRAM entry
        // one above the colour of tile 1.
        put_word(&mut vdp, SPRITE_BASE + 4, (3 << 13) | 2);

        vdp.advance(MCLKS_PER_LINE - vdp.line_mclk);
        vdp.advance(MCLKS_PER_LINE);
        assert_eq!(
            row(&vdp, 10),
            sprite_row(SPRITE_ENTRY + 1),
            "the pattern of line 10 is read by line 10"
        );
    }
}
