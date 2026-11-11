//! The sprite attribute table, the list its link fields make, and the sprite
//! pixels of one line (M4-15 #93).
//!
//! genvdp.txt section 15: "All sprite data is stored in a region of VRAM called
//! sprite attribute table. The table is 640 bytes in size. Each 8-byte entry has
//! the following format". [`sprite_at`] is that format, [`SpriteList`] the
//! "Linking" walk of the entries, and [`Vdp::render_sprite_line`] the layer
//! buffer of one line they end in.
//!
//! The layer is the same shape the two scroll planes and the window hand over
//! (M4-11 #89, M4-14 #92): a [`u8`] per pixel of bits 5-0 CRAM index and bit 6
//! priority, and a whole zero byte for a pixel that draws nothing. Sprites are
//! column-major cells of their own, so they do not share [`crate::pattern`]'s
//! plane fetch, but they do share its two operations: the word read and
//! [`crate::pattern::tile_row`].
//!
//! Not here, and all of it M4-16 (#94): the per-line sprite and pixel limits and
//! the SOVR status bit they set, collision, and the masking a sprite at X = 0
//! does to the sprites behind it. Neither is the priority resolution against the
//! planes, which is M4-17 (#95), nor the 11-bit y coordinate and 8x16 cells of
//! interlace mode 2, which are M4-20 (#98).

use crate::pattern::{NameEntry, read_word, tile_row};
use crate::planes::{CELL_ROWS, PRIORITY_SHIFT};
use crate::{Vdp, WIDTH_H40};

/// Bytes of one SAT entry: genvdp.txt section 15, "The table is 640 bytes in
/// size. Each 8-byte entry has the following format".
const ENTRY_BYTES: usize = 8;

/// The four words of one entry, as the `Index + 0`, `Index + 2`, `Index + 4` and
/// `Index + 6` of that section: the y coordinate, the two size fields with the
/// link below them, the pattern, and the x coordinate.
const Y_WORD: usize = 0;
const SIZE_WORD: usize = 1;
const PATTERN_WORD: usize = 2;
const X_WORD: usize = 3;

/// Bits 11-10 (`hs`) and 9-8 (`vs`) of the size word are the width and the height
/// of a sprite in cells, and bits 6-0 its link: `-hhvv` and `-lllllll` of the
/// section, whose "00b=1 cell, 11b=4 cells" is the `+ 1` of [`sprite_at`].
const HEIGHT_SHIFT: u32 = 8;
const WIDTH_SHIFT: u32 = 10;
const SIZE_MASK: u16 = 0x03;

/// Bits 6-0 of the size word: "l = Link field", and "The 7-bit link field in each
/// list entry is an index to the next entry of the sprite that will be drawn".
const LINK_MASK: u16 = 0x7F;

/// The most sprites a walk of the list may visit, by display width: "The VDP will
/// stop drawing sprites under the following conditions: The 80th sprite has been
/// drawn in 40-cell mode. The 64th sprite has been drawn in 32-cell mode"
/// (genvdp.txt section 15). 80 is also the number of entries the 640-byte table
/// holds, "The Genesis can display a total of eighty 32x32 15-color sprites".
const MAX_SPRITES_H40: usize = 80;
const MAX_SPRITES_H32: usize = 64;

/// The most sprites one line may draw, by display width: "Twenty sprites on the
/// same scanline have been drawn in 40 cell mode. Sixteen sprites on the same
/// scanline have been drawn in 32 cell mode" (genvdp.txt section 15 "Sprite
/// Drawing Limitations"). Parsing the one past the limit is what sets the status
/// bit - "Set when too many sprites are on the current scan line, meaning when the
/// VDP parses the 21st sprite in 40-cell mode or the 17th sprite in 32-cell mode
/// from the sprite list" (section 6) - so the sprite over the limit is read and
/// not drawn, and neither it nor anything later in the list is drawn either.
const MAX_SPRITES_LINE_H40: usize = 20;
const MAX_SPRITES_LINE_H32: usize = 16;

/// The origin of the display inside the sprite space: "Sprites are positioned in
/// a virtual 512x512 space. The display starts at coordinate 128, 128" (section
/// 15), so the screen coordinate of a sprite is its raw one less this.
const ORIGIN: i32 = 128;

/// One SAT entry: the `y`, `h`, `v`, `l`, `p`, `c`, `n` and `x` of genvdp.txt
/// section 15.
///
/// The 10-bit y and the 9-bit x are kept as the table holds them, still in the
/// virtual space of that section; the screen position is the raw one less
/// [`ORIGIN`], which [`Vdp::render_sprite_line`] works out instead of storing it
/// twice.
///
/// TODO(M4-20 #98): interlace mode 2 makes the y field 11 bits and turns the 8x8
/// cells of a sprite into 8x16 ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sprite {
    /// Word 0 bits 9-0: the y coordinate of the sprite in the virtual space.
    pub y: u16,
    /// Word 3 bits 8-0: the x coordinate of the sprite in the virtual space.
    pub x: u16,
    /// Word 1 bits 11-10 plus one: cells across the sprite, 1..=4.
    pub width_cells: u8,
    /// Word 1 bits 9-8 plus one: cells down the sprite, 1..=4.
    pub height_cells: u8,
    /// Word 1 bits 6-0: the index of the next entry of the list, and 0 for "no
    /// more sprites will be drawn after the current sprite".
    pub link: u8,
    /// Word 2, "pccvhnnn nnnnnnnn": "p = Priority, c = Color palette, v =
    /// Vertical flip, h = Horizontal flip, n = Sprite pattern start index", which
    /// is the layout of a name table entry in all but its 11-bit name.
    pub pattern: NameEntry,
}

/// The SAT entry `index`: the four words at `base + index * ENTRY_BYTES`, read the
/// way [`read_word`] reads every VDP table.
pub(crate) fn sprite_at(vram: &[u8], base: usize, index: usize) -> Sprite {
    let entry = base + index * ENTRY_BYTES;
    let size = read_word(vram, entry + SIZE_WORD * 2);
    Sprite {
        y: read_word(vram, entry + Y_WORD * 2) & 0x3FF,
        x: read_word(vram, entry + X_WORD * 2) & 0x1FF,
        width_cells: ((size >> WIDTH_SHIFT) & SIZE_MASK) as u8 + 1,
        height_cells: ((size >> HEIGHT_SHIFT) & SIZE_MASK) as u8 + 1,
        link: (size & LINK_MASK) as u8,
        pattern: NameEntry::decode(read_word(vram, entry + PATTERN_WORD * 2)),
    }
}

/// The sprites of one SAT list, in the order the VDP draws them.
///
/// genvdp.txt section 15 "Linking": "The VDP draws sprites in a front-to-back
/// order, starting with sprite zero. The 7-bit link field in each list entry is an
/// index to the next entry of the sprite that will be drawn. If the link field is
/// zero, then no more sprites will be drawn after the current sprite."
///
/// The list is the shape of VRAM and not a node graph, so this is an iterator over
/// an address: it allocates nothing and reads only the entries it walks.
///
/// A link of 80..=127, which the 7-bit field allows and the 80-entry table of
/// 640 bytes does not, is fetched at the address it names rather than stopped at:
/// genvdp.txt neither bounds the field by the table nor says what the VDP reads
/// past the table, and every limit it does give is a count of sprites rather than
/// of addresses. TODO(M4-16 #94): what a link past the table really reads.
pub(crate) struct SpriteList<'a> {
    /// The VRAM the table is read from.
    vram: &'a [u8],
    /// The table base, from [`Vdp::sprite_base`].
    base: usize,
    /// The entry to read next, and `None` once an entry's link field is zero or
    /// once there is nothing left to read.
    next: Option<usize>,
    /// Visits left before the limit of [`MAX_SPRITES_H40`] / [`MAX_SPRITES_H32`],
    /// which is what ends a list whose links loop: "Link settings that create an
    /// 'infinite loop' or that have self-referencing will not cause any unforseen
    /// problem, these kinds of loops will be broken out of when the above sprite
    /// limitations are eventually reached".
    remaining: usize,
}

impl Iterator for SpriteList<'_> {
    type Item = Sprite;

    fn next(&mut self) -> Option<Sprite> {
        let index = self.next?;
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let sprite = sprite_at(self.vram, self.base, index);
        self.next = match sprite.link {
            0 => None,
            link => Some(usize::from(link)),
        };
        Some(sprite)
    }
}

impl Vdp {
    /// The sprite list of the SAT at [`Vdp::sprite_base`], starting at entry 0 and
    /// ending at the first zero link or at the sprite limit of the display width.
    pub(crate) fn sprite_list(&self) -> SpriteList<'_> {
        SpriteList {
            vram: &self.vram,
            base: usize::from(self.sprite_base()),
            next: Some(0),
            remaining: if self.h40() {
                MAX_SPRITES_H40
            } else {
                MAX_SPRITES_H32
            },
        }
    }

    /// Fill `out` with the sprite layer of `line`: one [`u8`] per pixel, bits 5-0
    /// the CRAM index (`palette * 16 + colour`, genvdp.txt section 9) and bit 6 the
    /// entry's priority flag, and 0 for the pixels no sprite draws on.
    ///
    /// `out` is zeroed first and then painted in list order, a pixel only being
    /// written while it is still transparent: "The VDP draws sprites in a
    /// front-to-back order, starting with sprite zero" (section 15) and "The sprite
    /// priority bit does not affect inter-sprite priority" (section 14). So the
    /// first sprite of the list with a non transparent pixel at a screen position
    /// owns it, and the priority flag of an entry only says where the whole sprite
    /// layer sits against the two planes, which is M4-17 (#95).
    ///
    /// A colour 0 pixel draws nothing, as in a plane cell ("A pixel within a
    /// pattern that uses value zero isn't shown", section 12), and is written as a
    /// whole zero byte for the same reason [`Vdp::render_plane_line`] gives: the
    /// transparency test of the compositor is then unambiguous and cannot pick up a
    /// stale priority bit.
    ///
    /// A sprite is `width_cells * height_cells` of the 8 pixel cells of a plane,
    /// so at most 4 of them across and 4 down: four [`tile_row`] fetches per line
    /// per sprite at most, and no allocation. Which cell a sprite's pattern index
    /// names is the column-major rule of [`Vdp::fill_sprite_line`].
    ///
    /// Of the limits of section 15 "Sprite Drawing Limitations" the two that count
    /// sprites are here: a sprite whose y does not reach `line` is not on the line
    /// and spends nothing, and one whose y does spends a slot of it whether or not
    /// any of its pixels land on the display - "Sprites that are outside of the
    /// physical display area are still taken into account".
    ///
    /// TODO(M4-16 #94): the 320/256 pixels of sprite data per line, collision and
    /// the masking of a sprite at x = 0.
    pub fn render_sprite_line(&mut self, line: u16, out: &mut [u8; WIDTH_H40]) {
        out.fill(0);
        let max_sprites = if self.h40() {
            MAX_SPRITES_LINE_H40
        } else {
            MAX_SPRITES_LINE_H32
        };
        let mut walk = LineWalk::default();

        for sprite in self.sprite_list() {
            // The sprite occupies the lines `y - ORIGIN` to `y - ORIGIN` plus its
            // own height, and `row` is which row of that it is drawing.
            let row = i32::from(line) - (i32::from(sprite.y) - ORIGIN);
            let height = (usize::from(sprite.height_cells) * CELL_ROWS) as i32;
            if row < 0 || row >= height {
                continue;
            }
            if !walk.admit(max_sprites) {
                break;
            }
            self.fill_sprite_line(&sprite, row as usize, out);
        }

        self.sprite_overflow |= walk.overflow;
    }

    /// Draw `row` - the row of `sprite` counting down from its top, 0..the height
    /// of the sprite in pixels - over `out`, writing only the pixels that are
    /// still transparent; see [`Vdp::render_sprite_line`] for who that leaves.
    ///
    /// The cells of a sprite are numbered down the columns, so the cell at
    /// `column` and `row` is `pattern + column * height_cells + row_cell`. That is
    /// INFERRED: genvdp.txt section 15 gives only "n = Sprite pattern start index"
    /// and the size of a sprite, and says nothing about the order of the cells it
    /// is the start of. It is what the tests of #93 fix in, and it is what makes a
    /// sprite of any shape a contiguous run of patterns down a column. The 11-bit
    /// name is added to before [`crate::pattern::tile_addr`] is worked out, so a
    /// sprite at the very top of VRAM has its last cells fetched at the bottom of
    /// the 16 address lines; genvdp.txt says nothing about an address that overruns
    /// the pattern name and a cell offset together.
    ///
    /// A flip mirrors the whole sprite, and does so twice over: the cell order is
    /// reversed and the pattern inside each cell is mirrored, which is exactly what
    /// [`tile_row`] does with the same flag. Mirroring the cell index and passing
    /// `row` to [`tile_row`] unmirrored is the same thing as mirroring the row and
    /// dropping the flag, so each axis is mirrored once here and nowhere else.
    fn fill_sprite_line(&self, sprite: &Sprite, row: usize, out: &mut [u8]) {
        let NameEntry {
            priority,
            palette,
            vflip,
            hflip,
            tile,
        } = sprite.pattern;
        let height_cells = usize::from(sprite.height_cells);
        // The cell row of the sprite to fetch, mirrored by vflip; the row of the
        // pattern inside it stays `row % CELL_ROWS` because `tile_row` mirrors that
        // with the same flag, and the two together are the one mirror.
        let cell_row = if vflip {
            height_cells - 1 - row / CELL_ROWS
        } else {
            row / CELL_ROWS
        };
        let row_in_cell = row % CELL_ROWS;
        let layer_base = (u16::from(palette) << 4) | (u16::from(priority) << PRIORITY_SHIFT);
        let left = i32::from(sprite.x) - ORIGIN;

        for cell in 0..usize::from(sprite.width_cells) {
            let column = if hflip {
                usize::from(sprite.width_cells) - 1 - cell
            } else {
                cell
            };
            let sprite_tile = tile + (column * height_cells + cell_row) as u16;
            let indices = tile_row(
                &self.vram,
                sprite_tile,
                row_in_cell,
                vflip,
                hflip,
                CELL_ROWS,
            );
            let x0 = left + (cell * CELL_ROWS) as i32;
            for (offset, index) in indices.into_iter().enumerate() {
                let x = x0 + offset as i32;
                if x < 0 || usize::try_from(x).unwrap_or(usize::MAX) >= out.len() {
                    continue;
                }
                let x = x as usize;
                if index != 0 && out[x] == 0 {
                    out[x] = (layer_base + u16::from(index)) as u8;
                }
            }
        }
    }
}

/// The counters of genvdp.txt section 15 "Sprite Drawing Limitations" that one line
/// of [`Vdp::render_sprite_line`] keeps, and the two status flags its walk ends in.
///
/// They live here rather than in the VDP because they are about one line and are
/// rebuilt every time one is drawn; only [`Vdp::sprite_overflow`] and
/// [`Vdp::sprite_collision`] outlive a line, and `render_sprite_line` copies those
/// over once the list has been walked.
#[derive(Debug, Default)]
struct LineWalk {
    /// Sprites of this line reached so far.
    sprites: usize,
    /// SOVR, from a sprite past [`MAX_SPRITES_LINE_H40`] /
    /// [`MAX_SPRITES_LINE_H32`]: see [`Vdp::sprite_overflow`].
    overflow: bool,
}

impl LineWalk {
    /// Take one more sprite of the line and say whether it may draw: `false` for
    /// the sprite past the limit of `max_sprites`, which the VDP parses but does not
    /// draw and stops the whole line at - genvdp.txt lists its limits as things the
    /// VDP "will stop drawing sprites under".
    fn admit(&mut self, max_sprites: usize) -> bool {
        self.sprites += 1;
        if self.sprites > max_sprites {
            self.overflow = true;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planes::testutil::{fill_tile_row, put_tile_row, put_word};

    /// SAT base of these tests: R5 = `0x78` is 0xF000 both under the H32 mask of
    /// `0x7F` and under the H40 one of `0x7E`, so changing the display width does
    /// not move the table (genvdp.txt section 17 "$05").
    const SPRITE_BASE: usize = 0xF000;

    /// A VDP with its SAT at [`SPRITE_BASE`].
    fn sprite_vdp() -> Vdp {
        let mut vdp = Vdp::new(false);
        vdp.write_register(5, 0x78);
        vdp
    }

    /// The 320-pixel line [`Vdp::render_sprite_line`] is called with, ready to fill.
    fn blank_line() -> [u8; WIDTH_H40] {
        [0; WIDTH_H40]
    }

    /// SAT entry `index`: `y` and `x` the raw coordinates of the virtual 512x512
    /// space of genvdp.txt section 15, `cells` the `(width, height)` of the sprite
    /// in cells of 8 pixels, `link` the index of the next entry of the list or 0 to
    /// stop, and `pattern` word 2, the name-table-style pattern of the entry.
    fn put_sprite(
        vdp: &mut Vdp,
        index: usize,
        y: u16,
        x: u16,
        cells: (u8, u8),
        link: u8,
        pattern: u16,
    ) {
        let (width, height) = cells;
        let entry = SPRITE_BASE + index * ENTRY_BYTES;
        put_word(vdp, entry, y);
        put_word(
            vdp,
            entry + 2,
            ((u16::from(width) - 1) << WIDTH_SHIFT)
                | ((u16::from(height) - 1) << HEIGHT_SHIFT)
                | u16::from(link),
        );
        put_word(vdp, entry + 4, pattern);
        put_word(vdp, entry + 6, x);
    }

    /// The word 2 naming `tile` in `palette`, priority and flips clear.
    fn palette_tile(palette: u8, tile: u16) -> u16 {
        u16::from(palette) << 13 | tile
    }

    /// `count` solid tiles from `first`: tile `first + n` is colour `n + 1` in all
    /// 8 of its rows, so a sprite cell that draws colour `c` names tile
    /// `first + c - 1` and the colour of a pixel says which cell it was fetched
    /// from. Up to 15 tiles, since colour 0 is transparency and 4 bits are all a
    /// pattern pixel has.
    fn solid_cells(vdp: &mut Vdp, first: u16, count: usize) {
        for n in 0..count {
            for row in 0..CELL_ROWS {
                fill_tile_row(vdp, first + n as u16, row, (n + 1) as u8);
            }
        }
    }

    /// `count` rows of `tile` filled with `colour`, the helper the flips and the
    /// row tests need because a sprite reads one row per line.
    fn solid_tile(vdp: &mut Vdp, tile: u16, colour: u8) {
        for row in 0..CELL_ROWS {
            fill_tile_row(vdp, tile, row, colour);
        }
    }

    /// The five fields of genvdp.txt section 15 sit where that section says, and
    /// the bits it does not name - bits 15-12 of the size word and bit 7 of it -
    /// reach none of them.
    #[test]
    fn an_entry_is_decoded_field_by_field() {
        let mut vdp = sprite_vdp();
        let entry = SPRITE_BASE + 3 * ENTRY_BYTES;
        for offset in (0..ENTRY_BYTES).step_by(2) {
            put_word(&mut vdp, entry + offset, 0xFFFF);
        }
        let sprite = sprite_at(&vdp.vram, SPRITE_BASE, 3);
        assert_eq!(sprite.y, 0x3FF, "word 0 bits 9-0 are y");
        assert_eq!(sprite.x, 0x1FF, "word 3 bits 8-0 are x");
        assert_eq!(sprite.width_cells, 4, "word 1 bits 11-10: 11b is 4 cells");
        assert_eq!(sprite.height_cells, 4, "word 1 bits 9-8: 11b is 4 cells");
        assert_eq!(sprite.link, 0x7F, "word 1 bits 6-0 are the link");
        assert_eq!(
            sprite.pattern,
            NameEntry::decode(0xFFFF),
            "word 2 is a name table entry, so its 11-bit name is a name"
        );

        for offset in (0..ENTRY_BYTES).step_by(2) {
            put_word(&mut vdp, entry + offset, 0);
        }
        put_word(&mut vdp, entry + 2, 0x8080);
        let sprite = sprite_at(&vdp.vram, SPRITE_BASE, 3);
        assert_eq!(
            sprite.link, 0,
            "`-lllllll`: bit 7 of the byte is not an eighth link bit"
        );
        assert_eq!(
            (sprite.width_cells, sprite.height_cells),
            (1, 1),
            "`----hhvv`: bits 15-12 are not a third size bit"
        );
    }

    /// One 8x8 sprite at X = 128 + 10 and Y = 128 sits at screen x 10, y 0: "The
    /// display starts at coordinate 128, 128" (section 15). Line 0 is therefore its
    /// one row and pixels 10..18 its colour, palette 1 colour 3 being CRAM index 19.
    ///
    /// The line arrives full of the leftover of the last one, and every pixel the
    /// sprite does not cover comes back 0: the buffer is the caller's, so zeroing
    /// it belongs to this call.
    #[test]
    fn a_one_by_one_sprite_covers_eight_pixels() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 3);
        put_sprite(&mut vdp, 0, 128, 128 + 10, (1, 1), 0, palette_tile(1, 1));

        let mut line = [0x55; WIDTH_H40];
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[10..18],
            &[19; 8],
            "palette 1 colour 3 is the CRAM index 19"
        );
        assert_eq!(&line[..10], &[0; 10], "nothing is drawn before the sprite");
        assert_eq!(
            &line[18..],
            &[0; WIDTH_H40 - 18],
            "and nothing after it, whatever the buffer held"
        );
    }

    /// The cells of a sprite are numbered down its columns, so a 2x2 sprite of
    /// patterns 1, 2, 3, 4 is 1 top-left, 2 bottom-left, 3 top-right, 4
    /// bottom-right. INFERRED, see [`Vdp::fill_sprite_line`].
    #[test]
    fn sprite_cells_are_column_major() {
        let mut vdp = sprite_vdp();
        solid_cells(&mut vdp, 1, 4);
        put_sprite(&mut vdp, 0, 128, 128, (2, 2), 0, 1);

        let mut top = blank_line();
        vdp.render_sprite_line(0, &mut top);
        assert_eq!(
            &top[0..8],
            &[1; 8],
            "cell (0,0) is the start of the pattern"
        );
        assert_eq!(
            &top[8..16],
            &[3; 8],
            "cell (1,0) is pattern + 1 * height, not pattern + 1"
        );

        let mut bottom = blank_line();
        vdp.render_sprite_line(8, &mut bottom);
        assert_eq!(&bottom[0..8], &[2; 8], "cell (0,1) is the next one down");
        assert_eq!(&bottom[8..16], &[4; 8], "cell (1,1) ends the pattern");
    }

    /// A sprite that is not square tells the column-major rule of
    /// [`Vdp::fill_sprite_line`] from the two wrong ones a square sprite cannot:
    /// `pattern + column * width + row` and `pattern + row * height + col` both
    /// number a 2x2 the same way as the right rule and a 4x4 differently.
    #[test]
    fn a_two_by_four_sprite_numbers_its_cells_down_the_columns() {
        let mut vdp = sprite_vdp();
        solid_cells(&mut vdp, 1, 8);
        put_sprite(&mut vdp, 0, 128, 128, (2, 4), 0, 1);

        for cell_row in 0..4usize {
            let mut line = blank_line();
            vdp.render_sprite_line((cell_row * CELL_ROWS) as u16, &mut line);
            assert_eq!(
                &line[0..8],
                &[(cell_row + 1) as u8; 8],
                "cell (0,{cell_row}) is tile {}",
                cell_row + 1
            );
            assert_eq!(
                &line[8..16],
                &[(cell_row + 5) as u8; 8],
                "cell (1,{cell_row}) is tile {}, a whole column of 4 further on",
                cell_row + 5
            );
        }

        let mut line = blank_line();
        vdp.render_sprite_line(32, &mut line);
        assert_eq!(
            line,
            blank_line(),
            "the sprite is 4 cells = 32 pixels tall, so line 32 is below it"
        );
    }

    /// `h = Horizontal flip` of a sprite mirrors the whole sprite: the columns come
    /// back reversed, and the pixels of each of them are reversed by
    /// [`crate::pattern::tile_row`] with the same flag.
    #[test]
    fn hflip_mirrors_the_columns_and_their_pixels() {
        let mut vdp = sprite_vdp();
        put_tile_row(&mut vdp, 1, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        put_tile_row(&mut vdp, 2, 0, &[9, 10, 11, 12, 13, 14, 15, 1]);

        put_sprite(&mut vdp, 0, 128, 128, (2, 1), 0, 1);
        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..16],
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 1],
            "unflipped: cell (0,0) then cell (1,0), each read left to right"
        );

        put_sprite(&mut vdp, 0, 128, 128, (2, 1), 0, 1 | 0x0800);
        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..16],
            &[1, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1],
            "hflip: cell (1,0) first, and its own pixels reversed"
        );
    }

    /// `v = Vertical flip` of a sprite mirrors it the same way along the rows, the
    /// top cell becoming the bottom one and every row of patterns its mirror.
    #[test]
    fn vflip_mirrors_the_rows_and_their_pixels() {
        let mut vdp = sprite_vdp();
        // Tile 1 is colour 1 but its top row, which is colour 7, and tile 2 is
        // colour 2 but its top row, which is colour 8: the colour of a pixel then
        // says both which cell of the sprite it came from and which row of that
        // cell it was fetched at.
        solid_tile(&mut vdp, 1, 1);
        solid_tile(&mut vdp, 2, 2);
        fill_tile_row(&mut vdp, 1, 0, 7);
        fill_tile_row(&mut vdp, 2, 0, 8);

        put_sprite(&mut vdp, 0, 128, 128, (1, 2), 0, 1);
        for (line, colour) in [(0, 7), (1, 1), (8, 8), (9, 2), (15, 2)] {
            let mut out = blank_line();
            vdp.render_sprite_line(line, &mut out);
            assert_eq!(&out[0..8], &[colour; 8], "line {line} of the sprite");
        }

        put_sprite(&mut vdp, 0, 128, 128, (1, 2), 0, 1 | 0x1000);
        for (line, colour) in [(0, 2), (7, 8), (8, 1), (15, 7)] {
            let mut out = blank_line();
            vdp.render_sprite_line(line, &mut out);
            assert_eq!(
                &out[0..8],
                &[colour; 8],
                "line {line} of the flipped sprite, which is the unflipped one read from the bottom"
            );
        }
    }

    /// genvdp.txt section 15 "Linking", the worked example: "In this case, sprites
    /// #00, #02, #04, #03 will be drawn, in that order." The x of an entry says
    /// which entry was visited and its link where the walk went from there, so the
    /// pair identifies both.
    #[test]
    fn the_list_follows_the_link_of_the_documented_example() {
        let mut vdp = sprite_vdp();
        let links = [2u8, 7, 4, 0, 3, 2];
        for (index, link) in links.iter().enumerate() {
            put_sprite(
                &mut vdp,
                index,
                128,
                128 + 8 * index as u16,
                (1, 1),
                *link,
                0,
            );
        }

        let list: Vec<(u16, u8)> = vdp.sprite_list().map(|s| (s.x, s.link)).collect();
        assert_eq!(
            list,
            vec![(128, 2), (144, 4), (160, 3), (152, 0)],
            "x is 128 plus 8 per entry index: 0, 2, 4, 3, in that order and nothing else"
        );
    }

    /// The walk starts at entry 0 and an entry with a zero link is the last of the
    /// list even when the entries after it go on linking: "starting with sprite
    /// zero" and "If the link field is zero, then no more sprites will be drawn
    /// after the current sprite".
    #[test]
    fn a_zero_link_ends_the_list_at_the_first_one() {
        let mut vdp = sprite_vdp();
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 0, 0);
        put_sprite(&mut vdp, 1, 128, 128, (1, 1), 2, 0);
        put_sprite(&mut vdp, 2, 128, 128, (1, 1), 3, 0);
        assert_eq!(vdp.sprite_list().count(), 1);
    }

    /// "Link settings that create an 'infinite loop' or that have self-referencing
    /// will not cause any unforseen problem, these kinds of loops will be broken
    /// out of when the above sprite limitations are eventually reached": the limit
    /// of the display width is what ends a self-referencing link, 64 sprites of a
    /// 32-cell list and 80 of a 40-cell one.
    #[test]
    fn a_looping_list_stops_at_the_sprite_limit() {
        let mut vdp = sprite_vdp();
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 5, 0);
        put_sprite(&mut vdp, 5, 128, 128, (1, 1), 5, 0);

        assert_eq!(
            vdp.sprite_list().count(),
            MAX_SPRITES_H32,
            "the 64th sprite of a 32-cell list"
        );

        vdp.write_register(12, 0x81);
        assert_eq!(
            vdp.sprite_list().count(),
            MAX_SPRITES_H40,
            "the 80th sprite of a 40-cell list"
        );
    }

    /// Only the linked entries are drawn, so an entry that holds a whole sprite and
    /// is not linked to is not on the line at all.
    #[test]
    fn the_link_list_skips_the_sprites_nothing_links_to() {
        let mut vdp = sprite_vdp();
        solid_cells(&mut vdp, 1, 3);
        put_sprite(&mut vdp, 0, 128, 128 + 10, (1, 1), 2, 1);
        put_sprite(&mut vdp, 1, 128, 128 + 10, (1, 1), 0, 2);
        put_sprite(&mut vdp, 2, 128, 128, (1, 1), 0, 3);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..8], &[3; 8], "sprite 2, which sprite 0 links to");
        assert_eq!(&line[10..18], &[1; 8], "and sprite 0, the head of the list");
        assert!(
            !line.contains(&2),
            "sprite 1 holds a sprite of its own that nothing links to"
        );
    }

    /// The first sprite of the list owns a pixel, and a later one only shows
    /// through where the earlier is transparent: "The VDP draws sprites in a
    /// front-to-back order, starting with sprite zero" (section 15) and "The sprite
    /// priority bit does not affect inter-sprite priority" (section 14).
    #[test]
    fn the_first_sprite_of_the_list_covers_the_later_ones() {
        let mut vdp = sprite_vdp();
        put_tile_row(&mut vdp, 1, 0, &[0, 0, 0, 0, 5, 5, 5, 5]);
        solid_tile(&mut vdp, 2, 9);
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 1, 1);
        put_sprite(&mut vdp, 1, 128, 128, (1, 1), 0, 2);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..4],
            &[9; 4],
            "sprite 1 shows through the transparent left half of sprite 0"
        );
        assert_eq!(
            &line[4..8],
            &[5; 4],
            "and not where sprite 0, which comes first in the list, is opaque"
        );
        assert_eq!(&line[8..], &[0; WIDTH_H40 - 8]);
    }

    /// X = 0 is screen x -128, off the line: "unwanted sprites can easily be hidden
    /// off screen" (section 15). Even the largest sprite there is, 4x4 cells of 32
    /// pixels, ends 96 pixels before the display.
    ///
    /// What an X of 0 means as well, the masking of every later sprite of those
    /// lines, is TODO(M4-16 #94).
    #[test]
    fn a_sprite_at_x_zero_is_off_screen() {
        let mut vdp = sprite_vdp();
        // One opaque tile for every cell of both sprites, so that nothing the
        // clipping gets wrong can pass by being transparent instead.
        solid_tile(&mut vdp, 1, 15);
        put_sprite(&mut vdp, 0, 128, 0, (1, 1), 1, 1);
        put_sprite(&mut vdp, 1, 128, 0, (4, 4), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(line, blank_line(), "both sprites are at screen x -128");
    }

    /// "sprites can be shown partially at the top and left screen edges" (section
    /// 15): Y = 128 - 3 has its first 3 rows above the display, so line 0 is row 3
    /// of the sprite and line 4 its last row.
    #[test]
    fn a_sprite_partly_above_the_display_shows_its_lower_rows() {
        let mut vdp = sprite_vdp();
        for row in 0..CELL_ROWS {
            fill_tile_row(&mut vdp, 1, row, (row + 1) as u8);
        }
        put_sprite(&mut vdp, 0, 128 - 3, 128, (1, 1), 0, 1);

        for (line, colour) in [(0, 4), (4, 8)] {
            let mut out = blank_line();
            vdp.render_sprite_line(line, &mut out);
            assert_eq!(&out[0..8], &[colour; 8], "line {line} of the sprite");
        }

        let mut line = blank_line();
        vdp.render_sprite_line(5, &mut line);
        assert_eq!(line, blank_line(), "the sprite ended on line 4");
    }

    /// X = 128 - 2 is screen x -2: the first 2 pixels of the sprite are lost and
    /// screen x 0 is the sprite's own column 2.
    #[test]
    fn a_sprite_partly_left_of_the_display_is_clipped() {
        let mut vdp = sprite_vdp();
        put_tile_row(&mut vdp, 1, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        put_sprite(&mut vdp, 0, 128, 128 - 2, (1, 1), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..6], &[3, 4, 5, 6, 7, 8]);
        assert_eq!(&line[6..], &[0; WIDTH_H40 - 6]);
    }

    /// X = 128 + 316 is screen x 316, so 4 of the 8 pixels of a sprite are on the
    /// line and the 4 past pixel 319 are not drawn anywhere.
    #[test]
    fn a_sprite_partly_right_of_the_display_is_clipped() {
        let mut vdp = sprite_vdp();
        put_tile_row(&mut vdp, 1, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        put_sprite(&mut vdp, 0, 128, 128 + 316, (1, 1), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[316..320], &[1, 2, 3, 4]);
        assert_eq!(&line[..316], &[0; 316]);
    }

    /// The priority flag of a sprite entry lands in bit 6 of its pixels exactly as
    /// a plane cell's does, and its palette in the top CRAM index bits: the flag
    /// says where the sprite sits against the planes - "Low priority sprites *can*
    /// overlap high priority sprites" (section 14) - which M4-17 (#95) reads.
    #[test]
    fn a_sprite_pixel_is_a_cram_index_plus_the_priority_bit() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 3);

        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 0, palette_tile(1, 1));
        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..8], &[19; 8], "palette 1 colour 3");

        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 0, 0xE000 | 1);
        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..8],
            &[51 | (1 << 6); 8],
            "palette 3 colour 3 with the priority flag of a high priority sprite"
        );
    }

    /// The table is read at [`Vdp::sprite_base`], R5 shifted: the 512-byte step of
    /// one R5 bit is bit 0, which H40 forces to zero (section 17 "$05"), so 0x78
    /// and 0x79 are the same table in H40 and two tables apart here.
    #[test]
    fn the_table_is_read_at_the_r5_base() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 3);
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..8], &[3; 8], "entry 0 of the table at 0xF000");

        vdp.write_register(5, 0x79);
        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            line,
            blank_line(),
            "R5 = 0x79 is 0xF200, where nothing is set up yet"
        );

        put_sprite(&mut vdp, 64, 128, 128 + 8, (1, 1), 0, 1);
        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[8..16],
            &[3; 8],
            "the entry at 0xF200 is entry 0 of the new base, and the one at 0xF000 is no longer read"
        );
    }

    // ---- The per-line limits, SOVR, collision and masking (M4-16 #94) ----

    /// The 40-cell display, because both per-line limits of genvdp.txt section 15
    /// "Sprite Drawing Limitations" are given per display width.
    fn in_h40(vdp: &mut Vdp) {
        vdp.write_register(12, 0x81);
    }

    /// `count` opaque 1x1 sprites on the lines of `y`, at `x(index)`, each linking
    /// to the next and the last to entry `count`: the shape the per-line limit
    /// tests need, one sprite per 8 pixels of the line. Entry `count` is left
    /// alone, so it ends the list (a zero link) unless a test goes on to fill it.
    fn put_line(vdp: &mut Vdp, y: u16, count: usize, x: impl Fn(usize) -> u16) {
        for index in 0..count {
            put_sprite(vdp, index, y, x(index), (1, 1), index as u8 + 1, 1);
        }
    }

    /// genvdp.txt section 15 "Sprite Drawing Limitations": "Twenty sprites on the
    /// same scanline have been drawn in 40 cell mode", and section 6 says of the
    /// flag that it is set "when the VDP parses the 21st sprite in 40-cell mode".
    /// The 21st is parsed and not drawn, so the line ends with the 20th.
    #[test]
    fn a_twenty_first_sprite_of_a_line_is_dropped_and_sets_overflow() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        solid_tile(&mut vdp, 1, 7);
        put_line(&mut vdp, 128, 21, |i| 128 + 8 * i as u16);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..160],
            &[7; 160],
            "the twenty sprites of the line, 8 pixels each"
        );
        assert_eq!(&line[160..168], &[0; 8], "and nothing of the 21st");
        assert!(
            vdp.sprite_overflow,
            "d6 SOVR, set when the 21st sprite of the line is parsed"
        );
    }

    /// The same limit of a 32-cell line is sixteen sprites, which is the default
    /// display of [`Vdp::new`] and so needs no R12 write.
    #[test]
    fn a_seventeenth_sprite_of_a_thirty_two_cell_line_is_dropped_and_sets_overflow() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 7);
        put_line(&mut vdp, 128, 17, |i| 128 + 8 * i as u16);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..128], &[7; 128], "sixteen sprites of 8 pixels");
        assert_eq!(&line[128..136], &[0; 8], "and nothing of the 17th");
        assert!(vdp.sprite_overflow, "the 17th sprite of a 32-cell line");
    }

    /// The limit is inclusive: twenty sprites are what a 40-cell line may draw, so
    /// the flag stays clear until there is a 21st to parse.
    #[test]
    fn twenty_sprites_on_a_line_do_not_overflow() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        solid_tile(&mut vdp, 1, 7);
        put_line(&mut vdp, 128, 20, |i| 128 + 8 * i as u16);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..160], &[7; 160], "all twenty are drawn");
        assert!(!vdp.sprite_overflow, "reaching the limit is not passing it");
    }

    /// The 20 slots are spent by the sprites of the line, not by the sprites the
    /// viewer can see: "Sprites that are outside of the physical display area are
    /// still taken into account" (section 15). x = 460 is screen x 332, so all 20
    /// of these are past the right hand edge of the display.
    #[test]
    fn a_sprite_outside_the_display_uses_a_slot_of_the_line() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        solid_tile(&mut vdp, 1, 7);
        put_line(&mut vdp, 128, 20, |_| 460);
        put_sprite(&mut vdp, 20, 128, 128, (1, 1), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(line, blank_line(), "the visible sprite is the 21st");
        assert!(
            vdp.sprite_overflow,
            "and the twenty off-screen ones used up the line to get there"
        );
    }
}
