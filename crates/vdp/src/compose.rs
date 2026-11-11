//! The priority compositor: the layers of one line into the pixels of one
//! frame-buffer row (M4-17, #95).
//!
//! genvdp.txt section 14 gives the order the layers of a line are seen in:
//!
//! ```text
//! (back)     A  >  B  >  C  >  D  >  E' >  F' >  G'     (front)
//! ```
//!
//! `A` is the backdrop colour, `B`/`E` plane B, `C`/`F` plane A and `D`/`G` the
//! sprites; a prime is the priority flag of the name table entry, which
//! [`crate::planes`] and [`crate::sprites`] carry into bit 6 of their layer pixel
//! ([`planes::PRIORITY_SHIFT`]). The window is not a fourth entry in that chain:
//! "plane W ... can be thought of a 'replacement' for plane A" (genvdp.txt
//! section 13), so [`Vdp::render_plane_a_line_with_window`] has already chosen
//! between the two by the time a pixel is composed.
//!
//! A line is therefore three layer buffers, resolved per pixel against that chain
//! and painted at [`Intensity::Normal`]: the shadow and highlight intensities of
//! genvdp.txt section 16 are M4-18 (#96), the left column and the rest of the mode
//! detail M4-19 (#97), and the interlaced fields M4-20 (#98).

use crate::color::Intensity;
use crate::planes::{self, Plane};
use crate::{CRAM_WORDS, Vdp, WIDTH_H40};

/// Bits 5-0 of a layer pixel: its CRAM index, `palette * 16 + colour`
/// (genvdp.txt section 9's 64 words). Bit 6 is the priority flag of
/// [`planes::PRIORITY_SHIFT`], so a layer pixel becomes a CRAM index by masking
/// this off.
const CRAM_INDEX_MASK: u8 = CRAM_WORDS as u8 - 1;

/// Which layer of genvdp.txt section 14 shows at one screen position.
///
/// The backdrop is one of them rather than the absence of one because it is the
/// `A` at the back of that chain: a pixel none of the three layer buffers draws
/// is not transparent, it is the R7 colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// `A`: R7's CRAM entry, what shows where nothing else does.
    Backdrop,
    /// `B` and `E'`: plane B, low and high priority.
    PlaneB,
    /// `C` and `F'`: plane A where the window is not, the window where it is.
    PlaneA,
    /// `D` and `G'`: the sprite layer, which [`Vdp::render_sprite_line`] has
    /// already resolved internally ("The sprite priority bit does not affect
    /// inter-sprite priority", genvdp.txt section 14).
    Sprite,
}

/// True for a layer pixel that draws: the three layer buffers write a pixel that
/// shows nothing as a whole zero byte, so colour 0 of a palette (genvdp.txt
/// section 12: "A pixel within a pattern that uses value zero isn't shown") and a
/// pixel no layer covers are the same value, and both let the layer under through.
#[inline]
fn shows(pixel: u8) -> bool {
    pixel != 0
}

/// True for a layer pixel with the priority flag of [`planes::PRIORITY_SHIFT`]:
/// the prime on the `E'`/`F'`/`G'` of genvdp.txt section 14.
#[inline]
fn high(pixel: u8) -> bool {
    (pixel >> planes::PRIORITY_SHIFT) & 1 != 0
}

/// Which layer shows at one screen position: the genvdp.txt section 14 chain
/// `(back) A > B > C > D > E' > F' > G' (front)` read from the front, so the first
/// pixel that draws wins. Every layer is asked twice, once for its prime and once
/// without, and [`shows`] is what keeps a transparent pixel out of the front of
/// the chain both times.
#[inline]
fn top_layer(plane_b: u8, plane_a: u8, sprite: u8) -> Layer {
    if shows(sprite) && high(sprite) {
        Layer::Sprite // G'
    } else if shows(plane_a) && high(plane_a) {
        Layer::PlaneA // F'
    } else if shows(plane_b) && high(plane_b) {
        Layer::PlaneB // E'
    } else if shows(sprite) {
        Layer::Sprite // D
    } else if shows(plane_a) {
        Layer::PlaneA // C
    } else if shows(plane_b) {
        Layer::PlaneB // B
    } else {
        Layer::Backdrop // A
    }
}

/// Paint `row`, the active pixels of one line, from the three layer buffers of
/// that line.
///
/// `cram` is [`Vdp::cram_pixels`], so a pixel costs one index into a table of
/// 64 instead of a CRAM read and a LUT read. `row` is as wide as the mode says,
/// which is how an H32 line drops the 64 overscan columns that
/// [`Vdp::render_plane_line`] fills in every mode.
fn compose_row(
    row: &mut [u32],
    plane_b: &[u8; WIDTH_H40],
    plane_a: &[u8; WIDTH_H40],
    sprites: &[u8; WIDTH_H40],
    cram: &[u32; CRAM_WORDS],
    backdrop: u32,
) {
    let layers = plane_b.iter().zip(plane_a).zip(sprites);
    for (pixel, ((&b, &a), &s)) in row.iter_mut().zip(layers) {
        let winner = top_layer(b, a, s);
        *pixel = match winner {
            Layer::Backdrop => backdrop,
            Layer::PlaneB => cram[usize::from(b & CRAM_INDEX_MASK)],
            Layer::PlaneA => cram[usize::from(a & CRAM_INDEX_MASK)],
            Layer::Sprite => cram[usize::from(s & CRAM_INDEX_MASK)],
        };
    }
}

impl Vdp {
    /// Every CRAM word as a frame-buffer pixel at [`Intensity::Normal`], the 64
    /// lookups a line composites with read once for that line.
    ///
    /// [`Vdp::backdrop_pixel`] is this at [`Vdp::backdrop_index`]. Reading CRAM
    /// once a line rather than once a pixel is also what lets [`compose_row`] be
    /// a free function, so the row write borrows [`Vdp::frame_buffer`] alone and
    /// not the whole `Vdp`.
    fn cram_pixels(&self) -> [u32; CRAM_WORDS] {
        let mut pixels = [0; CRAM_WORDS];
        for (pixel, colour) in pixels.iter_mut().zip(self.cram) {
            *pixel = self.color_lut.pixel(colour, Intensity::Normal);
        }
        pixels
    }

    /// Draw one line of the picture: genvdp.txt section 14 over the plane B, the
    /// plane A / window and the sprite layer buffers of `line`, into row `line`
    /// of [`Vdp::frame`].
    ///
    /// Public and callable on its own, as `docs/ARCHITECTURE.md` puts it
    /// ("Render one line directly. Public for tests; the scheduler uses
    /// advance()"); `timing::finish_line` calls it once for every line of the
    /// active display as the beam finishes it (M4-06, #84). Lines outside the
    /// active display are not drawn at all, which is what the `M4-20` (#98)
    /// interlaced fields will have to live with.
    ///
    /// With R1 bit 6 (DISP) clear the layers are not drawn and the whole line is
    /// the backdrop colour (genvdp.txt section 17 "$01"). Nothing of the line is
    /// walked either, so a switched-off display spends no sprite slot and sets no
    /// SOVR or collision status of its own.
    ///
    /// `frame.width` and `frame.height` are set from the mode on every call, not
    /// once at line 0: the stride of a row is the frame width, so a line drawn
    /// after a mid-frame mode change has to describe the mode it was drawn in,
    /// and [`Vdp::render_line`] stays callable on its own.
    pub fn render_line(&mut self, line: u16) {
        let height = self.active_height();
        let y = usize::from(line);
        if y >= height {
            return;
        }
        let width = self.active_width();
        let backdrop = self.backdrop_pixel();
        {
            let frame = &mut self.frame_buffer;
            frame.width = width;
            frame.height = height;
        }

        let start = y * width;
        if !self.display_enabled() {
            self.frame_buffer.pixels[start..start + width].fill(backdrop);
            return;
        }

        // The three layer buffers of the line are locals, not fields: the
        // sprites need `&mut self` to draw themselves, and a field would be
        // borrowed against it. 960 bytes of stack is not the per-frame
        // allocation docs/ARCHITECTURE.md rules out.
        let mut plane_b = [0; WIDTH_H40];
        let mut plane_a = [0; WIDTH_H40];
        let mut sprites = [0; WIDTH_H40];
        self.render_plane_line(Plane::B, line, &mut plane_b);
        self.render_plane_a_line_with_window(line, &mut plane_a);
        self.render_sprite_line(line, &mut sprites);

        let cram = self.cram_pixels();
        let row = &mut self.frame_buffer.pixels[start..start + width];
        compose_row(row, &plane_b, &plane_a, &sprites, &cram, backdrop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planes::testutil::{
        CELL_TILE_ROWS, PLANE_A_BASE, PLANE_B_BASE, PLANE_COLUMNS, PLANE_ROWS, fill_tile_row,
        plane_vdp, put_entry, put_word,
    };

    /// The three layer pixels of the priority tests, each colour 1 of a palette of
    /// its own and [`CRAM_COLOURS`] filled to match, so a pixel of the frame says
    /// which layer drew it: plane B palette 1, plane A palette 2, sprites palette
    /// 3 (genvdp.txt section 15: sprites use the last two palettes only).
    const PLANE_B: u8 = 1;
    const PLANE_A: u8 = 2;
    const SPRITE: u8 = 3;
    /// The colour index every layer of these tests draws with: 1, since colour 0
    /// is transparency.
    const COLOUR: u8 = 1;

    /// A low priority layer pixel of `palette`: `palette * 16 + colour`, the 6-bit
    /// CRAM index of genvdp.txt section 9.
    fn low(palette: u8) -> u8 {
        (palette << 4) | COLOUR
    }

    /// [`low`] with the priority flag of genvdp.txt section 14 set: the prime on
    /// `E'`/`F'`/`G'`.
    fn high(palette: u8) -> u8 {
        low(palette) | (1 << planes::PRIORITY_SHIFT)
    }

    /// The six links of genvdp.txt section 14 in order, each as the `(plane B,
    /// plane A, sprite)` layer pixels that put it on the screen on its own.
    fn chain() -> [(Layer, u8, u8, u8); 6] {
        [
            (Layer::PlaneB, low(PLANE_B), 0, 0),
            (Layer::PlaneA, 0, low(PLANE_A), 0),
            (Layer::Sprite, 0, 0, low(SPRITE)),
            (Layer::PlaneB, high(PLANE_B), 0, 0),
            (Layer::PlaneA, 0, high(PLANE_A), 0),
            (Layer::Sprite, 0, 0, high(SPRITE)),
        ]
    }

    #[test]
    fn nothing_opaque_shows_the_backdrop() {
        assert_eq!(top_layer(0, 0, 0), Layer::Backdrop);
    }

    #[test]
    fn plane_a_low_covers_plane_b_low() {
        assert_eq!(top_layer(low(PLANE_B), low(PLANE_A), 0), Layer::PlaneA);
    }

    #[test]
    fn plane_b_high_covers_plane_a_low() {
        assert_eq!(top_layer(high(PLANE_B), low(PLANE_A), 0), Layer::PlaneB);
    }

    #[test]
    fn sprite_low_covers_plane_a_low() {
        assert_eq!(top_layer(0, low(PLANE_A), low(SPRITE)), Layer::Sprite);
    }

    #[test]
    fn plane_b_high_covers_a_low_priority_sprite() {
        assert_eq!(top_layer(high(PLANE_B), 0, low(SPRITE)), Layer::PlaneB);
    }

    #[test]
    fn sprite_high_covers_plane_a_high() {
        assert_eq!(top_layer(0, high(PLANE_A), high(SPRITE)), Layer::Sprite);
    }

    /// A pixel of colour 0 is not a black pixel and takes no priority: plane A
    /// draws nothing there, so whatever is under it shows (genvdp.txt section 12:
    /// "A pixel within a pattern that uses value zero isn't shown").
    #[test]
    fn a_transparent_plane_a_shows_the_plane_b_under_it() {
        assert_eq!(top_layer(low(PLANE_B), 0, 0), Layer::PlaneB);
        assert_eq!(top_layer(high(PLANE_B), 0, high(SPRITE)), Layer::Sprite);
    }

    /// Every pair of links of the genvdp.txt section 14 chain, in every order:
    /// the one nearer the front of the chain is what shows.
    #[test]
    fn every_pair_of_chain_links_shows_the_front_one() {
        let links = chain();
        for back in 0..links.len() {
            for front in back..links.len() {
                let (b, a, s) = paint(&links, back, front);
                assert_eq!(
                    top_layer(b, a, s),
                    links[front].0,
                    "link {back} behind link {front}"
                );
            }
        }
    }

    /// The layer pixels of chain links `back` and `front`, `front` painted last:
    /// a link only sets the byte of the layer it is on, so a plane's two
    /// priorities never meet in one pixel and every pair is a legal line.
    fn paint(links: &[(Layer, u8, u8, u8); 6], back: usize, front: usize) -> (u8, u8, u8) {
        let mut pixels = (0u8, 0u8, 0u8);
        for link in [back, front] {
            let (layer, b, a, s) = links[link];
            match layer {
                Layer::Backdrop => {}
                Layer::PlaneB => pixels.0 = b,
                Layer::PlaneA => pixels.1 = a,
                Layer::Sprite => pixels.2 = s,
            }
        }
        pixels
    }

    // ---------------------------------------------------------------------
    // The frame buffer: `render_line` end to end.
    // ---------------------------------------------------------------------

    /// The CRAM the tests below read their colours out of, indexed by the CRAM
    /// entry they are written to and asserted against as a frame pixel: the
    /// backdrop at R7 = 0x05 and one primary per layer palette.
    const BACKDROP: u8 = 0x05;
    const COLOURS: [(u8, u16, u32); 4] = [
        (BACKDROP, 0x0EEE, 0xFFFF_FFFF),
        ((PLANE_B << 4) | COLOUR, 0x0E00, 0xFFFF_0000),
        ((PLANE_A << 4) | COLOUR, 0x00E0, 0xFF00_FF00),
        ((SPRITE << 4) | COLOUR, 0x000E, 0xFF00_00FF),
    ];

    /// A VDP in H32 with the display on, [`BACKDROP`] as the backdrop palette and
    /// [`COLOURS`] in CRAM: the base every `render_line` test below starts from.
    fn compose_vdp() -> Vdp {
        let mut vdp = plane_vdp();
        vdp.write_register(1, 0x40);
        vdp.write_register(7, BACKDROP);
        vdp.write_register(5, SPRITE_REG);
        for &(index, word, _) in &COLOURS {
            vdp.cram[usize::from(index)] = word;
        }
        vdp
    }

    /// The frame pixel of a CRAM entry of [`COLOURS`].
    fn pixel_of(cram_index: u8) -> u32 {
        let (_, _, pixel) = COLOURS
            .into_iter()
            .find(|(index, _, _)| *index == cram_index)
            .unwrap_or_else(|| panic!("no test colour for CRAM entry {cram_index}"));
        pixel
    }

    /// SAT base of these tests: R5 = `0x78` is 0xF000 under both the H32 and the
    /// H40 mask of [`Vdp::sprite_base`].
    const SPRITE_REG: u8 = 0x78;
    const SPRITE_BASE: usize = 0xF000;
    /// Window name table base: R3 = `0x28` is 0xA000, clear of the two plane
    /// tables and of the patterns the fills write at the bottom of VRAM.
    const WINDOW_REG: u8 = 0x28;
    const WINDOW_BASE: usize = 0xA000;
    /// Cells in a row of the H32 window table: genvdp.txt section 13, "The name
    /// table for plane W is 32x32 in 32-cell mode".
    const WINDOW_CELLS_H32: usize = 32;

    /// A name table word (genvdp.txt section 13, `pccvhnnnnnnnnnnn`) naming
    /// `tile` in `palette`, with the priority flag of section 14 as `priority`.
    fn entry(palette: u8, tile: u16, priority: bool) -> u16 {
        u16::from(priority) << 15 | u16::from(palette) << 13 | tile
    }

    /// `tile` filled with `colour` in all of its rows, so a plane cell or sprite
    /// cell that names it is one flat colour.
    fn solid(vdp: &mut Vdp, tile: u16, colour: u8) {
        for row in 0..CELL_TILE_ROWS {
            fill_tile_row(vdp, tile, row, colour);
        }
    }

    /// SAT entry `index` as one 1x1 cell of `pattern` at the screen position
    /// `(x, y)`, its link ending the list (genvdp.txt section 15).
    fn put_sprite(vdp: &mut Vdp, index: usize, x: u16, y: u16, pattern: u16) {
        let entry = SPRITE_BASE + index * 8;
        put_word(vdp, entry, y + 128);
        put_word(vdp, entry + 2, 0);
        put_word(vdp, entry + 4, pattern);
        put_word(vdp, entry + 6, x + 128);
    }

    /// Plane A and the sprite of [`compose_vdp`] at `(0, 0)`, each 8 pixels wide,
    /// over a plane B that covers the whole display: the three layers of the
    /// `render_line` tests, each a different colour.
    ///
    /// `plane_b_priority` and `plane_a_priority` are the name table flags of
    /// section 14; the sprite pattern carries its own.
    fn three_layers(
        vdp: &mut Vdp,
        plane_b_priority: bool,
        plane_a_priority: bool,
        sprite_priority: bool,
    ) {
        solid(vdp, 1, COLOUR);
        solid(vdp, 2, COLOUR);
        for cell in 0..PLANE_COLUMNS * PLANE_ROWS {
            put_entry(vdp, PLANE_B_BASE, cell, entry(PLANE_B, 1, plane_b_priority));
        }
        put_entry(vdp, PLANE_A_BASE, 0, entry(PLANE_A, 2, plane_a_priority));
        put_sprite(vdp, 0, 0, 0, entry(SPRITE, 2, sprite_priority));
    }

    /// Row `line` of the frame, as wide as the frame says.
    fn row(vdp: &Vdp, line: usize) -> Vec<u32> {
        let frame = vdp.frame();
        let start = frame.pixel_index(0, line);
        frame.pixels[start..start + frame.width].to_vec()
    }

    /// Only the backdrop: R7 = 0x05 and CRAM[5] = 0x000E make the whole of an
    /// active line red, and nothing outside the active display is painted.
    #[test]
    fn a_line_of_nothing_but_the_backdrop_is_that_colour_across() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(7, 0x05);
        vdp.cram[5] = 0x000E;

        vdp.render_line(0);

        let red = 0xFF00_00FF;
        let frame = vdp.frame();
        assert_eq!(frame.width, 256, "H32 by default");
        assert_eq!(frame.height, 224);
        assert_eq!(row(&vdp, 0), vec![red; 256]);
        assert!(
            row(&vdp, 1).iter().all(|&p| p == 0),
            "line 1 was not drawn by this call"
        );

        vdp.render_line(223);
        assert_eq!(row(&vdp, 223), vec![red; 256]);

        vdp.render_line(224);
        assert!(
            row(&vdp, 224).iter().all(|&p| p == 0),
            "line 224 is past the active display"
        );
    }

    /// The three layers of one pixel, in every priority combination that the
    /// chain of genvdp.txt section 14 has to settle end to end: what arrives in
    /// the frame buffer is the front colour, and plane B shows again where only
    /// it is opaque.
    #[test]
    fn render_line_sorts_planes_and_a_sprite_by_priority() {
        let cases: [(bool, bool, bool, u8); 6] = [
            (false, false, false, SPRITE),
            (true, false, false, PLANE_B),
            (false, true, false, PLANE_A),
            (false, false, true, SPRITE),
            (true, true, true, SPRITE),
            (true, false, true, SPRITE),
        ];
        for (plane_b, plane_a, sprite, want) in cases {
            let mut vdp = compose_vdp();
            three_layers(&mut vdp, plane_b, plane_a, sprite);

            vdp.render_line(0);

            let line = row(&vdp, 0);
            assert_eq!(
                line[0],
                pixel_of((want << 4) | COLOUR),
                "plane B priority {plane_b}, plane A priority {plane_a}, sprite priority {sprite}"
            );
            assert!(
                line[8..].iter().all(|&p| p == pixel_of((PLANE_B << 4) | COLOUR)),
                "only plane B is opaque from pixel 8 on, and it showed as {:?}",
                &line[8..12]
            );
        }
    }

    /// R1 bit 6 clear: "the display is blanked" (genvdp.txt section 17 "$01"),
    /// so the same three layers show nothing but the backdrop.
    #[test]
    fn a_display_that_is_off_shows_only_the_backdrop() {
        let mut vdp = compose_vdp();
        three_layers(&mut vdp, false, false, false);
        vdp.write_register(1, 0x00);

        vdp.render_line(0);

        let backdrop = pixel_of(BACKDROP);
        assert_eq!(row(&vdp, 0), vec![backdrop; 256]);
    }

    /// The window replaces plane A over the columns R17 and R18 give it (genvdp.txt
    /// section 13: "plane A cannot be displayed in any area where plane W is
    /// located") and leaves the rest of plane A alone.
    #[test]
    fn the_window_replaces_plane_a_where_it_shows() {
        let mut vdp = compose_vdp();
        solid(&mut vdp, 3, COLOUR);
        for cell in 0..PLANE_COLUMNS * PLANE_ROWS {
            put_entry(&mut vdp, PLANE_A_BASE, cell, entry(PLANE_A, 3, false));
        }
        for cell in 0..WINDOW_CELLS_H32 * PLANE_ROWS {
            put_entry(&mut vdp, WINDOW_BASE, cell, entry(SPRITE, 4, false));
        }
        solid(&mut vdp, 4, COLOUR);
        // R17: window on the right of x = 8 x 16 = 128; R18: every line.
        vdp.write_register(3, WINDOW_REG);
        vdp.write_register(17, 0x80 | 8);
        vdp.write_register(18, 0x80);

        vdp.render_line(0);

        let line = row(&vdp, 0);
        let plane_a = pixel_of((PLANE_A << 4) | COLOUR);
        let window = pixel_of((SPRITE << 4) | COLOUR);
        assert!(line[..128].iter().all(|&p| p == plane_a), "x < 128 is plane A");
        assert!(
            line[128..].iter().all(|&p| p == window),
            "x >= 128 is the window"
        );
    }
}
