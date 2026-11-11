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
//! and painted at one of the three intensities of [`Intensity`]: shadow where no
//! plane has its priority bit, normal where one does, and highlight where a sprite
//! operator raised what was already normal (genvdp.txt section 16, M4-18 #96). The
//! row is then cut to the width of the line's mode, blanked at the left for R0 bit
//! 5 and padded with the backdrop for whatever the frame has room for but the mode
//! does not (M4-19 #97). Interlace mode 2 draws half a frame per field, into every
//! other row of the frame buffer, which is [`Vdp::display_line`] (M4-20 #98).
//!
//! Two oddities of that section are outside the ticket and are not modelled: a
//! sprite pixel of colour 14 of any palette line shows at normal intensity
//! whatever its priority is (genvdp.txt's "bug rather than a feature", confirmed
//! in the SpritesMind thread), and a high priority tile is said to lift the shadow
//! of the whole 8x8 block it covers even where it is transparent - which the layer
//! buffers of [`crate::planes`] cannot say, as a pixel they draw nothing at is a
//! whole zero byte, priority and all. Both are for the M6 test-ROM triage.

use crate::color::Intensity;
use crate::planes::{self, Plane};
use crate::{CRAM_WORDS, Vdp, WIDTH_H40};

/// Bits 5-0 of a layer pixel: its CRAM index, `palette * 16 + colour`
/// (genvdp.txt section 9's 64 words). Bit 6 is the priority flag of
/// [`planes::PRIORITY_SHIFT`], so a layer pixel becomes a CRAM index by masking
/// this off.
const CRAM_INDEX_MASK: u8 = CRAM_WORDS as u8 - 1;

/// The sprite pixel that is not a colour but a word about the pixel under it:
/// palette 3 colour 14, which raises that pixel one step - the shadow of a plane
/// to normal, a normal plane to highlight.
///
/// genvdp.txt section 16 has the two operators the other way round ("Pixels with
/// color 3Eh are not drawn. Instead, the underlying pixel ... will be shown at
/// half intensity"), which the research that settled the mode has as wrong: "I
/// noticed that Charles MacDonald got the purpose of sprite color 0x3e and 0x3f
/// backward" (SpritesMind "Shadow/Highlight" thread 2692), and the two agree with
/// the games Raster Scroll's "Shadow and Highlight" works from - the spotlight of
/// Mega Turrican is palette 3 colour 14 and it is a highlight.
const HIGHLIGHT_OPERATOR: u8 = 0x3E;

/// [`HIGHLIGHT_OPERATOR`] going down instead of up: palette 3 colour 15, and down
/// is all the way - "shadow + shadow = shadow" (SpritesMind thread 2692).
const SHADOW_OPERATOR: u8 = 0x3F;

/// The width of the column R0 bit 5 (LCB) blanks: one cell, the leftmost one,
/// "filled with the backdrop color" whatever the layers hold in it (genvdp.txt
/// section 17 "$00"; plutiedev "$80xx").
const LEFT_COLUMN: usize = 8;

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

/// The intensity of the background of one position, which is the whole of the
/// plane half of genvdp.txt section 16: "Background tiles are shown at half
/// intensity, or at normal intensity if their priority bit is set", which the
/// SpritesMind thread that settled the mode has as "if any layer is high priority,
/// default to normal, otherwise default to shadowed".
///
/// A [`Layer::Backdrop`] is under this too, and shadowed with the planes: "The
/// backdrop is shown at half intensity". Only the planes speak: a sprite's own
/// priority makes its own pixel normal in [`compose_pixel`] and nothing else,
/// which is what the same thread found on hardware ("the docs had the 'if either
/// plane A or plane B priority is set, everything becomes normal' part wrong ...
/// If sprite has priority set, that doesn't happen").
fn background_level(shadow_highlight: bool, plane_b: u8, plane_a: u8) -> Intensity {
    let high_priority = high(plane_a) || high(plane_b);
    match (shadow_highlight, high_priority) {
        (true, false) => Intensity::Shadow,
        _ => Intensity::Normal,
    }
}

/// One step up the ladder of genvdp.txt section 16, the "brighten" of the
/// SpritesMind thread's list: "shadow + highlight = normal" and "normal +
/// highlight = highlight". Highlight is the top of the ladder, so nothing steps
/// above it.
fn raise(level: Intensity) -> Intensity {
    match level {
        Intensity::Shadow => Intensity::Normal,
        Intensity::Normal => Intensity::Highlight,
        Intensity::Highlight => Intensity::Highlight,
    }
}

/// What one screen position shows: the layer whose CRAM index to paint, at what
/// intensity.
///
/// The planes set the intensity of everything under them; a sprite pixel either
/// takes that of a low priority one, escapes it with its own priority bit, or -
/// for the two operator colours of genvdp.txt section 16 - steps it by one and
/// stands aside to let the background through.
///
/// An operator steps the background only where it is the sprite pixel that shows,
/// and that means the whole chain of [`top_layer`]: a low priority operator under
/// a high priority plane "is behind" it and commands nothing ("This only works if
/// the sprite in question is at the 'top' of the display - meaning there are no
/// planes set to higher priority than the sprite", Raster Scroll's "Shadow and
/// Highlight").
fn compose_pixel(
    shadow_highlight: bool,
    plane_b: u8,
    plane_a: u8,
    sprite: u8,
) -> (Layer, Intensity) {
    let level = background_level(shadow_highlight, plane_b, plane_a);
    let winner = top_layer(plane_b, plane_a, sprite);
    if winner != Layer::Sprite {
        // Whatever won is a plane or the backdrop, and it wins the same way with
        // the sprite taken out of the chain: that is the background of this pixel.
        return (winner, level);
    }
    match (shadow_highlight, sprite & CRAM_INDEX_MASK) {
        (true, HIGHLIGHT_OPERATOR) => (top_layer(plane_b, plane_a, 0), raise(level)),
        (true, SHADOW_OPERATOR) => (top_layer(plane_b, plane_a, 0), Intensity::Shadow),
        _ => (
            Layer::Sprite,
            if high(sprite) {
                Intensity::Normal
            } else {
                level
            },
        ),
    }
}

/// What the mode registers say about the line being drawn, read once by
/// [`Vdp::render_line`] for it and taken as one by [`compose_row`].
#[derive(Debug, Clone, Copy)]
struct LineMode {
    /// Pixels the line itself draws, [`Vdp::active_width`], which a mode change
    /// mid-frame makes less than the frame is wide.
    width: usize,
    /// Columns of the left edge that go to the backdrop: 8 for R0 bit 5
    /// ([`Vdp::left_column_blank`]) and 0 for a display that has it clear.
    blank: usize,
    /// The CRAM entry the backdrop shows from, [`Vdp::backdrop_index`].
    backdrop: u8,
    /// Whether R12 bit 3 makes the two extra intensities of genvdp.txt section
    /// 16 live, [`Vdp::shadow_highlight_enabled`].
    shadow_highlight: bool,
}

/// Paint `row`, one row of the frame buffer, from the three layer buffers of one
/// line and its [`LineMode`].
///
/// `cram` is [`Vdp::cram_pixels`], so a pixel costs one index into a table of
/// 64 instead of a CRAM read and a LUT read, and the entry the backdrop shows
/// from is `mode.backdrop`, at whatever intensity [`compose_pixel`] settled.
///
/// `row` is as wide as the frame ([`crate::FrameBuffer::width`], the mode of its
/// first line) while `mode.width` is as wide as this line's own mode draws, two
/// numbers that part company the moment a game writes R12 mid-frame:
///
/// * columns `mode.width..` of the row are not overscan data but nothing at all,
///   the display of the line having ended, so they are read as empty layers and
///   compose to the backdrop;
/// * columns `..mode.blank` of it are R0 bit 5, [`Vdp::left_column_blank`],
///   which shows the backdrop colour and nothing else.
///
/// The blank takes the colour only, not the intensity: genvdp.txt section 16
/// settles the two for a pixel separately, so the left column of a shadowed line
/// is the shadowed backdrop and, with shadow/highlight off as everywhere else,
/// plain R7.
fn compose_row(
    row: &mut [u32],
    mode: LineMode,
    plane_b: &[u8; WIDTH_H40],
    plane_a: &[u8; WIDTH_H40],
    sprites: &[u8; WIDTH_H40],
    cram: &[[u32; Intensity::ALL.len()]; CRAM_WORDS],
) {
    let LineMode {
        width,
        blank,
        backdrop,
        shadow_highlight,
    } = mode;
    for (x, pixel) in row.iter_mut().enumerate() {
        // A column past the width of this line has no layer data: the buffers
        // are as wide as the widest mode, and what they hold beyond the display
        // of the line is the overscan of a display this line is not drawing.
        let (b, a, s) = if x < width {
            (plane_b[x], plane_a[x], sprites[x])
        } else {
            (0, 0, 0)
        };
        let (layer, level) = compose_pixel(shadow_highlight, b, a, s);
        let index = if x < blank {
            backdrop
        } else {
            match layer {
                Layer::Backdrop => backdrop,
                Layer::PlaneB => b & CRAM_INDEX_MASK,
                Layer::PlaneA => a & CRAM_INDEX_MASK,
                Layer::Sprite => s & CRAM_INDEX_MASK,
            }
        };
        *pixel = cram[usize::from(index)][level.index()];
    }
}

impl Vdp {
    /// Every CRAM word as a frame-buffer pixel at every [`Intensity`]: the 64 x 3
    /// lookups a line composites with, read once for that line and indexed
    /// `[cram_index][Intensity::index]`.
    ///
    /// [`Vdp::backdrop_pixel`] is this at [`Vdp::backdrop_index`] and
    /// [`Intensity::Normal`]. Reading CRAM once a line rather than once a pixel is
    /// also what lets [`compose_row`] be a free function, so the row write borrows
    /// [`Vdp::frame_buffer`] alone and not the whole `Vdp`.
    fn cram_pixels(&self) -> [[u32; Intensity::ALL.len()]; CRAM_WORDS] {
        let mut pixels = [[0; Intensity::ALL.len()]; CRAM_WORDS];
        for (entry, colour) in pixels.iter_mut().zip(self.cram) {
            for (pixel, level) in entry.iter_mut().zip(Intensity::ALL) {
                *pixel = self.color_lut.pixel(colour, level);
            }
        }
        pixels
    }

    /// Draw one line of the picture: genvdp.txt section 14 over the plane B, the
    /// plane A / window and the sprite layer buffers of `line`, at the intensity
    /// section 16 gives each pixel of them, into row [`Vdp::display_line`]`(line)`
    /// of [`Vdp::frame`] - row `line` itself except in interlace mode 2.
    ///
    /// Public and callable on its own, as `docs/ARCHITECTURE.md` puts it
    /// ("Render one line directly. Public for tests; the scheduler uses
    /// advance()"); `timing::finish_line` calls it once for every line of the
    /// active display as the beam finishes it (M4-06, #84). `line` is a line of the
    /// beam, `0..Vdp::active_height`, and a line outside it is not drawn at all: in
    /// interlace mode 2 the beam still scans 224 lines of a 448-line picture and the
    /// other half of the frame belongs to the field before and after this one, which
    /// is what keeps a woven frame of #98 woven.
    ///
    /// With R1 bit 6 (DISP) clear the layers are not drawn and the whole line is
    /// the backdrop colour (genvdp.txt section 17 "$01"). Nothing of the line is
    /// walked either, so a switched-off display spends no sprite slot and sets no
    /// SOVR or collision status of its own.
    ///
    /// `frame.width` and `frame.height` are the mode of the first line of the
    /// frame, latched by the `line == 0` call and left alone for the rest of it.
    /// The stride of a row is the frame width, so a mid-frame mode change may not
    /// move it or the lines already drawn would be read back at the wrong offset;
    /// a line narrower than the frame is padded with the backdrop, by
    /// [`compose_row`], and a line wider than it - the switch the other way, which
    /// shows nothing of the extra 64 columns - is cut short.
    ///
    /// Everything else of the line is read at the moment it is drawn, R12
    /// included, as [`Vdp::window_base`] and [`Vdp::sprite_base`] do too.
    pub fn render_line(&mut self, line: u16) {
        if usize::from(line) >= self.active_height() {
            return;
        }
        // The line of the picture this beam line of this field draws: `line` itself
        // uninterlaced, `line * 2 + field` in interlace mode 2, where the two fields
        // of a frame share one frame buffer and each writes the lines of its own
        // field and leaves the other field's lines alone (#98).
        let display_line = self.display_line(line);
        let width = self.active_width();
        if usize::from(line) == 0 {
            self.frame_buffer.width = width;
            self.frame_buffer.height = self.frame_height();
        }

        let stride = self.frame_buffer.width;
        let start = usize::from(display_line) * stride;
        if !self.display_enabled() {
            // The backdrop of a switched-off display is the whole line, at the
            // normal intensity of [`Vdp::backdrop_pixel`]: the half of section 16
            // belongs to the display, and "the overscan region outside of the
            // display area is always shown at normal intensity". It is the whole
            // row of the frame, so a display switched off on an H32 line of an
            // H40 frame leaves no live pixels at its right edge either.
            let backdrop = self.backdrop_pixel();
            self.frame_buffer.pixels[start..start + stride].fill(backdrop);
            return;
        }

        // The three layer buffers of the line are locals, not fields: the
        // sprites need `&mut self` to draw themselves, and a field would be
        // borrowed against it. 960 bytes of stack is not the per-frame
        // allocation docs/ARCHITECTURE.md rules out.
        let mut plane_b = [0; WIDTH_H40];
        let mut plane_a = [0; WIDTH_H40];
        let mut sprites = [0; WIDTH_H40];
        self.render_plane_line(Plane::B, display_line, &mut plane_b);
        self.render_plane_a_line_with_window(display_line, &mut plane_a);
        self.render_sprite_line(display_line, &mut sprites);

        let cram = self.cram_pixels();
        let mode = LineMode {
            width,
            blank: if self.left_column_blank() {
                LEFT_COLUMN
            } else {
                0
            },
            backdrop: self.backdrop_index(),
            shadow_highlight: self.shadow_highlight_enabled(),
        };
        let row = &mut self.frame_buffer.pixels[start..start + stride];
        compose_row(row, mode, &plane_b, &plane_a, &sprites, &cram);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WIDTH_H32;
    use crate::color::color;
    use crate::planes::testutil::{
        CELL_TILE_ROWS, PLANE_A_BASE, PLANE_B_BASE, PLANE_COLUMNS, PLANE_ROWS, fill_tile_row,
        fill_tile_row_of_height, plane_vdp, put_entry, put_word,
    };

    /// The palette each layer of these tests draws from, one apiece, with
    /// [`COLOURS`] filled to match: a pixel of the frame says which layer drew it.
    /// Plane B palette 1, plane A palette 2, sprites palette 3 (genvdp.txt section
    /// 15 gives sprites the last two palettes only).
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
        let slot = SPRITE_BASE + index * 8;
        put_word(vdp, slot, y + 128);
        put_word(vdp, slot + 2, 0);
        put_word(vdp, slot + 4, pattern);
        put_word(vdp, slot + 6, x + 128);
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

        vdp.write_register(12, 0x81);
        vdp.render_line(0);
        assert_eq!(
            row(&vdp, 0),
            vec![red; 320],
            "and an H40 line is the 320 pixels of the mode it was drawn in"
        );
    }

    /// The three layers of one pixel, in every priority combination that the
    /// chain of genvdp.txt section 14 has to settle end to end: what arrives in
    /// the frame buffer is the front colour, and plane B shows again where only
    /// it is opaque.
    #[test]
    fn render_line_sorts_planes_and_a_sprite_by_priority() {
        let cases: [(bool, bool, bool, u8); 7] = [
            (false, false, false, SPRITE),
            (true, false, false, PLANE_B),
            (false, true, false, PLANE_A),
            (false, false, true, SPRITE),
            (true, true, false, PLANE_A),
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
                line[8..]
                    .iter()
                    .all(|&p| p == pixel_of((PLANE_B << 4) | COLOUR)),
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
        assert!(
            line[..128].iter().all(|&p| p == plane_a),
            "x < 128 is plane A"
        );
        assert!(
            line[128..].iter().all(|&p| p == window),
            "x >= 128 is the window"
        );
    }

    // ---------------------------------------------------------------------
    // Display modes: the size of the frame and the edges of it (M4-19, #97).
    // ---------------------------------------------------------------------

    /// Plane A over the whole display: every active pixel of every line is
    /// [`PLANE_A`]'s colour, which is what makes the backdrop of a blanked left
    /// column and of the padding of a short line stand out.
    fn plane_a_everywhere(vdp: &mut Vdp) {
        solid(vdp, 3, COLOUR);
        for cell in 0..PLANE_COLUMNS * PLANE_ROWS {
            put_entry(vdp, PLANE_A_BASE, cell, entry(PLANE_A, 3, false));
        }
    }

    /// The width the frame reports is the width of the mode, R12's resolution
    /// bits: 256 for `00` and 320 for `0x81` (genvdp.txt section 17 "$0C", and
    /// [`Vdp::active_width`]).
    #[test]
    fn line_zero_sizes_the_frame_to_the_horizontal_resolution() {
        let mut vdp = compose_vdp();
        vdp.render_line(0);
        assert_eq!(vdp.frame().width, 256, "R12 = 0 is the 32-cell display");

        let mut vdp = compose_vdp();
        vdp.write_register(12, 0x81);
        vdp.render_line(0);
        assert_eq!(vdp.frame().width, 320, "R12 = 0x81 is the 40-cell one");
    }

    /// R1 bit 3 (M2) is V30, 30 cells of 8 lines: 240 lines on a PAL chip, and
    /// still 224 on NTSC, which cannot clock out the extra 16 and shows no stable
    /// picture with the bit set (genvdp.txt section 17 "$01"; plutiedev "$81xx":
    /// "V30 mode only works properly on PAL systems").
    #[test]
    fn line_zero_sizes_the_frame_to_the_vertical_resolution_of_the_crystal() {
        for (pal, height) in [(true, 240), (false, 224)] {
            let mut vdp = Vdp::new(pal);
            vdp.write_register(1, 0x48);
            vdp.render_line(0);
            assert_eq!(
                vdp.frame().height,
                height,
                "{} with R1 bit 3 set",
                if pal { "PAL" } else { "NTSC" }
            );
        }

        let mut vdp = Vdp::new(true);
        vdp.write_register(1, 0x40);
        vdp.render_line(0);
        assert_eq!(vdp.frame().height, 224, "PAL in V28 is 224 like NTSC");
    }

    /// R0 bit 5 (LCB): "the leftmost 8 pixels ... are filled with the backdrop
    /// color" (genvdp.txt section 17 "$00"; plutiedev "$80xx"). Plane A and a
    /// sprite both cover the column here, and both go.
    #[test]
    fn the_left_column_blank_replaces_the_first_eight_pixels_with_the_backdrop() {
        let mut vdp = compose_vdp();
        plane_a_everywhere(&mut vdp);
        solid(&mut vdp, 4, COLOUR);
        put_sprite(&mut vdp, 0, 0, 0, entry(SPRITE, 4, true));

        vdp.render_line(0);
        let plane_a = pixel_of((PLANE_A << 4) | COLOUR);
        let sprite = pixel_of((SPRITE << 4) | COLOUR);
        let line = row(&vdp, 0);
        assert!(
            line[..8].iter().all(|&p| p == sprite),
            "before the blank is switched on, the sprite is what the column shows"
        );

        vdp.write_register(0, 1 << 5);
        vdp.render_line(0);
        let line = row(&vdp, 0);
        let backdrop = pixel_of(BACKDROP);
        assert!(
            line[..8].iter().all(|&p| p == backdrop),
            "the 8 pixels of R0 bit 5 are the backdrop, sprite included"
        );
        assert!(
            line[8..].iter().all(|&p| p == plane_a),
            "and pixel 8 on is the plane it always was"
        );
    }

    /// A game that drops from H40 to H32 part way down the frame does not move
    /// the frame: it stays 320 wide, the geometry of line 0, and the lines that
    /// are only 256 pixels long get the backdrop where the display is not.
    #[test]
    fn a_line_narrower_than_the_frame_is_padded_with_the_backdrop() {
        let mut vdp = compose_vdp();
        plane_a_everywhere(&mut vdp);
        vdp.write_register(12, 0x81);
        vdp.render_line(0);
        vdp.write_register(12, 0x00);
        vdp.render_line(1);

        let frame = vdp.frame();
        assert_eq!(frame.width, 320, "the frame keeps the width of line 0");
        let backdrop = pixel_of(BACKDROP);
        let plane_a = pixel_of((PLANE_A << 4) | COLOUR);
        let line0 = row(&vdp, 0);
        assert!(
            line0.iter().all(|&p| p == plane_a),
            "the H40 line of line 0 is still plane A end to end"
        );
        let line1 = row(&vdp, 1);
        assert!(
            line1[..WIDTH_H32].iter().all(|&p| p == plane_a),
            "the 256 pixels of the H32 line are plane A"
        );
        assert!(
            line1[WIDTH_H32..].iter().all(|&p| p == backdrop),
            "and the 64 the mode has no display for are the backdrop"
        );
    }

    // ---------------------------------------------------------------------
    // Shadow / highlight: genvdp.txt section 16, R12 bit 3 (M4-18, #96).
    // ---------------------------------------------------------------------

    /// The pattern colour of the highlight operator: palette 3 colour 14, the
    /// `3E` of genvdp.txt section 16.
    const HIGHLIGHT: u8 = 14;
    /// [`HIGHLIGHT`]'s partner: palette 3 colour 15, the `3F` of that section.
    const SHADOW: u8 = 15;
    /// What [`compose_vdp`] writes to the two operator entries of CRAM for
    /// [`operators_are_ordinary_colours_when_shadow_highlight_is_off`], one word
    /// apiece so the two cannot be confused for each other.
    const OPERATOR_WORDS: [(u8, u16); 2] = [
        ((SPRITE << 4) | HIGHLIGHT, 0x00C0),
        ((SPRITE << 4) | SHADOW, 0x0C00),
    ];

    /// The frame pixel of a CRAM entry of [`COLOURS`] at `level`.
    ///
    /// [`crate::color::ColorLut`] is [`color`] for every colour at every
    /// intensity, which is what lets a shadow or highlight expectation be
    /// written from the CRAM word rather than from another table.
    fn pixel_at(cram_index: u8, level: Intensity) -> u32 {
        let (_, word, _) = COLOURS
            .into_iter()
            .find(|(index, _, _)| *index == cram_index)
            .unwrap_or_else(|| panic!("no test colour for CRAM entry {cram_index}"));
        color(word, level)
    }

    /// [`compose_vdp`] with R12 bit 3 (S/TE) set: shadow/highlight mode
    /// (genvdp.txt section 16, plutiedev "$8Cxx").
    fn shadow_vdp() -> Vdp {
        let mut vdp = compose_vdp();
        vdp.write_register(12, 1 << 3);
        vdp
    }

    /// Plane B at low priority over the whole display and plane A over the first
    /// cell at `plane_a_priority`: the background every shadow/highlight test
    /// below has a sprite pass over.
    fn shadow_background(vdp: &mut Vdp, plane_a_priority: bool) {
        solid(vdp, 1, COLOUR);
        solid(vdp, 2, COLOUR);
        for cell in 0..PLANE_COLUMNS * PLANE_ROWS {
            put_entry(vdp, PLANE_B_BASE, cell, entry(PLANE_B, 1, false));
        }
        put_entry(vdp, PLANE_A_BASE, 0, entry(PLANE_A, 2, plane_a_priority));
    }

    /// The pattern [`sprite_over`] draws its 1x1 cell from: its own, so that
    /// plane A keeps drawing [`COLOUR`] under it and an operator left in no doubt
    /// about what it is raising or darkening.
    const SPRITE_TILE: u16 = 5;

    /// A 1x1 cell sprite at (0, 0) drawing `colour` from the [`SPRITE`] palette
    /// at `priority`, over the plane A cell of [`shadow_background`].
    fn sprite_over(vdp: &mut Vdp, colour: u8, priority: bool) {
        solid(vdp, SPRITE_TILE, colour);
        put_sprite(vdp, 0, 0, 0, entry(SPRITE, SPRITE_TILE, priority));
    }

    /// Both planes low: everything is shadowed, plane B as much as plane A.
    #[test]
    fn a_low_priority_background_shows_at_shadow_intensity() {
        let mut vdp = shadow_vdp();
        shadow_background(&mut vdp, false);

        vdp.render_line(0);

        let line = row(&vdp, 0);
        let plane_a = pixel_at((PLANE_A << 4) | COLOUR, Intensity::Shadow);
        let plane_b = pixel_at((PLANE_B << 4) | COLOUR, Intensity::Shadow);
        assert!(
            line[..8].iter().all(|&p| p == plane_a),
            "the plane A cell is shadowed, not the normal colour of its CRAM entry"
        );
        assert!(
            line[8..].iter().all(|&p| p == plane_b),
            "plane B is shadowed too, and it is the only layer from pixel 8 on"
        );
    }

    /// Plane A high: its own pixels are normal, and so is everything else in
    /// their 8x8 block - while the rest of the line, plane B alone and low, is
    /// still shadowed (genvdp.txt section 16: "Background tiles are shown at half
    /// intensity, or at normal intensity if their priority bit is set").
    #[test]
    fn a_high_priority_plane_shows_at_normal_intensity() {
        let mut vdp = shadow_vdp();
        shadow_background(&mut vdp, true);

        vdp.render_line(0);

        let line = row(&vdp, 0);
        let plane_a = pixel_at((PLANE_A << 4) | COLOUR, Intensity::Normal);
        let plane_b = pixel_at((PLANE_B << 4) | COLOUR, Intensity::Shadow);
        assert!(line[..8].iter().all(|&p| p == plane_a), "plane A is high");
        assert!(
            line[8..].iter().all(|&p| p == plane_b),
            "and plane B, low and on its own, is not"
        );
    }

    /// The shadow of nothing is the shadow of the backdrop: "The backdrop is
    /// shown at half intensity" (genvdp.txt section 16), which is the R7 colour
    /// at [`Intensity::Shadow`] with no plane opaque anywhere to argue with it.
    #[test]
    fn the_backdrop_is_shadowed_too() {
        let mut vdp = shadow_vdp();

        vdp.render_line(0);

        assert_eq!(
            row(&vdp, 0),
            vec![pixel_at(BACKDROP, Intensity::Shadow); 256],
            "nothing but R7 is opaque on this line"
        );
    }

    /// A shadow operator over a shadowed background is that background still
    /// shadowed, and a highlight operator lifts it one step: "shadow + shadow =
    /// shadow" and "shadow + highlight = normal" (SpritesMind "Shadow/Highlight"
    /// thread 2692, which is also what settles genvdp.txt section 16 the other
    /// way round on which of the two colours is which).
    #[test]
    fn a_highlight_operator_raises_a_shadowed_background_one_step() {
        let mut vdp = shadow_vdp();
        shadow_background(&mut vdp, false);
        sprite_over(&mut vdp, HIGHLIGHT, false);

        vdp.render_line(0);

        let line = row(&vdp, 0);
        let normal = pixel_at((PLANE_A << 4) | COLOUR, Intensity::Normal);
        assert!(
            line[..8].iter().all(|&p| p == normal),
            "the operator draws nothing and the plane under it shows at normal"
        );

        let mut vdp = shadow_vdp();
        shadow_background(&mut vdp, false);
        sprite_over(&mut vdp, SHADOW, false);

        vdp.render_line(0);

        let line = row(&vdp, 0);
        let shadowed = pixel_at((PLANE_A << 4) | COLOUR, Intensity::Shadow);
        assert!(
            line[..8].iter().all(|&p| p == shadowed),
            "a shadow over a shadow is still the shadow of the same colour"
        );
    }

    /// A normal background - a high priority plane - under a highlight operator
    /// goes to highlight, which is the top step of the ladder; and the operator
    /// is one even on a high priority sprite, whose own priority is what puts it
    /// in front of that plane in the first place.
    #[test]
    fn a_highlight_operator_raises_a_normal_background_to_highlight() {
        let mut vdp = shadow_vdp();
        shadow_background(&mut vdp, true);
        sprite_over(&mut vdp, HIGHLIGHT, true);

        vdp.render_line(0);

        let line = row(&vdp, 0);
        let highlight = pixel_at((PLANE_A << 4) | COLOUR, Intensity::Highlight);
        assert!(
            line[..8].iter().all(|&p| p == highlight),
            "the sprite pixel of palette 3 colour 14 is not drawn, and the plane under it is highlighted"
        );
    }

    /// A shadow operator over a normal background: the plane colour, shadowed.
    #[test]
    fn a_shadow_operator_shadows_the_background_under_it() {
        let mut vdp = shadow_vdp();
        shadow_background(&mut vdp, true);
        sprite_over(&mut vdp, SHADOW, true);

        vdp.render_line(0);

        let line = row(&vdp, 0);
        let shadowed = pixel_at((PLANE_A << 4) | COLOUR, Intensity::Shadow);
        assert!(
            line[..8].iter().all(|&p| p == shadowed),
            "the plane colour, one step down from the normal it was drawn at"
        );
    }

    /// A low priority sprite over a shadowed background is shadowed with it, and
    /// a high priority sprite over the same background is not: "sprites are shown
    /// at normal or half intensity just like background tiles" (genvdp.txt
    /// section 16).
    #[test]
    fn a_sprite_takes_its_intensity_from_its_own_priority() {
        let cases = [(false, Intensity::Shadow), (true, Intensity::Normal)];
        for (priority, level) in cases {
            let mut vdp = shadow_vdp();
            shadow_background(&mut vdp, false);
            sprite_over(&mut vdp, COLOUR, priority);

            vdp.render_line(0);

            let line = row(&vdp, 0);
            let want = pixel_at((SPRITE << 4) | COLOUR, level);
            assert!(
                line[..8].iter().all(|&p| p == want),
                "sprite priority {priority} is {level:?}"
            );
            assert!(
                line[8..]
                    .iter()
                    .all(|&p| p == pixel_at((PLANE_B << 4) | COLOUR, Intensity::Shadow)),
                "the plane B behind is shadowed either way"
            );
        }
    }

    /// An operator only commands the pixel under it when it is the sprite the
    /// display shows there: "this only works if the sprite in question is at the
    /// top of the display - meaning there are no planes set to higher priority
    /// than the sprite" (Raster Scroll, "Shadow and Highlight"), which is also
    /// the shape of the hardware-verified implementation of the SpritesMind
    /// thread the mode was settled on.
    #[test]
    fn an_operator_under_a_high_priority_plane_does_nothing() {
        for colour in [HIGHLIGHT, SHADOW] {
            let mut vdp = shadow_vdp();
            shadow_background(&mut vdp, true);
            sprite_over(&mut vdp, colour, false);

            vdp.render_line(0);

            let line = row(&vdp, 0);
            let plane_a = pixel_at((PLANE_A << 4) | COLOUR, Intensity::Normal);
            assert!(
                line[..8].iter().all(|&p| p == plane_a),
                "palette 3 colour {colour} is behind a high priority plane A, which shows as itself"
            );
        }
    }

    /// R12 bit 3 clear: the two operator entries of the sprite palette are
    /// colours like any other, drawn where the sprite draws them.
    #[test]
    fn operators_are_ordinary_colours_when_shadow_highlight_is_off() {
        for (index, word) in OPERATOR_WORDS {
            let mut vdp = compose_vdp();
            shadow_background(&mut vdp, false);
            vdp.cram[usize::from(index)] = word;
            sprite_over(&mut vdp, index & 0x0F, false);

            vdp.render_line(0);

            let line = row(&vdp, 0);
            let want = color(word, Intensity::Normal);
            assert!(
                line[..8].iter().all(|&p| p == want),
                "CRAM entry {index:#04x} of a display with no shadow/highlight is its own colour"
            );
        }
    }

    // ---------------------------------------------------------------------
    // Interlace mode 2 (M4-20 #98): two fields, 448 lines of picture.
    // ---------------------------------------------------------------------

    /// Rows of a plane cell of interlace mode 2 (genvdp.txt section 12).
    const MODE2_ROWS: usize = 16;

    /// [`compose_vdp`] in interlace mode 2, R12 LSM = `11`, with cell (0,0) naming
    /// tile 3: a 16-row pattern whose rows 0 and 3 are [`COLOUR`] and whose other
    /// rows are transparent. Whether a given frame line is colour or the backdrop
    /// therefore says which pattern row it was drawn from.
    fn interlaced_compose_vdp() -> Vdp {
        let mut vdp = compose_vdp();
        vdp.write_register(12, 0x06);
        put_entry(&mut vdp, PLANE_A_BASE, 0, entry(PLANE_A, 3, false));
        for row in [0usize, 3] {
            fill_tile_row_of_height(&mut vdp, 3, row, COLOUR, MODE2_ROWS);
        }
        vdp
    }

    /// Interlace mode 2: the frame is two fields and each field writes every other
    /// line of the frame buffer - `line * 2 + field` - so the two fields of a beam
    /// line land on the two lines of the picture they belong to and leave the lines
    /// of the other field where they are, which is the woven frame of #98.
    #[test]
    fn interlace_mode_2_draws_each_field_on_its_own_frame_lines() {
        let mut vdp = interlaced_compose_vdp();
        let plane_a = pixel_of((PLANE_A << 4) | COLOUR);
        let backdrop = pixel_of(BACKDROP);

        vdp.odd_frame = false;
        vdp.render_line(0);
        vdp.odd_frame = true;
        vdp.render_line(0);
        vdp.odd_frame = false;
        vdp.render_line(1);
        vdp.odd_frame = true;
        vdp.render_line(1);

        assert!(
            row(&vdp, 0)[..8].iter().all(|&p| p == plane_a),
            "beam line 0 of field 0 is pattern row 0, which is opaque"
        );
        assert!(
            row(&vdp, 1)[..8].iter().all(|&p| p == backdrop),
            "beam line 0 of field 1 is pattern row 1, which is transparent"
        );
        assert!(
            row(&vdp, 2)[..8].iter().all(|&p| p == backdrop),
            "beam line 1 of field 0 is pattern row 2, which is transparent"
        );
        assert!(
            row(&vdp, 3)[..8].iter().all(|&p| p == plane_a),
            "and beam line 1 of field 1 is pattern row 3, which is opaque again"
        );
        assert!(
            row(&vdp, 4)[..8].iter().all(|&p| p == 0),
            "nothing past the four lines asked for was drawn"
        );
    }

    /// Interlace mode 2 doubles the height of the picture: 448 lines, 480 with PAL
    /// in V30. The beam still scans 224 of them, so [`Vdp::active_height`] - the
    /// number `timing` starts VBlank on - does not move (#98).
    #[test]
    fn interlace_mode_2_doubles_the_frame_height() {
        let mut vdp = interlaced_compose_vdp();
        vdp.render_line(0);
        assert_eq!(vdp.frame().height, 448, "224 lines drawn twice");
        assert_eq!(vdp.active_height(), 224, "the beam still scans 224 lines");

        let mut mode1 = compose_vdp();
        mode1.write_register(12, 0x02);
        mode1.render_line(0);
        assert_eq!(mode1.frame().height, 224, "mode 1 is progressive");

        let mut progressive = compose_vdp();
        progressive.render_line(0);
        assert_eq!(progressive.frame().height, 224, "and so is no interlace");

        let mut pal = Vdp::new(true);
        pal.write_register(1, 0x48); // display on, V30
        pal.write_register(12, 0x06);
        pal.render_line(0);
        assert_eq!(pal.frame().height, 480, "240 lines drawn twice");
        assert!(
            pal.frame().pixels.len() >= 320 * 480,
            "the frame buffer has room for the tallest picture it can be asked for"
        );
    }

    /// Mode 1 is the one #98 calls "render like non interlaced": the field flag and
    /// the HV counter move and the picture does not, so the odd field draws line 0
    /// into line 0 of the frame buffer just as the even one does.
    #[test]
    fn interlace_mode_1_draws_every_line_of_its_field() {
        let mut vdp = compose_vdp();
        vdp.write_register(12, 0x02);
        put_entry(&mut vdp, PLANE_A_BASE, 0, entry(PLANE_A, 1, false));
        solid(&mut vdp, 1, COLOUR);
        vdp.odd_frame = true;

        vdp.render_line(0);

        let plane_a = pixel_of((PLANE_A << 4) | COLOUR);
        assert_eq!(
            vdp.frame().height,
            224,
            "mode 1 does not double the picture"
        );
        assert!(
            row(&vdp, 0)[..8].iter().all(|&p| p == plane_a),
            "the odd field draws its line 0 into frame line 0"
        );
        assert!(
            row(&vdp, 1).iter().all(|&p| p == 0),
            "and nothing was shifted down to frame line 1"
        );
    }
}
