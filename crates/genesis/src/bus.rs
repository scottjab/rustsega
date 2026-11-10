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
//! [`m68k::Bus::int_ack`] and [`m68k::Bus::reset_devices`] are deliberately left
//! at the trait defaults: returning `None` *is* what a Genesis does, and the
//! devices that need a reset arrive with M3-08 (#68) and M3-09 (#69).

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
    /// the whole decoded area, of which the 0xA10000-0xA1001F block #63 lists is
    /// the part that answers today.
    ///
    /// TODO(M3-04 #64): the hardware version register at 0xA10001.
    /// TODO(M3-05 #65): controller port data and direction pins, the byte
    /// registers at 0xA10001-0xA1000D (plutiedev "I/O ports").
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
}

impl Bus {
    /// A bus with `rom` loaded. Work RAM starts zeroed; a real console powers
    /// up with undefined contents, which nothing depends on yet.
    pub fn new(rom: Vec<u8>) -> Self {
        Self {
            rom,
            ram: Box::new([0; 0x10000]),
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

    /// I/O byte read. Placeholder for [`Region::Io`].
    fn read_io_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// I/O word read. Placeholder for [`Region::Io`].
    fn read_io_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// I/O byte write. Placeholder for [`Region::Io`].
    fn write_io_byte(&mut self, _addr: u32, _value: u8) {}

    /// I/O word write. Placeholder for [`Region::Io`].
    fn write_io_word(&mut self, _addr: u32, _value: u16) {}

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

// `int_ack` and `reset_devices` are deliberately left at the `m68k::Bus`
// defaults; the module docs say why.
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

    #[test]
    fn reads_rom_bytes_and_words() {
        let mut bus = Bus::new(vec![0xAB, 0xCD, 0x00, 0xFF]);
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
        let mut bus = Bus::new(rom);
        assert_eq!(bus.read_word(0x001234), 0x1234);
        assert_eq!(bus.read_byte(ROM_END), 0xFF);
        assert_eq!(bus.read_word(ROM_END - 1), 0xFEFF);
    }

    #[test]
    fn rom_is_read_only() {
        let mut bus = Bus::new(vec![0xAB, 0xCD, 0xEF, 0x01]);
        bus.write_byte(ROM_START, 0x00);
        bus.write_word(ROM_START + 2, 0x0000);
        assert_eq!(bus.read_byte(ROM_START), 0xAB);
        assert_eq!(bus.read_word(ROM_START + 2), 0xEF01);
        assert_eq!(bus.rom, vec![0xAB, 0xCD, 0xEF, 0x01]);
    }

    #[test]
    fn ram_read_write_byte_and_word() {
        let mut bus = Bus::new(Vec::new());

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
        let mut bus = Bus::new(Vec::new());

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
        let mut bus = Bus::new(Vec::new());
        bus.write_byte(0x01FF_0010, 0x5C);
        assert_eq!(bus.read_byte(0x01FF_0010), 0x5C);
        assert_eq!(bus.read_byte(0x00FF_0010), 0x5C);

        // Same on the ROM side: masked, 0x01100010 is cartridge, not nothing.
        let mut rom = rom_image(0x200000);
        rom[0x100010] = 0x7E;
        rom[0x100011] = 0x7D;
        let mut bus = Bus::new(rom);
        assert_eq!(bus.read_word(0x0110_0010), 0x7E7D);
        bus.write_byte(0x0110_0010, 0x00);
        assert_eq!(bus.read_byte(0x0110_0010), 0x7E);
    }

    #[test]
    fn reads_past_rom_end_return_ff() {
        // An odd number of bytes on purpose: it is the only way a word access
        // straddles the end of the image.
        let mut bus = Bus::new(vec![0x12, 0x34, 0x56]);
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
        let mut bus = Bus::new(Vec::new());
        for addr in [
            0x400000, 0x7FFFFE, 0x800000, 0x9FFFFE, 0xA12000, 0xA15000, 0xBFFFFE,
        ] {
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "byte at {addr:#08x}");
            assert_eq!(bus.read_word(addr), OPEN_BUS_WORD, "word at {addr:#08x}");
        }
    }

    #[test]
    fn unmapped_writes_are_dropped() {
        let mut bus = Bus::new(Vec::new());
        for addr in [0x400000, 0x800000, 0xA12000, 0xBFFFFE] {
            bus.write_byte(addr, 0x11);
            bus.write_word(addr, 0x2222);
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "byte at {addr:#08x}");
            assert_eq!(bus.read_word(addr), OPEN_BUS_WORD, "word at {addr:#08x}");
        }
    }

    #[test]
    fn stub_regions_read_open_bus_until_their_ticket() {
        let mut bus = Bus::new(Vec::new());
        // One address per stub region, including every register the map names.
        for addr in [
            0xA00000, 0xA10000, 0xA11100, 0xA11200, 0xA130F1, 0xA14000, 0xA14101, 0xC00000,
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

    #[test]
    fn int_ack_and_device_reset_keep_the_m68k_defaults() {
        // A Genesis always autovectors, and there are no devices to reset yet.
        let mut bus = Bus::new(Vec::new());
        assert_eq!(bus.int_ack(2), None);
        bus.reset_devices();
    }
}
