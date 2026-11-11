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
//! The entries are not read straight out of VRAM. The VDP keeps the first four
//! bytes of each of them - the y, the size and the link - in its own
//! [`SpriteCache`], which only a write to that area of VRAM feeds
//! ([`Vdp::write_vram_byte`]) and which [`sprite_at`] reads (M4-23b, #103).
//!
//! The per-line sprite and pixel limits, the SOVR status bit they set, collision
//! and the masking a sprite at X = 0 does to the sprites behind it are all here as
//! of M4-16 (#94), and the 8x16 cells and doubled y origin of interlace mode 2 of
//! M4-20 (#98) are: [`Vdp::cell_height`] and [`ORIGIN_MODE2`]. Not here is the
//! priority resolution against the planes, which is M4-17 (#95), nor the X = 1
//! masking mode 2 of genvdp.txt section 15, which is #292.

use crate::pattern::{NameEntry, read_word, tile_row};
use crate::planes::{CELL_WIDTH, PRIORITY_SHIFT};
use crate::{InterlaceMode, Vdp, WIDTH_H32, WIDTH_H40};

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

/// The bytes of an entry the VDP keeps for itself: its `Index + 0` and `Index + 2`
/// words, the y and the size/link (#103). The other four bytes of an entry, the
/// pattern and the x, are read from VRAM on every fetch.
const ENTRY_CACHE_BYTES: usize = 4;

/// The cache lines of the table: its 80 entries of [`ENTRY_BYTES`] apiece, which
/// is the 640 bytes of genvdp.txt section 15 and the `MAX_SPRITES_H40` sprites a
/// walk of it may visit. The cache does not shrink in 32-cell mode, where only 64
/// of them are ever reached.
const CACHE_ENTRIES: usize = 80;

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

/// The most sprite pixels one line may fetch, by display width: "320 pixels worth
/// of sprite data has been drawn on the same scanline in 40 cell mode. 256 pixels
/// worth of sprite data has been drawn on the same scanline in 32 cell mode"
/// (genvdp.txt section 15 "Sprite Drawing Limitations"). Both are the width of the
/// display they belong to, so they are the width and not another number next to it.
const MAX_DOTS_LINE_H40: usize = WIDTH_H40;
const MAX_DOTS_LINE_H32: usize = WIDTH_H32;

/// The origin of the display inside the sprite space: "Sprites are positioned in
/// a virtual 512x512 space. The display starts at coordinate 128, 128" (section
/// 15), so the screen coordinate of a sprite is its raw one less this.
const ORIGIN: i32 = 128;

/// The origin of the display on the vertical axis of interlace mode 2: double
/// [`ORIGIN`], because the y field counts the 448 lines of that picture and not
/// the 224 of the beam (ticket #98: "Sprite Y is 10 bits with offset 256 instead
/// of 128"). The field itself stays the 10 bits of genvdp.txt section 15; only
/// what a bit of it means moves, exactly as it does for the plane fetch of
/// [`crate::planes`] and for [`Vdp::display_line`].
const ORIGIN_MODE2: i32 = 256;

/// One SAT entry: the `y`, `h`, `v`, `l`, `p`, `c`, `n` and `x` of genvdp.txt
/// section 15.
///
/// The 10-bit y and the 9-bit x are kept as the table holds them, still in the
/// virtual space of that section; the screen position is the raw one less the
/// origin of its axis, [`ORIGIN`] across and [`ORIGIN`] or [`ORIGIN_MODE2`]
/// down, which [`Vdp::render_sprite_line`] works out instead of storing it
/// twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sprite {
    /// Word 0 bits 9-0: the y coordinate of the sprite in the virtual space, in
    /// the lines of the display - 224 of them, or the 448 of interlace mode 2
    /// where the origin of the screen is 256 rather than [`ORIGIN`].
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

/// The SAT entry `index`: its y, size and link out of [`SpriteCache`], and its
/// pattern and x out of the two words at `base + index * ENTRY_BYTES + 4` of
/// `vram`, read the way [`read_word`] reads every VDP table (M4-23b, #103).
///
/// The split is the cache's: the four bytes the VDP holds are the y word and the
/// size/link word, and the two it never holds are the pattern and the x.
pub(crate) fn sprite_at(vram: &[u8], cache: &SpriteCache, base: usize, index: usize) -> Sprite {
    let entry = base + index * ENTRY_BYTES;
    let [y, size] = match cache.line(index) {
        // `Y_WORD` is 0 and `SIZE_WORD` 1, so a cache line holds the two words in
        // the same order VRAM holds them.
        Some(line) => [
            u16::from_be_bytes([line[Y_WORD * 2], line[Y_WORD * 2 + 1]]),
            u16::from_be_bytes([line[SIZE_WORD * 2], line[SIZE_WORD * 2 + 1]]),
        ],
        None => [
            read_word(vram, entry + Y_WORD * 2),
            read_word(vram, entry + SIZE_WORD * 2),
        ],
    };
    Sprite {
        y: y & 0x3FF,
        x: read_word(vram, entry + X_WORD * 2) & 0x1FF,
        width_cells: ((size >> WIDTH_SHIFT) & SIZE_MASK) as u8 + 1,
        height_cells: ((size >> HEIGHT_SHIFT) & SIZE_MASK) as u8 + 1,
        link: (size & LINK_MASK) as u8,
        pattern: NameEntry::decode(read_word(vram, entry + PATTERN_WORD * 2)),
    }
}

/// The VDP's own copy of the first four bytes of every entry of the SAT
/// (M4-23b, #103).
///
/// A sprite fetch is a walk of a table in VRAM and cannot afford to wait on the
/// bus for all eight bytes of every entry it touches, so the VDP keeps the y, the
/// size and the link of each one itself and reads only the pattern and the x from
/// VRAM. [`SpriteCache::write`] is the only thing that fills this, which makes the
/// cache a mirror of the table *as it was written* rather than of the table as it
/// is:
///
/// - a VRAM write whose address lies in the SAT area of the moment, and in the
///   first four bytes of one of its entries, updates that entry's line;
/// - moving the SAT with R5 refills nothing, so a table moved to under one that
///   was never written is drawn with the y, the size and the link of the old one
///   until the game writes the new entries. [`crate::latch`] is the same trick
///   played on the scroll tables by #102, and the tests of this file are what fix
///   the two apart.
///
/// It follows that the cache is chip state rather than line state: the chip takes
/// no VRAM write while a line is being drawn, so there is nothing for
/// [`crate::latch`] to keep it out of, and it is deliberately not part of
/// [`crate::latch::LineState`].
///
/// All zero to begin with, because that is what VRAM is to begin with: the two
/// agree before the chip has been talked to, and [`Vdp::new`] is the only thing
/// that clears VRAM, so a reset - which rebuilds the chip, `genesis::Bus` - leaves
/// them agreeing again.
#[derive(Debug)]
pub(crate) struct SpriteCache {
    /// One line per entry, holding `line[0..2]` the y word and `line[2..4]` the
    /// size/link word as big-endian bytes.
    lines: [[u8; ENTRY_CACHE_BYTES]; CACHE_ENTRIES],
}

impl SpriteCache {
    /// The cache of a table nobody has written yet: all zero, like VRAM.
    pub(crate) fn new() -> Self {
        Self {
            lines: [[0; ENTRY_CACHE_BYTES]; CACHE_ENTRIES],
        }
    }

    /// The cache line of entry `index`, or `None` for an index past the 80
    /// entries the cache holds. A link of 80..=127 therefore keeps reading VRAM,
    /// the one corner of the walk this model does not cache; see the TODO on
    /// [`SpriteList`] for what such an entry really reads.
    pub(crate) fn line(&self, index: usize) -> Option<&[u8; ENTRY_CACHE_BYTES]> {
        self.lines.get(index)
    }

    /// The byte `byte` written to VRAM byte `address`, of a table based at `base`:
    /// [`Vdp::write_vram_byte`] calls this for every write the chip makes, so the
    /// cache follows the table the SAT is on now rather than any table this model
    /// knows the name of.
    pub(crate) fn write(&mut self, address: usize, byte: u8, base: usize) {
        // `checked_sub` because an address below `base` is simply not in the
        // table, and a `usize` subtraction of the kind would wrap instead.
        let Some(offset) = address.checked_sub(base) else {
            return;
        };
        if offset >= CACHE_ENTRIES * ENTRY_BYTES {
            return;
        }
        let entry = offset / ENTRY_BYTES;
        let inside = offset % ENTRY_BYTES;
        if inside >= ENTRY_CACHE_BYTES {
            return;
        }
        self.lines[entry][inside] = byte;
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
    /// The VRAM the table is read from: the pattern and the x of every entry.
    vram: &'a [u8],
    /// The y, the size and the link of every entry, which the VDP does not go to
    /// VRAM for (#103).
    cache: &'a SpriteCache,
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
        let sprite = sprite_at(self.vram, self.cache, self.base, index);
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
    ///
    /// The y, the size and the link come from [`Vdp::sprite_cache`] and the
    /// pattern and the x from [`Vdp::vram`], which is what makes moving R5 between
    /// the two a move of half an entry (#103).
    pub(crate) fn sprite_list(&self) -> SpriteList<'_> {
        SpriteList {
            vram: &self.vram,
            cache: &self.sprite_cache,
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
    /// A sprite is `width_cells * height_cells` of the cells of a plane, [`CELL_WIDTH`]
    /// pixels across and [`Vdp::cell_height`] lines down, so at most 4 of them across
    /// and 4 down: four [`tile_row`] fetches per line per sprite at most, and no
    /// allocation. Which cell a sprite's pattern index names is the column-major rule
    /// of [`Vdp::fill_sprite_line`].
    ///
    /// `line` is a line of the picture, [`Vdp::display_line`] of the beam line, so in
    /// interlace mode 2 a sprite is 16 lines to a cell rather than 8 and `y` counts
    /// from [`ORIGIN_MODE2`] rather than from [`ORIGIN`] (#98).
    ///
    /// Both limits of section 15 "Sprite Drawing Limitations" that a line spends are
    /// counted by [`LineWalk`] as the list is walked, and both are spent by the
    /// sprites of the line rather than by the pixels the viewer sees: a sprite whose
    /// y does not reach `line` is not on the line and spends nothing, and one whose y
    /// does spends its own slot and its own dots whether or not any of them land on
    /// the display - "Sprites that are outside of the physical display area are still
    /// taken into account".
    ///
    /// Masking is genvdp.txt section 15 "Sprite masking, mode 1": "If a sprite has an
    /// X coordinate of zero, and has a Y coordinate that is within range of the
    /// display, then all sprites of lower priority will not be displayed on the lines
    /// which the sprite takes up", with the height of those lines from the vertical
    /// size bits and nothing else. A mask is a sprite of the line and spends its slot
    /// and its dots, and the sprites in front of it are unaffected: "sprites #0
    /// through #3 could still be displayed in this area".
    ///
    /// That the mask only bites once a sprite of a non-zero X has been found on the
    /// line before it, or once the line before has run out of dots, is INFERRED from
    /// Nemesis's masking test - genvdp.txt does not cover it, and there is no test
    /// ROM for it until M6-06. Masking mode 2, the X = 1 of genvdp.txt, is not
    /// implemented either: it has its own issue, #292.
    ///
    /// Collision is d5 of the status register, "Set when any sprites have
    /// non-transparent pixels overlapping one another" (section 6), and it is
    /// detected here as the second sprite of a pair finding a pixel already taken:
    /// [`Vdp::fill_sprite_line`] only writes a transparent pixel, so an overlap of
    /// two opaque ones is exactly the write it does not make. What the sprite did not
    /// draw collides with nothing, so a pixel clipped off the display, one the dot
    /// limit removed and the whole of a masked sprite are not detection material.
    /// genvdp.txt does not say whether the pixels off the display count - it lists
    /// "Sprite collisions and overflows outside of the physical display area" as an
    /// open question of its own - so this follows the display-area-only reading and
    /// the tests of #94 fix it in for M6-06 to change loudly.
    ///
    /// TODO(M4-16 #94): the masking of a sprite at x = 0.
    pub fn render_sprite_line(&mut self, line: u16, out: &mut [u8; WIDTH_H40]) {
        out.fill(0);
        let (max_sprites, max_dots) = if self.h40() {
            (MAX_SPRITES_LINE_H40, MAX_DOTS_LINE_H40)
        } else {
            (MAX_SPRITES_LINE_H32, MAX_DOTS_LINE_H32)
        };
        let prev_line_dot_overflow = self.prev_line_dot_overflow;
        let origin_y = self.sprite_y_origin();
        let cell_rows = self.cell_height();
        let mut walk = LineWalk::default();

        for sprite in self.sprite_list() {
            // The sprite occupies the lines `y - origin_y` to `y - origin_y` plus
            // its own height, and `row` is which row of that it is drawing. `line`
            // is a line of the picture, so in interlace mode 2 a sprite is twice
            // as tall as it is elsewhere and 256 of `y` is the top of the screen.
            let row = i32::from(line) - (i32::from(sprite.y) - origin_y);
            let height = (usize::from(sprite.height_cells) * cell_rows) as i32;
            if row < 0 || row >= height {
                continue;
            }
            let Some(pixels) = walk.admit(&sprite, max_sprites, max_dots) else {
                break;
            };
            if sprite.x == 0 {
                walk.mask(prev_line_dot_overflow);
            } else {
                walk.x_nonzero = true;
            }
            if !walk.masked {
                walk.collision |= self.fill_sprite_line(&sprite, row as usize, pixels, out);
            }
        }

        self.sprite_overflow |= walk.overflow;
        self.sprite_collision |= walk.collision;
        self.prev_line_dot_overflow = walk.dot_overflow;
    }

    /// Where the top of the display is in the y coordinates of the sprite table:
    /// [`ORIGIN`], or [`ORIGIN_MODE2`] in interlace mode 2, where y counts the
    /// doubled lines of the picture (#98).
    #[inline]
    fn sprite_y_origin(&self) -> i32 {
        if self.interlace_mode() == InterlaceMode::Mode2 {
            ORIGIN_MODE2
        } else {
            ORIGIN
        }
    }

    /// Draw `row` - the row of `sprite` counting down from its top, 0..the height
    /// of the sprite in pixels - over `out`, writing only the pixels that are
    /// still transparent; see [`Vdp::render_sprite_line`] for who that leaves.
    ///
    /// `pixels` is how much of the sprite the dot limit left it, and cells of
    /// [`CELL_WIDTH`] is the step it is cut in: INFERRED, and the rule the tests of
    /// this ticket fix in. genvdp.txt gives only the 320 and 256 of the limit and
    /// never says what a line that runs out of them does to the sprite it runs out
    /// on, and a sprite is fetched a cell at a time in the cell order of its own
    /// pattern, so what is left of one is its first `pixels / CELL_WIDTH` cells and
    /// not its first `pixels` pixels of pattern read any which way. The cells kept
    /// are the ones the loop reaches first, which are the leftmost ones on screen -
    /// the cut is at the right of the sprite, where a line runs out.
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
    fn fill_sprite_line(&self, sprite: &Sprite, row: usize, pixels: usize, out: &mut [u8]) -> bool {
        let NameEntry {
            priority,
            palette,
            vflip,
            hflip,
            tile,
        } = sprite.pattern;
        let height_cells = usize::from(sprite.height_cells);
        let cell_rows = self.cell_height();
        let mut collision = false;
        // The cell row of the sprite to fetch, mirrored by vflip; the row of the
        // pattern inside it stays `row % cell_rows` because `tile_row` mirrors that
        // with the same flag, and the two together are the one mirror. `cell_rows`
        // is the 16 of interlace mode 2 there, not the 8 of a normal cell (#98).
        let cell_row = if vflip {
            height_cells - 1 - row / cell_rows
        } else {
            row / cell_rows
        };
        let row_in_cell = row % cell_rows;
        let layer_base = (u16::from(palette) << 4) | (u16::from(priority) << PRIORITY_SHIFT);
        let left = i32::from(sprite.x) - ORIGIN;

        for cell in 0..pixels / CELL_WIDTH {
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
                cell_rows,
            );
            let x0 = left + (cell * CELL_WIDTH) as i32;
            for (offset, index) in indices.into_iter().enumerate() {
                let x = x0 + offset as i32;
                if x < 0 || usize::try_from(x).unwrap_or(usize::MAX) >= out.len() {
                    continue;
                }
                let x = x as usize;
                if index != 0 {
                    if out[x] == 0 {
                        out[x] = (layer_base + u16::from(index)) as u8;
                    } else {
                        // Two sprites, one pixel, both opaque: d5 of section 6.
                        collision = true;
                    }
                }
            }
        }
        collision
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
    /// Sprite pixels of this line fetched so far, of the [`MAX_DOTS_LINE_H40`] /
    /// [`MAX_DOTS_LINE_H32`] the display width allows.
    dots: usize,
    /// SOVR, from a sprite past either of the two limits of the line: see
    /// [`Vdp::sprite_overflow`].
    overflow: bool,
    /// Whether the dot limit cut one of the sprites of this line, which is what
    /// [`Vdp::prev_line_dot_overflow`] becomes: see [`Vdp::render_sprite_line`].
    dot_overflow: bool,
    /// d5: two sprites of this line drew an opaque pixel in the same place, and
    /// [`Vdp::fill_sprite_line`] is what noticed.
    collision: bool,
    /// A sprite of a non-zero X has been found on this line, which is what makes a
    /// mask of a later x = 0 one: see [`Vdp::render_sprite_line`].
    x_nonzero: bool,
    /// The sprites after the first mask of this line draw nothing.
    masked: bool,
}

impl LineWalk {
    /// Take one more sprite of the line and say how much of it may draw:
    ///
    /// * `None` for the sprite past the limit of `max_sprites`, which the VDP parses
    ///   but does not draw and stops the whole line at - genvdp.txt lists its limits
    ///   as things the VDP "will stop drawing sprites under".
    /// * the number of pixels of the sprite that the dot limit still leaves it,
    ///   `MAX_DOTS_LINE_H40` / [`MAX_DOTS_LINE_H32`] of them in all, with the whole
    ///   of the sprite counted against the limit whether or not any of it lands on
    ///   the display: "Sprites that are outside of the physical display area are
    ///   still taken into account".
    ///
    /// A sprite is the last thing the limit cuts in one step, so a sprite that does
    /// not fit takes the rest of the line and leaves nothing for the sprites after
    /// it, which are then cut to 0 pixels each. Every case of the two limits sets
    /// SOVR, which is what genvdp.txt section 15 has them all do.
    fn admit(&mut self, sprite: &Sprite, max_sprites: usize, max_dots: usize) -> Option<usize> {
        self.sprites += 1;
        if self.sprites > max_sprites {
            self.overflow = true;
            return None;
        }
        let dots = usize::from(sprite.width_cells) * CELL_WIDTH;
        let allowed = if self.dots + dots > max_dots {
            self.overflow = true;
            self.dot_overflow = true;
            max_dots - self.dots
        } else {
            dots
        };
        self.dots += allowed;
        Some(allowed)
    }

    /// A mask: the sprites after this one draw nothing on this line, and neither do
    /// the ones after it on the lines the mask reaches, which is why [`LineWalk`]
    /// ends at every line and this is asked again for the next one.
    ///
    /// Only once the line has something to mask: `previous_line_overflowed` is the
    /// dot overflow of the line before, and `x_nonzero` a sprite of a non-zero X on
    /// this one, either of them arming the mask and neither of them being the mask
    /// itself. INFERRED, and see [`Vdp::render_sprite_line`].
    fn mask(&mut self, previous_line_overflowed: bool) {
        self.masked |= self.x_nonzero || previous_line_overflowed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planes::testutil::{
        CELL_TILE_ROWS, fill_tile_row, fill_tile_row_of_height, put_tile_row, put_word,
    };

    /// SAT base of these tests: R5 = `0x78` is 0xF000 both under the H32 mask of
    /// `0x7F` and under the H40 one of `0x7E`, so changing the display width does
    /// not move the table (genvdp.txt section 17 "$05").
    const SPRITE_BASE: usize = 0xF000;

    /// The other table of the #103 tests: R5 = `0x70` is 0xE000 under the H32 mask
    /// these tests run in, far enough from [`SPRITE_BASE`] that the two 640-byte
    /// tables do not overlap.
    const OTHER_SPRITE_REG: u8 = 0x70;
    const OTHER_SPRITE_BASE: usize = 0xE000;

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
        put_sprite_at(vdp, SPRITE_BASE, index, y, x, cells, link, pattern);
    }

    /// [`put_sprite`] for a table at `base` rather than at [`SPRITE_BASE`], for
    /// the tests that move the SAT with R5 (#103).
    fn put_sprite_at(
        vdp: &mut Vdp,
        base: usize,
        index: usize,
        y: u16,
        x: u16,
        cells: (u8, u8),
        link: u8,
        pattern: u16,
    ) {
        let (width, height) = cells;
        let entry = base + index * ENTRY_BYTES;
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
            for row in 0..CELL_TILE_ROWS {
                fill_tile_row(vdp, first + n as u16, row, (n + 1) as u8);
            }
        }
    }

    /// `count` rows of `tile` filled with `colour`, the helper the flips and the
    /// row tests need because a sprite reads one row per line.
    fn solid_tile(vdp: &mut Vdp, tile: u16, colour: u8) {
        for row in 0..CELL_TILE_ROWS {
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
        let sprite = sprite_at(&vdp.vram, &vdp.sprite_cache, SPRITE_BASE, 3);
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
        let sprite = sprite_at(&vdp.vram, &vdp.sprite_cache, SPRITE_BASE, 3);
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
            vdp.render_sprite_line((cell_row * CELL_TILE_ROWS) as u16, &mut line);
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
    /// What an X of 0 means as well - the masking of every later sprite of those
    /// lines - is [`Vdp::render_sprite_line`], and it changes nothing here: both
    /// sprites are at x = 0, so the second is masked by the first and the line the
    /// two of them leave is the same blank one either way.
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
        for row in 0..CELL_TILE_ROWS {
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

    /// `count` solid tiles from `first`, all of `colour`: a sprite of any width
    /// whose pattern starts at `first` then draws `colour` in every cell of it,
    /// which the column-major numbering of [`Vdp::fill_sprite_line`] otherwise
    /// undoes because a wide sprite is several tiles and not one.
    fn solid_span(vdp: &mut Vdp, first: u16, count: usize, colour: u8) {
        for tile in 0..count {
            for row in 0..CELL_TILE_ROWS {
                fill_tile_row(vdp, first + tile as u16, row, colour);
            }
        }
    }

    /// The patterns of the 4-cell sprites of the dot tests: sprite `index` names the
    /// four tiles at `4 * index + 1` and is colour `index + 1` in all four of them,
    /// so the colour of a pixel says which sprite of the line drew it. Up to 15
    /// sprites, since a pattern pixel has 4 bits and colour 0 is transparency.
    fn wide_sprites(vdp: &mut Vdp, count: usize) {
        for index in 0..count {
            solid_span(vdp, 4 * index as u16 + 1, 4, index as u8 + 1);
        }
    }

    /// The 4-cell sprite of a dot test: `index` picks its pattern out of
    /// [`wide_sprites`], so it is solid `index + 1` for all 32 of its pixels.
    fn wide_sprite(vdp: &mut Vdp, index: usize, y: u16, x: u16, link: u8) {
        put_sprite(vdp, index, y, x, (4, 1), link, 4 * index as u16 + 1);
    }

    /// genvdp.txt section 15: "320 pixels worth of sprite data has been drawn on
    /// the same scanline in 40 cell mode", which eleven 4-cell sprites of 32 pixels
    /// overrun. The tenth ends the line at exactly 320, the eleventh has no dots
    /// left, and SOVR is set: every pixel of the display here belongs to one of the
    /// first ten, each of which drew all 32 pixels of itself.
    #[test]
    fn a_line_of_eleven_wide_sprites_is_cut_at_the_dot_limit() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        wide_sprites(&mut vdp, 11);
        for index in 0..11usize {
            wide_sprite(
                &mut vdp,
                index,
                128,
                128 + 32 * index as u16,
                index as u8 + 1,
            );
        }

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        for index in 0..10usize {
            let from = index * 32;
            assert_eq!(
                &line[from..from + 32],
                &[index as u8 + 1; 32],
                "sprite {index}, the last of them ending at the 320th dot"
            );
        }
        assert!(
            vdp.sprite_overflow,
            "352 pixels of sprite data on a line that holds 320"
        );
    }

    /// The dot limit is spent by the sprite data of the line, not by the pixels of it
    /// the viewer sees: "Sprites that are outside of the physical display area are
    /// still taken into account" (section 15). Ten 4-cell sprites off the right hand
    /// edge therefore leave nothing for the eleventh, which is cut to 0 pixels - the
    /// exact-zero case of a cut, and what makes the limit a `>` and not a `>=`.
    #[test]
    fn a_full_dot_line_cuts_a_later_sprite_to_nothing() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        wide_sprites(&mut vdp, 11);
        for index in 0..10usize {
            wide_sprite(&mut vdp, index, 128, 460, index as u8 + 1);
        }
        wide_sprite(&mut vdp, 10, 128, 128, 0);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            line,
            blank_line(),
            "the eleventh sprite draws nothing at all, not even its first cell"
        );
        assert!(vdp.sprite_overflow, "and the line overflowed to get there");
    }

    /// The other side of the same boundary: a line that uses exactly 320 dots of
    /// sprite data is a full line and not an overflowed one, so the sprite that
    /// fills it to its last dot still draws all 32 pixels of itself.
    #[test]
    fn a_line_that_uses_exactly_the_dot_limit_does_not_overflow() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        wide_sprites(&mut vdp, 10);
        for index in 0..9usize {
            wide_sprite(&mut vdp, index, 128, 460, index as u8 + 1);
        }
        wide_sprite(&mut vdp, 9, 128, 128, 0);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..32],
            &[10; 32],
            "the tenth sprite of the line is the one that filled it"
        );
        assert!(
            !vdp.sprite_overflow,
            "320 dots is the limit, and reaching it is not passing it"
        );
    }

    /// A sprite the dot limit cuts in mid-sprite draws the whole cells it has dots
    /// left for and no more: 320 - 288 - 24 = 8 of them, so the 4-cell sprite at the
    /// end of this line draws its first 8 pixels and loses its other three cells.
    /// INFERRED, see [`Vdp::fill_sprite_line`]: the fetch step of a sprite is one 8
    /// pixel cell, so a cut lands on a cell boundary and cuts the sprite from its
    /// right, the end the pixels of a line are fetched in.
    ///
    /// Returns whether any opaque pixel of it found an opaque pixel of an earlier
    /// sprite of the line already in place, which is d5 of the status register.
    ///
    /// The 288 and the 24 are spent outside the display, as in
    /// [`a_full_dot_line_cuts_a_later_sprite_to_nothing`].
    #[test]
    fn a_sprite_cut_by_the_dot_limit_draws_whole_cells() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        wide_sprites(&mut vdp, 13);
        solid_tile(&mut vdp, 1, 1);
        for index in 0..9usize {
            wide_sprite(&mut vdp, index, 128, 460, index as u8 + 1);
        }
        for index in 9..12 {
            put_sprite(&mut vdp, index, 128, 460, (1, 1), index as u8 + 1, 1);
        }
        put_sprite(&mut vdp, 12, 128, 128, (4, 1), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..8], &[1; 8], "the first cell of the cut sprite");
        assert_eq!(
            &line[8..32],
            &[0; 24],
            "and nothing of its other three cells, which had no dots left"
        );
        assert!(vdp.sprite_overflow, "344 dots on a 320 dot line");
    }

    /// genvdp.txt section 6 d5: "Set when any sprites have non-transparent pixels
    /// overlapping one another", and section 15's front-to-back rule means an overlap
    /// is the second sprite of the pair finding a pixel already taken. The two here
    /// overlap by 4 pixels, which is enough: one pixel of overlap is enough on real
    /// hardware too.
    #[test]
    fn overlapping_opaque_pixels_of_two_sprites_set_the_collision_flag() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 1);
        solid_tile(&mut vdp, 2, 2);
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 1, 1);
        put_sprite(&mut vdp, 1, 128, 128 + 4, (1, 1), 0, 2);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..8], &[1; 8], "the first sprite owns the overlap");
        assert_eq!(
            &line[8..12],
            &[2; 4],
            "and the second draws the rest of itself"
        );
        assert!(
            vdp.sprite_collision,
            "d5 C, set by the 4 pixels they both cover"
        );
    }

    /// The other half of "non-transparent": the second sprite lands on pixels the
    /// first does not draw, so nothing overlaps and the flag stays clear even though
    /// the two sprites sit on the very same 8 pixels.
    #[test]
    fn a_transparent_overlap_does_not_collide() {
        let mut vdp = sprite_vdp();
        put_tile_row(&mut vdp, 1, 0, &[0, 0, 0, 0, 5, 5, 5, 5]);
        put_tile_row(&mut vdp, 2, 0, &[9, 9, 9, 9, 0, 0, 0, 0]);
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 1, 1);
        put_sprite(&mut vdp, 1, 128, 128, (1, 1), 0, 2);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..4],
            &[9; 4],
            "the second sprite draws through the transparency of the first"
        );
        assert_eq!(
            &line[4..8],
            &[5; 4],
            "and the first through that of the second, the two covering one line"
        );
        assert!(
            !vdp.sprite_collision,
            "and a pixel the first sprite does not draw cannot collide with it"
        );
    }

    /// Nothing overlapping, nothing set: sprites that miss each other, on the line and
    /// down the frame, leave d5 alone.
    #[test]
    fn sprites_that_do_not_overlap_do_not_collide() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 1);
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 1, 1);
        put_sprite(&mut vdp, 1, 128, 128 + 8, (1, 1), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[0..16], [1; 16].as_slice(), "side by side");
        assert!(!vdp.sprite_collision, "touching is not overlapping");

        let mut below = sprite_vdp();
        put_sprite(&mut below, 0, 128, 128, (1, 1), 1, 1);
        put_sprite(&mut below, 1, 128 + 8, 128, (1, 1), 0, 1);
        below.render_sprite_line(0, &mut line);
        assert!(!below.sprite_collision, "the two are on different lines");
    }

    /// Only what the sprite draws can collide, so the pixels of it the dot limit
    /// removed collide with nothing: the same two sprites that overlap in
    /// [`overlapping_opaque_pixels_of_two_sprites_set_the_collision_flag`], with the
    /// second one reached by a line that has no dots left for it.
    #[test]
    fn a_sprite_cut_to_nothing_does_not_collide() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        wide_sprites(&mut vdp, 11);
        for index in 0..10usize {
            wide_sprite(&mut vdp, index, 128, 460, index as u8 + 1);
        }
        put_sprite(&mut vdp, 10, 128, 128, (1, 1), 0, 1);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(line, blank_line(), "the eleventh sprite is cut to nothing");
        assert!(!vdp.sprite_collision, "and so collides with nothing");
    }

    /// genvdp.txt section 15 "Sprite masking, mode 1": "If a sprite has an X
    /// coordinate of zero, and has a Y coordinate that is within range of the
    /// display, then all sprites of lower priority will not be displayed on the
    /// lines which the sprite takes up", and the worked example of that section
    /// keeps the earlier ones: "sprites #0 through #3 could still be displayed in
    /// this area".
    ///
    /// That the mask only bites once a sprite with a non-zero X has been found on the
    /// line before it is INFERRED, from Nemesis's masking test as the ticket cites
    /// and not from genvdp.txt: that is what the first sprite of this list is for,
    /// and [`a_mask_as_the_first_sprite_of_a_line_hides_nothing`] is the case without
    /// it.
    #[test]
    fn a_sprite_at_x_zero_hides_the_sprites_after_it_on_its_lines() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 1);
        solid_tile(&mut vdp, 2, 2);
        put_sprite(&mut vdp, 0, 128, 200, (1, 1), 1, 1);
        put_sprite(&mut vdp, 1, 128, 0, (1, 1), 2, 0);
        put_sprite(&mut vdp, 2, 128, 128, (1, 1), 0, 2);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[72..80],
            &[1; 8],
            "the sprite in front of the mask still draws"
        );
        assert_eq!(&line[0..8], &[0; 8], "and the sprite behind it does not");

        put_sprite(&mut vdp, 1, 128, 208, (1, 1), 2, 0);
        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..8],
            &[2; 8],
            "the same list with a sprite where the mask was does draw it"
        );
    }

    /// "The height of the sprite is determined by the vertical size bits in the
    /// sprite attributes", so an 8 pixel tall mask masks 8 lines and no more: the
    /// sprite of line 8 is behind the mask in the list and draws anyway.
    #[test]
    fn a_mask_covers_only_the_lines_of_its_own_sprite() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 1);
        solid_tile(&mut vdp, 2, 2);
        solid_tile(&mut vdp, 3, 3);
        put_sprite(&mut vdp, 0, 128, 200, (1, 1), 1, 1);
        put_sprite(&mut vdp, 1, 128, 0, (1, 1), 2, 0);
        put_sprite(&mut vdp, 2, 128, 128, (1, 1), 3, 2);
        put_sprite(&mut vdp, 3, 136, 128, (1, 1), 0, 3);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(&line[72..80], &[1; 8], "the sprite before the mask");
        assert_eq!(&line[0..8], &[0; 8], "line 0 is masked");

        let mut below = blank_line();
        vdp.render_sprite_line(8, &mut below);
        assert_eq!(
            &below[0..8],
            &[3; 8],
            "one cell of mask is eight lines, and line 8 is past them"
        );
    }

    /// The masking rule of Nemesis's test, INFERRED and pinned for M6-06: a mask with
    /// nothing but masks in front of it on the line has taken nothing to mask, so it
    /// masks nothing. genvdp.txt says nothing about this case at all.
    #[test]
    fn a_mask_as_the_first_sprite_of_a_line_hides_nothing() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 1);
        solid_tile(&mut vdp, 2, 2);
        put_sprite(&mut vdp, 0, 128, 0, (1, 1), 1, 0);
        put_sprite(&mut vdp, 1, 128, 128, (1, 1), 0, 2);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert_eq!(
            &line[0..8],
            &[2; 8],
            "no sprite of a non-zero x came before the mask on this line"
        );
    }

    /// The other half of the same rule: a line whose sprite dots ran out arms the
    /// mask of the next line even when nothing precedes it there. The overflow is
    /// taken from the line before, so this reads the lines in order: line 8, the one
    /// of the mask, alone draws, and line 8 again after line 0 has overflowed does
    /// not. The 11 wide sprites are 8 lines tall, so line 8 is the first line that
    /// is not one of them.
    #[test]
    fn a_mask_after_a_dot_overflowing_line_hides_even_as_the_first_sprite() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        wide_sprites(&mut vdp, 11);
        for index in 0..11usize {
            wide_sprite(
                &mut vdp,
                index,
                128,
                128 + 32 * index as u16,
                index as u8 + 1,
            );
        }
        // Tile 45, clear of the 44 tiles `wide_sprites` fills with its own colours.
        solid_tile(&mut vdp, 45, 2);
        put_sprite(&mut vdp, 11, 136, 0, (1, 1), 12, 0);
        put_sprite(&mut vdp, 12, 136, 128, (1, 1), 0, 45);

        let mut line = blank_line();
        vdp.render_sprite_line(8, &mut line);
        assert_eq!(
            &line[0..8],
            &[2; 8],
            "with nothing before it and no overflow behind it, no masking"
        );

        vdp.render_sprite_line(0, &mut line);
        assert!(vdp.sprite_overflow, "line 0 is the 352 dot one");
        vdp.render_sprite_line(8, &mut line);
        assert_eq!(
            &line[0..8],
            &[0; 8],
            "the same mask masks once the line before it ran out of dots"
        );
    }

    /// A count overflow is not a dot overflow: the field of [`Vdp`] says dots, the
    /// rule says dots, and the 21 sprites of line 0 here are 168 dots of a 320 dot
    /// line, so the mask that follows them on line 1 is still an unarmed one.
    #[test]
    fn a_count_overflow_on_the_previous_line_is_not_a_dot_overflow() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        solid_tile(&mut vdp, 1, 1);
        solid_tile(&mut vdp, 2, 2);
        put_line(&mut vdp, 128, 21, |_| 460);
        put_sprite(&mut vdp, 21, 136, 0, (1, 1), 22, 0);
        put_sprite(&mut vdp, 22, 136, 128, (1, 1), 0, 2);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert!(vdp.sprite_overflow, "the 21st sprite of the line");

        vdp.render_sprite_line(8, &mut line);
        assert_eq!(
            &line[0..8],
            &[2; 8],
            "an overflow of sprites is not an overflow of dots"
        );
    }

    /// The mask is a sprite of the line, and spends a slot of it: "The height of the
    /// sprite is determined by the vertical size bits in the sprite attributes; other
    /// factors like horizontal size, pattern data used, priority bit, and color
    /// palette have no effect" (genvdp.txt section 15), and the section's own rule
    /// that off-display sprites count. Twenty sprites and a mask are therefore 21
    /// sprites, which is an overflow.
    #[test]
    fn a_mask_counts_toward_the_sprite_limit_of_the_line() {
        let mut vdp = sprite_vdp();
        in_h40(&mut vdp);
        solid_tile(&mut vdp, 1, 7);
        put_line(&mut vdp, 128, 20, |i| 128 + 8 * i as u16);
        put_sprite(&mut vdp, 20, 128, 0, (1, 1), 0, 0);

        let mut line = blank_line();
        vdp.render_sprite_line(0, &mut line);
        assert!(
            vdp.sprite_overflow,
            "the mask is the 21st sprite of the line, and is counted as one"
        );
    }

    // ---------------------------------------------------------------------
    // Interlace mode 2 (M4-20 #98): 8x16 cells and the 256 origin.
    // ---------------------------------------------------------------------

    /// Rows of a sprite cell of interlace mode 2 (genvdp.txt section 12: "A
    /// pattern is an 8x8 or 8x16 (interlace mode 2 only) pixel block").
    const MODE2_ROWS: usize = 16;

    /// `tile` as a 16-row pattern of `colour`.
    fn solid_cell_16(vdp: &mut Vdp, tile: u16, colour: u8) {
        for row in 0..MODE2_ROWS {
            fill_tile_row_of_height(vdp, tile, row, colour, MODE2_ROWS);
        }
    }

    /// Interlace mode 2 doubles the height of a sprite cell and moves the origin of
    /// the y coordinate from 128 to 256 (ticket #98: "Sprite Y is 10 bits with
    /// offset 256 instead of 128, sprite heights double"), so a 2-cell sprite at
    /// `y = 256` covers display lines 0..32: the first cell of 16 lines, then the
    /// second, and nothing at line 32.
    #[test]
    fn interlace_mode_2_sprites_are_8x16_from_an_origin_of_256() {
        let mut vdp = sprite_vdp();
        vdp.write_register(12, 0x06);
        solid_cell_16(&mut vdp, 1, 1);
        solid_cell_16(&mut vdp, 2, 2);
        put_sprite(&mut vdp, 0, 256, 128, (1, 2), 0, 1);

        for line in [0u16, 15] {
            let mut out = blank_line();
            vdp.render_sprite_line(line, &mut out);
            assert_eq!(
                &out[0..8],
                &[1; 8],
                "display line {line} is still the first cell of the sprite"
            );
        }
        for line in [16u16, 31] {
            let mut out = blank_line();
            vdp.render_sprite_line(line, &mut out);
            assert_eq!(
                &out[0..8],
                &[2; 8],
                "display line {line} is the cell below it, 16 lines down"
            );
        }

        let mut out = blank_line();
        vdp.render_sprite_line(32, &mut out);
        assert_eq!(
            out,
            blank_line(),
            "two cells of 16 lines end at display line 32"
        );
    }

    /// The other half of the 256 origin: the `y` that sat a sprite on the top line
    /// of an uninterlaced display is half the picture above it in mode 2, so it
    /// draws on no line at all.
    #[test]
    fn interlace_mode_2_moves_the_sprite_y_origin_to_256() {
        let mut vdp = sprite_vdp();
        vdp.write_register(12, 0x06);
        solid_cell_16(&mut vdp, 1, 3);
        put_sprite(&mut vdp, 0, 128, 128, (1, 1), 0, 1);

        for line in [0u16, 15] {
            let mut out = blank_line();
            vdp.render_sprite_line(line, &mut out);
            assert_eq!(
                out,
                blank_line(),
                "y = 128 is 128 lines of the picture above the display in mode 2"
            );
        }
    }

    // The internal sprite attribute cache of M4-23b (#103): what the VDP keeps of
    // the SAT, and what it therefore does not go back to VRAM for.

    /// #103 test 1: "Write a SAT at 0xF000, render: sprite visible. Move R5 to
    /// point at 0xE000 ... Y/size/link still come from the old cache, X/tile from
    /// the new table."
    ///
    /// The second table is written while R5 still names the first, which is what
    /// keeps it out of the cache: only a write inside the SAT of the moment
    /// updates it. Moving the table is not such a write, and nothing else
    /// refills it either - "Games (and test ROMs) see stale Y/size/link until
    /// they rewrite the entries".
    #[test]
    fn a_moved_sat_table_keeps_the_y_size_and_link_of_the_old_one() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 3);
        solid_tile(&mut vdp, 2, 5);

        // The table R5 points at now: one 8x8 sprite on the top line at x 10,
        // the end of the list.
        put_sprite(&mut vdp, 0, 128, 128 + 10, (1, 1), 0, 1);
        // And the table R5 is about to point at. Its entry 0 is 32 lines below
        // the top of the display and links on to entry 1, which is on it, so a
        // walk of *that* list draws at x 60 and not at all on line 0.
        put_sprite_at(
            &mut vdp,
            OTHER_SPRITE_BASE,
            0,
            128 + 32,
            128 + 20,
            (1, 1),
            1,
            2,
        );
        put_sprite_at(&mut vdp, OTHER_SPRITE_BASE, 1, 128, 128 + 60, (1, 1), 0, 2);

        let mut before = blank_line();
        vdp.render_sprite_line(0, &mut before);
        assert_eq!(
            &before[10..18],
            &[3; 8],
            "the table at 0xF000 draws its one sprite"
        );

        vdp.write_register(5, OTHER_SPRITE_REG);

        // Entry 0 of the new table, as the VDP still sees it: the y, the size and
        // the link of the old one - so on this line, and the list ends with it -
        // and the pattern and the x of the new one.
        let mut after = blank_line();
        vdp.render_sprite_line(0, &mut after);
        let mut want = [0; WIDTH_H40];
        want[20..28].fill(5);
        assert_eq!(after, want, "x and the pattern word are read from VRAM");
    }

    /// The other half of the same rule, in the direction a game actually goes in:
    /// "Games ... see stale Y/size/link *until they rewrite the entries*", so a
    /// write to the table after the move is a write to the cache.
    #[test]
    fn a_rewrite_after_the_move_is_what_refreshes_the_cache() {
        let mut vdp = sprite_vdp();
        solid_tile(&mut vdp, 1, 3);
        put_sprite(&mut vdp, 0, 128, 128 + 10, (1, 1), 0, 1);

        vdp.write_register(5, OTHER_SPRITE_REG);
        put_sprite_at(
            &mut vdp,
            OTHER_SPRITE_BASE,
            0,
            128 + 8,
            128 + 20,
            (1, 1),
            0,
            1,
        );

        let mut line = blank_line();
        vdp.render_sprite_line(8, &mut line);
        assert_eq!(
            &line[20..28],
            &[3; 8],
            "0xE000 is the SAT now, so its writes are cached"
        );
    }

    /// "Any VRAM write whose address falls in `[sat_base, sat_base + 80 * 8)` and
    /// in the first 4 bytes of an entry also updates the cache", and the other
    /// four bytes of an entry never do: the pattern word and the x stay VRAM's.
    #[test]
    fn only_the_first_four_bytes_of_an_entry_are_cached() {
        let mut vdp = sprite_vdp();
        // y = 0x1234, size = 2 cells across and 3 down with link 9, which is the
        // word `(1 << 10) | (2 << 8) | 9` = 0x0609.
        put_sprite(&mut vdp, 0, 0x1234, 0x0ABC, (2, 3), 9, 0x7654);
        assert_eq!(
            vdp.sprite_cache.line(0).copied(),
            Some([0x12, 0x34, 0x06, 0x09]),
            "the y word and the size/link word of the entry, as VRAM holds them"
        );
        assert_eq!(
            vdp.sprite_cache.line(1).copied(),
            Some([0; ENTRY_CACHE_BYTES]),
            "an entry nobody has written is still the zero VRAM starts at"
        );

        put_word(&mut vdp, SPRITE_BASE + 4, 0xFFFF);
        put_word(&mut vdp, SPRITE_BASE + 6, 0xFFFF);
        assert_eq!(
            vdp.sprite_cache.line(0).copied(),
            Some([0x12, 0x34, 0x06, 0x09]),
            "the pattern and x words of an entry are not cache material"
        );
    }
}
