//! Name table entries, and the pattern pixels they point at (M4-10, #88).
//!
//! Two jobs that the two scroll planes, the window and the sprites all share:
//! reading the 16-bit words a table is made of ([`read_word`],
//! [`NameEntry::decode`]) and fetching one row of the pattern a word names
//! ([`tile_addr`], [`tile_row`]).
//!
//! Nothing here reaches the `Vdp`, it takes the VRAM slice it is given. That is
//! what keeps the 64 KiB image where `Vdp::vram` puts it - boxed, off every
//! caller's stack - and costs a rendering loop nothing but a borrow.

use crate::VRAM_SIZE;

/// Every VRAM address is masked to this. genvdp.txt section 8: "The VDP is
/// connected to 64K of video RAM" and "The address register wraps past address
/// FFFFh", so a fetch that runs off the top of VRAM comes back at 0 rather than
/// trapping or reading past a slice.
const VRAM_MASK: usize = VRAM_SIZE - 1;

/// Bytes in one row of a pattern: 8 pixels at 4 bits apiece. genvdp.txt
/// section 12: "there are four bytes (4 bits * 8 pixels = 4 bytes) per line of
/// the pattern".
const ROW_BYTES: usize = 4;

/// One cell of a plane A, plane B or window name table.
///
/// genvdp.txt section 13 "Name Tables": "Each table is a matrix of 16-bit
/// words. Each word has the following format: `pccvhnnnnnnnnnnn`" - "p =
/// Priority flag, c = Palette select, v = Vertical flip, h = Horizontal flip,
/// n = Pattern name". [`NameEntry::decode`] is that layout, [`tile_row`] the
/// pattern it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameEntry {
    /// Bit 15: draw this cell's opaque pixels in front of plane A and the
    /// sprites (genvdp.txt section 14, where a plane's entries move from `B`/`C`
    /// to `E'`/`F'` when the bit is set).
    pub priority: bool,
    /// Bits 14-13: which of the four 16-colour palettes the pattern's 4-bit
    /// indices read: `palette * 16 + index`, the caller's CRAM base to add.
    pub palette: u8,
    /// Bit 12: draw the pattern upside down ([`tile_row`] mirrors `row`).
    pub vflip: bool,
    /// Bit 11: draw the pattern left for right ([`tile_row`] reverses the row).
    pub hflip: bool,
    /// Bits 10-0: the pattern name, "the upper 11 bits of the physical address
    /// of pattern in video RAM" (genvdp.txt section 13), so [`tile_addr`] of it
    /// is the address to fetch it at.
    pub tile: u16,
}

impl NameEntry {
    /// The name table `word`, split into its five fields.
    ///
    /// Everything the word holds goes somewhere: the layout of genvdp.txt
    /// section 13 has no spare bits.
    #[inline]
    pub const fn decode(word: u16) -> Self {
        Self {
            priority: word & 0x8000 != 0,
            palette: ((word >> 13) & 0x03) as u8,
            vflip: word & 0x1000 != 0,
            hflip: word & 0x0800 != 0,
            tile: word & 0x07FF,
        }
    }
}

/// The VRAM byte address of `tile`'s first row, for `tile_height` of 8 or 16.
///
/// genvdp.txt section 13: "The pattern name is the upper 11 bits of the
/// physical address of pattern in video RAM. Bit zero of the name is ignored in
/// interlace mode 2." An 8x8 pattern therefore starts at `tile * 32`, and an
/// 8x16 one at `(tile & !1) * 32`: sixteen rows of [`ROW_BYTES`] bytes need a
/// row counter of 4 bits whose top step is 32 bytes, exactly what name bit 0
/// selects, so the VDP drops the name bit instead of addressing a 128 KiB VRAM
/// it does not have.
///
/// `tile * 64`, which is how the same rule is sometimes written, is a different
/// address: it numbers the 8x16 patterns instead of the 32-byte blocks the name
/// field actually selects, so it doubles every address below `tile_addr` and
/// would need a 128 KiB VRAM to hold an 11-bit name. The 8x16 case is interlace
/// mode 2 (M4-20, #98), whose cell height `crate::planes` reads from R12 and
/// passes to this function and to `tile_row` from the plane, window and sprite
/// fetches that call it.
#[inline]
pub const fn tile_addr(tile: u16, tile_height: usize) -> usize {
    let name = tile as usize;
    let name = if tile_height == 16 { name & !1 } else { name };
    (name << 5) & VRAM_MASK
}

/// The 16-bit word at byte `addr` of `vram`, big endian: the high byte of the
/// word is at the even address, which is how the data port wrote it (M4-02,
/// #80 in `ports`).
///
/// `addr` is a byte count that may have run past VRAM on the way here - a
/// table base plus an index is how these addresses get made - so it is masked
/// to the 16 address lines VRAM has. A word at the very last byte takes its low
/// byte from address 0.
#[inline]
pub fn read_word(vram: &[u8], addr: usize) -> u16 {
    let addr = addr & VRAM_MASK;
    let high = vram[addr];
    let low = vram[(addr + 1) & VRAM_MASK];
    u16::from_be_bytes([high, low])
}

/// The 8 colour indices of one row of a pattern, flips applied.
///
/// `tile`, `vflip` and `hflip` are three of the fields of one [`NameEntry`]
/// and `row` is the row to draw, counting from the top of the tile: with
/// `vflip` set row 0 answers with the tile's bottom row, and with `hflip` set
/// the row comes back reversed. `tile_height` is 8 or 16 - genvdp.txt section
/// 12: "A pattern is an 8x8 or 8x16 (interlace mode 2 only) pixel block" - and
/// picks both the number of rows the flip counts over and the address rule of
/// [`tile_addr`]. `row` outside 0..tile_height wraps around inside the tile,
/// which is why `tile_height` has to be 8 or 16 and nothing else.
///
/// The indices are 0..15 of the entry's own palette and no transparency is
/// decided here: "A pixel within a pattern that uses value zero isn't shown"
/// (section 12) is the caller's test to make.
///
/// The high nibble of a byte is the left pixel, which is INFERRED: genvdp.txt
/// section 12 fixes the bit depth and the four bytes per row and no section of
/// it says which nibble of a byte comes first. The values this ticket gives for
/// bytes `01 23 45 67` are the high-nibble-left reading and the tests here lock
/// it in; plane rendering (M4-11 #89 onwards) is where a test ROM can falsify
/// it.
#[inline]
pub fn tile_row(
    vram: &[u8],
    tile: u16,
    row: usize,
    vflip: bool,
    hflip: bool,
    tile_height: usize,
) -> [u8; 8] {
    let last_row = tile_height - 1;
    let row = row & last_row;
    let row = if vflip { last_row - row } else { row };
    let base = tile_addr(tile, tile_height) + row * ROW_BYTES;

    let mut pixels = [0u8; 8];
    for (x, pixel) in pixels.iter_mut().enumerate() {
        let byte = vram[(base + (x / 2)) & VRAM_MASK];
        *pixel = if x % 2 == 0 { byte >> 4 } else { byte & 0x0F };
    }
    if hflip {
        pixels.reverse();
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VRAM as the `Vdp` holds it: `VRAM_SIZE` bytes, heap allocated.
    fn blank_vram() -> Vec<u8> {
        vec![0; VRAM_SIZE]
    }

    /// The name table word of genvdp.txt section 13 is `pccvhnnnnnnnnnnn`, one
    /// field per bit or pair of bits.
    #[test]
    fn decode_pulls_each_field_out_of_its_own_bits() {
        assert!(NameEntry::decode(0x8000).priority);
        assert!(!NameEntry::decode(0x7FFF).priority);
        assert_eq!(NameEntry::decode(0x0000).palette, 0);
        assert_eq!(NameEntry::decode(0x2000).palette, 1);
        assert_eq!(NameEntry::decode(0x4000).palette, 2);
        assert_eq!(NameEntry::decode(0x6000).palette, 3);
        assert!(NameEntry::decode(0x1000).vflip);
        assert!(!NameEntry::decode(0xEFFF).vflip);
        assert!(NameEntry::decode(0x0800).hflip);
        assert!(!NameEntry::decode(0xF7FF).hflip);
        assert_eq!(NameEntry::decode(0x07FF).tile, 0x7FF);
        assert_eq!(NameEntry::decode(0xF800).tile, 0);
    }

    /// All five fields at once, and the all-zero entry a blank name table holds.
    #[test]
    fn decode_reads_the_whole_entry() {
        assert_eq!(
            NameEntry::decode(0x0000),
            NameEntry {
                priority: false,
                palette: 0,
                vflip: false,
                hflip: false,
                tile: 0,
            }
        );
        assert_eq!(
            NameEntry::decode(0xA123),
            NameEntry {
                priority: true,
                palette: 1,
                vflip: false,
                hflip: false,
                tile: 0x123,
            }
        );
        assert_eq!(
            NameEntry::decode(0xFFFF),
            NameEntry {
                priority: true,
                palette: 3,
                vflip: true,
                hflip: true,
                tile: 0x7FF,
            }
        );
    }

    /// genvdp.txt section 12: a pattern row is 4 bytes of 4-bit pixels. Which
    /// nibble of a byte is the left pixel the doc never says (see `tile_row`);
    /// the high nibble is, so bytes `01 23 45 67` read left to right are pixels
    /// 0..8, and `hflip` reverses them.
    #[test]
    fn tile_row_reads_the_high_nibble_first_and_hflip_reverses_the_row() {
        let mut vram = blank_vram();
        vram[32..36].copy_from_slice(&[0x01, 0x23, 0x45, 0x67]);
        assert_eq!(
            tile_row(&vram, 1, 0, false, false, 8),
            [0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(
            tile_row(&vram, 1, 0, false, true, 8),
            [7, 6, 5, 4, 3, 2, 1, 0]
        );
    }

    /// `vflip` mirrors the row index inside the tile, so row 0 of a flipped tile
    /// is its last row of data.
    #[test]
    fn tile_row_vflip_reads_from_the_far_end_of_the_tile() {
        let mut vram = blank_vram();
        vram[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        vram[28..32].copy_from_slice(&[0xAB, 0xCD, 0xEF, 0x0F]);
        assert_eq!(
            tile_row(&vram, 0, 0, true, false, 8),
            [0xA, 0xB, 0xC, 0xD, 0xE, 0xF, 0x0, 0xF]
        );
        assert_eq!(
            tile_row(&vram, 0, 7, true, false, 8),
            [1, 1, 2, 2, 3, 3, 4, 4]
        );
        assert_eq!(
            tile_row(&vram, 0, 0, true, true, 8),
            [0xF, 0x0, 0xF, 0xE, 0xD, 0xC, 0xB, 0xA]
        );
    }

    /// The pattern name is the upper 11 bits of the pattern address
    /// (genvdp.txt section 13), so the last tile of all starts 0x20 below the
    /// top of VRAM.
    #[test]
    fn tile_index_0x7ff_reads_the_top_of_vram() {
        let mut vram = blank_vram();
        assert_eq!(tile_addr(0x7FF, 8), 0xFFE0);
        vram[0xFFE0..0xFFE4].copy_from_slice(&[0x01, 0x23, 0x45, 0x67]);
        assert_eq!(
            tile_row(&vram, 0x7FF, 0, false, false, 8),
            [0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(tile_row(&vram, 0x7FF, 7, false, false, 8), [0; 8]);
    }

    /// Interlace mode 2: patterns are 8x16, so 16 rows of 4 bytes lie end to
    /// end and bit 0 of the name is dropped (genvdp.txt sections 12 and 13).
    #[test]
    fn tile_height_16_spans_64_bytes_and_drops_bit_0_of_the_name() {
        let mut vram = blank_vram();
        for row in 0..16u8 {
            let start = 64 + usize::from(row) * 4;
            vram[start] = row << 4;
            vram[start + 1] = row;
        }
        assert_eq!(tile_addr(2, 16), 64);
        assert_eq!(tile_addr(3, 16), 64, "bit 0 of the name is ignored");
        assert_eq!(tile_addr(1, 16), 0, "so tile 1 is tile 0 at 8x16");
        assert_eq!(
            tile_row(&vram, 2, 15, false, false, 16),
            [0xF, 0, 0, 0xF, 0, 0, 0, 0]
        );
        assert_eq!(
            tile_row(&vram, 3, 15, false, false, 16),
            [0xF, 0, 0, 0xF, 0, 0, 0, 0]
        );
        assert_eq!(
            tile_row(&vram, 2, 0, true, false, 16),
            [0xF, 0, 0, 0xF, 0, 0, 0, 0]
        );
        assert_eq!(
            tile_row(&vram, 2, 0, false, false, 16),
            [0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    /// VRAM holds words big endian (the data port wrote them that way in
    /// `ports`, M4-02 #80), and every VRAM address is a 16-bit one.
    #[test]
    fn read_word_is_big_endian_and_wraps_at_the_top_of_vram() {
        let mut vram = blank_vram();
        vram[0x10] = 0x12;
        vram[0x11] = 0x34;
        vram[0x21] = 0xAB;
        vram[0x22] = 0xCD;
        vram[VRAM_SIZE - 1] = 0xFF;
        assert_eq!(read_word(&vram, 0x10), 0x1234);
        assert_eq!(read_word(&vram, 0x21), 0xABCD, "an odd address is legal");
        assert_eq!(read_word(&vram, VRAM_SIZE + 0x10), 0x1234);
        assert_eq!(read_word(&vram, VRAM_SIZE - 1), 0xFF00);
    }
}
