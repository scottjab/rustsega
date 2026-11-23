//! The copy protection hardware of Taiwanese unlicensed carts (#669, #670).
//!
//! A number of these boards carry a small chip that answers bytes at a few
//! addresses outside the ROM, and the game reads them at boot and crashes (or
//! loops) when they are missing. Some of the chips are only a few fixed bytes;
//! some take writes and answer what was written, shifted or bit-swapped; and two
//! boards bank the low megabyte of the image in 32 KiB steps from writes to
//! `$700000-$7FFFFF` ([`crate::cart::Mapper::Bank32k`]).
//!
//! Nothing in the header says a cart has one, so a board is picked out by its
//! two checksums, the one the header claims at 0x18E and the one
//! [`compute_checksum`] works out, the way Genesis Plus GX does it: its
//! `rom_database` in `core/cart_hw/md_cart.c` is where every address, value and
//! behaviour below comes from, entry by entry, with the access rules of its
//! `default_regs_r`, `default_regs_r_16`, `custom_regs_r`, `default_regs_w` and
//! `custom_regs_w`, and of `mem68k.c`'s `/TIME` cases. Only the boards a local
//! image has been measured on are listed; the rest of that table can come over
//! cart by cart, with a name and a measurement each.
//!
//! The registers a cart can write are machine state and live on the bus
//! ([`Registers`], saved with it); this module holds the fixed part, the board.

use super::header::{compute_checksum, parse_header};

/// The four register bytes of a chip, indexed as the board's [`Register`]s are.
pub type Registers = [u8; 4];

/// One register: the addresses that reach it, as GPGX's `(address & mask) ==
/// addr` decode, and the byte it holds at power on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Register {
    addr: u32,
    mask: u32,
    value: u8,
}

/// A register at exactly one address.
const fn at(addr: u32, value: u8) -> Register {
    Register {
        addr,
        mask: 0xFF_FFFF,
        value,
    }
}

/// How the `/TIME` block (`$A13000-$A130FF`) answers a read of a register.
///
/// Both are word answers: GPGX's `ctrl_io_read_word` hands back what the
/// handler returns, and its byte read takes the low byte at an odd address and
/// the high byte at an even one, so a byte read of a register at an even
/// address reads 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Time {
    /// `default_regs_r`: the register's byte.
    Plain,
    /// `custom_regs_r`: the register's byte shifted right by one ("basic
    /// register shifting hardware (Bug's Life, Pocket Monster)").
    Shifted,
}

/// Which data lines a chip in the expansion area drives on a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    /// `default_regs_r`: the byte on a byte read of the register's own
    /// address, and the low half of a word read.
    Low,
    /// `default_regs_r_16`, "uses 16-bits reads": the high half of a word read,
    /// so the byte at the even address.
    High,
}

/// The part of the expansion area (`$400000-$7FFFFF`) a board decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    /// First and last address, GPGX's `bank_start`/`bank_end` in 64 KiB units.
    start: u32,
    end: u32,
    /// Whether reads reach the registers (`regs_r`), and on which lanes.
    read: Option<Lane>,
    /// Whether writes reach the board (`regs_w = custom_regs_w`): the
    /// registers below `$700000`, with the bit swap, and the 32 KiB bank from
    /// there up.
    write: bool,
}

/// A row of the table: the two checksums that name the board, and its chip.
#[derive(Debug, PartialEq, Eq)]
pub struct Board {
    header_checksum: u16,
    checksum: u16,
    registers: &'static [Register],
    /// The `/TIME` read handler (`time_r`), for the boards that have one.
    time: Option<Time>,
    /// Whether `/TIME` writes store into the registers (`time_w =
    /// default_regs_w`); every other board's is `m68k_unused_8_w`.
    time_write: bool,
    area: Option<Area>,
}

/// The `$A13000`/`$A13002`/`$A1303E` register set the Bug's Life and Pocket
/// Monster boards share, with the first one's value.
const fn shifter(first: u8) -> [Register; 3] {
    [
        at(0xA1_3000, first),
        at(0xA1_3002, 0x01),
        at(0xA1_303E, 0x1F),
    ]
}

/// The boards, with GPGX's comment for each.
const BOARDS: &[Board] = &[
    // "Rockman X3 (bootleg version ? two last register returned values are
    // ignored, note that 0xaa/0x18 would work as well)": {0x0000,0x9d0e}.
    Board {
        header_checksum: 0x0000,
        checksum: 0x9D0E,
        registers: &[
            at(0xA1_3000, 0x0C),
            at(0x40_0004, 0xC9),
            at(0x40_0006, 0xF0),
        ],
        time: Some(Time::Plain),
        time_write: false,
        area: Some(Area {
            start: 0x40_0000,
            end: 0x40_FFFF,
            read: Some(Lane::Low),
            write: false,
        }),
    },
    // "Shi Liu Zhang Ma Jiang II - 16 Tiles Mahjong II (uses 16-bits reads)":
    // {0xffff,0x0903}.
    Board {
        header_checksum: 0xFFFF,
        checksum: 0x0903,
        registers: &[at(0x40_0004, 0xC9)],
        time: None,
        time_write: false,
        area: Some(Area {
            start: 0x40_0000,
            end: 0x40_FFFF,
            read: Some(Lane::High),
            write: false,
        }),
    },
    // "A Bug's Life (bootleg version ? two last register returned values are
    // ignored & first register test has been modified ?)": {0x7f7f,0x2aad}.
    Board {
        header_checksum: 0x7F7F,
        checksum: 0x2AAD,
        registers: &shifter(0x28),
        time: Some(Time::Plain),
        time_write: false,
        area: None,
    },
    // "Pocket Monster": {0xd6fc,0x1eb1}, the shifter itself.
    Board {
        header_checksum: 0xD6FC,
        checksum: 0x1EB1,
        registers: &shifter(0x00),
        time: Some(Time::Shifted),
        time_write: true,
        area: None,
    },
    // "Pocket Monster (bootleg version ? two last register returned values are
    // ignored & first register test has been modified)": {0xd6fc,0x6319}.
    Board {
        header_checksum: 0xD6FC,
        checksum: 0x6319,
        registers: &shifter(0x14),
        time: Some(Time::Plain),
        time_write: false,
        area: None,
    },
    // "Pokemon Stadium": {0x0000,0x843c}, the 32 KiB bank and nothing else.
    Board {
        header_checksum: 0x0000,
        checksum: 0x843C,
        registers: &[],
        time: None,
        time_write: false,
        area: Some(Area {
            start: 0x70_0000,
            end: 0x7F_FFFF,
            read: None,
            write: true,
        }),
    },
    // "Soul Edge VS Samurai Spirits": {0x00ff,0x5d34}, three registers repeated
    // through `$600000-$6FFFFF` and the 32 KiB bank above them.
    Board {
        header_checksum: 0x00FF,
        checksum: 0x5D34,
        registers: &[
            Register {
                addr: 0x60_0001,
                mask: 0xF0_0007,
                value: 0,
            },
            Register {
                addr: 0x60_0003,
                mask: 0xF0_0007,
                value: 0,
            },
            Register {
                addr: 0x60_0005,
                mask: 0xF0_0007,
                value: 0,
            },
        ],
        time: None,
        time_write: false,
        area: Some(Area {
            start: 0x60_0000,
            end: 0x7F_FFFF,
            read: Some(Lane::Low),
            write: true,
        }),
    },
];

/// Where `custom_regs_w` stops writing registers and starts switching banks:
/// `(address >> 16) > 0x6f`.
const BANK_WRITES: u32 = 0x70_0000;

/// What a write to the expansion area did, for the bus to finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaWrite {
    /// The board does not decode the address: the write goes where it would
    /// on a cart without one.
    Ignored,
    /// The registers took it.
    Registers,
    /// A 32 KiB bank select, for [`crate::cart::Mapper::set_bank_32k`].
    Bank(u16),
}

/// A board's protection hardware, from [`detect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protection {
    board: &'static Board,
}

/// The protection hardware of `rom`, or `None` for every cart not in the table.
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
        .map(|board| Protection { board })
}

impl Protection {
    /// The registers as the board powers on, and as a reset leaves them: GPGX
    /// copies them out of the table at load and only a Realtec board's are
    /// cleared by `md_cart_reset`.
    pub fn power_on_registers(&self) -> Registers {
        let mut regs = [0; 4];
        for (reg, register) in regs.iter_mut().zip(self.board.registers) {
            *reg = register.value;
        }
        regs
    }

    /// Whether this board banks the low megabyte in 32 KiB steps.
    pub fn banks_32k(&self) -> bool {
        self.board
            .area
            .is_some_and(|area| area.write && area.end >= BANK_WRITES)
    }

    /// The index of the register `addr` reaches, if any.
    fn register(&self, addr: u32) -> Option<usize> {
        self.board
            .registers
            .iter()
            .position(|reg| addr & reg.mask == reg.addr)
    }

    /// The word the `/TIME` block answers at `addr`, or `None` where the board
    /// has nothing to say and the block answers as it would without one.
    pub fn read_time_word(&self, regs: &Registers, addr: u32) -> Option<u16> {
        let time = self.board.time?;
        let value = regs[self.register(addr)?];
        Some(u16::from(match time {
            Time::Plain => value,
            Time::Shifted => value >> 1,
        }))
    }

    /// The byte the `/TIME` block answers at `addr`: the lane of the handler's
    /// word that `addr` is on (`ctrl_io_read_byte`).
    pub fn read_time_byte(&self, regs: &Registers, addr: u32) -> Option<u8> {
        let [high, low] = self.read_time_word(regs, addr)?.to_be_bytes();
        Some(if addr & 1 == 1 { low } else { high })
    }

    /// A `/TIME` write of `value` to `addr`: whether the board took it.
    pub fn write_time(&self, regs: &mut Registers, addr: u32, value: u8) -> bool {
        if !self.board.time_write {
            return false;
        }
        match self.register(addr) {
            Some(index) => {
                regs[index] = value;
                true
            }
            None => false,
        }
    }

    /// The area the board decodes, if `addr` is in it.
    fn area(&self, addr: u32) -> Option<Area> {
        self.board
            .area
            .filter(|area| (area.start..=area.end).contains(&addr))
    }

    /// The byte the expansion area answers at `addr`, or `None` for the float.
    pub fn read_area_byte(&self, regs: &Registers, addr: u32) -> Option<u8> {
        match self.area(addr)?.read? {
            Lane::Low => self.register(addr).map(|index| regs[index]),
            Lane::High => {
                let index = self.register(addr & !1)?;
                Some(if addr & 1 == 0 { regs[index] } else { 0 })
            }
        }
    }

    /// The word the expansion area answers at `addr`, or `None` for the float.
    pub fn read_area_word(&self, regs: &Registers, addr: u32) -> Option<u16> {
        let lane = self.area(addr)?.read?;
        let value = u16::from(regs[self.register(addr)?]);
        Some(match lane {
            Lane::Low => value,
            Lane::High => value << 8,
        })
    }

    /// A write of `value` to the expansion area at `addr` (`custom_regs_w`): a
    /// word write hands the whole word over, a byte write the byte.
    ///
    /// Below `$700000` it stores into the register `addr` reaches, which keeps
    /// the low byte, and then sets register 2 from register 0 by the mode in
    /// register 1's low two bits: shifted left, shifted right, nibbles swapped,
    /// or bit-reversed. From `$700000` up it is a bank select.
    pub fn write_area(&self, regs: &mut Registers, addr: u32, value: u16) -> AreaWrite {
        if !self.area(addr).is_some_and(|area| area.write) {
            return AreaWrite::Ignored;
        }
        if addr >= BANK_WRITES {
            return AreaWrite::Bank(value);
        }
        if let Some(index) = self.register(addr) {
            regs[index] = value as u8;
        }
        let temp = regs[0];
        regs[2] = match regs[1] & 3 {
            0 => temp << 1,
            1 => temp >> 1,
            2 => temp.rotate_left(4),
            _ => temp.reverse_bits(),
        };
        AreaWrite::Registers
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
        let regs = chip.power_on_registers();
        assert_eq!(chip.read_time_word(&regs, 0xA1_3000), Some(0x000C));
        assert_eq!(
            chip.read_time_byte(&regs, 0xA1_3000),
            Some(0x00),
            "the even lane of $000C"
        );
        assert_eq!(chip.read_area_word(&regs, 0x40_0004), Some(0x00C9));
        assert_eq!(chip.read_area_byte(&regs, 0x40_0006), Some(0xF0));
        assert_eq!(chip.read_area_byte(&regs, 0x40_0007), None);
        assert_eq!(chip.read_area_word(&regs, 0x40_0000), None);
    }

    #[test]
    fn mahjong_ii_answers_on_the_high_lane() {
        let chip = chip(0xFFFF, 0x0903).expect("the board is in the table");
        let regs = chip.power_on_registers();
        assert_eq!(chip.read_area_word(&regs, 0x40_0004), Some(0xC900));
        assert_eq!(chip.read_area_byte(&regs, 0x40_0004), Some(0xC9));
        assert_eq!(chip.read_area_byte(&regs, 0x40_0005), Some(0x00));
        assert_eq!(chip.read_area_word(&regs, 0x40_0006), None);
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

    #[test]
    fn pocket_monster_answers_what_was_written_shifted_right() {
        let chip = chip(0xD6FC, 0x1EB1).expect("the board is in the table");
        let mut regs = chip.power_on_registers();
        assert_eq!(
            chip.read_time_word(&regs, 0xA1_303E),
            Some(0x000F),
            "$1F >> 1"
        );
        assert!(chip.write_time(&mut regs, 0xA1_3000, 0x50));
        assert_eq!(chip.read_time_word(&regs, 0xA1_3000), Some(0x0028));
        assert!(
            !chip.write_time(&mut regs, 0xA1_3004, 0x50),
            "no register there"
        );
    }

    #[test]
    fn the_bootleg_pocket_monster_does_not_take_writes() {
        let chip = chip(0xD6FC, 0x6319).expect("the board is in the table");
        let mut regs = chip.power_on_registers();
        assert!(!chip.write_time(&mut regs, 0xA1_3000, 0x50));
        assert_eq!(chip.read_time_word(&regs, 0xA1_3000), Some(0x0014));
    }

    #[test]
    fn soul_edge_swaps_register_0_into_register_2_by_the_mode_in_register_1() {
        let chip = chip(0x00FF, 0x5D34).expect("the board is in the table");
        let mut regs = chip.power_on_registers();
        assert_eq!(
            chip.write_area(&mut regs, 0x60_0001, 0x81),
            AreaWrite::Registers
        );
        for (mode, swapped) in [(0, 0x02), (1, 0x40), (2, 0x18), (3, 0x81)] {
            // The registers repeat through the area: `$6x xxx3` is register 1.
            chip.write_area(&mut regs, 0x6A_BCD3, mode);
            assert_eq!(
                chip.read_area_byte(&regs, 0x60_0005),
                Some(swapped),
                "mode {mode}"
            );
        }
        assert_eq!(
            chip.write_area(&mut regs, 0x70_0000, 0x0005),
            AreaWrite::Bank(5)
        );
        assert!(chip.banks_32k());
    }

    #[test]
    fn pokemon_stadium_only_banks() {
        let chip = chip(0x0000, 0x843C).expect("the board is in the table");
        let mut regs = chip.power_on_registers();
        assert!(chip.banks_32k());
        assert_eq!(chip.write_area(&mut regs, 0x60_0001, 1), AreaWrite::Ignored);
        assert_eq!(
            chip.write_area(&mut regs, 0x7F_FFFE, 0x0102),
            AreaWrite::Bank(0x0102)
        );
        assert_eq!(chip.read_area_byte(&regs, 0x70_0000), None);
    }
}
