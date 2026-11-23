//! The copy protection registers of Taiwanese unlicensed carts (#669).
//!
//! A number of these boards carry a small chip that answers fixed bytes at a
//! few addresses outside the ROM, and the game reads them at boot and crashes
//! (or loops) when they are missing. Three carts in the local set stopped in the
//! `JMP $000200` every one of their exception vectors points at because of it.
//!
//! Nothing in the header says a cart has the chip, so a board is picked out by
//! its two checksums, the one the header claims at 0x18E and the one
//! [`compute_checksum`] works out, the way Genesis Plus GX does it: its
//! `rom_database` in `core/cart_hw/md_cart.c` is where the addresses and values
//! below come from, entry by entry. Only the boards a local image has been
//! measured on are listed; the rest of that table can come over cart by cart,
//! with a name and a measurement each.
//!
//! The registers are read-only. GPGX installs `m68k_unused_8_w` as the write
//! handler of both entries here, so a write to the area is dropped, as it is on
//! a cart without the chip.

use super::header::{compute_checksum, parse_header};

/// One register: the byte the chip answers at `addr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Register {
    addr: u32,
    value: u8,
}

/// Which data lines the chip drives.
///
/// GPGX's `default_regs_r` answers the byte on the low lane of a word read
/// (`D0-D7`) and `default_regs_r_16` on the high one (`D8-D15`): the latter is
/// the "uses 16-bits reads" of the table's comments, carts whose code reads the
/// register with `MOVE.W` and looks at the top byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Low,
    High,
}

/// A board's protection chip, from [`detect`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Protection {
    registers: &'static [Register],
    lane: Lane,
}

/// A row of the table: the two checksums that name the board, and its chip.
struct Board {
    header_checksum: u16,
    checksum: u16,
    registers: &'static [Register],
    lane: Lane,
}

/// The boards, with GPGX's comment for each.
const BOARDS: &[Board] = &[
    // "Rockman X3 (bootleg version ? two last register returned values are
    // ignored, note that 0xaa/0x18 would work as well)": {0x0000,0x9d0e}, one
    // register on the /TIME area at $A13000 (`time_r = default_regs_r`) and two
    // in the expansion area.
    Board {
        header_checksum: 0x0000,
        checksum: 0x9D0E,
        registers: &[
            Register {
                addr: 0xA1_3000,
                value: 0x0C,
            },
            Register {
                addr: 0x40_0004,
                value: 0xC9,
            },
            Register {
                addr: 0x40_0006,
                value: 0xF0,
            },
        ],
        lane: Lane::Low,
    },
    // "Shi Liu Zhang Ma Jiang II - 16 Tiles Mahjong II (uses 16-bits reads)":
    // {0xffff,0x0903}, one register.
    Board {
        header_checksum: 0xFFFF,
        checksum: 0x0903,
        registers: &[Register {
            addr: 0x40_0004,
            value: 0xC9,
        }],
        lane: Lane::High,
    },
];

/// The protection chip of `rom`, or `None` for every cart not in [`BOARDS`].
pub fn detect(rom: &[u8]) -> Option<Protection> {
    let header = parse_header(rom).ok()?;
    // The header match is the cheap one; only a cart that passes it pays for the
    // sum over the whole image.
    if !BOARDS
        .iter()
        .any(|board| board.header_checksum == header.checksum)
    {
        return None;
    }
    let sum = compute_checksum(rom);
    BOARDS
        .iter()
        .find(|board| board.header_checksum == header.checksum && board.checksum == sum)
        .map(|board| Protection {
            registers: board.registers,
            lane: board.lane,
        })
}

impl Protection {
    /// The byte the chip answers at `addr`, or `None` where it has nothing.
    ///
    /// A [`Lane::Low`] chip answers its byte at either address of the register,
    /// as GPGX's `default_regs_r` does for a byte read. A [`Lane::High`] chip
    /// drives the even byte of its word, so a byte read finds the value there
    /// and 0 on the odd byte. GPGX's `_16` handler serves byte reads with the
    /// same `regs << 8` it serves words with, which truncates to 0 on either
    /// byte; no cart is known to read one of these a byte at a time, and the
    /// lane is what the word read says the wire is.
    pub fn read_byte(&self, addr: u32) -> Option<u8> {
        let [high, low] = self.read_word(addr & !1)?.to_be_bytes();
        Some(match self.lane {
            Lane::Low => low,
            Lane::High if addr & 1 == 0 => high,
            Lane::High => low,
        })
    }

    /// The word the chip answers at `addr` (even), or `None` where it has
    /// nothing.
    pub fn read_word(&self, addr: u32) -> Option<u16> {
        let register = self.registers.iter().find(|reg| reg.addr == addr)?;
        Some(match self.lane {
            Lane::Low => u16::from(register.value),
            Lane::High => u16::from(register.value) << 8,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 0x400 byte image whose header claims `header_checksum` and whose words
    /// from 0x200 sum to `sum`.
    fn rom(header_checksum: u16, sum: u16) -> Vec<u8> {
        let mut rom = vec![0u8; 0x400];
        rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");
        rom[0x18E..0x190].copy_from_slice(&header_checksum.to_be_bytes());
        rom[0x200..0x202].copy_from_slice(&sum.to_be_bytes());
        rom
    }

    fn chip(header_checksum: u16, sum: u16) -> Option<Protection> {
        detect(&rom(header_checksum, sum))
    }

    #[test]
    fn the_rockman_x3_bootleg_answers_a13000_and_two_expansion_registers() {
        let chip = chip(0x0000, 0x9D0E).expect("the board is in the table");
        assert_eq!(chip.read_byte(0xA1_3000), Some(0x0C));
        assert_eq!(chip.read_word(0x40_0004), Some(0x00C9));
        assert_eq!(chip.read_byte(0x40_0007), Some(0xF0));
        assert_eq!(chip.read_word(0x40_0000), None);
    }

    #[test]
    fn mahjong_ii_answers_on_the_high_lane() {
        let chip = chip(0xFFFF, 0x0903).expect("the board is in the table");
        assert_eq!(chip.read_word(0x40_0004), Some(0xC900));
        assert_eq!(chip.read_byte(0x40_0004), Some(0xC9));
        assert_eq!(chip.read_byte(0x40_0005), Some(0x00));
        assert_eq!(chip.read_word(0x40_0006), None);
    }

    #[test]
    fn a_cart_needs_both_checksums_to_have_the_chip() {
        assert_eq!(
            chip(0x0000, 0x0903),
            None,
            "the real sum of the wrong board"
        );
        assert_eq!(
            chip(0x1234, 0x9D0E),
            None,
            "the right sum under another header"
        );
    }
}
