//! The 68k side of the memory map: cartridge ROM, work RAM, and one stub arm
//! per region for the device tickets to fill in.
//!
//! The layout is the "68000 address space overview" table of plutiedev "Memory
//! map", which is what the private `Region` enum reproduces. The 68000 has a 24
//! bit address bus (M68000UM, "Address Definition": A23-A1 are decoded, A0
//! selects the byte lane), so the top 8 bits of a `u32` address are never
//! meaningful and every address decodes exactly once.
//!
//! ## Who masks
//!
//! The CPU masks every address with [`m68k::ADDR_MASK`] before it calls a
//! [`m68k::Bus`] method (see `read_byte` and friends in
//! `crates/m68k/src/cpu.rs`), which is why the trait says implementors "never
//! have to". This bus masks again, once at each of the four entry points.
//! [`m68k::Bus`] is public API and its accessors are called directly as well as
//! through a CPU: by the tests here, and by the device tickets that will drive
//! this bus from their own tests before any CPU exists. Masking at dispatch
//! costs one `and` on a value that is already in a register and guarantees the
//! region functions never see an address outside the region that decoded them.
//!
//! ## Reading what is not there
//!
//! Reads from an area nothing drives return `OPEN_BUS` (0xFF) and writes there
//! are dropped. That is a placeholder, not a model: on a real console the data
//! bus
//! floats, so the value read is whatever the previous transfer left on the
//! lines, and some decoded-but-unpopulated corners stall the bus instead -
//! plutiedev "Memory map" documents 0xC00020-0xC0003F, inside the VDP area, as
//! "hangs up". Modelling open bus is M3-16 (#77).
//!
//! [`m68k::Bus::int_ack`] is deliberately left at the trait default: returning
//! `None` *is* what a Genesis does, always autovectoring.
//! [`m68k::Bus::reset_devices`] is implemented for [`Bus::io`], the first device
//! behind the bus; the rest arrive with M3-08 (#68) and M3-09 (#69).

use crate::io::Io;
use m68k::ADDR_MASK;

/// What an undriven data bus reads as: the placeholder for every area that
/// nothing drives yet. See the module docs, and M3-16 (#77) for the real thing.
const OPEN_BUS: u8 = 0xFF;
/// A word read entirely from an area that nothing drives.
const OPEN_BUS_WORD: u16 = 0xFFFF;

// Region boundaries, from plutiedev "Memory map".
const ROM_START: u32 = 0x00_0000;
const ROM_END: u32 = 0x3F_FFFF;
const Z80_START: u32 = 0xA0_0000;
const Z80_END: u32 = 0xA0_FFFF;
const IO_START: u32 = 0xA1_0000;
const IO_END: u32 = 0xA1_0FFF;
const Z80_CTRL_START: u32 = 0xA1_1000;
const Z80_CTRL_END: u32 = 0xA1_1FFF;
const CART_EXTRA_START: u32 = 0xA1_3000;
const CART_EXTRA_END: u32 = 0xA1_30FF;
const TMSS_START: u32 = 0xA1_4000;
const TMSS_END: u32 = 0xA1_4FFF;
const VDP_START: u32 = 0xC0_0000;
const VDP_END: u32 = 0xDF_FFFF;
const RAM_START: u32 = 0xE0_0000;
const RAM_END: u32 = 0xFF_FFFF;
/// Work RAM is 64 KiB behind a 2 MiB window, so only these bits select a byte.
const RAM_MASK: u32 = 0x0000_FFFF;
/// The hardware version register, the first word of the I/O area: 0xA10001 read
/// as a byte, 0xA10000 read as a word (plutiedev "Using the Sub-CPU in mode 1",
/// `HwVersion: equ $A10001`).
const IO_VERSION: u32 = IO_START;
/// Address mask that drops A0: the I/O registers sit two bytes apart and decode
/// without A0 (plutiedev "I/O ports" lists them at $A10001, $A10003, ..., and
/// BlastEm's `io_read` decodes with `location >> 1`), so `addr & EVEN_LANE`
/// names the register a masked I/O address belongs to.
const EVEN_LANE: u32 = !1;

/// A decoded region of the 24 bit address space.
///
/// Ranges are the 68000 address space overview of plutiedev "Memory map". A
/// range the console decodes gets its own variant even when nothing implements
/// it yet, so a device ticket fills in its own functions instead of cutting a
/// new arm out of the dispatch.
enum Region {
    /// 0x000000-0x3FFFFF, the cartridge slot.
    Rom,
    /// 0xE00000-0xFFFFFF, 64 KiB of work RAM mirrored every 0x10000.
    Ram,
    /// 0xC00000-0xDFFFFF, VDP and PSG ports.
    ///
    /// TODO(M3-09 #69): the data/control/HV-counter/PSG ports at
    /// 0xC00000-0xC0001C, mirrored every 0x40 (plutiedev "Memory map").
    Vdp,
    /// 0xA00000-0xA0FFFF, the Z80 bus seen from the 68k.
    ///
    /// TODO(M3-08 #68): Z80 RAM, YM2612 and PSG through the sound CPU's side.
    Z80,
    /// 0xA10000-0xA10FFF, I/O: version register and the three controller ports -
    /// the whole decoded area, not just the 0xA10000-0xA1001F block of registers
    /// (plutiedev "I/O ports", and "68000 address space overview" for the width
    /// of the area).
    ///
    /// [`Bus::version`] answers [`IO_VERSION`] and [`Bus::io`] the register block
    /// at 0xA10000-0xA1001F; the rest of the area is [`OPEN_BUS`].
    Io,
    /// 0xA11000-0xA11FFF, Z80 bus control.
    ///
    /// TODO(M3-08 #68): BUSREQ at 0xA11100 and Z80 RESET at 0xA11200, which
    /// decide whether a 68k access to [`Region::Z80`] is allowed at all.
    Z80Ctrl,
    /// 0xA13000-0xA130FF, the cartridge slot extra.
    ///
    /// TODO(M3-11 #71): SRAM control at 0xA130F1-0xA130FF.
    /// TODO(M3-12 #72): the Sega SSF2 mapper in the same block.
    CartExtra,
    /// 0xA14000-0xA14FFF, TMSS ports.
    ///
    /// TODO(M3-10 #70): the revision register at 0xA14000 and the MMU registers
    /// at 0xA14101.
    Tmss,
    /// Everything the console does not decode: the expansion slot at
    /// 0x400000-0x7FFFFF, the 32X area at 0x800000-0x9FFFFF, the Sega CD extra
    /// at 0xA12000-0xA120FF and the 0xA15000-0xBFFFFF gap.
    ///
    /// TODO(M3-16 #77): open bus, including the corners of the map that stall
    /// the 68k instead of returning a value.
    Unmapped,
}

impl Region {
    /// Decode `addr`, which the caller has already masked to 24 bits.
    fn decode(addr: u32) -> Self {
        match addr {
            ROM_START..=ROM_END => Self::Rom,
            RAM_START..=RAM_END => Self::Ram,
            VDP_START..=VDP_END => Self::Vdp,
            Z80_START..=Z80_END => Self::Z80,
            IO_START..=IO_END => Self::Io,
            Z80_CTRL_START..=Z80_CTRL_END => Self::Z80Ctrl,
            CART_EXTRA_START..=CART_EXTRA_END => Self::CartExtra,
            TMSS_START..=TMSS_END => Self::Tmss,
            _ => Self::Unmapped,
        }
    }
}

/// The 68k memory map.
///
/// Only what exists today is a field; the device tickets add theirs. Every
/// region has a byte function and a word function, and a word is never built
/// from two byte calls, because the devices that land here (VDP, I/O) answer a
/// word differently from two bytes.
///
/// `Debug` prints all 64 KiB of RAM, so assert on `&bus.ram[a..b]` slices.
#[derive(Debug)]
pub struct Bus {
    /// Cartridge ROM in address order, mapped at 0x000000.
    ///
    /// A plain image on purpose: detection, deinterleave
    /// ([`crate::cart::deinterleave`]) and header decoding are steps of
    /// [`crate::cart`], and the code that turns a file into a `Bus` is a later
    /// ticket.
    pub rom: Vec<u8>,
    /// 64 KiB of work RAM, mapped at 0xE00000 and mirrored every 0x10000.
    pub ram: Box<[u8; 0x10000]>,
    /// The byte 0xA10001 reports: [`crate::Region::version_register`] for the
    /// region [`crate::Config::resolve_region`] picked, with that config's TMSS
    /// flag. See [`Bus::new`].
    pub version: u8,
    /// The I/O chip: the controller ports behind 0xA10003-0xA1000D and the serial
    /// registers behind 0xA1000F-0xA1001F.
    ///
    /// Public because a front end has to be able to say which buttons are held:
    /// `bus.io.set_buttons(port, buttons)` once per frame. See [`crate::Buttons`].
    pub io: Io,
}

impl Bus {
    /// A bus with `rom` loaded and `version` behind 0xA10001. Work RAM starts
    /// zeroed; a real console powers up with undefined contents, which nothing
    /// depends on yet.
    ///
    /// `version` is what the console decided to be, not something the bus
    /// works out: the caller has [`crate::Config`] and the header, and says
    ///
    /// ```ignore
    /// let region = config.resolve_region(&header);
    /// Bus::new(rom, region.version_register(config.tmss))
    /// ```
    ///
    /// Taking the byte rather than a whole [`crate::Config`] keeps the bus out of
    /// the parts of the config that are not addresses - and taking it at all,
    /// instead of defaulting a region here, is what stops a bus existing that
    /// quietly lies to a game about the machine it is running on.
    pub fn new(rom: Vec<u8>, version: u8) -> Self {
        Self {
            rom,
            ram: Box::new([0; 0x10000]),
            version,
            io: Io::new(),
        }
    }

    /// Cartridge slot byte read.
    ///
    /// A read above the end of the loaded image returns [`OPEN_BUS`]. Real
    /// hardware leaves the bus floating there, and some dumps are sold as
    /// mirrored copies of the ROM; either way an image shorter than the slot is
    /// the common case and 0xFF is what nothing driving the lines reads as.
    fn read_rom_byte(&mut self, addr: u32) -> u8 {
        let offset = (addr - ROM_START) as usize;
        self.rom.get(offset).copied().unwrap_or(OPEN_BUS)
    }

    /// Cartridge slot word read, big endian: the byte at the lower address is
    /// the high byte (M68000UM, "Data Organization").
    ///
    /// The two bytes are looked up separately rather than as a slice so that a
    /// word straddling the end of the image still returns the byte that is
    /// there, with [`OPEN_BUS`] in the half that is not.
    fn read_rom_word(&mut self, addr: u32) -> u16 {
        let offset = (addr - ROM_START) as usize;
        let high = self.rom.get(offset).copied().unwrap_or(OPEN_BUS);
        let low = self.rom.get(offset + 1).copied().unwrap_or(OPEN_BUS);
        u16::from_be_bytes([high, low])
    }

    /// Cartridge slot byte write: ignored, ROM is read-only.
    fn write_rom_byte(&mut self, _addr: u32, _value: u8) {}

    /// Cartridge slot word write: ignored, ROM is read-only.
    fn write_rom_word(&mut self, _addr: u32, _value: u16) {}

    /// The index of `addr` in [`Bus::ram`]: the 2 MiB window at 0xE00000 mirrors
    /// the same 64 KiB, so only the low 16 bits name a byte (plutiedev "Memory
    /// map"). `addr` is masked and already decoded as [`Region::Ram`].
    fn ram_index(addr: u32) -> usize {
        (addr & RAM_MASK) as usize
    }

    /// Work RAM byte read.
    fn read_ram_byte(&mut self, addr: u32) -> u8 {
        self.ram[Self::ram_index(addr)]
    }

    /// Work RAM word read, big endian (M68000UM, "Data Organization").
    ///
    /// Masking with [`RAM_MASK`] keeps the parity of the address, so an even
    /// `addr` always lands on an even index and the second byte is inside the
    /// array: the highest word the window can address is 0xFFFFFE. An odd
    /// address here is a caller bug and panics on the index.
    fn read_ram_word(&mut self, addr: u32) -> u16 {
        let index = Self::ram_index(addr);
        u16::from_be_bytes([self.ram[index], self.ram[index + 1]])
    }

    /// Work RAM byte write.
    fn write_ram_byte(&mut self, addr: u32, value: u8) {
        self.ram[Self::ram_index(addr)] = value;
    }

    /// Work RAM word write, big endian, at an even `addr`.
    fn write_ram_word(&mut self, addr: u32, value: u16) {
        let index = Self::ram_index(addr);
        self.ram[index..index + 2].copy_from_slice(&value.to_be_bytes());
    }

    /// VDP port byte read. Placeholder for [`Region::Vdp`]: returns open bus,
    /// where a real console reads the VDP data port, the status register, the HV
    /// counter or nothing, and 0xC00020-0xC0003F hangs the bus (plutiedev
    /// "Memory map").
    fn read_vdp_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// VDP port word read. Placeholder for [`Region::Vdp`].
    fn read_vdp_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// VDP port byte write. Placeholder for [`Region::Vdp`]: dropped, where a
    /// real console would push a byte to the PSG at 0xC00011.
    fn write_vdp_byte(&mut self, _addr: u32, _value: u8) {}

    /// VDP port word write. Placeholder for [`Region::Vdp`].
    fn write_vdp_word(&mut self, _addr: u32, _value: u16) {}

    /// Z80 bus byte read. Placeholder for [`Region::Z80`].
    fn read_z80_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// Z80 bus word read. Placeholder for [`Region::Z80`].
    fn read_z80_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// Z80 bus byte write. Placeholder for [`Region::Z80`].
    fn write_z80_byte(&mut self, _addr: u32, _value: u8) {}

    /// Z80 bus word write. Placeholder for [`Region::Z80`].
    fn write_z80_word(&mut self, _addr: u32, _value: u16) {}

    /// I/O byte read: the version register, then the I/O chip.
    ///
    /// A0 does not select between the two bytes of a register - the I/O
    /// registers are one per two bytes and decode without it (plutiedev "I/O
    /// ports" lists them two bytes apart; BlastEm's `io_read` decodes the pair
    /// with `location >> 1`) - so 0xA10000 answers as 0xA10001 does, and 0xA10002
    /// answers as 0xA10003 does. [`Io::read`] does that for its own registers and
    /// answers `None` past the end of the register block, where nothing drives the
    /// bus and [`OPEN_BUS`] is what a read gets.
    fn read_io_byte(&mut self, addr: u32) -> u8 {
        if is_version_register(addr) {
            self.version
        } else {
            self.io.read(addr).unwrap_or(OPEN_BUS)
        }
    }

    /// I/O word read: one register answers the whole 16 bit access, with its byte
    /// in both halves.
    ///
    /// Its own function rather than two [`Bus::read_io_byte`] calls, because a
    /// word is a different access here: the register pair decodes without A0, so
    /// one register answers the whole 16 bit access, and the answer is that byte
    /// twice - through [`io_word`], so the version register and the port registers
    /// of [`Region::Io`] cannot grow different word rules.
    ///
    /// BlastEm models it that way for the whole area: `io_read_w` in `genesis.c`
    /// reads the register once for a word access and mirrors it,
    /// `value = value | (value << 8)`.
    ///
    /// The alternative is MAME: `src/mame/sega/megadriv.cpp` maps 0xA10000-0xA10001
    /// to a 16 bit handler returning the byte zero extended, so a word read there
    /// reports 0 in the half of the word nothing is wired to. On real hardware
    /// that half floats, and only the byte read is documented (plutiedev "I/O
    /// ports": the I/O registers are "byte-sized"), so no game can be relying on
    /// either. The mirror is what keeps a word read consistent with the two byte
    /// reads it covers, which is why #65 kept it rather than the ticket's "the byte
    /// in the low half": the ticket's rule is MAME's, and taking it would have
    /// meant rewriting what #64 settled and documented for the version register in
    /// order to standardise the region on the half of the word nothing drives.
    ///
    /// An odd `addr` is an address error the 68k never completes, so nothing here
    /// needs to care which half of the pair was asked for.
    fn read_io_word(&mut self, addr: u32) -> u16 {
        io_word(self.read_io_byte(addr))
    }

    /// I/O byte write: the port and serial registers of [`Bus::io`]. The version
    /// register is read-only, and `Io::write` drops that write along with the ones
    /// past the end of the register block.
    fn write_io_byte(&mut self, addr: u32, value: u8) {
        self.io.write(addr, value);
    }

    /// I/O word write: the low byte of `value`, which is the byte at the odd
    /// address every register of the area is documented at.
    ///
    /// A 16 bit write to a byte register is not something a game does - the setup
    /// code of plutiedev "Controllers", "Setting up", is all `move.b` - so all this
    /// has to settle is which half of the word reaches the register, and it is the
    /// half that a split into two byte accesses delivers.
    fn write_io_word(&mut self, addr: u32, value: u16) {
        self.write_io_byte(addr, value as u8);
    }

    /// Z80 bus control byte read. Placeholder for [`Region::Z80Ctrl`].
    fn read_z80_ctrl_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// Z80 bus control word read. Placeholder for [`Region::Z80Ctrl`].
    fn read_z80_ctrl_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// Z80 bus control byte write. Placeholder for [`Region::Z80Ctrl`].
    fn write_z80_ctrl_byte(&mut self, _addr: u32, _value: u8) {}

    /// Z80 bus control word write. Placeholder for [`Region::Z80Ctrl`].
    fn write_z80_ctrl_word(&mut self, _addr: u32, _value: u16) {}

    /// Cartridge slot extra byte read. Placeholder for [`Region::CartExtra`].
    fn read_cart_extra_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// Cartridge slot extra word read. Placeholder for [`Region::CartExtra`].
    fn read_cart_extra_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// Cartridge slot extra byte write. Placeholder for [`Region::CartExtra`].
    fn write_cart_extra_byte(&mut self, _addr: u32, _value: u8) {}

    /// Cartridge slot extra word write. Placeholder for [`Region::CartExtra`].
    fn write_cart_extra_word(&mut self, _addr: u32, _value: u16) {}

    /// TMSS port byte read. Placeholder for [`Region::Tmss`].
    fn read_tmss_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// TMSS port word read. Placeholder for [`Region::Tmss`].
    fn read_tmss_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// TMSS port byte write. Placeholder for [`Region::Tmss`].
    fn write_tmss_byte(&mut self, _addr: u32, _value: u8) {}

    /// TMSS port word write. Placeholder for [`Region::Tmss`].
    fn write_tmss_word(&mut self, _addr: u32, _value: u16) {}

    /// Undecoded byte read. Placeholder for [`Region::Unmapped`].
    fn read_unmapped_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// Undecoded word read. Placeholder for [`Region::Unmapped`].
    fn read_unmapped_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// Undecoded byte write. Placeholder for [`Region::Unmapped`].
    fn write_unmapped_byte(&mut self, _addr: u32, _value: u8) {}

    /// Undecoded word write. Placeholder for [`Region::Unmapped`].
    fn write_unmapped_word(&mut self, _addr: u32, _value: u16) {}
}

/// Is `addr`, masked and already decoded as [`Region::Io`], one of the two bytes
/// of the hardware version register at [`IO_VERSION`]?
fn is_version_register(addr: u32) -> bool {
    (addr & EVEN_LANE) == IO_VERSION
}

/// The one word rule of [`Region::Io`]: a register answers a 16 bit access to its
/// pair by repeating its byte into both halves. See [`Bus::read_io_word`] for the
/// sources and for the alternative that was not taken.
fn io_word(byte: u8) -> u16 {
    u16::from_be_bytes([byte, byte])
}

/// `int_ack` is deliberately left at the `m68k::Bus` default; the module docs say
/// why.
impl m68k::Bus for Bus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.read_rom_byte(addr),
            Region::Ram => self.read_ram_byte(addr),
            Region::Vdp => self.read_vdp_byte(addr),
            Region::Z80 => self.read_z80_byte(addr),
            Region::Io => self.read_io_byte(addr),
            Region::Z80Ctrl => self.read_z80_ctrl_byte(addr),
            Region::CartExtra => self.read_cart_extra_byte(addr),
            Region::Tmss => self.read_tmss_byte(addr),
            Region::Unmapped => self.read_unmapped_byte(addr),
        }
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.read_rom_word(addr),
            Region::Ram => self.read_ram_word(addr),
            Region::Vdp => self.read_vdp_word(addr),
            Region::Z80 => self.read_z80_word(addr),
            Region::Io => self.read_io_word(addr),
            Region::Z80Ctrl => self.read_z80_ctrl_word(addr),
            Region::CartExtra => self.read_cart_extra_word(addr),
            Region::Tmss => self.read_tmss_word(addr),
            Region::Unmapped => self.read_unmapped_word(addr),
        }
    }

    fn write_byte(&mut self, addr: u32, value: u8) {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.write_rom_byte(addr, value),
            Region::Ram => self.write_ram_byte(addr, value),
            Region::Vdp => self.write_vdp_byte(addr, value),
            Region::Z80 => self.write_z80_byte(addr, value),
            Region::Io => self.write_io_byte(addr, value),
            Region::Z80Ctrl => self.write_z80_ctrl_byte(addr, value),
            Region::CartExtra => self.write_cart_extra_byte(addr, value),
            Region::Tmss => self.write_tmss_byte(addr, value),
            Region::Unmapped => self.write_unmapped_byte(addr, value),
        }
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.write_rom_word(addr, value),
            Region::Ram => self.write_ram_word(addr, value),
            Region::Vdp => self.write_vdp_word(addr, value),
            Region::Z80 => self.write_z80_word(addr, value),
            Region::Io => self.write_io_word(addr, value),
            Region::Z80Ctrl => self.write_z80_ctrl_word(addr, value),
            Region::CartExtra => self.write_cart_extra_word(addr, value),
            Region::Tmss => self.write_tmss_word(addr, value),
            Region::Unmapped => self.write_unmapped_word(addr, value),
        }
    }

    /// The `RESET` instruction restarts the console, so the I/O chip's registers
    /// go back to their power-on values. Work RAM does not, and cannot: this is the
    /// console's `RESET`, not a reinitialisation of everything the emulator holds.
    fn reset_devices(&mut self) {
        self.io.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use m68k::Bus as _;

    /// An image of `len` bytes with no word of it reading as a plausible
    /// default, so a test cannot pass by accident.
    fn rom_image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// What the version register at 0xA10001 reports for these tests: an export
    /// NTSC machine without TMSS ([`crate::Region::Usa`]). Deliberately not
    /// [`OPEN_BUS`], so a stub read cannot pass on the version register's value.
    const TEST_VERSION: u8 = 0xA0;

    #[test]
    fn reads_rom_bytes_and_words() {
        let mut bus = Bus::new(vec![0xAB, 0xCD, 0x00, 0xFF], TEST_VERSION);
        assert_eq!(bus.read_byte(ROM_START), 0xAB);
        assert_eq!(bus.read_byte(ROM_START + 1), 0xCD);
        assert_eq!(bus.read_word(ROM_START), 0xABCD);
        assert_eq!(bus.read_word(ROM_START + 2), 0x00FF);

        // Anywhere in the region, and at its very top, not just at 0x000000.
        let mut rom = rom_image(0x400000);
        rom[0x1234] = 0x12;
        rom[0x1235] = 0x34;
        rom[ROM_END as usize - 1] = 0xFE;
        rom[ROM_END as usize] = 0xFF;
        let mut bus = Bus::new(rom, TEST_VERSION);
        assert_eq!(bus.read_word(0x001234), 0x1234);
        assert_eq!(bus.read_byte(ROM_END), 0xFF);
        assert_eq!(bus.read_word(ROM_END - 1), 0xFEFF);
    }

    #[test]
    fn rom_is_read_only() {
        let mut bus = Bus::new(vec![0xAB, 0xCD, 0xEF, 0x01], TEST_VERSION);
        bus.write_byte(ROM_START, 0x00);
        bus.write_word(ROM_START + 2, 0x0000);
        assert_eq!(bus.read_byte(ROM_START), 0xAB);
        assert_eq!(bus.read_word(ROM_START + 2), 0xEF01);
        assert_eq!(bus.rom, vec![0xAB, 0xCD, 0xEF, 0x01]);
    }

    #[test]
    fn ram_read_write_byte_and_word() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        bus.write_byte(RAM_START, 0x5A);
        assert_eq!(bus.read_byte(RAM_START), 0x5A);
        assert_eq!(bus.ram[0], 0x5A);

        bus.write_word(RAM_START + 2, 0x1234);
        assert_eq!(bus.read_word(RAM_START + 2), 0x1234);
        assert_eq!(bus.read_byte(RAM_START + 2), 0x12);
        assert_eq!(bus.read_byte(RAM_START + 3), 0x34);

        // 0xFFFFFF is odd, so 0xFFFFFE is the last legal word of the window.
        bus.write_word(RAM_END - 1, 0xBEEF);
        assert_eq!(bus.read_word(RAM_END - 1), 0xBEEF);
        assert_eq!(&bus.ram[0xFFFE..], &[0xBE, 0xEF]);
    }

    #[test]
    fn ram_is_mirrored() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        bus.write_byte(0xFF0010, 0x77);
        assert_eq!(bus.read_byte(0xE00010), 0x77);
        assert_eq!(bus.read_byte(0xE80010), 0x77);
        assert_eq!(bus.read_byte(0xFF0010), 0x77);

        bus.write_word(0xE10020, 0xCAFE);
        assert_eq!(bus.read_word(0xF90020), 0xCAFE);
        assert_eq!(bus.read_byte(0xE90021), 0xFE);
    }

    #[test]
    fn addresses_masked_to_24_bits() {
        // The CPU masks too; this is the bus masking for its direct callers.
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.write_byte(0x01FF_0010, 0x5C);
        assert_eq!(bus.read_byte(0x01FF_0010), 0x5C);
        assert_eq!(bus.read_byte(0x00FF_0010), 0x5C);

        // Same on the ROM side: masked, 0x01100010 is cartridge, not nothing.
        let mut rom = rom_image(0x200000);
        rom[0x100010] = 0x7E;
        rom[0x100011] = 0x7D;
        let mut bus = Bus::new(rom, TEST_VERSION);
        assert_eq!(bus.read_word(0x0110_0010), 0x7E7D);
        bus.write_byte(0x0110_0010, 0x00);
        assert_eq!(bus.read_byte(0x0110_0010), 0x7E);
    }

    #[test]
    fn reads_past_rom_end_return_ff() {
        // An odd number of bytes on purpose: it is the only way a word access
        // straddles the end of the image.
        let mut bus = Bus::new(vec![0x12, 0x34, 0x56], TEST_VERSION);
        assert_eq!(bus.read_byte(0x000001), 0x34);
        assert_eq!(bus.read_byte(0x000002), 0x56);
        assert_eq!(bus.read_byte(0x000003), OPEN_BUS);
        assert_eq!(bus.read_byte(ROM_END), OPEN_BUS);
        // A word straddling the end keeps the byte that is in the image.
        assert_eq!(bus.read_word(0x000002), 0x56FF);
        assert_eq!(bus.read_word(0x000004), OPEN_BUS_WORD);
    }

    #[test]
    fn unmapped_reads_return_open_bus_placeholder() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        for addr in [
            0x400000, 0x7FFFFE, 0x800000, 0x9FFFFE, 0xA12000, 0xA15000, 0xBFFFFE,
        ] {
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "byte at {addr:#08x}");
            assert_eq!(bus.read_word(addr), OPEN_BUS_WORD, "word at {addr:#08x}");
        }
    }

    #[test]
    fn unmapped_writes_are_dropped() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        for addr in [0x400000, 0x800000, 0xA12000, 0xBFFFFE] {
            bus.write_byte(addr, 0x11);
            bus.write_word(addr, 0x2222);
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "byte at {addr:#08x}");
            assert_eq!(bus.read_word(addr), OPEN_BUS_WORD, "word at {addr:#08x}");
        }
    }

    #[test]
    fn stub_regions_read_open_bus_until_their_ticket() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        // One address per stub region, including every register the map names.
        // The I/O entry is past the version register, which #64 implements and
        // the tests below cover.
        for addr in [
            0xA00000, 0xA10020, 0xA11100, 0xA11200, 0xA130F1, 0xA14000, 0xA14101, 0xC00000,
        ] {
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "byte at {addr:#08x}");
            assert_eq!(
                bus.read_word(addr + 2),
                OPEN_BUS_WORD,
                "word at {addr:#08x}"
            );
            bus.write_byte(addr, 0x11);
            bus.write_word(addr + 2, 0x2222);
            assert_eq!(
                bus.read_byte(addr),
                OPEN_BUS,
                "byte at {addr:#08x} after write"
            );
        }
        assert!(bus.ram.iter().all(|&b| b == 0), "stub writes reached RAM");
    }

    /// The version register answers a byte at 0xA10001, which is how a game
    /// finds out what console it is on (plutiedev "Using the Sub-CPU in mode 1",
    /// `HwVersion: equ $A10001`), and the value is exactly what
    /// [`crate::Region::version_register`] produced for the resolved region.
    #[test]
    fn version_register_reads_at_a10001() {
        // Japan NTSC, export NTSC and export PAL, each with and without TMSS.
        for version in [0x20u8, 0xA0, 0xE0, 0x21, 0xA1, 0xE1] {
            let mut bus = Bus::new(Vec::new(), version);
            assert_eq!(bus.read_byte(0xA10001), version, "byte at 0xA10001");
        }
    }

    /// A word read at 0xA10000 reports the version register in *both* bytes.
    ///
    /// The verification the ticket asked for. A0 does not pick between the two
    /// halves of the register pair, and BlastEm models a word access to the I/O
    /// area as the register read once and mirrored into both halves
    /// (`io_read_w`: `value = value | (value << 8)`), which is what this bus
    /// does. See [`Bus::read_io_word`] for the sources and for how MAME differs
    /// on the half of the word nothing is wired to.
    #[test]
    fn version_register_word_read_reports_both_bytes() {
        let mut bus = Bus::new(Vec::new(), 0xA0);
        assert_eq!(bus.read_word(0xA10000), 0xA0A0);
        // A0 is ignored, so the even byte of the pair answers too.
        assert_eq!(bus.read_byte(0xA10000), 0xA0);
    }

    /// The version register is read-only: the writes a game makes to the I/O
    /// area are to the port registers of M3-05 (#65), never to this one.
    #[test]
    fn version_register_is_read_only() {
        let mut bus = Bus::new(Vec::new(), 0xA0);
        bus.write_byte(0xA10000, 0x00);
        bus.write_byte(0xA10001, 0x00);
        bus.write_word(0xA10000, 0x0000);
        assert_eq!(bus.read_byte(0xA10001), 0xA0);
        assert_eq!(bus.read_word(0xA10000), 0xA0A0);
    }

    /// Everything [`Region::Io`] covers past the register block at
    /// 0xA10000-0xA1001F is still nothing's: it must read open bus, and neither
    /// the version register nor the port registers of #65 may answer for it.
    ///
    /// The addresses of #64's version of this test - 0xA10003, 0xA10009, 0xA1001F
    /// and the rest of the register block - are answered now, and
    /// [`Bus::controller_ports_are_readable_through_the_bus`] and `io::tests` are
    /// what cover them. The point being kept is the one the loop makes: an address
    /// of the I/O area that no register lives at reads open bus rather than
    /// whatever its neighbour holds.
    #[test]
    fn io_past_the_register_block_is_still_a_stub() {
        let mut bus = Bus::new(Vec::new(), 0x20);
        for addr in [0xA10020, 0xA10021, 0xA10023, IO_END] {
            let word_addr = addr & EVEN_LANE;
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "{addr:#08x} byte");
            assert_eq!(bus.read_word(word_addr), OPEN_BUS_WORD, "{word_addr:#08x}");
            bus.write_byte(addr, 0x11);
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "{addr:#08x} after write");
        }
    }

    /// A byte read at `0xA10003` is player 1's data port: the point of wiring
    /// [`Io`] into the `Region::Io` dispatch is that a game's
    /// `move.b (IoData1),d0` reaches the pad, and that the two pad ports, the
    /// expansion port and the version register beside them do not answer for each
    /// other (plutiedev "I/O ports", "Basic usage", for the addresses).
    #[test]
    fn controller_ports_are_readable_through_the_bus() {
        use crate::io::Buttons;

        let mut bus = Bus::new(Vec::new(), 0xA0);
        bus.io.set_buttons(0, Buttons::UP);
        bus.write_byte(0xA10009, 0x40); // ctrl port 1: TH an output, every other pin an input
        bus.write_byte(0xA10003, 0x40); // TH high, so the pad reports the dpad and B/C
        assert_eq!(bus.read_byte(0xA10003), 0x7E, "Up held on port 1");
        assert_eq!(bus.read_byte(0xA10009), 0x40, "ctrl port 1 reads back");

        // The other half of the TH protocol: drop TH and read A and Start.
        bus.write_byte(0xA10003, 0x00);
        assert_eq!(bus.read_byte(0xA10003), 0x32, "TH low, only Up held");

        // Word access answers data port 1 with the byte in both halves, which is
        // the same rule the version register uses. See `Bus::read_io_word`.
        bus.write_byte(0xA10003, 0x40);
        assert_eq!(
            bus.read_word(0xA10002),
            0x7E7E,
            "word at the port 1 data pair"
        );

        // Port 2 is its own port with nothing on it, and the expansion port is
        // idle too; neither saw port 1's setup or its pad.
        bus.write_byte(0xA1000B, 0x40);
        bus.write_byte(0xA10005, 0x40);
        assert_eq!(bus.read_byte(0xA10005), 0x7F, "port 2, no buttons");
        assert_eq!(
            bus.read_byte(0xA10007),
            0x7F,
            "expansion port, nothing attached"
        );

        // The serial registers store through the bus, and the version register
        // took none of this.
        bus.write_byte(0xA1000F, 0x5A);
        assert_eq!(bus.read_byte(0xA1000F), 0x5A);
        assert_eq!(
            bus.read_byte(0xA10001),
            0xA0,
            "version register still answers"
        );
    }

    /// `RESET` puts the I/O chip back to its power-on values and leaves work RAM
    /// alone: `m68k::Bus::reset_devices` is "the 68000 `RESET` instruction, which
    /// restarts the whole console, not just the CPU", and the console's RAM
    /// survives that.
    #[test]
    fn reset_devices_resets_the_io_chip_and_not_ram() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.write_byte(0xA10009, 0x40);
        bus.write_byte(0xA10003, 0x00);
        bus.write_byte(0xA1000F, 0x5A);
        bus.write_byte(RAM_START, 0x12);

        bus.reset_devices();

        assert_eq!(
            bus.read_byte(0xA10009),
            0x00,
            "ctrl port 1 back to all-input"
        );
        assert_eq!(bus.read_byte(0xA10003), 0x7F, "data port 1 back to TH high");
        assert_eq!(bus.read_byte(0xA1000F), 0x00, "serial register back to 0");
        assert_eq!(bus.read_byte(RAM_START), 0x12, "work RAM survives RESET");
    }

    #[test]
    fn int_ack_keeps_the_m68k_default() {
        // A Genesis always autovectors, so the bus never gives the CPU a vector.
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert_eq!(bus.int_ack(2), None);
    }
}
