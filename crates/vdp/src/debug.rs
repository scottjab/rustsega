//! Debug views of the VDP: the pattern, plane, sprite and palette state of the
//! chip as RGBA buffers and a plain list (M4-24, #104).
//!
//! These are a second, slow renderer over the memories and registers the real one
//! of [`crate::planes`], [`crate::sprites`] and [`crate::compose`] already reads,
//! for `headless` and the web UI to show and for an agent to work out where a
//! frame went wrong: what the 2048 patterns of VRAM look like, what a name table
//! holds before the scroll is put to it, what CRAM holds, and what the sprite
//! list of the moment walks to. Allocation is fine here and slowness is too -
//! nothing of this runs on the beam.
//!
//! Behind the `debug-views` cargo feature, default off, so that neither the
//! emulator nor the WASM build of the core crates carries it. It is tested with
//! `cargo test -p vdp --features debug-views`, which `just check` runs as its
//! own step: without the feature the module is not compiled at all, so neither
//! `just test` nor `just clippy` would notice it breaking.
//!
//! Pixels are [`crate::framebuffer`]'s `0xAABBGGRR` at [`Intensity::Normal`]:
//! shadow, highlight and the sprite operators are the compositor's business and a
//! dump of one plane or palette says nothing about them.

use crate::Vdp;
use crate::color::{self, Intensity};
use crate::pattern::tile_row;
use crate::planes::{self, CELL_ROWS, CELL_WIDTH};
use crate::window::TABLE_ROWS;

/// Patterns of the pattern dump: every 8x8 pattern an 11-bit name can number,
/// the 2048 of them filling [VRAM](crate::VRAM_SIZE) exactly
/// (genvdp.txt section 13, and [`crate::pattern::tile_addr`]).
const TILES: usize = 2048;
/// Tiles across the pattern dump: the width of the plane view next to it, so the
/// two line up on a screen. [`TILES`] / 32 = 64 rows, so the dump is 256 x 512.
const TILE_COLUMNS: usize = 32;
/// CRAM entries across the palette view, and the count of 16-colour lines its
/// four rows are made of (genvdp.txt section 9: 64 words).
const PALETTE_COLUMNS: usize = 16;
const PALETTE_LINES: usize = 4;
/// Size of one swatch of the palette view, in pixels.
const SWATCH: usize = 8;
/// The outline of the visible area of a plane view.
const OUTLINE: u32 = 0xFFFF_FFFF;

/// Bits 0-5 of a layer pixel: its CRAM index, bit 6 being
/// [`planes::PRIORITY_SHIFT`]'s priority flag. The same mask
/// [`crate::compose`] reads a layer buffer with.
const LAYER_INDEX_MASK: u8 = (1 << planes::PRIORITY_SHIFT) - 1;

/// An RGBA image a debug view produced: `width * height` pixels of
/// `0xAABBGGRR`, row-major, stride `width`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugImage {
    /// Pixels across the image.
    pub width: usize,
    /// Lines of the image.
    pub height: usize,
    /// The pixels, `width * height` of them.
    pub pixels: Vec<u32>,
}

impl DebugImage {
    /// A black image of `width` x `height`.
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0xFF00_0000; width * height],
        }
    }

    /// The pixel at `(x, y)`.
    ///
    /// Panics past the edge: a caller of a debug view knows the size it asked
    /// for, and a view that got it wrong should say so loudly.
    pub fn pixel(&self, x: usize, y: usize) -> u32 {
        self.pixels[y * self.width + x]
    }

    /// Set the pixel at `(x, y)`.
    fn set(&mut self, x: usize, y: usize, pixel: u32) {
        self.pixels[y * self.width + x] = pixel;
    }

    /// Paint the 1 px [`OUTLINE`] border of the `width` x `height` rectangle
    /// whose top-left corner is the image's, clamped to the image: the plane of a
    /// small R16 is smaller than the area it is seen through.
    fn outline(&mut self, width: usize, height: usize) {
        let width = width.min(self.width);
        let height = height.min(self.height);
        if width == 0 || height == 0 {
            return;
        }
        let last_x = width - 1;
        let last_y = height - 1;
        for x in 0..width {
            self.set(x, 0, OUTLINE);
            self.set(x, last_y, OUTLINE);
        }
        for y in 0..height {
            self.set(0, y, OUTLINE);
            self.set(last_x, y, OUTLINE);
        }
    }
}

/// Which name table [`plane`] dumps: the two scroll planes of
/// [`crate::planes::Plane`] and the window of [`crate::window`].
///
/// [`crate::planes::Plane`] holds only the two scroll planes, and rightly so for
/// the renderer - the window has no scroll, no R16 size and a fixed table of its
/// own. A dump does not care which of the three a fetch belongs to, so this is
/// the three-way choice of this module rather than a third variant of the
/// renderer's enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneView {
    /// Plane A, name table at [`Vdp::plane_a_base`] (genvdp.txt section 17 "$02").
    A,
    /// Plane B, name table at [`Vdp::plane_b_base`] (genvdp.txt section 17 "$04").
    B,
    /// The window plane, name table at [`Vdp::window_base`] (genvdp.txt
    /// section 17 "$03", and section 13 "Window").
    Window,
}

impl PlaneView {
    /// The name table this view dumps: its base and its size in cells. The two
    /// scroll planes take their size from R16; the window's is fixed by the
    /// display width alone - genvdp.txt section 13: "The name table for plane W
    /// is 32x32 in 32-cell mode, and 64x32 in 40-cell mode. This size is fixed
    /// and is entirely dependant on the display width."
    fn table(self, vdp: &Vdp) -> (usize, usize, usize) {
        match self {
            PlaneView::A => (
                usize::from(vdp.plane_a_base()),
                usize::from(vdp.plane_width_cells()),
                usize::from(vdp.plane_height_cells()),
            ),
            PlaneView::B => (
                usize::from(vdp.plane_b_base()),
                usize::from(vdp.plane_width_cells()),
                usize::from(vdp.plane_height_cells()),
            ),
            PlaneView::Window => (
                usize::from(vdp.window_base()),
                vdp.window_table_cells(),
                TABLE_ROWS,
            ),
        }
    }

    /// Whether the view draws the rectangle of the area it is seen through: the
    /// ticket asks for it of the two scroll planes, whose size is set by a game
    /// and can be anything from 256 x 256 up.
    fn outlines_visible_area(self) -> bool {
        match self {
            PlaneView::A | PlaneView::B => true,
            PlaneView::Window => false,
        }
    }
}

/// One entry of the sprite list, for a text dump: the five fields of genvdp.txt
/// section 15 as [`crate::sprites`] decodes them, flattened for printing.
///
/// [`sprite_list`] walks the link chain, so this holds no `Vec` and needs no
/// borrow of the VDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteInfo {
    /// The SAT entry this line describes: 0 for the first of the walk and the
    /// `link` of the entry before it otherwise. The order of the list is the
    /// order of the chain, which is not the order of the entries.
    pub index: u8,
    /// The x of the entry in the virtual 512x512 sprite space, screen at 128,128
    /// (genvdp.txt section 15), unmoved from what the table holds.
    pub x: u16,
    /// The y of the entry, in the same space.
    pub y: u16,
    /// Width in pixels: the entry's width in cells of 8 (genvdp.txt section 15
    /// word 1 bits 11-10 plus one), so 8, 16, 24 or 32.
    pub w: u8,
    /// Height in pixels, as [`SpriteInfo::w`].
    pub h: u8,
    /// The link field: the index of the next entry, 0 to stop.
    pub link: u8,
    /// The pattern name of word 2.
    pub tile: u16,
    /// The palette line of word 2, 0..=3.
    pub palette: u8,
    /// The priority flag of word 2.
    pub priority: bool,
    /// The horizontal flip of word 2.
    pub hflip: bool,
    /// The vertical flip of word 2.
    pub vflip: bool,
}

/// The CRAM word `index` as a pixel of the dump: the 6-bit index of a layer
/// pixel, which is all of CRAM a name table entry can name
/// (genvdp.txt section 9's 64 words).
fn cram_pixel(vdp: &Vdp, index: u8) -> u32 {
    let word = vdp.cram[usize::from(index & LAYER_INDEX_MASK)];
    color::color(word, Intensity::Normal)
}

/// The 64 CRAM words as 16 swatches of [`SWATCH`] pixels apiece, [`PALETTE_LINES`] rows
/// deep: 128 x 32 pixels, genvdp.txt section 9's 64 colours in address order.
///
/// Word 0 is the top-left swatch, which is the colour of the backdrop of a VDP
/// whose R7 is zero and so the one worth seeing first.
pub fn palette(vdp: &Vdp) -> DebugImage {
    let mut image = DebugImage::new(PALETTE_COLUMNS * SWATCH, PALETTE_LINES * SWATCH);
    for (index, &word) in vdp.cram.iter().enumerate() {
        let x = (index % PALETTE_COLUMNS) * SWATCH;
        let y = (index / PALETTE_COLUMNS) * SWATCH;
        let pixel = color::color(word, Intensity::Normal);
        for row in y..y + SWATCH {
            for column in x..x + SWATCH {
                image.set(column, row, pixel);
            }
        }
    }
    image
}

/// Every pattern of VRAM as [`TILE_COLUMNS`] tiles to a row, in the palette line
/// `palette` (bits 1-0; the CRAM words `palette * 16 + index`).
///
/// 2048 tiles of 8x8 = 256 x 512 pixels. Colour 0 is a colour here, not
/// transparency: the dump shows a pattern as VRAM holds it, and only the
/// compositor of [`crate::compose`] decides that a 0 is not drawn.
///
/// The patterns are read as [`CELL_ROWS`] rows of 8x8, [`crate::pattern::tile_addr`]'s
/// 32-byte stride, whatever the mode: interlace mode 2's 8x16 patterns are the
/// same bytes and show here as two rows of the grid.
pub fn tiles(vdp: &Vdp, palette: u8) -> DebugImage {
    let base = (palette & 0x03) << 4;
    let mut image = DebugImage::new(
        TILE_COLUMNS * CELL_WIDTH,
        TILES.div_ceil(TILE_COLUMNS) * CELL_ROWS,
    );
    for tile in 0..TILES {
        let x = (tile % TILE_COLUMNS) * CELL_WIDTH;
        let y = (tile / TILE_COLUMNS) * CELL_ROWS;
        for row in 0..CELL_ROWS {
            let indices = tile_row(&vdp.vram, tile as u16, row, false, false, CELL_ROWS);
            for (offset, index) in indices.iter().enumerate() {
                image.set(x + offset, y + row, cram_pixel(vdp, base + *index));
            }
        }
    }
    image
}

/// The whole of one name table as its own picture, with no scroll on it:
/// cells across and down of the view, in the mode and the R16 size the chip is
/// in now, with the [`OUTLINE`] of the visible area over the two scroll planes.
///
/// The cells are fetched by [`Vdp::fill_cell_run`], the one fetch all three name
/// tables end with, so the decode of genvdp.txt section 13 - the tile, the
/// flips, the palette line and the colour 0 that is not drawn - is the
/// renderer's and not a second reading of it. A pixel the cell draws nothing at
/// reads as CRAM 0, which is what the compositor falls back to for it too.
pub fn plane(vdp: &Vdp, view: PlaneView) -> DebugImage {
    let (base, columns, rows) = view.table(vdp);
    // A cell is as tall as the mode says - 16 lines of interlace mode 2 - which
    // is why a mode 2 dump is twice as tall as R16's cell count.
    let cell_rows = vdp.cell_height();
    let width = columns * CELL_WIDTH;
    let height = rows * cell_rows;
    let mut image = DebugImage::new(width, height);

    let mut run = [0u8; CELL_WIDTH];
    for y in 0..height {
        let cell_row = y / cell_rows;
        let row = y % cell_rows;
        for column in 0..columns {
            // The same 8 KiB name-table wrap [`crate::planes`] applies, so a
            // 128x128 R16 - which that file quotes genvdp.txt section 13 calling
            // invalid - dumps as the wrapped table the fetch reads.
            let entry = (cell_row * columns + column) & (planes::MAX_ENTRIES - 1);
            vdp.fill_cell_run(base, entry, row, 0, &mut run);
            for (offset, index) in run.iter().enumerate() {
                image.set(column * CELL_WIDTH + offset, y, cram_pixel(vdp, *index));
            }
        }
    }

    if view.outlines_visible_area() {
        image.outline(vdp.active_width(), vdp.frame_height());
    }
    image
}

/// The sprite list of the SAT as it stands now, in link order, for a text dump:
/// [`Vdp::sprite_list`]'s walk of the chain of genvdp.txt section 15, flattened
/// to [`SpriteInfo`]s.
///
/// This is the live walk, not the list of the line being drawn: what
/// [`crate::sprites`] searched one line early is chip state no view of the
/// moment can reach without stopping the beam, and a dump of it is a different
/// question.
pub fn sprite_list(vdp: &Vdp) -> Vec<SpriteInfo> {
    let mut list = Vec::new();
    let mut index = 0u8;
    for sprite in vdp.sprite_list() {
        list.push(SpriteInfo {
            index,
            x: sprite.x,
            y: sprite.y,
            w: sprite.width_cells * CELL_WIDTH as u8,
            h: sprite.height_cells * CELL_WIDTH as u8,
            link: sprite.link,
            tile: sprite.pattern.tile,
            palette: sprite.pattern.palette,
            priority: sprite.pattern.priority,
            hflip: sprite.pattern.hflip,
            vflip: sprite.pattern.vflip,
        });
        // The link of the last entry ends the walk and is not followed; `index`
        // is left where the iterator stopped for want of a next entry.
        index = sprite.link;
    }
    list
}

#[cfg(test)]
mod tests {
    use crate::Vdp;
    use crate::color::{self, Intensity};
    use crate::planes::testutil::{fill_tile_row, put_entry, put_sprite_at, put_word};
    use crate::window::TABLE_ROWS;

    use super::{DebugImage, PlaneView, palette, plane, sprite_list, tiles};

    /// SAT base of these tests: R5 = `0x78` is 0xF000 under both the H32 and the
    /// H40 address mask (genvdp.txt section 17 "$05"), so the display width of a
    /// test does not move the table.
    const SPRITE_BASE: usize = 0xF000;
    /// Window name table base: R3 = `0x28` is 0xA000, clear of the plane tables
    /// and of the patterns every `solid_tile` writes at the bottom of VRAM.
    const WINDOW_BASE: usize = 0xA000;
    /// Plane A name table base: R2 = `0x30` is 0xC000 (16 KiB steps).
    const PLANE_A_BASE: usize = 0xC000;
    /// Plane B name table base: R4 = `0x04` is 0x8000, clear of the window
    /// table, the scroll table and the patterns at the bottom of VRAM.
    const PLANE_B_BASE: usize = 0x8000;
    /// CRAM word for red: `----bbb-ggg-rrr-` with rrr = 1 (genvdp.txt section 9).
    const RED: u16 = 0x0002;
    /// CRAM word for blue.
    const BLUE: u16 = 0x0800;
    /// Normal-intensity pixels are what every view paints with, so a test can
    /// name the pixel a CRAM word has to reach rather than spelling the DAC
    /// table out again.
    fn pixel_of(cram: u16) -> u32 {
        color::color(cram, Intensity::Normal)
    }

    /// `tile` filled with colour index `colour` in all 8 of its rows.
    fn solid_tile(vdp: &mut Vdp, tile: u16, colour: u8) {
        for row in 0..8 {
            fill_tile_row(vdp, tile, row, colour);
        }
    }

    /// True when every pixel of the `8` x `8` block of `image` whose top-left
    /// corner is `x`, `y` is `want`.
    fn block_is(image: &DebugImage, x: usize, y: usize, want: u32) -> bool {
        area_is(image, x, y, 8, 8, want)
    }

    /// [`block_is`] for a block of any size.
    fn area_is(image: &DebugImage, x: usize, y: usize, w: usize, h: usize, want: u32) -> bool {
        (y..y + h).all(|py| (x..x + w).all(|px| image.pixel(px, py) == want))
    }

    /// A VDP with its SAT at [`SPRITE_BASE`], its window table at [`WINDOW_BASE`]
    /// and plane A at [`PLANE_A_BASE`].
    fn vdp_with_tables() -> Vdp {
        let mut vdp = Vdp::new(false);
        vdp.write_register(5, 0x78);
        vdp.write_register(3, 0x28);
        vdp.write_register(2, 0x30);
        vdp.write_register(4, 0x04);
        vdp
    }

    // ---- palette() ----

    #[test]
    fn palette_view_is_128x32_and_its_first_swatch_is_cram_zero() {
        let mut vdp = Vdp::new(false);
        vdp.cram[0] = RED;
        let image = palette(&vdp);
        assert_eq!(image.width, 128);
        assert_eq!(image.height, 32);
        assert!(
            block_is(&image, 0, 0, pixel_of(RED)),
            "CRAM[0] is the whole of the top-left 8x8 block"
        );
    }

    #[test]
    fn palette_view_lays_the_64_cram_words_in_four_rows_of_sixteen() {
        let mut vdp = Vdp::new(false);
        for (index, word) in vdp.cram.iter_mut().enumerate() {
            // Every entry a different colour, by walking the 9 bits of CRAM.
            *word = crate::color::color_word(index);
        }
        let image = palette(&vdp);
        for index in 0..64 {
            let x = (index % 16) * 8;
            let y = (index / 16) * 8;
            let want = pixel_of(crate::color::color_word(index));
            assert!(
                block_is(&image, x, y, want),
                "CRAM entry {index} is the swatch at {x},{y}"
            );
        }
    }

    // ---- tiles() ----

    #[test]
    fn tiles_view_draws_tile_one_at_eight_by_zero_and_is_256x512() {
        let mut vdp = Vdp::new(false);
        vdp.cram[0] = BLUE;
        vdp.cram[1] = RED;
        solid_tile(&mut vdp, 1, 1);
        let image = tiles(&vdp, 0);
        assert_eq!(image.width, 256);
        assert_eq!(image.height, 512);
        assert!(
            block_is(&image, 8, 0, pixel_of(RED)),
            "tile 1, solid colour 1, is the second cell of the first row"
        );
        assert!(
            block_is(&image, 0, 0, pixel_of(BLUE)),
            "tile 0 is all index 0 of the same palette line"
        );
        assert!(
            block_is(&image, 16, 0, pixel_of(BLUE)),
            "and tile 2 is nothing but index 0 too"
        );
    }

    #[test]
    fn tiles_view_wraps_the_grid_at_thirty_two_tiles() {
        let mut vdp = Vdp::new(false);
        vdp.cram[2] = RED;
        solid_tile(&mut vdp, 32, 2);
        let image = tiles(&vdp, 0);
        assert!(
            block_is(&image, 0, 8, pixel_of(RED)),
            "tile 32 is the first cell of the second row"
        );
    }

    #[test]
    fn tiles_view_reads_the_palette_line_it_is_given() {
        let mut vdp = Vdp::new(false);
        vdp.cram[1] = BLUE;
        vdp.cram[32 + 1] = RED;
        solid_tile(&mut vdp, 1, 1);
        let image = tiles(&vdp, 2);
        assert!(
            block_is(&image, 8, 0, pixel_of(RED)),
            "palette 2 reads CRAM 32 + index"
        );
    }

    // ---- plane() ----

    #[test]
    fn plane_view_size_follows_r16_and_not_the_screen() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(16, 0x11);
        let image = plane(&vdp, PlaneView::A);
        assert_eq!(vdp.plane_width_cells(), 64);
        assert_eq!(vdp.plane_height_cells(), 64);
        assert_eq!(image.width, 64 * 8);
        assert_eq!(image.height, 64 * 8);
    }

    #[test]
    fn plane_view_outlines_the_visible_area_and_stops_where_the_plane_does() {
        let vdp = Vdp::new(false);
        let image = plane(&vdp, PlaneView::A);
        let white = 0xFFFF_FFFF;
        assert_eq!(image.width, 256);
        assert_eq!(image.height, 256);
        assert_eq!(image.pixel(0, 0), white);
        assert_eq!(image.pixel(255, 0), white);
        assert_eq!(image.pixel(0, 223), white);
        assert_eq!(image.pixel(255, 223), white);
        assert_ne!(image.pixel(128, 224), white, "the outline is a rectangle");
        assert_ne!(image.pixel(1, 1), white, "and only one pixel wide");
        assert_ne!(image.pixel(128, 128), white, "with the plane inside it");
    }

    #[test]
    fn plane_view_draws_the_name_table_and_ignores_the_scroll() {
        let mut vdp = vdp_with_tables();
        vdp.cram[0x21] = RED;
        solid_tile(&mut vdp, 5, 1);
        // Entry 0: palette line 2 (bits 14-13), tile 5.
        put_entry(&mut vdp, PLANE_A_BASE, 0, 0x4005);
        // Horizontal scroll of plane A far enough right that a scrolled read of
        // the plane could not answer at x 0 with cell 0.
        vdp.write_register(11, 0x01);
        vdp.write_register(13, 0x3F);
        for line in 0..256usize {
            put_word(&mut vdp, 0xFC00 + line * 4, 0x0F8);
        }

        let image = plane(&vdp, PlaneView::A);
        // The 7x7 inside cell 0, clear of the 1 px outline of the visible area.
        assert!(
            area_is(&image, 1, 1, 7, 7, pixel_of(RED)),
            "the plane is dumped from its own origin, scroll and vscroll and all"
        );
        assert!(
            !area_is(&image, 8, 1, 7, 7, pixel_of(RED)),
            "and it is one cell of the table, not a fill: cell 1 is empty"
        );
    }

    #[test]
    fn plane_view_b_is_the_table_of_r4_and_plane_a_the_table_of_r2() {
        let mut vdp = vdp_with_tables();
        vdp.cram[0x11] = RED;
        solid_tile(&mut vdp, 6, 1);
        put_entry(&mut vdp, PLANE_B_BASE, 0, 0x2006);

        let b = plane(&vdp, PlaneView::B);
        assert!(
            area_is(&b, 1, 1, 7, 7, pixel_of(RED)),
            "plane B is dumped from R4's table, not R2's"
        );
        let a = plane(&vdp, PlaneView::A);
        assert_ne!(a.pixel(4, 4), pixel_of(RED), "and plane A is still empty");
    }

    #[test]
    fn window_view_is_the_fixed_table_of_the_display_width() {
        let mut vdp = vdp_with_tables();
        vdp.cram[0x11] = RED;
        solid_tile(&mut vdp, 3, 1);
        put_entry(&mut vdp, WINDOW_BASE, 0, 0x2003);

        let image = plane(&vdp, PlaneView::Window);
        assert_eq!(image.width, 32 * 8, "32 cells across in H32");
        assert_eq!(image.height, TABLE_ROWS * 8);
        assert!(block_is(&image, 0, 0, pixel_of(RED)));

        let mut h40 = vdp_with_tables();
        h40.write_register(12, 0x81);
        let wide = plane(&h40, PlaneView::Window);
        assert_eq!(wide.width, 64 * 8, "64 cells across in H40");
        assert_eq!(wide.height, TABLE_ROWS * 8);
    }

    // ---- sprite_list() ----

    #[test]
    fn sprite_list_view_follows_the_link_chain_and_reports_every_field() {
        let mut vdp = vdp_with_tables();
        put_sprite_at(&mut vdp, SPRITE_BASE, 0, 10, 20, (2, 3), 2, 0x8023);
        put_sprite_at(&mut vdp, SPRITE_BASE, 2, 30, 40, (1, 1), 1, 0x0004);
        put_sprite_at(&mut vdp, SPRITE_BASE, 1, 50, 60, (4, 4), 0, 0x1805);
        put_sprite_at(&mut vdp, SPRITE_BASE, 3, 70, 80, (1, 1), 0, 0x0006);

        let list = sprite_list(&vdp);
        assert_eq!(
            list.len(),
            3,
            "entry 3 is linked by nobody and is not listed"
        );
        let [first, second, third] = list.as_slice() else {
            panic!("three entries: {list:?}");
        };
        assert_eq!(first.index, 0);
        assert_eq!(second.index, 2, "the link of entry 0 is 2");
        assert_eq!(third.index, 1, "and the link of entry 2 is 1");

        assert_eq!((first.x, first.y), (20, 10));
        assert_eq!((first.w, first.h), (16, 24), "cells of 8 pixels, in pixels");
        assert_eq!(first.link, 2);
        assert_eq!(first.tile, 0x023);
        assert!(first.priority);
        assert_eq!(first.palette, 0);
        assert!(!first.hflip && !first.vflip);

        assert_eq!((second.x, second.y), (40, 30));
        assert_eq!((second.w, second.h), (8, 8));
        assert_eq!(second.link, 1);
        assert_eq!(second.tile, 4);

        assert_eq!((third.x, third.y), (60, 50));
        assert_eq!((third.w, third.h), (32, 32));
        assert_eq!(third.link, 0);
        assert_eq!(third.tile, 5);
    }

    #[test]
    fn sprite_list_view_carries_the_palette_and_the_flips_of_the_pattern_word() {
        let mut vdp = vdp_with_tables();
        // p=0, c=3, v=1, h=1, n=7: 0b0111_1000_0000_0111.
        put_sprite_at(&mut vdp, SPRITE_BASE, 0, 0, 0, (1, 1), 0, 0x7807);
        let list = sprite_list(&vdp);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].palette, 3);
        assert!(list[0].vflip);
        assert!(list[0].hflip);
        assert!(!list[0].priority);
        assert_eq!(list[0].tile, 7);
    }
}
