//! The Z80 side of the memory map: 8 KiB of sound RAM, the YM2612, the PSG behind
//! the VDP ports, the bank shift register, and a 32 KiB window into the 68k's
//! address space.
//!
//! The ranges are the Z80 map every Genesis emulator implements the same way.
//! MAME's `megadriv_z80_map` (`src/mame/sega/megadriv.cpp`), BlastEm's Z80 memory
//! map (`src/genesis.c`) and Genesis Plus GX's `z80_memory_r`/`z80_memory_w`
//! (`core/memz80.c`) agree on all of it byte for byte, and each of the decoders
//! below cites the one that settles its mask.
//!
//! ## Who owns the state
//!
//! [`Z80View`] is a borrow of [`Bus`], and every byte of hardware state it needs -
//! sound RAM, the bank register, the audio logs - is on `Bus`, not in the view. A
//! Z80 runs with the 68k on the machine at the same time, so a view is a
//! temporary, made each time the sound CPU is stepped: a bank register living in
//! one would reset to zero between steps and silently point the window at the
//! wrong 32 KiB. This is also why there is no `Rc<RefCell<..>>` here: `Bus` owns
//! everything and the view reaches it through `&mut`.
//!
//! ## Reaching the 68k
//!
//! Both the window at 0x8000-0xFFFF and the VDP ports at 0x7F00-0x7FFF go to the
//! 68k through [`m68k::Bus`], never to [`Bus`]'s fields. That is what makes the
//! window inherit work RAM's mirroring, the 24 bit mask, and the cartridge slot's
//! `rom` being read-only - a Z80 write to ROM vanishes because the 68k says it
//! does, and it is what lets a region that does not exist yet answer a Z80 access
//! exactly as it answers a 68k one.
//!
//! ## Audio is not here
//!
//! `crates/ym2612` and `crates/sn76489` are stubs, so YM2612 and PSG accesses go
//! into logs on [`Z80Side`] for now; the TODOs on those fields name the tickets
//! that replace them. Reads of the YM2612 report a status byte that is not busy.

use crate::bus::{Bus, OPEN_BUS};
use m68k::Bus as _;
use z80::Z80Bus;

/// Sound RAM: 8 KiB, at 0x0000-0x1FFF and mirrored at 0x2000-0x3FFF.
pub const ZRAM_SIZE: usize = 0x2000;
/// These bits of a Z80 address name a byte of sound RAM. Used through
/// [`ram_index`], which is what the 68k's `0xA00000` window uses too - see
/// [`crate::bus`].
const ZRAM_MASK: usize = ZRAM_SIZE - 1;

// Region boundaries of the Z80 map. See `Region`.
const RAM_END: u16 = 0x3FFF;
const YM_START: u16 = 0x4000;
const YM_END: u16 = 0x5FFF;
const BANK_START: u16 = 0x6000;
const BANK_END: u16 = 0x60FF;
const UNUSED_START: u16 = 0x6100;
const UNUSED_END: u16 = 0x7EFF;
const VDP_START: u16 = 0x7F00;
const VDP_END: u16 = 0x7FFF;
const WINDOW_START: u16 = 0x8000;

/// The YM2612 decodes four ports, so these - and no more - address bits reach it.
const YM_PORT_MASK: u16 = 0x03;
/// A YM2612 status byte with bit 7 clear. Bit 7 is the busy/IRQ flag the driver
/// waits on; the chip is never busy while it is a log, and `ym2612::Ym2612`
/// (M5-03 #107) will answer this itself.
///
/// Crate-visible because the 68k reads the same chip through its `0xA04000` window
/// (plutiedev "YM2612 register reference": the flag-clearing read is at $A04000).
pub(crate) const YM_STATUS_IDLE: u8 = 0x00;

/// The bank register selects 68k addresses this far up: it holds bits 15-23, and
/// a 9-bit register times 32 KiB is the whole 24 bit address bus.
const BANK_SHIFT: u32 = 15;
/// The bank window is 32 KiB of the 68k space.
const WINDOW_MASK: u32 = 0x7FFF;

/// The 68k's VDP port area, which is what the Z80's 0x7F00-0x7FFF window reaches.
const VDP_PORTS: u32 = 0xC0_0000;
/// These bits of a Z80 VDP window address name a port: the ports are at
/// 0xC00000-0xC0001C and the area mirrors every $40 (plutiedev "Memory map",
/// "VDP mirrors every $40 up to $DFFFFF").
///
/// Crate-visible because they are the same five bits the 68k's own addresses name
/// the same ports with, and `crate::bus`'s VDP decode (`vdp_port`, M3-09 #69) is
/// the one table of them.
pub(crate) const VDP_PORT_MASK: u32 = 0x1F;

/// A decoded region of the Z80's 16 bit address space.
///
/// Same shape as [`crate::bus`]'s private `Region`, for the same reason: each
/// region gets its own arm so that the ticket filling one in does not cut a new
/// arm out of the dispatch.
enum Region {
    /// 0x0000-0x3FFF: 8 KiB of sound RAM, mirrored.
    ///
    /// The 68k reaches the same RAM at 0xA00000-0xA0FFFF, byte accesses only
    /// (plutiedev "Using the Z80": "you must use byte accesses when touching Z80
    /// RAM, word accesses won't work"). Wiring the two together is M3-08 (#68).
    Ram,
    /// 0x4000-0x5FFF: the YM2612, four ports at `addr & 3`.
    ///
    /// See [`ym_port`] for where the `& 3` comes from.
    Ym,
    /// 0x6000-0x60FF: the bank shift register, write-only.
    ///
    /// The whole 0x100 block, not just 0x6000: MAME maps 0x6000-0x60FF to the
    /// register with the note "wackyrac uses $6001, d_titov2 uses the full
    /// range".
    ///
    /// A read is [`OPEN_BUS`]: none of the three references gives the register a
    /// read handler (MAME maps it `.w`, BlastEm's map entry has a null read
    /// handler, and Genesis Plus GX falls through to its `z80_unused_r`, which is
    /// 0xFF), so the shifted-in bit is not readable back and nothing here
    /// invents a read-back.
    Bank,
    /// 0x6100-0x7EFF: nothing, reading [`OPEN_BUS`].
    ///
    /// TODO(M3-16 #77): open bus. This area floats like every other undriven read,
    /// and Genesis Plus GX is content with 0xFF for it.
    Unused,
    /// 0x7F00-0x7FFF: the VDP ports, which the Z80 sees as the 68k does - see
    /// [`vdp_port_addr`]. The PSG is in here at 0x7F11.
    Vdp,
    /// 0x8000-0xFFFF: 32 KiB of the 68k space, at the bank register's address.
    Window,
}

impl Region {
    /// Decode a Z80 address. The boundaries are not powers of two, so this is a
    /// range match rather than a shift like the 68k side's.
    fn decode(addr: u16) -> Self {
        match addr {
            0x0000..=RAM_END => Self::Ram,
            YM_START..=YM_END => Self::Ym,
            BANK_START..=BANK_END => Self::Bank,
            UNUSED_START..=UNUSED_END => Self::Unused,
            VDP_START..=VDP_END => Self::Vdp,
            WINDOW_START..=0xFFFF => Self::Window,
        }
    }
}

/// The hardware that hangs off the Z80's own address lines, owned by [`Bus`] so
/// that it outlives every [`Z80View`] - see the module docs for why that matters.
///
/// `Debug` prints 8 KiB of sound RAM, so assert on `&bus.z80.ram[a..b]` slices.
#[derive(Debug)]
pub struct Z80Side {
    /// 8 KiB of sound RAM at 0x0000-0x1FFF, mirrored at 0x2000-0x3FFF.
    ///
    /// Zeroed, like [`Bus::ram`]: a real console starts with undefined contents
    /// and nothing depends on it yet.
    pub ram: Box<[u8; ZRAM_SIZE]>,
    /// The bank shift register: 68k address bits 15-23, nine bits wide.
    ///
    /// [`Z80Side`] holds it rather than [`Z80View`] because a view is a temporary;
    /// see the module docs. It is not cleared by [`m68k::Bus::reset_devices`]: the
    /// Z80's reset line and bus request are M3-08 (#68), which is where any reset
    /// of it belongs.
    pub bank: u16,
    /// Every YM2612 write as `(port, value)`, port being 0-3 from [`ym_port`].
    ///
    /// TODO(M5-13 #118): replaced by the `ym2612::Ym2612` the bus will own
    /// (register interface M5-03 #107), which takes exactly this
    /// `write(port, value)` (docs/ARCHITECTURE.md, "Audio chips").
    pub ym_log: Vec<(u8, u8)>,
    /// How many times this bus has driven the YM2612's reset line.
    ///
    /// The chip's `/RESET` pin is tied to the Z80's own reset line, so the 68k resets
    /// it with `0xA11200`: BlastEm's reset handler calls `ym_reset` in the branch that
    /// asserts Z80 reset, and Genesis Plus GX's `gen_zreset_w` calls `fm_reset` on
    /// both edges of the line and its `gen_reset` once more for the console's
    /// `RESET`. `crates/ym2612` is still a one-line stub, so the event is counted
    /// rather than delivered; [`Z80Side::ym_reset`] is the hook M5 replaces.
    ///
    /// Counted per edge, not per level: `crate::bus::Bus::new` starts with the line held
    /// asserted, and that is the Z80 not having been let go yet rather than a reset
    /// anyone performed. The console's `RESET` counts even when the line was already
    /// held, because Genesis Plus GX's `gen_reset` calls `fm_reset` there too.
    pub ym_resets: usize,
    /// Every PSG write: the byte that reached the PSG port, oldest first.
    ///
    /// TODO(M5-13 #118): replaced by the `sn76489::Sn76489` the bus will own
    /// (M5-01 #105), which takes exactly this `write(value)`
    /// (docs/ARCHITECTURE.md, "Audio chips").
    pub psg_log: Vec<u8>,
}

impl Z80Side {
    /// A sound side with empty RAM, an empty bank register and no audio logged.
    pub fn new() -> Self {
        Self {
            ram: Box::new([0; ZRAM_SIZE]),
            bank: 0,
            ym_log: Vec::new(),
            ym_resets: 0,
            psg_log: Vec::new(),
        }
    }

    /// Assert the YM2612's reset line, which the hardware ties to the Z80's.
    ///
    /// The chip is a stub, so all this does is record that it happened. See
    /// [`Z80Side::ym_resets`] for the hardware, and `crate::bus` for the two callers:
    /// the 68k's write to `0xA11200` and [`m68k::Bus::reset_devices`].
    ///
    /// TODO(M5-13 #118): calls `ym2612::Ym2612::reset` on the chip the bus will own.
    pub fn ym_reset(&mut self) {
        self.ym_resets += 1;
    }

    /// Shift one write of the bank register in: bit 0 of `value` becomes the new
    /// top bit, and everything else moves down one (BlastEm's
    /// `z80_write_bank_reg`, Genesis Plus GX's `gen_zbank_w(data & 1)`).
    ///
    /// Needs no mask: `bank` only ever holds 9 bits, so the shift cannot drop
    /// anything but the bit the hardware drops, and cannot widen the register.
    ///
    /// Crate-visible because the 68k drives the same register from `0xA06000`: that
    /// window and the Z80's `0x6000` are one register, so they share one shift
    /// (BlastEm's `io_write` and Genesis Plus GX's `z80_write_byte` both take 68k
    /// writes to it, and MAME maps `0xA06000-0xA06001` to a bank write handler).
    pub(crate) fn shift_in(&mut self, value: u8) {
        self.bank = (self.bank >> 1) | (u16::from(value & 1) << 8);
    }

    /// The 68k address a bank window access to `addr` names: the register selects
    /// the top 9 bits of the 24 bit space, the offset the low 15 (BlastEm's
    /// `z80_read_bank`: `address = gen->z80_bank_reg << 15 | location`).
    fn window_addr(&self, addr: u16) -> u32 {
        (u32::from(self.bank) << BANK_SHIFT) | (u32::from(addr) & WINDOW_MASK)
    }
}

impl Default for Z80Side {
    fn default() -> Self {
        Self::new()
    }
}

/// The Z80's view of the machine, implementing [`z80::Z80Bus`].
///
/// Make one each time the sound CPU runs - `Z80View::new(&mut bus)` before
/// `z80.step(&mut view)` - and throw it away after. It holds no hardware state of
/// its own; the module docs say why it must not.
pub struct Z80View<'a> {
    sys: &'a mut Bus,
}

impl<'a> Z80View<'a> {
    /// A view of `bus` for the sound CPU to use.
    pub fn new(bus: &'a mut Bus) -> Self {
        Self { sys: bus }
    }
}

/// `io_read`/`io_write` are the whole story on the Genesis: the Z80's port space
/// is not connected to anything, so a read floats and a write goes nowhere
/// (MAME's `megadriv_z80_io_map` maps all 256 ports to nop handlers). The z80
/// crate's own suites and its CP/M harness rely on reads answering [`OPEN_BUS`].
impl Z80Bus for Z80View<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        match Region::decode(addr) {
            Region::Ram => self.sys.z80.ram[ram_index(addr)],
            // A read of the YM2612 reads its status, whatever the port was: the
            // chip only has the one readable register (plutiedev "YM2612 register
            // reference": the flag-clearing read is at $A04000).
            Region::Ym => YM_STATUS_IDLE,
            Region::Bank => OPEN_BUS,
            Region::Unused => OPEN_BUS,
            // The PSG is write-only, so a Z80 read of any VDP port is the 68k's
            // VDP answer - open bus until M3-09 (#69) puts a VDP behind it.
            Region::Vdp => self.sys.read_byte(vdp_port_addr(addr)),
            Region::Window => {
                let addr = self.sys.z80.window_addr(addr);
                self.sys.read_byte(addr)
            }
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match Region::decode(addr) {
            Region::Ram => self.sys.z80.ram[ram_index(addr)] = value,
            Region::Ym => self.sys.z80.ym_log.push((ym_port(addr), value)),
            Region::Bank => self.sys.z80.shift_in(value),
            Region::Unused => {}
            Region::Vdp => {
                if is_psg_port(addr) {
                    self.sys.z80.psg_log.push(value);
                } else {
                    self.sys.write_byte(vdp_port_addr(addr), value);
                }
            }
            Region::Window => {
                let addr = self.sys.z80.window_addr(addr);
                self.sys.write_byte(addr, value);
            }
        }
    }

    fn io_read(&mut self, _port: u16) -> u8 {
        OPEN_BUS
    }

    fn io_write(&mut self, _port: u16, _value: u8) {}
}

/// The index of a Z80 address in [`Z80Side::ram`]: the 8 KiB is mirrored, so only
/// these bits name a byte, and 0x2000-0x3FFF is the same RAM as 0x0000-0x1FFF.
///
/// Crate-visible because the 68k reaches the same array at `0xA00000` and must wrap
/// its address exactly this way, not a second time (M3-08 #68): the Z80's low bus
/// address is what survives the window, and the Z80 map already says which bits of it
/// name a byte.
pub(crate) fn ram_index(addr: u16) -> usize {
    usize::from(addr) & ZRAM_MASK
}

/// The YM2612 port a Z80 address selects: 0 = part I address, 1 = part I data,
/// 2 = part II address, 3 = part II data.
///
/// The mask is `& 3`, which is the trap in this window - `& 1` would make 0x4002
/// a second copy of the part I address port and the second address/data pair would
/// not exist. Both of the YM2612's address lines reach the Z80 bus, so four ports
/// decode and the group repeats every 4 bytes across the whole 8 KiB:
///
/// - plutiedev "YM2612 register reference": "YM2612 has two sets of registers
///   split in banks: bank 0 is accessed at $A04000/1 and bank 1 is accessed at
///   $A04002/3" - 0xA00000 is Z80 address 0,
/// - MAME's `megadriv_z80_map`: `map(0x4000, 0x4003).mirror(0x1ffc)`,
/// - BlastEm's Z80 map: `0x4000..0x6000` with mask `0x0003`,
/// - Genesis Plus GX's `z80_memory_w`: `fm_write(Z80.cycles, address & 3, data)`.
///
/// Crate-visible because the 68k's `0xA04000` window reaches the same four ports and
/// must not grow a second decode of them (M3-08 #68).
pub(crate) fn ym_port(addr: u16) -> u8 {
    (addr & YM_PORT_MASK) as u8
}

/// The 68k address of the VDP port a Z80 window access names: the Z80 sees the
/// same ports the 68k does at `0xC00000 + (addr & 0x1F)` (plutiedev "Memory map"
/// for the port list and its $40 mirror; Genesis Plus GX routes a Z80 access to
/// 0x7F00-0x7FFF into the 68k's own 0xC0 area handler).
///
/// 0x7F00-0x7FFF is one window, not a VDP window with a PSG window laid over it:
/// the PSG is 0xC00011 and 0x7F11 is how it looks from here. See [`is_psg_port`].
fn vdp_port_addr(addr: u16) -> u32 {
    VDP_PORTS | (u32::from(addr) & VDP_PORT_MASK)
}

/// Is this Z80 address the PSG port - 0x7F11, and the 0x7F13/0x7F15/0x7F17 that
/// mirror it?
///
/// The PSG hangs off the VDP's port area and is reached on the odd lane of the
/// pair at 0xC00010/0xC00014 (plutiedev "Memory map": "PSG is normally accessed by
/// byte, so you'll generally see $C00011 instead"; Genesis Plus GX's
/// `vdp_write_byte` takes `case 0x10: case 0x14:` and then writes only
/// `if (address & 1)`). Writes on the even lane are not the PSG, and MAME's
/// `megadriv_z80_vdp_write` handles exactly 0x11, 0x13, 0x15 and 0x17 this way too.
///
/// What the rest of the window - the data, control, HV counter and debug ports -
/// decodes to from here is M3-09's (#69) to settle; it goes to [`vdp_port_addr`]
/// and answers whatever the 68k's VDP region answers, which today is
/// [`OPEN_BUS`] on reads and nothing on writes.
///
/// Crate-visible for the same reason as [`ym_port`]: the 68k's `0xA07F11` is this
/// port, not another one, so this is the one rule about which lanes of the window are
/// the PSG (M3-08 #68).
pub(crate) fn is_psg_port(addr: u16) -> bool {
    matches!(u32::from(addr) & VDP_PORT_MASK, 0x11 | 0x13 | 0x15 | 0x17)
}

#[cfg(test)]
mod tests {
    use crate::bus::Bus;
    use crate::z80_bus::{Z80View, ZRAM_SIZE};
    use m68k::Bus as _;
    use z80::Z80Bus as _;

    /// What the version register at 0xA10001 reports for these tests: an export
    /// NTSC machine without TMSS ([`crate::Region::Usa`]). Never read through the
    /// Z80 address space, so no Z80 stub can pass on its value.
    const TEST_VERSION: u8 = 0xA0;

    /// A ROM image big enough for two bank windows, with no byte of it reading as
    /// a plausible default.
    fn rom_image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// One byte read through a fresh view. Every access in these tests builds a
    /// view, so a view that kept hardware state in itself could not pass them.
    fn read(bus: &mut Bus, addr: u16) -> u8 {
        Z80View::new(bus).read(addr)
    }

    /// One byte written through a fresh view. See [`read`].
    fn write(bus: &mut Bus, addr: u16, value: u8) {
        Z80View::new(bus).write(addr, value);
    }

    /// Load a bank value into the shift register: nine writes, each supplying the
    /// next bit at the top, so the bits go in least significant first.
    fn set_bank(bus: &mut Bus, bank: u16) {
        for bit in 0..9 {
            write(bus, 0x6000, ((bank >> bit) & 1) as u8);
        }
    }

    #[test]
    fn z80_ram_read_write() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        write(&mut bus, 0x0100, 0x5A);
        assert_eq!(read(&mut bus, 0x0100), 0x5A);
        assert_eq!(bus.z80.ram[0x0100], 0x5A);

        // The whole 8 KiB is RAM, not just the low corner.
        write(&mut bus, 0x1FFF, 0x3C);
        assert_eq!(read(&mut bus, 0x1FFF), 0x3C);
        assert_eq!(bus.z80.ram[0x1FFF], 0x3C);
        assert_eq!(bus.z80.ram.len(), ZRAM_SIZE);
    }

    #[test]
    fn z80_ram_mirror_at_2000() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        write(&mut bus, 0x0010, 0x77);
        assert_eq!(read(&mut bus, 0x2010), 0x77);

        // And the other way round: one byte, two addresses, one array of 8 KiB.
        write(&mut bus, 0x3FFF, 0x1E);
        assert_eq!(read(&mut bus, 0x1FFF), 0x1E);
        assert_eq!(bus.z80.ram[0x1FFF], 0x1E);
        assert_eq!(bus.z80.ram.len(), ZRAM_SIZE);
    }

    #[test]
    fn bank_register_shift_in() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // Nine writes of $01 hold the register at 0x1FF: bit 0 of the written
        // value is the only bit that moves, and it enters at the top each time.
        for _ in 0..9 {
            write(&mut bus, 0x6000, 0x01);
        }
        assert_eq!(bus.z80.bank, 0x1FF);

        // A tenth write of $00 pushes a zero in and drops the low bit: the
        // register is 9 bits wide and shifts, it is not replaced.
        write(&mut bus, 0x6000, 0x00);
        assert_eq!(bus.z80.bank, 0x0FF);

        // Bits 1-7 of the written value are ignored, and any address in
        // 0x6000-0x60FF takes the write: MAME maps 0x6000-0x60FF to the register,
        // noting "wackyrac uses $6001, d_titov2 uses the full range", and BlastEm's
        // map entry is 0x6000..0x6100.
        write(&mut bus, 0x60FF, 0xFE);
        assert_eq!(bus.z80.bank, 0x07F);

        // A loaded register is the base address of the window: 0x1FE << 15 =
        // 0xFF0000, so the window sits over work RAM and 0x8010 reads what the 68k
        // left at 0xFF0010.
        set_bank(&mut bus, 0x1FE);
        assert_eq!(bus.z80.bank, 0x1FE);
        bus.write_byte(0xFF_0010, 0x39);
        assert_eq!(read(&mut bus, 0x8010), 0x39);
    }

    #[test]
    fn bank_window_reads_68k_rom() {
        // Two bank windows of ROM: the bottom of the cartridge slot, and the next
        // 32 KiB of it.
        let mut rom = rom_image(0x1_0000);
        rom[0] = 0xAB;
        rom[0x7FFF] = 0xCD;
        rom[0x8000] = 0x12;
        rom[0xFFFF] = 0x34;
        let mut bus = Bus::new(rom, TEST_VERSION);

        // The register powers up at zero, so the window starts at 0x000000.
        assert_eq!(read(&mut bus, 0x8000), 0xAB);
        assert_eq!(read(&mut bus, 0xFFFF), 0xCD);

        // Bank 0x001 -> 0x001 << 15 = 0x008000: the window slides by 32 KiB.
        set_bank(&mut bus, 0x001);
        assert_eq!(read(&mut bus, 0x8000), 0x12);
        assert_eq!(read(&mut bus, 0xFFFF), 0x34);
    }

    #[test]
    fn bank_window_reads_68k_ram() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // 0xFF0000 >> 15 = 0x1FE, the bank the ticket asks for.
        set_bank(&mut bus, 0x1FE);
        bus.write_byte(0xFF_0010, 0x9E);
        assert_eq!(read(&mut bus, 0x8010), 0x9E);

        // Through the 68k bus, so work RAM's mirroring comes for free: 0xFF0020
        // and 0xE00020 are the same byte.
        bus.write_byte(0xE0_0020, 0x2B);
        assert_eq!(read(&mut bus, 0x8020), 0x2B);

        // And a write through the window lands in the RAM the 68k sees.
        write(&mut bus, 0x8030, 0x4D);
        assert_eq!(bus.read_byte(0xFF_0030), 0x4D);
    }

    #[test]
    fn bank_window_covers_the_top_of_the_68k_space() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // A 9-bit bank tops out at 0x1FF << 15 = 0xFF8000, which with the 0x7FFF
        // offset of the window reaches the last byte of the 24 bit address bus.
        set_bank(&mut bus, 0x1FF);
        bus.write_byte(0xFF_FFFF, 0x7A);
        assert_eq!(read(&mut bus, 0xFFFF), 0x7A);
    }

    #[test]
    fn bank_window_write_to_rom_is_dropped() {
        let mut rom = rom_image(0x8000);
        rom[0] = 0xAB;
        let mut bus = Bus::new(rom, TEST_VERSION);

        write(&mut bus, 0x8000, 0x00);
        assert_eq!(bus.rom[0], 0xAB);
        assert_eq!(read(&mut bus, 0x8000), 0xAB);
    }

    #[test]
    fn bank_survives_view_recreation() {
        let mut rom = rom_image(0x1_0000);
        rom[0x8010] = 0x6C;
        let mut bus = Bus::new(rom, TEST_VERSION);

        {
            let mut view = Z80View::new(&mut bus);
            for bit in [1u8, 0, 0, 0, 0, 0, 0, 0, 0] {
                view.write(0x6000, bit);
            }
            assert_eq!(view.read(0x8010), 0x6C);
        }

        // The window still points at 0x008000 with a view built from scratch.
        assert_eq!(bus.z80.bank, 0x001);
        assert_eq!(read(&mut bus, 0x8010), 0x6C);
    }

    #[test]
    fn ym_writes_logged_with_port() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        write(&mut bus, 0x4000, 0x28);
        write(&mut bus, 0x4001, 0xB3);
        write(&mut bus, 0x4002, 0x27);
        write(&mut bus, 0x4003, 0x00);
        assert_eq!(
            bus.z80.ym_log,
            vec![(0x00, 0x28), (0x01, 0xB3), (0x02, 0x27), (0x03, 0x00)]
        );

        // The four ports repeat every 4 bytes across the whole 8 KiB window.
        write(&mut bus, 0x4004, 0x11);
        write(&mut bus, 0x5FFD, 0x22);
        assert_eq!(&bus.z80.ym_log[4..], &[(0x00, 0x11), (0x01, 0x22)]);
    }

    #[test]
    fn ym_reads_report_not_busy() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // Bit 7 of the YM2612 status byte is the busy/IRQ flag; the stub is never
        // busy, and a read is not an access worth logging.
        for port in [0x4000u16, 0x4001, 0x4002, 0x4003, 0x5FFC] {
            assert_eq!(read(&mut bus, port), 0x00);
        }
        assert!(bus.z80.ym_log.is_empty());
    }

    #[test]
    fn psg_write_at_7f11_logged() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        write(&mut bus, 0x7F11, 0x9F);
        assert_eq!(bus.z80.psg_log, vec![0x9F]);

        // The VDP ports mirror every other word, so 0x7F13/15/17 are the same PSG
        // port (plutiedev "Memory map": PSG at $C00010/$C00014, "all VDP ports are
        // mirrored every other word"; Genesis Plus GX's `vdp_write_byte` takes
        // $11/$13/$15/$17 that way too).
        write(&mut bus, 0x7F13, 0x10);
        write(&mut bus, 0x7F15, 0x20);
        write(&mut bus, 0x7F17, 0x30);
        assert_eq!(bus.z80.psg_log, vec![0x9F, 0x10, 0x20, 0x30]);

        // The even byte of the pair is not the PSG port: the PSG is reached by byte
        // access on the odd lane only.
        write(&mut bus, 0x7F10, 0x00);
        assert_eq!(bus.z80.psg_log, vec![0x9F, 0x10, 0x20, 0x30]);
    }

    #[test]
    fn vdp_window_is_not_mixed_into_the_audio_logs() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // The data and control ports of the VDP, inside the same 0x7F00 window.
        write(&mut bus, 0x7F00, 0xFF);
        write(&mut bus, 0x7F04, 0x80);
        assert!(bus.z80.psg_log.is_empty());
        assert!(bus.z80.ym_log.is_empty());

        // What the 68k's VDP ports answer is what the Z80 gets. The data port has
        // a read of the chip's own since M4-03 (#81), so it no longer answers open
        // bus: after `Bus::new` the code register is a VRAM read and VRAM is
        // zeroed, so the port answers a real VRAM word and the Z80 sees its high
        // byte - the same answer the 68k gets at 0xC00000, which is what this
        // window exists to guarantee.
        assert_eq!(read(&mut bus, 0x7F00), 0x00);
        // The control port is the chip's status register since M4-04 (#82), so
        // this lane answers the high byte of the word a fresh NTSC VDP gives:
        // 0x3608, whose 0x36 is the d15-d10 constants the VDP drives (0x3400)
        // with the FIFO-empty flag (d9) on top. The PSG lane is still the
        // open-bus placeholder.
        assert_eq!(read(&mut bus, 0x7F04), 0x36);
        // The HV counter has been the chip's own here since M4-05 (#83): the V
        // half of the word, which of a VDP that has never been given a beam
        // position is 0x00 rather than the 0xFF of the undecoded bus.
        assert_eq!(read(&mut bus, 0x7F08), 0x00);
        // The PSG is write-only, so reading its port answers the same way.
        assert_eq!(read(&mut bus, 0x7F11), 0xFF);
    }

    /// The window reaches the VDP the 68k has at 0xC00000 (M3-09 #69): both
    /// sides go through [`vdp_port_addr`], so a data port write from the sound
    /// CPU lands in the same VRAM a 68k write lands in, a control port write
    /// sets the same register, and the PSG byte keeps the one log the 68k's
    /// `0xC00011` writes to as well.
    ///
    /// Byte writes only, which is what the Z80 has: a byte to one of the VDP's
    /// 16-bit ports is the byte in both halves of the word (genvdp.txt section 7
    /// "8-bit port access"), and that duplication is [`crate::bus`]'s rule
    /// rather than one this window adds.
    #[test]
    fn z80_7f00_window_reaches_vdp() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // The set-up from the 68k, so that only the access under test comes
        // through the window: R15 = 2 and a VRAM write command for address 0.
        bus.write_word(0xC00004, 0x8F02);
        bus.write_word(0xC00004, 0x4000);
        bus.write_word(0xC00004, 0x0000);

        write(&mut bus, 0x7F00, 0x12);
        write(&mut bus, 0x7F00, 0x34);
        assert_eq!(
            &bus.vdp.vram[0..4],
            &[0x12, 0x12, 0x34, 0x34],
            "two byte writes to the data port, doubled by the 68k's own rule"
        );

        write(&mut bus, 0x7F04, 0x8F);
        assert_eq!(bus.vdp.reg(15), 0x8F, "0x7F04 is the control port");

        write(&mut bus, 0x7F11, 0x2A);
        assert_eq!(bus.z80.psg_log, vec![0x2A], "and 0x7F11 is the PSG");
    }

    #[test]
    fn unused_area_reads_ff() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        for addr in [0x6100u16, 0x7000, 0x7EFD, 0x7EFF] {
            assert_eq!(read(&mut bus, addr), 0xFF);
            write(&mut bus, addr, 0x00);
            assert_eq!(read(&mut bus, addr), 0xFF);
        }
    }

    #[test]
    fn bank_register_read_is_open_bus() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // Neither MAME's nor BlastEm's Z80 map gives 0x6000-0x60FF a read handler,
        // and Genesis Plus GX reads it as 0xFF, so there is no read-back here:
        // loading the register is not something a Z80 program can check.
        write(&mut bus, 0x6000, 0x01);
        assert_eq!(bus.z80.bank, 0x100);
        assert_eq!(read(&mut bus, 0x6000), 0xFF);
        assert_eq!(read(&mut bus, 0x60FF), 0xFF);
    }

    #[test]
    fn io_ports_read_ff() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // The Z80's I/O port space is not connected on the Genesis.
        for port in [0x0000u16, 0x003F, 0xBF00, 0xFFFF] {
            assert_eq!(Z80View::new(&mut bus).io_read(port), 0xFF);
            Z80View::new(&mut bus).io_write(port, 0x00);
        }
        assert_eq!(bus.z80.ym_log, Vec::new());
        assert!(bus.z80.psg_log.is_empty());
        assert_eq!(bus.z80.ram[0x0000], 0x00);
    }
}
