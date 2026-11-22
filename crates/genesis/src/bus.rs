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
//! ## The TMSS gate
//!
//! On a console that reports a TMSS in the version register, nothing behind
//! [`Region::Vdp`] can be reached until the 68k has written `SEGA` to
//! [`TMSS_SECURITY`]; a try before that freezes the CPU. The check is one line in
//! each [`m68k::Bus`] dispatch arm, in front of the region's own functions, so that
//! the ticket filling the VDP in cannot miss it - see [`Bus::vdp_access_denied`].
//! The PSG's other 68k-side spelling, `0xA07F11` inside [`Region::Z80`], is the
//! same port and sits behind the same gate from that region's own read/write
//! (M3-10 #70, #262); the Z80's bank-window route into the VDP area arrives
//! through the [`m68k::Bus`] dispatch, so the gate covers it without an arm of
//! its own.
//!
//! ## Reading what is not there
//!
//! An undriven read on the 68k side answers [`Bus::prefetched_word`]: the last
//! word the 68000 drove onto the bus, which in the case hardware has been
//! observed is the prefetch queue's residue. Kabuto's hardware notes (v1.5) are
//! the observation - of the VDP's debug ports, "Reading `$C00018`/`$C0001C` the
//! usual way (move.w addr/(reg), dX) returns the following opcode as the 68k
//! prefetches it before doing the actual read access in this case", with "a
//! value of `$FFFF` ... (due to bus pull-up)" behind it - and both references
//! implement it as the whole of their open-bus answer: Genesis Plus GX's
//! `m68k_read_bus_16` answers the word at `m68k.pc`, BlastEm's
//! `get_open_bus_value` answers its queue word, byte reads taking the lane the
//! address sits on ([`open_bus_lane`]). The CPU publishes the word on every
//! queue refill ([`m68k::Bus::note_prefetched_word`], #479), the pull-up shows
//! as `$FFFF` until the first fetch, and nothing else is modelled: no decay (no
//! source gives a value the float settles to after N cycles, and #508's honest
//! limit says the ticket that invents one is worse than the placeholder), and
//! no write residue (a data write leaving different lines than the queue is
//! Kabuto's "data left on the bus" too, and no source pins what it holds in
//! cart space; #194 is the case that wants it).
//!
//! Some decoded-but-unpopulated corners stall the bus instead: plutiedev
//! "Memory map" documents `$C00020`-`$C0003F`, inside the VDP area, as "hangs
//! up", and a hang is a machine state, not a return value - [`Bus::vdp_hang`]
//! sets the same freeze the TMSS lock-up does ([`Bus::locked_up`]), cleared by
//! nothing but [`m68k::Bus::reset_devices`].
//!
//! ## The cartridge slot, in order
//!
//! 0x000000-0x3FFFFF is one region with three owners: the [mapper] of a cart
//! bigger than the area, the ROM image behind it, and - for the window its header
//! declares - the battery-backed save RAM of [`crate::cart`] in front. The order
//! the slot answers them in is on [`Bus::read_rom_byte`].
//!
//! [mapper]: crate::cart::mapper
//!
//! [`m68k::Bus::int_ack`] returns `None`, which *is* what a Genesis does - it always
//! autovectors - but it is not a no-op: since M3-14 (#75) it is also the acknowledge
//! [`Bus::vdp`] is waiting for, and the only thing that clears a pending VINT or HINT
//! (genvdp.txt section 6: "Reading the control port does not clear this bit").
//! [`m68k::Bus::reset_devices`] is implemented for [`Bus::io`], the Z80's control
//! lines and - since M3-09 (#69) put one here - [`Bus::vdp`].

use serde::{Deserialize, Serialize};

use crate::audio::Audio;
use crate::cart::{Eeprom, Jcart, Mapper, Sram, Svp, eeprom, jcart, mapper, sram, svp};
use crate::config::version_is_pal;
use crate::io::Io;
use crate::z80_bus::{self, Z80Side};
use m68k::ADDR_MASK;
use vdp::Vdp;

/// What the Z80 side's undriven areas read, and what the 68k side reads before
/// anything drives the bus or a register answers with an unspecified value: the
/// pull-up state of a 16 bit data bus with pull-ups, the ones a bus settles to
/// when nothing has driven it (Kabuto, "SEGA Mega Drive / Genesis hardware
/// notes" v1.5: "a value of `$FFFF` is read (due to bus pull-up)").
///
/// On the 68k side this is the *reset state* of the float, not its answer: an
/// undriven read there answers [`Bus::prefetched_word`], the last word the CPU
/// drove onto the bus ([`Bus::open_bus_word`], #508), and this is what that
/// latch holds until the first queue refill publishes one. The Z80's own areas
/// keep the fixed value because no source describes the Z80 side's float and
/// the two references that answer it give `0xFF` (Genesis Plus GX's
/// `z80_unused_r`; MAME leaves the area unmapped), and because those lines are
/// the Z80's own bus, not the 68k's the pull-up sentence is about.
pub(crate) const OPEN_BUS: u8 = 0xFF;
/// [`OPEN_BUS`] as a word: the pull-up state, before anything drives the bus.
///
/// The 68k side answers undriven reads from [`Bus::prefetched_word`], which is
/// initialised to this; the constant remains for the Z80 side's word-width
/// answers and for `0xFFFF` register values that are a decision rather than a
/// bus state.
pub(crate) const OPEN_BUS_WORD: u16 = 0xFFFF;

// ---- Bus timing (M3-16, #77) ------------------------------------------------
//
// The waits below are master clocks rather than 68000 clocks or T-states: the
// scheduler keeps one clock (`crate::Scheduler`) and the two numbers the VDP
// hands over for its own waits (`vdp::Vdp::dma_68k_stall_mclk` and
// `vdp::Vdp::data_port_stall_mclk`) are master clocks, so a figure a source
// quotes in a chip's own clocks is converted on the way in and never back out.
//
// #308 checked these three against hardware documentation. Two are now quoted
// rather than inferred - the pair below, both from Kabuto's "SEGA Mega Drive /
// Genesis hardware notes" (v1.5, "The bus system"), which is the source the
// ticket's "commonly cited" was reaching for. The third is still unquoted, and
// says so where it stands.

/// Master clocks in one 68000 clock: the 68000 runs at master / 7
/// (docs/ARCHITECTURE.md, "Clocks and timing"), so one 68000 wait state is this
/// many master clocks.
const MCLK_PER_68K: u32 = 7;
/// Master clocks in one Z80 T-state: the Z80 runs at master / 15 (same source).
const MCLK_PER_Z80: u32 = 15;

/// Master clocks a 68000 access to the sound side at `0xA00000` costs beyond the
/// access itself: one wait state.
///
/// Z80 RAM, the YM2612 and the bank register are 8-bit devices on the Z80's own
/// bus, so a 68000 cycle that reaches one of them is stretched by the width
/// conversion - the same reason the area takes byte accesses only (plutiedev,
/// "Using the Z80": "you must use byte accesses when touching Z80 RAM, word
/// accesses won't work").
///
/// **#308 could not verify the count, and this stays one 68000 clock.** The
/// byte-only half of the claim is quoted above; the number of wait states is not
/// in anything available. Kabuto's hardware notes measure the opposite direction
/// (the Z80 asking for the 68k's bus), plutiedev "Memory map" gives the map and
/// no timing, and genvdp.txt never mentions the sound CPU at all. One 68000 clock
/// is the smallest claim that keeps the area slower than work RAM, which is what
/// the byte-only rule implies: an access to an 8-bit device behind a 16-bit bus
/// cannot be free. Treat the 7 as unquoted, and reopen
/// [#308](https://github.com/scottjab/rustsega/issues/308) with a page that
/// states a figure.
pub(crate) const Z80_AREA_WAIT_MCLK: u32 = MCLK_PER_68K;

/// Master clocks a Z80 access to the 68k bus costs the Z80: about 3.3 T-states,
/// which at [`MCLK_PER_Z80`] is 49.5 and is paid as 50.
///
/// Quoted by #308 from Kabuto, "SEGA Mega Drive / Genesis hardware notes" v1.5,
/// "The bus system": "When the Z80 accesses ROM the bus arbiter needs to pause
/// the 68k, let the Z80 finish its request, then unpause the 68k again. [...] This
/// typically means that the Z80's 68k bus accesses will be delayed by 2 to 5
/// cycles. IIRC reads and writes behave exactly the same way. The average Z80
/// delay is around 3.3 Z80 cycles". The same page's foreword says what to do with
/// it in an emulator: "just using a default value of 3.3 should already do the
/// trick here".
///
/// It is an average of a 2-to-5-cycle spread, so it is charged as one whole
/// number per access rather than as a distribution, and the fraction is rounded
/// up rather than carried (see [`Bus::pending_z80_stall`]). Footnote #1 of the
/// same page gives the per-access model behind the average - `readdelay = a + 3 +
/// b + 2`, then `Z80 delay = (readdelay*7 + 4 - subcycle)/15` - and running it
/// over the ranges the page states (a = 1..4, no refresh prolongation, subcycle
/// 0..6) returns 2 to 4 T-states and a mean of 3.11, which is the 3.3. A model
/// that sampled the spread per access instead of the mean would need to know the
/// 68k's own position in its cycle, the "subcycle"; #308 left that alone, and
/// #489 is unresolved in exactly this charging path.
pub(crate) const Z80_WINDOW_WAIT_MCLK: u32 = (33 * MCLK_PER_Z80 + 5) / 10;

/// Master clocks a Z80 access to the 68k bus costs the 68000: about 11 of its
/// clocks, the 68000 stopping for the width conversion while the sound CPU has
/// the line.
///
/// The 11 is the same page and the same sentence as the 3.3: "whereas the average
/// 68k delay is around 11 68k cycles".
///
/// **#308 closed the contradiction the ticket saw between this and
/// [`Z80_WINDOW_WAIT_MCLK`].** 3.3 T-states is 49.5 master clocks and 11 68000
/// clocks is 77, and they were read as two answers to one question. They are not:
/// they are the delay each of the two chips suffers on the same arbitration event,
/// which is why they differ and why both are charged here. Footnote #1 of the
/// source gives the relation directly - `68k delay = ((Z80 delay) * 15 +
/// subcycle + 46) / 7 - a` - and over the ranges the page states for its typical
/// case that returns 10 to 12 with a mean of 10.75, which is the 11. The 68k loses
/// more than the sound CPU because it also loses the cycle it was already running:
/// "if the 68k is just about to access the bus itself it finishes its own access
/// cycle first before releasing the bus".
///
/// **#532 measured what it multiplies**, on `Sonic & Knuckles (World)` at the
/// `0x584` anchor. Over 480 frames the sound CPU charges this 64,827 times, a
/// mean of 135 a frame with 134 frames charging none; over the decompress leg at
/// arrivals 47-51 it charges 208 a frame, and over 440-480 (where a voice sample
/// is also playing) 171. Every one of the 64,827 is a byte read of banked 68k
/// ROM, and there is not one write, so the split made at
/// [`Bus::z80_window_access`] is the split the cart gives: the 6,825,388 accesses
/// to its own RAM, the 106,984 to the YM2612 and the 1,921 PSG writes of the same
/// run charge the 68000 nothing.
///
/// **The count is the driver's, not the emulator's**, so it is not a lever. 97% of
/// it is three instructions streaming the YM2612 DAC: `LD A,(HL)` straight into
/// `LD ($4001),A` at `0x1150`, and the 4 bit version of the same pair at `0x10d5`
/// and `0x10f2`, where one ROM byte holds two samples and is read twice. Each of
/// those loops paces itself with a `DJNZ` delay, which is what sets the rate: 240
/// reads a frame is a 14.4 kHz 8 bit sample, and the sound CPU is 98.7% busy
/// running the loop on those frames (58,993 of the 59,736 T-states a frame holds),
/// so there is no reading left to find either way. On 47-51 the steal is therefore
/// 2,292 of the 128,006 clocks a frame holds, and that is the whole of the 68000
/// hold measured at the scheduler on those frames. Closing #530's gap from this
/// mechanism at the measured count would take 20 cycles an access on 47-51 and 26
/// on 440-480: two figures outside the quoted 10 to 12, and no single figure for
/// both. The count is right, and #530 does not live here.
pub(crate) const Z80_WINDOW_STEAL_MCLK: u32 = 11 * MCLK_PER_68K;

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
/// The SVP's register page, 0xA15000-0xA15FFF: the 64 KiB Genesis Plus GX
/// routes to its SVP handler (`case 0x50`, on `(address >> 8) & 0xFF`), of
/// which only the lanes [`crate::cart::svp::Svp`] decodes answer anything.
const SVP_PAGE_START: u32 = 0xA1_5000;
const SVP_PAGE_END: u32 = 0xA1_5FFF;
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
/// The Z80 bus request register, `Z80BusReq: equ $A11100` of plutiedev "Using the
/// Z80". Writing 1 asks the sound CPU for its bus; reading bit 0 says whether the
/// Z80 is still running.
const Z80_BUSREQ: u32 = 0xA11100;
/// The Z80 reset register, `Z80Reset: equ $A11200` of the same page. Writing 0
/// asserts reset - the Z80 stops and the YM2612 is reset with it - and 1 releases it,
/// which resets the Z80's `PC` to 0.
const Z80_RESET_REG: u32 = 0xA11200;
/// Address mask that names one of the two Z80 control registers: each sits at the
/// bottom of a 0x100 block and neither decodes A0-A7, so 0xA11100-0xA111FF are all
/// the bus request register and 0xA11200-0xA112FF are all the reset register.
///
/// BlastEm decodes the pair as `location & 0xFFF00` in `io_write` and Genesis Plus GX
/// as `(address >> 8) & 0xFF` in `ctrl_io_write_byte`; MAME maps only the two
/// addresses themselves (`map(0xa11100, 0xa11101)`), which the mask still hits.
const Z80_CTRL_MASK: u32 = !0xFF;
/// The save RAM control register, `SramLock: equ $A130F1` of plutiedev "Saving
/// progress with SRAM": bit 0 maps the save RAM over the cartridge area, bit 1
/// write protects it - the same two bits Genesis Plus GX's `mapper_sega_w` takes
/// from one write.
///
/// The pair, so that `0xA130F0` answers as `0xA130F1` does, because the register
/// decodes without A0 like [`IO_VERSION`] and the two Z80 control registers:
/// MAME maps both lanes of it, and Genesis Plus GX accepts the whole
/// 0xA13060-0xA130F1 range for it (`default_time_w`).
const SRAM_CONTROL: u32 = 0xA130F0;
/// The TMSS security register: the four bytes a TMSS console listens to for the
/// long `move.l #"SEGA", $A14000` that releases the VDP (segaretro "TradeMark
/// Security System", "Design", and [`TMSS_SECURITY_STRING`] for the value).
///
/// Just the longword, not the whole 0xA14000-0xA14FFF area: that is what segaretro
/// names and what BlastEm decodes (`location == 0xA14000 || location == 0xA14002`
/// for a word, `0xA14000..=0xA14003` for a byte in `write_tmss`). Genesis Plus GX
/// takes the whole 0xA140 page (`case 0x40` of `ctrl_io_write_byte`), which no game
/// has ever needed.
const TMSS_SECURITY: u32 = TMSS_START;
/// Address mask that names [`TMSS_SECURITY`] from an address in [`Region::Tmss`]:
/// the register is a longword, so dropping A0-A1 is what is left of its decode.
const TMSS_SECURITY_MASK: u32 = !3;
/// What [`TMSS_SECURITY`] must be written with, in address order: `S`,`E`,`G`,`A`.
const TMSS_SECURITY_STRING: [u8; 4] = *b"SEGA";
/// The TMSS ROM select register, `0xA14101` of the cartridge slot extra area: bit 0
/// selects the cartridge (1) or the TMSS boot ROM (0) at 0x000000. segaretro
/// "TradeMark Security System"; Genesis Plus GX's `gen_bankswitch_w` takes
/// `data & 1` for the pair.
const TMSS_ROM_SELECT: u32 = 0xA14100;
/// The bits of the version register that say whether this console has a TMSS: the
/// hardware version number, bits 3-0, of which 1 is TMSS and 0 is "no TMSS"
/// (see [`crate::Region::version_register`]). BlastEm asks the same question with
/// `version_reg & 0xF` in `io_write`.
const TMSS_VERSION_MASK: u8 = 0x0F;
/// Only these bits of a 68k address in [`Region::Z80`] reach the Z80's bus: the 68k
/// drops its top byte at the sound side, so 0xA08000 is 0xA00000 again. BlastEm
/// masks a 68k access to the area with `0x7FFF`, and Genesis Plus GX decodes it with
/// `(address >> 13) & 3` and `(address >> 8) & 0x7F` on a 16 bit offset, which wraps
/// the upper 32 KiB of the area onto the lower 32 KiB exactly the same way.
const Z80_AREA_MASK: u32 = 0x7FFF;

// Sub-decode of [`Region::Z80`]: the boundaries of the Z80's own map
// ([`crate::z80_bus`]) as offsets into its 32 KiB window. See [`SoundSide`].
const SND_RAM_END: u32 = 0x3FFF;
const SND_YM_START: u32 = 0x4000;
const SND_YM_END: u32 = 0x5FFF;
const SND_BANK_START: u32 = 0x6000;
const SND_BANK_END: u32 = 0x60FF;
const SND_VDP_START: u32 = 0x7F00;
const SND_VDP_END: u32 = 0x7FFF;
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
    /// Every access to this area passes the TMSS gate in the dispatch below first:
    /// on a TMSS console nothing here is reachable at all until the security string
    /// arrives (see [`Bus::vdp_access_denied`]).
    ///
    /// The area is the VDP's ports and their mirrors: the whole 0x20 block of
    /// plutiedev "Memory map", every other word, mirrored every 0x40 up to
    /// `$DFFFFF`. [`vdp_port`] is the decode, and the corner that decodes to
    /// nothing - `$C00020-$C0003F` of the same page - is [`VDP_HANG_BIT`]'s.
    Vdp,
    /// 0xA00000-0xA0FFFF, the Z80 bus seen from the 68k: the sound RAM, the YM2612,
    /// the bank register and the PSG, reached only while the 68k holds the Z80's bus
    /// and the Z80 is out of reset.
    ///
    /// [`SoundSide`] is the decode and [`Bus::z80_bus_granted`] the gate.
    Z80,
    /// 0xA10000-0xA10FFF, I/O: version register and the three controller ports -
    /// the whole decoded area, not just the 0xA10000-0xA1001F block of registers
    /// (plutiedev "I/O ports", and "68000 address space overview" for the width
    /// of the area).
    ///
    /// [`Bus::version`] answers [`IO_VERSION`] and [`Bus::io`] the register block
    /// at 0xA10000-0xA1001F; the rest of the area is [`OPEN_BUS`].
    Io,
    /// 0xA11000-0xA11FFF, Z80 bus control: [`Z80_BUSREQ`] and [`Z80_RESET_REG`],
    /// which decide whether a 68k access to [`Region::Z80`] is allowed at all.
    Z80Ctrl,
    /// 0xA13000-0xA130FF, the cartridge slot extra: the registers a cart's own
    /// hardware watches, on the /TIME signal of the slot (plutiedev "$A130xx area
    /// usage", which lists `$A130F0 to $A130FF` as "Sega mapper").
    ///
    /// [`SRAM_CONTROL`] is the save RAM control of a cart that declares save RAM,
    /// and [`mapper::bank_slot`] is the seven bank registers of a cart that has
    /// the Sega mapper. Anything else - the corners other hardware is reserved
    /// for, and either register on a cart that has neither - is nothing's: it
    /// reads [`OPEN_BUS`] and drops writes. Nothing in the block is readable, so
    /// both are write-only ([`Bus::read_cart_extra_byte`]).
    CartExtra,
    /// 0xA14000-0xA14FFF, TMSS ports: the security register at
    /// [`TMSS_SECURITY`] and the ROM select register at [`TMSS_ROM_SELECT`].
    ///
    /// Only a TMSS console answers either; the rest of the area reads
    /// [`OPEN_BUS`] and drops writes.
    Tmss,
    /// 0xA15000-0xA15FFF, the SVP's register page: the status/control
    /// registers of the cart that carries the board ([`crate::cart::svp`],
    /// #433). notaz *SVP doc* section 3 lists the registers at
    /// 0xA15000-0xA15009; Genesis Plus GX's `case 0x50` of `mem68k.c` routes
    /// the whole 64 KiB page to its SVP handler and answers only the XST and
    /// PM0 lanes of it, so the page is the region and the per-lane decode is
    /// [`crate::cart::svp::Svp`]'s.
    ///
    /// A cart without the board reads [`OPEN_BUS`] here and drops writes,
    /// exactly what this page did as part of [`Region::Unmapped`] before it
    /// was decoded - the handlers below answer for the missing board.
    Svp,
    /// Everything the console does not decode: the expansion slot at
    /// 0x400000-0x7FFFFF, the 32X area at 0x800000-0x9FFFFF, the Sega CD extra
    /// at 0xA12000-0xA120FF and the 0xA15000-0xBFFFFF gap, minus the SVP's own
    /// register page inside it.
    ///
    /// Reads answer the float ([`Bus::open_bus_word`], #508, which took this
    /// region off #77's list); the corners of the map that stall the 68k
    /// instead of returning a value - the VDP area's `$C00020`-`$C0003F` -
    /// hang through their own region's dispatch ([`Bus::vdp_hang`]).
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
            SVP_PAGE_START..=SVP_PAGE_END => Self::Svp,
            _ => Self::Unmapped,
        }
    }
}

/// One of the VDP's own ports, the sub-decode of [`Region::Vdp`] that
/// [`Bus::read_vdp_byte`] and its three siblings answer.
///
/// The table is plutiedev "Memory map", "List of VDP ports", read together with
/// the two mirrors the same page states:
///
/// | `addr & 0x1F` | port |
/// | ------------- | ---- |
/// | `$00`, `$02`  | data port, and its every-other-word mirror |
/// | `$04`, `$06`  | control port: write a command, read the status register |
/// | `$08`-`$0F`   | HV counter, read-only |
/// | `$10`-`$17`   | PSG, on the odd lane of the pair |
/// | `$18`-`$1F`   | the debug address (`$18`) and data (`$1C`) ports |
///
/// Genesis Plus GX's `vdp_read_byte`/`vdp_write_word` (`core/mem68k.c`) is the
/// same table, reached with `case 0x00:` for the data port, `case 0x04:` for the
/// control one and `case 0x10: case 0x14:` for the PSG.
enum VdpPort {
    /// The data port: the memories, at the address and memory the code register
    /// points at, and their read side (genvdp.txt section 7).
    Data,
    /// The control port: register writes and command words going in, the status
    /// register coming out (genvdp.txt section 7).
    Control,
    /// The HV counter, which reads the counter and takes no writes (genvdp.txt
    /// section 5 "HV Counter").
    HvCounter,
    /// The PSG port: the 68k's own way to the sound chip, one byte wide
    /// (plutiedev "Memory map": "PSG is normally accessed by byte, so you'll
    /// generally see $C00011 instead").
    Psg,
    /// The debug pair at `$18` and `$1C`. Nothing models it: writes are dropped
    /// and reads float. Genesis Plus GX sends `$1C` to a `vdp_test_w` whose ticket
    /// has not been written, and MAME's map gives the pair no function.
    Debug,
}

/// The one address line that separates the VDP's port block from the corner of
/// the area that hangs up: A5.
///
/// plutiedev "Memory map" gives the ports to `$C0001F` and then says "Accessing
/// address range `$C00020` to `$C0003F` hangs up", and one line later "VDP
/// mirrors every `$40` up to `$DFFFFF`". Both sentences are the same decoder: the
/// block the VDP decodes is 0x20 bytes - A4-A1 select the ports and A0 is the byte
/// lane - A5 has to be low for the chip to answer at all, and nothing above A5
/// reaches the decode, which is what "mirrors every $40" is.
///
/// The hang itself - no DTACK, so the 68k stops - is modelled by
/// [`Bus::vdp_hang`], called from the [`m68k::Bus`] dispatch in front of the
/// region's own functions (#508); an address with this bit set never reaches a
/// port function.
const VDP_HANG_BIT: u32 = 1 << 5;

/// The port that `addr` names inside [`Region::Vdp`], or `None` for the corner of
/// the area that is not a port at all.
///
/// `addr` is masked and already decoded as [`Region::Vdp`], and that region is
/// half the answer on its own: the VDP is selected for all of
/// `$C00000-$DFFFFF`, so nothing above A5 of the address takes part in the port
/// decode and only [`VDP_HANG_BIT`] and the five bits of the [`VdpPort`] table are
/// left.
///
/// ## The mask #69 suggested, checked
///
/// The ticket put `(addr & 0xE700E0) == 0xC00000` forward and asked for it to be
/// verified against plutiedev "Memory map" and genvdp.txt. It is not the decode,
/// in both directions, and it is not used:
///
/// - It tests A6 and A7 along with A5, so it says `$C00040` and `$C00080` are not
///   the VDP. plutiedev says the opposite one sentence after naming the hang-up
///   corner: "VDP mirrors every $40 up to $DFFFFF". A6 is above the 0x20 the VDP
///   decodes and takes no part in the hang.
/// - It tests A16-A18, so it says `$C10000`-`$CFFFFF` are not the VDP either,
///   while the range that same page's overview table gives, and which
///   [`Region::Vdp`] decodes, runs to `$DFFFFF`. It also leaves A19-A20 alone,
///   which accepts `$D00000`-`$D0FFFF` while its test on A16-A18 rejects
///   `$D10000`-`$DFFFFF`: a shape no address decoder has ever had.
///
/// What checking leaves behind is one bit, [`VDP_HANG_BIT`], inside a region that
/// is already the mirror. The other mirror, the every-other-word one of plutiedev
/// "all VDP ports are mirrored every other word, e.g. control port is at both
/// $C00004 and $C00006", is A1 - which the port table below simply does not look
/// at, and which is what lets a `move.l` reach a port as two word accesses.
///
/// The three sources this file quotes from do not agree with each other on the
/// upper mirror, which is why the difference is recorded rather than settled:
/// MAME's `megadriv_68k_base_map` maps `$C00000-$C0001F` and nothing else (plus
/// `$D00000-$D0001F`, commented "the earth defend"), and Genesis Plus GX's
/// `vdp_read_byte`/`vdp_write_word` mask `& 0xFD` and `& 0xFC`, so A1 and A8-A23
/// are don't-care but A6 and A7 are not and `$C00040` reaches its lockup handler.
/// plutiedev is the map this bus is built from throughout this file, and its rule
/// accepts everything those two accept near `$C00000` and nothing that hangs up.
fn vdp_port(addr: u32) -> Option<VdpPort> {
    if addr & VDP_HANG_BIT != 0 {
        return None;
    }
    Some(match addr & z80_bus::VDP_PORT_MASK {
        0x00..=0x03 => VdpPort::Data,
        0x04..=0x07 => VdpPort::Control,
        0x08..=0x0F => VdpPort::HvCounter,
        0x10..=0x17 => VdpPort::Psg,
        _ => VdpPort::Debug,
    })
}

/// The 68k memory a 68k to VDP DMA reads its source through: the cart and work
/// RAM, which is where every DMA source is.
///
/// This is what [`Vdp::run_dma`] is handed after each control port write, and it
/// is a separate type rather than a call into [`Bus`] because of a borrow: the
/// transfer holds `&mut self.vdp` for its whole length, and the source cannot
/// also borrow `self`. [`Bus::vdp_control_write`] is where the two borrows are
/// taken apart.
///
/// [`Vdp::run_dma`]: vdp::Vdp::run_dma
struct VdpDmaSource<'a> {
    rom: &'a [u8],
    ram: &'a [u8; 0x10000],
    mapper: &'a Mapper,
    svp: Option<&'a Svp>,
    /// The open-bus float at the moment the transfer started: the queue word
    /// the 68k last published. The 68k is frozen for the length of a 68k to VDP
    /// transfer, so the value does not move while it runs, and an undriven
    /// source word answers it ([`open_bus_lane`]).
    prefetched_word: u16,
}

impl vdp::DmaBus for VdpDmaSource<'_> {
    /// The word the 68k would read at `addr`.
    ///
    /// Not [`Bus::read_rom_word`], and that is a choice rather than an
    /// inconvenience: the save RAM overlay and the cart's EEPROM are what make
    /// that one `&mut self`, and this runs with the VDP borrowed. So a DMA whose
    /// source is a save window reads the ROM underneath it, and the cart's data
    /// line is always read as the image. No cart has ever put its save where its
    /// DMA sources point.
    ///
    /// The mapper *is* consulted, because a DMA out of a banked cart is ordinary
    /// and `Mapper::map` takes `&self`: a DMA reads what the 68k would read at
    /// that address today, which is the bank the cart last switched in.
    ///
    /// Nothing else answers a DMA at all, with one cart-specific exception:
    /// the SVP's shared DRAM ([`Bus::read_svp_dram_word`]) - a window of the
    /// cartridge area that Virtua Racing sources transfers from, and which a
    /// source read must answer with the DRAM and not the image at the same
    /// offsets. A source inside [`Region::Vdp`] is not modelled, which is a
    /// transfer reading the memory it is writing to - the
    /// VDP asking itself a question through the bus - and what hardware does
    /// about it is not settled: Genesis Plus GX splits the 68k transfer into
    /// `vdp_dma_68k_ext`, `vdp_dma_68k_ram` and `vdp_dma_68k_io`, and the middle
    /// one comments "access Work-RAM by default" for every internal address from
    /// `$800000` up. No game has pointed a DMA at the VDP ports.
    fn read_word(&mut self, addr: u32) -> u16 {
        let addr = addr & ADDR_MASK;
        let (high, low) = match Region::decode(addr) {
            Region::Rom => {
                // The SVP's DRAM window before the image at the same offsets.
                if let Some(word) = Bus::read_svp_dram_word(self.svp, addr) {
                    return word;
                }
                let offset = self.mapper.map(addr).unwrap_or((addr - ROM_START) as usize);
                (
                    self.rom.get(offset).copied(),
                    self.rom.get(offset + 1).copied(),
                )
            }
            Region::Ram => {
                let index = Bus::ram_index(addr);
                (Some(self.ram[index]), Some(self.ram[index + 1]))
            }
            _ => return self.open_bus_word(),
        };
        // Two bytes rather than a slice, so that a word straddling the end of the
        // image still returns the byte that is there - the same reason
        // [`Bus::read_rom_word`] looks its halves up separately. The half that is
        // not there is the float, lane by parity.
        u16::from_be_bytes([
            high.unwrap_or_else(|| open_bus_lane(self.prefetched_word, addr)),
            low.unwrap_or_else(|| open_bus_lane(self.prefetched_word, addr + 1)),
        ])
    }
}

impl VdpDmaSource<'_> {
    /// The word an undriven area answers a DMA source read with: the float the
    /// transfer started with.
    fn open_bus_word(&self) -> u16 {
        self.prefetched_word
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
///
/// `Serialize`/`Deserialize` are for the save state of M6-07 (#127), which is
/// every field of the bus **except [`Bus::rom`]**: a state names the cartridge
/// by checksum rather than carrying it (docs/save-state-format.md), so the ROM
/// is already loaded when a state is read and [`crate::Genesis::load_state`]
/// puts the bytes back on the bus it has just replaced. A `Bus` that came out of
/// a deserializer therefore has an empty `rom` for exactly one moment, and
/// `state::decode` is the only thing that constructs one.
#[derive(Debug, Serialize, Deserialize)]
pub struct Bus {
    /// Cartridge ROM in address order, mapped at 0x000000.
    ///
    /// A plain image on purpose: detection, deinterleave
    /// ([`crate::cart::deinterleave`]) and header decoding are steps of
    /// [`crate::cart`], and the code that turns a file into a `Bus` is a later
    /// ticket.
    ///
    /// Left out of a save state, which names the cartridge by checksum instead
    /// of carrying it - see the note on [`Bus`] and `docs/save-state-format.md`.
    #[serde(skip)]
    pub rom: Vec<u8>,
    /// The cart's save RAM, from [`crate::cart::sram::detect`] on [`Bus::rom`],
    /// or `None` for a cart that declares none.
    ///
    /// Not a region of its own: the save lives *inside* the cartridge area, so a
    /// mapped save is an overlay on [`Region::Rom`] and this is the field the slot
    /// consults first. [`Bus::read_rom_byte`] is the order, and the seam the
    /// M3-12 (#72) mapper extends.
    ///
    /// Private behind [`Bus::sram`], [`Bus::load_sram`] and [`Bus::take_dirty`]:
    /// the only writes that may reach a save are ones that can say so.
    sram: Option<Sram>,
    /// The cart's serial EEPROM, from [`crate::cart::eeprom::detect`] on
    /// [`Bus::rom`], or `None` for a cart that has no chip on its bus.
    ///
    /// The other save shape the cartridge slot answers, and never both at once:
    /// [`crate::cart::sram`] and [`crate::cart::eeprom`] decode one header field,
    /// and a cart that declares save RAM has had the chip of the serial EEPROM
    /// game it was replaced for. What the two have in common is that they live
    /// *inside* the cartridge area, so like the save RAM this is a field the slot
    /// consults rather than a region of its own - see [`Bus::read_rom_byte`] for
    /// the order, and [`Bus::sram`] for the one view of either that a front end
    /// is handed.
    eeprom: Option<Eeprom>,
    /// The cart's bank registers, from [`crate::cart::mapper::detect`] on
    /// [`Bus::rom`]: the Sega mapper for a cart too big for the cartridge area,
    /// and [`Mapper::None`] for one that is not.
    ///
    /// Not a region either, and not a second translation of the address space:
    /// the mapper is the one step of [`Bus::read_rom_byte`] between the save RAM
    /// overlay and the image, and the seven registers of
    /// [`Region::CartExtra`] that [`mapper::bank_slot`] names are the only thing
    /// that writes it. [`crate::z80_bus`] reaches the cartridge through the same
    /// read, which is what makes a bank switch visible to the sound CPU as it is
    /// to the 68k - the hazard plutiedev "Going beyond 4MB", "Things to watch out
    /// for" warns about.
    ///
    /// A cart with a save RAM window and a mapper is possible in principle and
    /// shipped by nobody: the overlay is asked first, so a save always covers the
    /// bank underneath it rather than trading places with it, which is what
    /// Genesis Plus GX does in `mapper_sega_w` ("save current cartridge ROM bank
    /// mapped to $200000-$20ffff" to put back later).
    mapper: Mapper,
    /// The J-CART register, from [`crate::cart::jcart::detect`] on [`Bus::rom`],
    /// for the carts that carry the board: the two extra controller ports of
    /// [`crate::cart::jcart`], answered at $380000-$3FFFFF.
    ///
    /// Not a region, like the two cart devices above it: the quadrant lives
    /// inside [`Region::Rom`], and [`Bus::read_rom_byte`] and
    /// [`Bus::write_rom_byte`] are the seams. The ownership rule of the one
    /// byte two pieces of hardware answer is stated where it is applied:
    /// **bit 7 of the odd byte is not this register's**. It is the wire the
    /// cart's EEPROM data line rides on, and the register's answer is built
    /// around it - GPGX's `mapper_i2c_jcart_read8` masks the port's own bit 7
    /// and puts the line there, which is what makes the two owners lossless:
    /// different bits. The EEPROM itself is [`Self::eeprom`] with
    /// [`crate::cart::Board::CODEMASTERS`], whose write quadrant is
    /// $300000-$37FFFF and whose own answer window is this same
    /// $380000-$3FFFFF for the carts that have no register - and the decode
    /// below is what keeps the two from ever answering the same read.
    jcart: Option<Jcart>,
    /// The SVP board, from [`crate::cart::svp::Svp::detect`] on [`Bus::rom`],
    /// for the one cart that carries it: the shared DRAM of the SSP1601 DSP
    /// behind $300000-$31FFFF and the status/control registers behind its
    /// 0xA15000 page ([`crate::cart::svp`], #433).
    ///
    /// Like the J-CART above it, half of the board lives inside
    /// [`Region::Rom`]: the DRAM is a window of the cartridge area, and
    /// [`Bus::read_rom_byte`] and [`Bus::write_rom_byte`] are the seams, with
    /// [`VdpDmaSource`] carrying its own view so a VDP transfer sourcing from
    /// the DRAM reads the DRAM and not the image underneath. The register
    /// page is its own [`Region::Svp`], because unlike the J-CART quadrant it
    /// sits in the part of the map nothing else decodes.
    svp: Option<Svp>,
    /// 64 KiB of work RAM, mapped at 0xE00000 and mirrored every 0x10000.
    ///
    /// Saved as a sequence of its bytes: 64 KiB is past the thirty-two entries
    /// `serde` derives, see `crate::big_array`.
    #[serde(with = "crate::big_array")]
    pub ram: Box<[u8; 0x10000]>,
    /// The byte 0xA10001 reports: [`crate::Region::version_register`] for the
    /// region [`crate::Config::resolve_region`] picked, with that config's TMSS
    /// flag. See [`Bus::new`].
    pub version: u8,
    /// The I/O chip: the controller ports behind 0xA10003-0xA1000D and the serial
    /// registers behind 0xA1000F-0xA1001F.
    ///
    /// Public because a front end has to be able to say which buttons are held:
    /// `bus.io.set_buttons(port, buttons)` once per frame, or
    /// `bus.io.set_buttons_6(port, buttons)` for a 6-button pad. See
    /// [`crate::Buttons`].
    ///
    /// The bus holds no cycle counter for the pads' `TH` timeouts: M3-14 #75,
    /// the scheduler, is the thing that will know what time it is, and it says so
    /// with `bus.io.set_time(cycles)` ([`Io::set_time`]).
    pub io: Io,
    /// The sound CPU's own hardware: 8 KiB of RAM and the bank shift register.
    /// The two chips that used to be logs here are on [`Bus::audio`] since
    /// M5-13 (#118).
    ///
    /// Owned by the 68k's bus rather than by a [`crate::Z80View`] so that the one
    /// machine has one bank register and one RAM, reachable from both sides; see
    /// the module docs of [`crate::z80_bus`]. The Z80 runs on the same board as
    /// this bus, and stepping it is `Z80View::new(&mut bus)`.
    pub z80: Z80Side,
    /// The sound chips: the YM2612 and the PSG, with the master-clock positions
    /// that say when each is asked for a sample.
    ///
    /// One of each for the whole machine, reached from both sides: the 68k's
    /// `$A04000`-`$A04003` and the Z80's `$4000`-`$4003` are one YM2612's four
    /// ports, and `$C00011`, `$A07F11` and `$7F11` are one PSG port (plutiedev,
    /// "Memory map"). M5-13 (#118) is what put chips here instead of the logs
    /// [`Z80Side`] used to hold, and the scheduler is what clocks them.
    pub audio: Audio,
    /// The VDP: the chip behind [`Region::Vdp`], and the one the ports of M3-09
    /// (#69) reach.
    ///
    /// Public because the bus has nothing else to hand a front end that wants to
    /// know what the VDP was told, and because the tests of those ports have to
    /// see the memory a port write landed in. Nothing outside the port decode of
    /// `read_vdp_*`/`write_vdp_*` writes it, and the scheduler (M3-14 #75) is the
    /// thing that will advance it.
    ///
    /// The `pal` it is built with comes from [`Bus::version`], the one byte this
    /// bus knows about the console (see [`crate::config`] and
    /// [`Vdp::new`]).
    pub vdp: Vdp,
    /// The Z80's bus request line, as the 68k last set it at [`Z80_BUSREQ`].
    ///
    /// `true` means the 68k has asked for the bus, which owns the sound side
    /// ([`Bus::z80_bus_granted`]) only while the reset line is released;
    /// `false` means the Z80 has it. There is one line, not
    /// a register per address: the decode drops A0-A7, so 0xA11100 and 0xA11101 are
    /// the same flag.
    z80_busreq: bool,
    /// The word the 68000 most recently fetched into the back of its prefetch
    /// queue: the instruction-stream word it will consume next. The CPU
    /// publishes it through [`m68k::Bus::note_prefetched_word`] on every queue
    /// refill, and it is what the unclaimed lanes of the Z80 control word reads
    /// answer ([`busreq_word`]), the way an unclaimed data lane answers the
    /// last word driven over it.
    prefetched_word: u16,
    /// The Z80's reset line, as the 68k last set it at [`Z80_RESET_REG`]. Inverted the
    /// way the hardware is: `true` is *asserted*, which is what the 68k asks for by
    /// writing 0, and while it holds the Z80 does not run and the YM2612 is reset with
    /// it.
    z80_reset: bool,
    /// Set when the 68k releases [`Z80_RESET_REG`], and cleared by
    /// [`Bus::take_z80_reset`].
    ///
    /// Coming out of reset is an event rather than a level: it is what puts the Z80's
    /// `PC` at 0, and the bus cannot reach into the CPU to do it, so the scheduler
    /// (M3-14 #75) asks with this.
    z80_reset_pulse: bool,
    /// How far through [`TMSS_SECURITY_STRING`] the writes to [`TMSS_SECURITY`] have
    /// got: the index of the byte the next one has to be.
    ///
    /// The detector, rather than an image of the register: segaretro "TradeMark
    /// Security System" only says the console watches for the string, and a wrong
    /// byte abandons the attempt (`tmss_seek` back to 0) rather than doing anything
    /// else to the machine. See [`Bus::feed_tmss`].
    tmss_seek: usize,
    /// Has the 68k written the security string, so that [`Region::Vdp`] is released?
    ///
    /// Always false on a console without TMSS ([`Bus::tmss_enabled`] is false),
    /// because there is nothing to write and nothing locks either.
    tmss_unlocked: bool,
    /// Has a VDP access been refused by TMSS, freezing the 68k?
    ///
    /// On real hardware there is no recovery from this short of a reset, so nothing
    /// here clears it but [`m68k::Bus::reset_devices`] - which is why it is a flag on
    /// the bus and not just a dropped access. See [`Bus::locked_up`] for who acts on
    /// it.
    locked_up: bool,
    /// Master clocks the 68000 has been held off its own bus for by accesses this
    /// bus has already answered, waiting for the scheduler to take them out of its
    /// clock: the one wait state of the sound side at `0xA00000`
    /// ([`Z80_AREA_WAIT_MCLK`]) and the 68000's share of every access the Z80 makes
    /// to the 68k bus ([`Z80_WINDOW_STEAL_MCLK`]).
    ///
    /// A one-way door rather than something the CPU asks for, so that the timing
    /// lives here and the 68000 core stays unaware of the bus under it: the access
    /// is what costs the time, and the scheduler spends it by moving the chip's
    /// clock without running the instruction ([`Bus::take_68k_stall`]). A total
    /// rather than a maximum because one instruction can make several contended
    /// accesses, and master clocks because that is the one unit the scheduler
    /// counts in.
    ///
    /// Only the waits that are durations of the chip's own bus time belong here. The
    /// freeze a 68k -> VDP transfer puts on the chip is counted from the beam, so it
    /// lives in [`Bus::pending_68k_hold`]; #489 is the ticket that split the two
    /// because spending a beam-relative figure as a duration lets a chip pay for one
    /// freeze again on every report of it. The data port's full-FIFO wait is
    /// beam-relative too and is still here - see [`Bus::vdp_data_write`].
    pending_68k_stall: u32,
    /// VDP port accesses the 68000 made in the instruction being stepped, which
    /// the scheduler takes after every step: a DRAM refresh that falls inside
    /// one costs the 68000 nothing ([`crate::refresh`], #665). Always 0 between
    /// instructions, so a save state has nothing to carry.
    #[serde(skip)]
    vdp_accesses_68k: u32,
    /// The sound CPU's half of [`Bus::pending_68k_stall`]: the extra T-states a
    /// Z80 access to the 68k bus costs the Z80 ([`Z80_WINDOW_WAIT_MCLK`]). Taken
    /// after every Z80 step by the scheduler ([`Bus::take_z80_stall`]).
    pending_z80_stall: u32,
    /// How far in front of the beam the 68000 is being held, in the master clocks
    /// [`vdp::Vdp::dma_68k_stall_mclk`] counts from it: "the time left until the last
    /// of the transfer's access slots", which is an instant on the raster, not a
    /// length of bus time.
    ///
    /// `vdp::Vdp::data_port_stall_mclk` is measured from the beam the same way and
    /// belongs here too; it is still in [`Bus::pending_68k_stall`] for the golden
    /// reason stated on [`Bus::vdp_data_write`].
    ///
    /// Kept apart from [`Bus::pending_68k_stall`] because the scheduler has to spend
    /// the two the other way round - that one added to the chip's clock, this one as
    /// the gate `beam + hold` ([`crate::Scheduler::freeze`]). Summed with
    /// the additive waits it is the #489 runaway: an unmoved beam reports the rest of
    /// the same transfer again on every control write, and each reading of it is then
    /// paid for twice.
    ///
    /// A maximum rather than a total for the same reason: two holds reported at one
    /// beam position are two names for one instant, and the later one is the further
    /// of the two. `0` is "nothing is holding it".
    pending_68k_hold: u32,
    /// The sound CPU's half of [`Bus::pending_68k_hold`]: the rest of a 68k -> VDP
    /// transfer a window access has to wait out
    /// ([`vdp::Vdp::dma_68k_stall_mclk`][vdp::Vdp::dma_68k_stall_mclk]), which is
    /// beam-relative like the 68000's own hold and not the [`Z80_WINDOW_WAIT_MCLK`]
    /// of the access itself. See [`Bus::z80_window_access`].
    pending_z80_hold: u32,
    /// Bit 0 of [`TMSS_ROM_SELECT`]: 1 for the cartridge at 0x000000, 0 for the
    /// TMSS boot ROM. Stored and nothing more - this emulator ships no boot ROM, so
    /// the cart stays mapped whatever this says (see [`Bus::write_tmss_byte`]).
    ///
    /// Public because the ticket that ships a TMSS boot ROM is the thing that will
    /// switch the cartridge slot on it, and reading it back is all that ticket needs
    /// of #70; nothing here reads it.
    pub tmss_cart_selected: bool,
}

impl Bus {
    /// A bus with `rom` loaded and `version` behind 0xA10001, and the Z80 parked the
    /// way a real console leaves it at power on: **reset asserted, bus released to the
    /// Z80**, so [`Bus::z80_can_run`] is false and [`Region::Z80`] is unreachable until a
    /// program asks for it. Work RAM starts zeroed; a real console powers up with
    /// undefined contents, which nothing depends on yet.
    ///
    /// Those two power-on levels are what both references that set the lines up use:
    /// MAME's `machine_reset` (`src/mame/sega/megadriv.cpp`) sets `z80_is_reset = 1` and
    /// `z80_has_bus = 1`, and Genesis Plus GX's `gen_reset` (`core/genesis.c`) sets
    /// `zstate = 0` on the line commented "Z80 bus is released & Z80 is reseted", plus
    /// `zbank = 0`. It is also what the init code of plutiedev "Using the Z80" expects:
    /// it asserts reset, requests the bus, releases reset, and only then copies its
    /// program into Z80 RAM, because the sound CPU has not initialised itself until it is
    /// let go.
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
    ///
    /// It is also the one thing that decides whether the VDP is locked: the hardware
    /// version number in that byte *is* TMSS, so a bus built from
    /// `region.version_register(config.tmss)` locks its VDP exactly when the config
    /// asked for TMSS and never otherwise. See [`Bus::tmss_enabled`].
    ///
    /// The save RAM is the one thing here that is read out of the ROM rather than
    /// handed in: [`crate::cart::sram::detect`] decodes the header at 0x100, so a
    /// bus cannot exist with a save that does not match its image and a cart with
    /// no `RA` in its header gets no save. The other two shapes - an
    /// `Option<SramInfo>` parameter, or an `attach_sram` the caller may or may not
    /// remember - both allow a bus whose save and ROM disagree, and the second one
    /// fails quietly: no save, no error, a player who loses progress. A parameter
    /// is what the M3-15 (#76) loader will use if it wants to pass what it has
    /// already parsed along; until then there is no loader to take it.
    pub fn new(rom: Vec<u8>, version: u8) -> Self {
        // The save RAM of a cart is declared in its header, which is 0x200 bytes
        // into the image and may not be there at all.
        let sram = sram::detect(&rom).map(|info| Sram::new(info, rom.len()));
        // The EEPROM is asked after the save RAM and is told by it to stop: the
        // two are the two shapes a cart's one save takes, and
        // [`crate::cart::eeprom::detect`] is what keeps them apart.
        let eeprom = eeprom::detect(&rom).map(Eeprom::new);
        // The mapper is read out of the image the same way, and for the same
        // reason: it is either the size of the cart or the cart's own request for
        // it in the system type, and both are in front of the code that turns a
        // file into a machine rather than next to it.
        let mapper = Mapper::detect(&rom);
        // And so is the J-CART register, for the carts whose boards carry it:
        // [`crate::cart::jcart::detect`] matches the serials and the two
        // zero-serial Micro Machines carts the way Genesis Plus GX's database
        // does.
        let jcart = jcart::detect(&rom);
        // And the SVP board, for the one cart that carries it:
        // [`crate::cart::svp::Svp::detect`] probes the image the way
        // Picodrive's own detection does (title + DSP program blob), and a
        // cart without the board gets nothing at all.
        let svp = svp::Svp::detect(&rom);
        Self {
            rom,
            sram,
            eeprom,
            mapper,
            jcart,
            svp,
            ram: Box::new([0; 0x10000]),
            version,
            io: Io::new(),
            z80: Z80Side::new(),
            audio: Audio::new(),
            // The VDP's `PAL` pin is wired to the same console that reports VMOD
            // in [`Bus::version`], so a PAL machine gives the chip the 240-line
            // crystal that [`Vdp::v30_mode`] is allowed to use.
            vdp: Vdp::new(version_is_pal(version)),
            z80_busreq: false,
            z80_reset: true,
            z80_reset_pulse: false,
            // Nothing has been fetched yet, so every lane reads the open-bus 1s.
            prefetched_word: OPEN_BUS_WORD,
            tmss_seek: 0,
            tmss_unlocked: false,
            locked_up: false,
            pending_68k_stall: 0,
            vdp_accesses_68k: 0,
            pending_z80_stall: 0,
            pending_68k_hold: 0,
            pending_z80_hold: 0,
            // A TMSS console comes out of reset with its own boot ROM in front of the
            // cart (Genesis Plus GX's `gen_reset` maps `boot_rom` at 0x000000 again),
            // so the select bit starts at 0. The cart is what answers 0x000000 here
            // either way: no boot ROM ships.
            tmss_cart_selected: false,
        }
    }

    /// The cart's battery-backed save, whatever chip it is: the bytes a front end
    /// persists, packed as [`crate::cart::sram`] describes them (the usual
    /// 0x200001-0x20FFFF odd-only window is 32 KiB here, not 64 KiB with a gap),
    /// or as the array of the cart's serial EEPROM when that is what the cart has
    /// ([`crate::cart::eeprom`], and 128 bytes for the X24C01 that most of those
    /// carts have). `None` for a cart with neither.
    ///
    /// One view of the two because a front end's half of the job - keep these
    /// bytes, write them back, and know when they changed - is the same for both,
    /// and the name has to answer to the cart slot's one save rather than to the
    /// chip behind it.
    ///
    /// Read-only, because the only writes that may reach a save are the 68k's -
    /// they are what [`Bus::take_dirty`] reports - and [`Bus::load_sram`] for the
    /// one thing a front end does with the bytes back.
    pub fn sram(&self) -> Option<&[u8]> {
        self.sram
            .as_ref()
            .map(|save| save.contents())
            .or_else(|| self.eeprom.as_ref().map(|chip| chip.contents()))
    }

    /// Put `data` back as the cart's save, where `data` is what [`Bus::sram`]
    /// gave for this cart.
    ///
    /// A blob of any other length is another cart's save, so it is refused whole:
    /// nothing is loaded, nothing is panicked on, and the contents stay exactly as
    /// they were. A cart with no save has nowhere for one to land and ignores it
    /// too. Both are why this reports nothing: see
    /// [`crate::cart::sram::Sram::load`] for the choice.
    ///
    /// Restoring is not a change to persist, so [`Bus::take_dirty`] is `false`
    /// after it.
    pub fn load_sram(&mut self, data: &[u8]) {
        if let Some(save) = self.sram.as_mut() {
            save.load(data);
            return;
        }
        if let Some(chip) = self.eeprom.as_mut() {
            chip.load(data);
        }
    }

    /// Has the cart's save changed since this was last asked, clearing it?
    ///
    /// The front end's reason to write anything, and true for exactly one call
    /// after any write the 68k has stored: never for a read, a
    /// [`Bus::load_sram`], or a write the cart refused with the `0xA130F1` write
    /// protect or by hitting the dead half of a half-width window. A serial
    /// EEPROM has no way to refuse a byte, so every byte the chip takes sets it.
    pub fn take_dirty(&mut self) -> bool {
        if let Some(save) = self.sram.as_mut() {
            return save.take_dirty();
        }
        self.eeprom.as_mut().is_some_and(|chip| chip.take_dirty())
    }

    /// Does this console have a TMSS at all?
    ///
    /// The version register's hardware version number answers it: [`Bus::new`] is
    /// handed the byte the caller built with
    /// [`crate::Region::version_register`], and that bit is [`crate::Config::tmss`].
    /// Deriving the lock from the register rather than carrying a second copy of the
    /// config means "the register says TMSS, the behaviour says off" cannot happen.
    /// BlastEm asks the same question the same way (`has_tmss = version_reg & 0xF` in
    /// `io_write`).
    fn tmss_enabled(&self) -> bool {
        self.version & TMSS_VERSION_MASK != 0
    }

    /// Is [`Region::Vdp`] being held by TMSS: this console has one, and the security
    /// string has not been written?
    fn vdp_locked(&self) -> bool {
        self.tmss_enabled() && !self.tmss_unlocked
    }

    /// Has this access to [`Region::Vdp`] been refused: is the VDP held by TMSS?
    ///
    /// Asking is also what records the lock-up, because a refused VDP access is the
    /// lock-up: on real hardware the next access to the VDP before `SEGA` freezes the
    /// 68000 (segaretro "TradeMark Security System": "the next VDP data port access
    /// will cause the 68000 to lock up - this is performed by the I/O controller"),
    /// so this sets [`Bus::locked_up`] and the caller drops the access - reads return
    /// [`OPEN_BUS`] as every other refused access here does, writes go nowhere.
    ///
    /// It is called from the `Region::Vdp` arms of the [`m68k::Bus`] dispatch, not
    /// from `read_vdp_*`/`write_vdp_*`, and that is deliberate: those four functions
    /// belong to M3-09 (#69) and a lock check written inside them is a lock check
    /// that gets rewritten away. M3-09: fill the stubs in and this still holds - just
    /// do not move the check into them.
    ///
    /// The scope is the whole area, which is what Genesis Plus GX does by pointing
    /// all of 0xC0-0xDF at its lockup handlers in `gen_tmss_w`: data, control, HV
    /// counter, debug and **PSG** ports, the every-0x40 mirrors and the
    /// 0xC00020-0xC0003F hang-up corner. The PSG port at 0xC00011 locks because it
    /// lives in the VDP's port pair (plutiedev "Memory map") and is reached from here.
    /// GPGX locks those pages in *both* maps that can reach them: the 68k's own
    /// `m68k.memory_map`, and the Z80's bank-window view `zbank_memory_map`, whose
    /// unlocked handler `zbank_write_vdp` answers the PSG lane (case 0x10) with
    /// `psg_write` - so the port is held, not the bus it is reached by. This is
    /// asked for [`Region::Vdp`] from the [`m68k::Bus`] dispatch, and for the PSG's
    /// spelling inside [`Region::Z80`] (`0xA07F11`, [`SoundSide::Psg`]) from the
    /// window's own read/write, which are the functions already carrying that
    /// area's other gate ([`Bus::z80_bus_granted`]); a Z80 bank-window access to
    /// pages 0xC0-0xDF needs no arm of its own because it goes through the
    /// [`m68k::Bus`] dispatch and this gate with it. GPGX's own 68k window is
    /// stricter than this bus in every TMSS state - its `z80_write_byte` answers
    /// the whole 0xA07F page with `m68k_lockup_w_8`, which is the M3-08 (#68)
    /// settlement of [`SoundSide`] answering the PSG lane instead - but what no
    /// reference does is let a TMSS-locked PSG write land on the chip, and
    /// [#262](https://github.com/scottjab/rustsega/issues/262) is this gate
    /// closing that hole.
    fn vdp_access_denied(&mut self) -> bool {
        if !self.vdp_locked() {
            return false;
        }
        self.locked_up = true;
        true
    }

    /// Does this access to [`Region::Vdp`] land in the corner of the area that
    /// decodes to nothing: `$C00020`-`$C0003F` and its every-0x40 mirrors, the
    /// addresses with [`VDP_HANG_BIT`] set?
    ///
    /// plutiedev "Memory map", on the VDP area, verbatim: "Accessing address
    /// range `$C00020` to `$C0003F` hangs up". A hang is no DTACK, and no DTACK
    /// is a stopped 68000, so asking is also what records the freeze - the same
    /// flag the TMSS gate sets ([`Bus::locked_up`]), cleared by nothing but
    /// [`m68k::Bus::reset_devices`]. Genesis Plus GX answers the same corner with
    /// its `m68k_lockup_w_8`/`m68k_lockup_r_8` handlers, which halt the CPU
    /// (its `mem68k.c`, "Illegal areas (cause system to lock-up since !DTACK is
    /// not returned)"); the lockup reads there return the prefetched word, which
    /// is the float this bus answers undriven reads with ([`Bus::open_bus_word`],
    /// #508).
    ///
    /// It is called from the `Region::Vdp` arms of the [`m68k::Bus`] dispatch, in
    /// front of `read_vdp_*`/`write_vdp_*`, for the same reason the TMSS gate is
    /// (see [`Bus::vdp_access_denied`]): a check written inside those four
    /// functions is a check that gets rewritten away when the stubs fill in.
    /// Every path into [`Region::Vdp`] goes through the dispatch - the 68k's own
    /// accesses, the Z80's `$7F00`-`$7FFF` port window (masked to the 0x20-byte
    /// port block by [`z80_bus::vdp_port_addr`], so it cannot reach the corner)
    /// and the Z80's bank window, whose 9 bit register can point it anywhere in
    /// the 68k space including here - so one guard covers every master. The Z80
    /// itself is not separately held: on
    /// hardware its access would stall mid-cycle with the rest of the bus, and
    /// this model freezes the 68k and lets the sound CPU run on, which no test
    /// ROM reaches and which #508 records as a known simplification.
    fn vdp_hang(&mut self, addr: u32) -> bool {
        let hangs = addr & VDP_HANG_BIT != 0;
        self.locked_up |= hangs;
        hangs
    }

    /// The byte an undriven read answers: one lane of [`Bus::open_bus_word`],
    /// the lane the read's address sits on - A0 of the access selects the byte
    /// lane of the 16 bit bus (genvdp.txt section 7 "8-bit port access"; BlastEm
    /// answers its unmapped byte reads the same way, `value = location & 1 ?
    /// word : word >> 8`, `genesis.c` `io_read_b`'s fall-through).
    fn open_bus_byte(&self, addr: u32) -> u8 {
        open_bus_lane(self.prefetched_word, addr)
    }

    /// The word an undriven read answers: the 68000's prefetched instruction
    /// word.
    ///
    /// That is the model both surviving references implement for every read
    /// nothing drives - Genesis Plus GX's `m68k_read_bus_16` answers the word at
    /// `m68k.pc`, BlastEm's `get_open_bus_value` answers `m68k->prefetch` (its
    /// new core) or the word at `last_prefetch_address` (its old one), and the
    /// place hardware has been observed doing it is Kabuto's VDP-port note
    /// (v1.5): reading `$C00018`/`$C0001C` "returns the following opcode as the
    /// 68k prefetches it before doing the actual read access". The last bus
    /// cycle before a typical undriven read is the fetch of the next opcode, so
    /// the lines the read samples hold that word - which is "data left on the
    /// bus" in Kabuto's phrasing, and what the 68k last drove onto the bus in
    /// this crate's. `prefetched_word` is that value: the CPU publishes it on
    /// every queue refill ([`m68k::Bus::note_prefetched_word`], #479).
    ///
    /// `$FFFF` before the first fetch is the pull-up state: the data lines have
    /// pull-ups (Kabuto: "a value of `$FFFF` is read (due to bus pull-up)"), so
    /// a bus nothing has driven reads as ones. That is also all the pull-up is
    /// here - Kabuto's "every now and then" `$FFFF` readings, the pull-up
    /// showing through after bus delays, are folded into the one state this
    /// model gives a bus that nothing recent has driven, and there is no decay:
    /// no source gives a value the float settles to after N cycles, and the
    /// ticket that invents one is worse than the placeholder it replaces
    /// (#508, "Sources, and the honest limit").
    fn open_bus_word(&self) -> u16 {
        self.prefetched_word
    }

    /// Feed one byte of the security string to the detector at [`TMSS_SECURITY`].
    ///
    /// Bytes arrive in the order the 68k writes them, which is the order that matters
    /// here: `move.l #"SEGA", (A14000).l` is two word writes with the high word first
    /// (`m68k::cpu`, "a long is two word accesses, high word first"), so `S` `E`
    /// `G` `A`, and four `move.b`s in address order are the same four bytes.
    ///
    /// A byte that is not the next one of `SEGA` restarts the detector: the console
    /// has stopped paying attention to this attempt, and the machine is untouched.
    /// A console without a TMSS ignores its security register entirely (BlastEm's
    /// `write_tmss` only accepts the string when the version register claims TMSS).
    fn feed_tmss(&mut self, byte: u8) {
        if !self.tmss_enabled() || self.tmss_unlocked {
            return;
        }
        if byte != TMSS_SECURITY_STRING[self.tmss_seek] {
            self.tmss_seek = 0;
            return;
        }
        self.tmss_seek += 1;
        if self.tmss_seek == TMSS_SECURITY_STRING.len() {
            self.tmss_unlocked = true;
        }
    }

    /// Has a VDP access frozen the 68k, so that it must not be stepped any more?
    ///
    /// This is the question the scheduler asks (M3-16, #77), and it answers it by
    /// moving the 68000's clock along without running it, the way the stopped Z80
    /// stays clocked ([`Bus::z80_can_run`]). Freezing is not stopping the machine:
    /// the beam goes on, and nothing clears the flag but `RESET` - see
    /// [`m68k::Bus::reset_devices`].
    pub fn locked_up(&self) -> bool {
        self.locked_up
    }

    /// Take the master clocks the 68000 has been held off its own bus for, and
    /// clear them.
    ///
    /// The scheduler asks after every 68000 step and adds what it gets to the
    /// 68000's clock without running another instruction
    /// ([`crate::Scheduler::charge_mclk`]), which is the whole of what these waits
    /// are: the access was answered, and the chip was simply not allowed to do
    /// anything else for a while. See [`Bus::pending_68k_stall`] for what goes
    /// into the total.
    /// Take [`Bus::vdp_accesses_68k`], the VDP port accesses of the 68000
    /// instruction just stepped, and clear it.
    pub fn take_vdp_accesses_68k(&mut self) -> u32 {
        std::mem::take(&mut self.vdp_accesses_68k)
    }

    pub fn take_68k_stall(&mut self) -> u32 {
        std::mem::take(&mut self.pending_68k_stall)
    }

    /// Take the master clocks the sound CPU has been held off the bus for, and
    /// clear them. The Z80 half of [`Bus::take_68k_stall`].
    pub fn take_z80_stall(&mut self) -> u32 {
        std::mem::take(&mut self.pending_z80_stall)
    }

    /// Take how far in front of the beam the 68000 is being held by a 68k -> VDP
    /// transfer, and clear it.
    ///
    /// The other kind of time the bus produces: [`Bus::pending_68k_hold`] is measured
    /// from the beam, so the scheduler spends it as the deadline
    /// `beam + mclk` ([`crate::Scheduler::freeze`]) and not as a duration. It is
    /// asked for at the same place as [`Bus::take_68k_stall`], after every 68000 step
    /// and before the VDP is walked to it, which is what keeps `beam` the position the
    /// two VDP figures were counted from.
    pub fn take_68k_hold(&mut self) -> u32 {
        std::mem::take(&mut self.pending_68k_hold)
    }

    /// Take the sound CPU's beam-relative hold, and clear it. The Z80 half of
    /// [`Bus::take_68k_hold`].
    pub fn take_z80_hold(&mut self) -> u32 {
        std::mem::take(&mut self.pending_z80_hold)
    }

    /// Hold the 68000 for `mclk` master clocks on top of the access being made.
    fn stall_68k(&mut self, mclk: u32) {
        self.pending_68k_stall = self.pending_68k_stall.saturating_add(mclk);
    }

    /// Hold the sound CPU for `mclk` master clocks on top of the access being made.
    fn stall_z80(&mut self, mclk: u32) {
        self.pending_z80_stall = self.pending_z80_stall.saturating_add(mclk);
    }

    /// Hold the sound CPU until `mclk` master clocks in front of the beam, keeping the
    /// further of that and any hold already recorded - a max, not a sum, because two
    /// figures counted from one beam position are two names for one instant. The
    /// 68000's own transfer freeze is recorded straight into
    /// [`Bus::pending_68k_hold`] by [`Bus::vdp_control_write`], which holds the chip
    /// for the whole of the transfer rather than for one access.
    fn hold_z80(&mut self, mclk: u32) {
        self.pending_z80_hold = self.pending_z80_hold.max(mclk);
    }

    /// One access by the sound CPU to the 68k address space through its bank
    /// window at `0x8000-0xFFFF`: both chips pay for it, and the 68000 may still
    /// be holding the line for a transfer it is not running.
    ///
    /// The Z80 pays [`Z80_WINDOW_WAIT_MCLK`] for the width conversion and the
    /// 68000 pays [`Z80_WINDOW_STEAL_MCLK`] for the bus it just lost.
    ///
    /// A window access made while a 68k -> VDP transfer is running waits out the
    /// rest of it ([`vdp::Vdp::dma_68k_stall_mclk`]) as well. #308 checked that
    /// against the page that describes the arbiter rather than inferring it from
    /// the 68000's own freeze: Kabuto, "SEGA Mega Drive / Genesis hardware notes"
    /// v1.5, "The bus system": "If the Z80 tries to access the bus while the VDP is
    /// doing a DMA from RAM/ROM [...] it has to wait for that to complete first."
    /// (Footnote #2 of the same page: "This is one of the reasons Sega advised
    /// developers to pause the Z80 before doing DMA.")
    ///
    /// **The same sentence has a half that is not implemented.** It goes on: "or
    /// the 68k is halted because it tried to read from the VDP while its FIFO is
    /// empty or write to the VDP while its FIFO is full it has to wait for that to
    /// complete first" - so a data-port stall holds the Z80 off the 68k bus too,
    /// and `data_port_stall_mclk` is not added to the Z80's hold here. It was not
    /// added while the two kinds of time were one number, because that figure is
    /// beam-relative and charging it into a chip clock is the defect #489 was open
    /// on. #489's split reduces it to the one line `hold_z80(...)`, and it is still
    /// not folded in here: it is a timing change to every cart that writes the data
    /// port while the sound CPU runs, and it wants a measurement of its own.
    ///
    /// Called from [`crate::z80_bus::Z80View`] for the window alone: the Z80's own
    /// RAM, the YM2612 and the PSG are on its side of the arbitration and cost it
    /// nothing extra - which #532 checked rather than took on faith, on a cart where
    /// those three take 6.8M, 107k and 1.9k accesses to the window's 64,827. See
    /// `z80_side_accesses_steal_nothing`.
    pub(crate) fn z80_window_access(&mut self) {
        let dma = self.vdp.dma_68k_stall_mclk();
        self.stall_z80(Z80_WINDOW_WAIT_MCLK);
        self.hold_z80(dma);
        self.stall_68k(Z80_WINDOW_STEAL_MCLK);
    }

    /// May the Z80 run: it is neither held in reset nor has the 68k taken its bus.
    ///
    /// This is the question the scheduler (M3-14 #75) asks before stepping the sound
    /// CPU; both lines are driven by the 68k at [`Z80_RESET_REG`] and [`Z80_BUSREQ`],
    /// and either one stops it. Neither line has a delay here - see
    /// [`Bus::z80_bus_granted`] for what is not modelled.
    pub fn z80_can_run(&self) -> bool {
        !(self.z80_busreq || self.z80_reset)
    }

    /// Take the "the Z80 has just come out of reset" pulse, and clear it.
    ///
    /// The scheduler answers a `true` with `z80::Z80::reset`, which is what puts `PC`
    /// back to 0; Genesis Plus GX's `gen_zreset_w` calls `z80_reset()` on exactly this
    /// edge. The pulse is one-shot so a scheduler that steps the CPU between two writes
    /// cannot reset it twice, or miss a reset, by looking at the level.
    pub fn take_z80_reset(&mut self) -> bool {
        let pulse = self.z80_reset_pulse;
        self.z80_reset_pulse = false;
        pulse
    }

    /// Drive the Z80's reset line: `true` asserts it, `false` releases it.
    ///
    /// Both edges matter and neither is silent:
    ///
    /// - asserting also resets the YM2612, whose `/RESET` pin is tied to this one
    ///   (BlastEm's `io_write` calls `ym_reset` in the branch that asserts Z80 reset);
    ///   see [`Z80Side::ym_reset`].
    /// - releasing is what resets the Z80 itself, so it raises [`Bus::z80_reset_pulse`]
    ///   for [`Bus::take_z80_reset`] to deliver.
    ///
    /// Writing the level the line is already at does neither, which is why this is
    /// edge-detecting rather than a field write: plutiedev "Using the Z80" asserts and
    /// releases the line repeatedly while loading a program, and the YM is only reset
    /// on the assert edges.
    fn set_z80_reset(&mut self, asserted: bool) {
        if asserted == self.z80_reset {
            return;
        }
        if asserted {
            self.audio.ym_reset();
        } else {
            self.z80_reset_pulse = true;
        }
        self.z80_reset = asserted;
    }

    /// Does the 68k hold the Z80's bus, so that [`Region::Z80`] reaches the sound
    /// hardware at all?
    ///
    /// The answer is the bus request line *and* the reset line: `0xA11100` written 1
    /// asks for the area, but the request only grants it while the reset line is
    /// released ([`Z80_RESET_REG`]), which is the order every sound driver's init
    /// follows. Refused, a read of the area is [`OPEN_BUS`] and a write to it is
    /// dropped - the placeholder every other unclaimed area of this bus answers
    /// with (#508). Genesis Plus GX models the refused state more harshly in two
    /// ways this does not follow: without a request at all it swaps the whole page
    /// over to a bus *lockup* handler (`gen_zbusreq_w`), so a program that pokes
    /// the sound side may hang rather than read junk; with only the reset line
    /// asserted the 68k reads prefetched bus data and its writes go nowhere. The
    /// dropped write logs nothing, because a Genesis bus has no logging facility
    /// and this is a hot path.
    ///
    /// The reset half of the gate is plutiedev "Using the Z80", "Loading a Z80
    /// program" ("we reset, then request the bus then release reset (this is
    /// because for some reason we can't access Z80 RAM while Z80 is reset)"), MAME
    /// and Genesis Plus GX, which agree; BlastEm is the dissenting reference, and
    /// this no longer follows it. MAME's `megadriv_68k_read_z80_ram` /
    /// `megadriv_68k_write_z80_ram` allow the access only when `z80_has_bus == 0`
    /// && `z80_is_reset == 0`; Genesis Plus GX installs the 0xa0 page handlers only
    /// with the reset line released (`gen_zbusreq_w` when `zstate == 1`,
    /// `gen_zreset_w` when `zstate == 2`). BlastEm's `io_read`/`io_write` for the
    /// whole area test `z80_get_busack` and nothing else, which is what this used
    /// to follow (M3-08 #68); see [#260](https://github.com/scottjab/rustsega/issues/260).
    ///
    /// The read-back of `0xA11100` inherits the same definition, and both sources
    /// agree on that corner too: with the reset line held they report the Z80
    /// *running* (MAME's `megadriv_68k_check_z80_bus`), which is what `!` of this
    /// answer gives - see [`Bus::read_z80_ctrl_byte`].
    ///
    /// Two things are deliberately not modelled, both M3-16 (#77)'s and the
    /// scheduler's: the grant is instant, so no cycle is spent waiting for the Z80
    /// to finish the bus cycle it is in and no poll of the read-back can spin; and
    /// an access to the area costs no penalty beyond the 8-bit wait state.
    fn z80_bus_granted(&self) -> bool {
        self.z80_busreq && !self.z80_reset
    }

    /// Cartridge slot byte read, in the order the slot decides:
    ///
    /// 0. **the serial EEPROM**, over the byte the rest of this order has
    ///    answered: [`crate::cart::eeprom::Eeprom::read`] replaces the one bit of
    ///    it that the cart board's data line is on, and answers `None` for every
    ///    other address of the cart. First, and over the two below it, because the
    ///    chip is not another claimant on the address but a wire across the slot:
    ///    the acknowledge it held low a moment ago has to reach the 68k whether the
    ///    address is save RAM the same cart cannot also have, a bank the mapper has
    ///    switched in, or the image past its end - which is where every cart with a
    ///    `$E8` in its header puts it.
    /// 1. **the save RAM**, for a cart that has one and a window that is mapped:
    ///    [`crate::cart::sram::Sram::read`] answers `None` for every address the
    ///    save has no claim on, so the one call is the whole overlay and never a
    ///    scattered `if sram`.
    /// 2. **the J-CART register**, for the carts that carry the board: the
    ///    quadrant $380000-$3FFFFF answers the register and not the image, with
    ///    the EEPROM's data line in bit 7 of its odd byte - the shared byte the
    ///    field docs on [`Bus::jcart`] own up to, and the overlay
    ///    [`Bus::read_jcart_byte`] applies. The cricket carts have no register
    ///    and skip this step, which is what leaves their chip's own answer -
    ///    step 0's [`crate::cart::Board::CODEMASTERS`] window - as the quadrant's
    ///    owner instead.
    /// 3. **the mapper**, between the overlay and the image, as a translation of
    ///    `addr` to an offset in [`Bus::rom`] ([`crate::cart::mapper`], M3-12).
    ///    Below the overlay and not above it because `0xA130F1` bit 0 *is* the
    ///    switch that picks ROM or save RAM for this window, and a mapper only
    ///    decides which ROM is on the other side of it: that is how Genesis Plus
    ///    GX's `mapper_sega_w` treats the same register, which maps the save over
    ///    the bank and remembers which bank was underneath to put back. The
    ///    mapper answers `None` for a cart that has no mapper, and the address is
    ///    then its own offset - the plain image.
    /// 4. **the image**, in address order. A read above its end is [`OPEN_BUS`].
    ///    That is a decided question - [#435](https://github.com/scottjab/rustsega/issues/435)
    ///    is where it was decided - so the reasoning is written down here rather
    ///    than left to be re-derived the next time a cart draws black.
    ///
    ///    An unbanked cart really has no "past the end": the ROM chip is wired to
    ///    the address lines it needs and ignores the rest, so on real hardware the
    ///    image repeats. Answering 0xFF there is therefore wrong for a *complete*
    ///    cart that reads its own echo. What has not been demonstrated is one such
    ///    cart: no image in the local 2775 is known to need the mirror.
    ///
    ///    What has been measured is the other case, and it is the reason mirroring
    ///    is not the answer here. The file is not the ROM. `QuackShot Starring
    ///    Donald Duck (World) (En,Ja) (Rev A)` is `0x80000` bytes of a `0x140000`
    ///    cart, and its boot self-test sums `$100000..$13FFFF` looking for the
    ///    missing top quarter. Mirroring has to wrap on the image length - the
    ///    header's declared size is unusable as a mask, and
    ///    [`crate::cart::RomHeader::declared_rom_size`] says why - so that range
    ///    lands on `$000000..$03FFFF`: the first 256 KiB, which is not the data the
    ///    cart is asking for. Measured with `offset % len` in this one lookup,
    ///    `(Rev A)` stays at `colours=1 max_colours=1` and the two complete
    ///    QuackShot images are unchanged at `colours=4 max_colours=12`. Mirroring a
    ///    truncated dump invents bytes and presents them as the cart's own; it
    ///    cannot put back what is not in the file, and it would change the answer
    ///    across the whole library to rescue one cart that it does not rescue.
    ///
    ///    So: filler, on one measured case and no demonstrated need for the
    ///    alternative. If a *complete* cart is ever shown to read its own image
    ///    above the end of it, that is a new issue with a name and a measurement on
    ///    it, not a licence to wrap here. The short dumps themselves are listed for
    ///    re-dumping under "Truncated dumps" in `docs/compat.md` and flagged by
    ///    [`crate::cart::RomHeader::truncated`].
    ///
    ///    A bank number pointing past the end of the cart reads the same way,
    ///    which is why [`crate::cart::mapper::Mapper::map`] never sees the image at
    ///    all.
    fn read_rom_byte(&mut self, addr: u32) -> u8 {
        if let Some(answer) = self.sram.as_ref().and_then(|save| save.read(addr)) {
            // `Some(None)` is the dead half of a half-width window: the save
            // owns the address, the image must not answer, and nothing drives
            // the byte - the float's lane, then (#508).
            return answer.unwrap_or_else(|| self.open_bus_byte(addr));
        }
        // The J-CART register, for the carts that carry it: the whole quadrant
        // answers the register and not the image (GPGX installs its handler
        // over $380000-$3fffff), which is what makes this an overlay like the
        // save RAM above rather than a bit of the answer below. Bit 7 of the
        // odd byte is not the register's, and is the one wire it shares with
        // [`Self::eeprom`]: the ownership rule, stated at the field and
        // covered by `a_jcart_write_moves_th_and_leaves_the_lines_alone` and
        // `an_eeprom_line_write_does_not_move_th` below.
        if let Some(byte) = self.read_jcart_byte(addr) {
            return byte;
        }
        // The SVP's shared DRAM, for the cart that carries the board: the
        // $300000-$31FFFF window answers the DSP's DRAM and not the image at
        // the same offsets, which is the whole point of the window ([`Svp`],
        // #433). It is an overlay like the ones above because the image
        // decoder below would otherwise eat it - the window is inside the
        // cartridge area, [`Region::Rom`] - and because it is memory the 68k
        // owns half of, not a register event.
        if let Some(byte) = self.read_svp_dram_byte(addr) {
            return byte;
        }
        // The one place a cart's addresses are translated, and the only place the
        // mapper is consulted on the way to a byte: the overlay above has already
        // had its say and keeps its priority.
        let offset = self.mapper.map(addr).unwrap_or((addr - ROM_START) as usize);
        let byte = self
            .rom
            .get(offset)
            .copied()
            .unwrap_or_else(|| self.open_bus_byte(addr));
        // Step 0 of the order above, and the last thing done to the byte: the one
        // bit of it that is the cart's data line, replaced with what the chip is
        // holding it at.
        match self.eeprom.as_ref().and_then(|chip| chip.read(addr, byte)) {
            Some(value) => value,
            None => byte,
        }
    }

    /// The J-CART register's byte at `addr`, or `None` where the register has
    /// no say: carts without one, and every address outside
    /// [`jcart::WINDOW`]. The `sda` bit is the cart's EEPROM data line as the
    /// chip holds it, or the released level - high - for the boards that
    /// carry the register with no chip (GPGX's `eeprom_i2c_out` returns the
    /// latched line, idle-high, for exactly those). This is the step that
    /// keeps the two owners of the quadrant apart: a cart with the register
    /// never reaches the EEPROM read below it, whose Codemasters answer
    /// window is the same addresses - the chip's bit arrives through the
    /// register instead, where the board wires it.
    fn read_jcart_byte(&self, addr: u32) -> Option<u8> {
        let cart = self.jcart.as_ref()?;
        Jcart::covers(addr).then(|| {
            let sda = self.eeprom.as_ref().map(Eeprom::sda_level).unwrap_or(true);
            cart.read_byte(addr, sda)
        })
    }

    /// The SVP's shared DRAM byte at `addr`, or `None` where the cart has no
    /// board or the address is outside the window ([`Svp::covers_dram`]).
    fn read_svp_dram_byte(&self, addr: u32) -> Option<u8> {
        let svp = self.svp.as_ref()?;
        Svp::covers_dram(addr).then(|| svp.read_dram_byte(addr))
    }

    /// The SVP's shared DRAM word at `addr`, for [`VdpDmaSource`], which
    /// cannot go through [`Bus::read_rom_byte`] (it runs with the VDP
    /// borrowed; the struct docs above say why). A DMA sourcing from the DRAM
    /// reads the DRAM and not the image underneath - Virtua Racing moves the
    /// DSP's output into VRAM exactly that way, which is the transfer
    /// Genesis Plus GX's HISTORY.txt calls "fixed broken VDP DMA from SVP ROM
    /// latency" on its side.
    fn read_svp_dram_word(svp: Option<&Svp>, addr: u32) -> Option<u16> {
        let svp = svp?;
        Svp::covers_dram(addr).then(|| svp.read_dram_word(addr))
    }

    /// Cartridge slot word read: the two bytes, high byte first (M68000UM, "Data
    /// Organization").
    ///
    /// The cartridge area is the one region of this bus where a word really is its
    /// two bytes - the image and the save RAM are both byte-addressed memory - so
    /// this is the two byte reads, and that is what makes a half-width save RAM
    /// window work: over an odd-only window one half of the word is the save and
    /// the other is not. The struct docs' "a word is never built from two byte
    /// calls" is about the device regions, where one register answers the whole
    /// word.
    ///
    /// Looking the two halves up separately rather than as a slice is also what
    /// lets a word straddling the end of the image still return the byte that is
    /// there, with [`OPEN_BUS`] in the half that is not.
    fn read_rom_word(&mut self, addr: u32) -> u16 {
        let high = self.read_rom_byte(addr);
        let low = self.read_rom_byte(addr + 1);
        u16::from_be_bytes([high, low])
    }

    /// Cartridge slot byte write: the save RAM, if the window is mapped and
    /// `addr` is inside it, and otherwise nothing at all - the image, like the
    /// mapper that will sit where [`Bus::read_rom_byte`] says, is read-only.
    fn write_rom_byte(&mut self, addr: u32, value: u8) {
        // The J-CART register first: bit 0 of the byte is the TH line of the
        // cart's two extra ports (GPGX's `jcart_write`), from either lane. For
        // the cricket carts - register-less - this arm is absent and the write
        // is dropped at the end, the way GPGX installs `m68k_unused_8_w` over
        // their $380000-$3fffff.
        if let Some(cart) = self.jcart.as_mut().filter(|_| Jcart::covers(addr)) {
            cart.write(value);
            return;
        }
        // The serial EEPROM first: what the 68k wrote here is two lines of the
        // cart's board, and the chip has to see it whether or not the address is
        // one its save RAM overlay claims. The two are decoded from one header
        // field and never both exist, so nothing else about this order changes.
        // On the Codemasters board this is the $300000-$37FFFF quadrant; on
        // every other board it is the pair the cart declares.
        if let Some(chip) = self.eeprom.as_mut() {
            chip.write(addr, value);
        }
        // `Sram::write` answers true for the whole of a mapped window, so a save
        // under the `0xA130F1` write protect and the dead half of a half-width
        // window are both refused here rather than falling through to the image.
        if let Some(save) = self.sram.as_mut() {
            save.write(addr, value);
        }
        // And the SVP's shared DRAM, the one writable window of the cartridge
        // area: a write the board's window claims is memory, not a dropped
        // write ([`Svp`], #433). It is last the way its read overlay is last
        // before the image.
        if let Some(svp) = self.svp.as_mut()
            && Svp::covers_dram(addr)
        {
            svp.write_dram_byte(addr, value);
        }
    }

    /// Cartridge slot word write: both bytes, high byte first, for the reason
    /// [`Bus::read_rom_word`] gives - a 16-bit save stores both halves and a
    /// half-width one stores the half that is its own.
    ///
    /// One exception, and the board that asks for it: the Acclaim 32 Mbit
    /// board's word write is its read-window latch and not a line write (see
    /// [`crate::cart::eeprom::Eeprom::write_word`]), so when the chip consumes
    /// the word the two byte writes below do not happen - with both /LWR and
    /// /UWR asserted that board drives no line at all.
    fn write_rom_word(&mut self, addr: u32, value: u16) {
        if self
            .eeprom
            .as_mut()
            .is_some_and(|chip| chip.write_word(addr, value))
        {
            return;
        }
        self.write_rom_byte(addr, (value >> 8) as u8);
        self.write_rom_byte(addr + 1, value as u8);
    }

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
    /// A word access is its two byte accesses, each decoded on its own through
    /// [`Bus::ram_index`], exactly as [`Bus::read_rom_word`] does for the cartridge
    /// slot. So the word at 0xFFFFFF is the byte at 0xFFFF and the byte at
    /// 0x1000000 - and only the low 16 bits name a byte behind this window, which
    /// makes the second one the byte at 0. Masking per half is what keeps the read
    /// total: before #488 the `self.ram[index + 1]` walked off the array and panicked
    /// the host.
    ///
    /// An address-error exception frame is how the bus comes to be asked at all.
    /// A 68000 *data* word access at an odd address never reaches it - M68000UM
    /// section 6.3.5 "Address Error" raises the exception instead of starting the
    /// cycle, and `crates/m68k` models that in `M68k::alignment_fault` - but the
    /// CPU's own exception traffic uses the unchecked `_raw` accessors, because the
    /// frame write has to land wherever the stack points
    /// (`crates/m68k/src/address_error.rs`).
    /// Masking the address down to the aligned pair instead, on the grounds that the
    /// RAM chips do not see A0, is the other defensible reading and would break both
    /// this invariant and [`Bus::read_ram_byte`]'s agreement with it; it is not what
    /// this bus does anywhere else.
    fn read_ram_word(&mut self, addr: u32) -> u16 {
        let high = self.read_ram_byte(addr);
        let low = self.read_ram_byte(addr + 1);
        u16::from_be_bytes([high, low])
    }

    /// Work RAM byte write.
    fn write_ram_byte(&mut self, addr: u32, value: u8) {
        self.ram[Self::ram_index(addr)] = value;
    }

    /// Work RAM word write, big endian: the two halves of `value` to `addr` and
    /// `addr + 1`, each decoded by [`Bus::write_ram_byte`] for the reason
    /// [`Bus::read_ram_word`] gives - including the wrap of the second half over the
    /// top of the 64 KiB mirror (#488).
    fn write_ram_word(&mut self, addr: u32, value: u16) {
        self.write_ram_byte(addr, (value >> 8) as u8);
        self.write_ram_byte(addr + 1, value as u8);
    }

    /// VDP port byte read: the lane of [`Bus::read_vdp_word`] that `addr` sits on.
    ///
    /// The even byte of a port is the high byte of its word and the odd byte the
    /// low one (genvdp.txt section 7 "8-bit port access"; Genesis Plus GX's
    /// `vdp_read_byte` answers `case 0x00:` with `vdp_68k_data_r() >> 8` and
    /// `case 0x01:` with `& 0xFF`).
    ///
    /// The TMSS gate is not here: it is in the dispatch, in front of every
    /// `Region::Vdp` arm, so that filling these four functions in cannot lose it.
    /// See [`Bus::vdp_access_denied`].
    fn read_vdp_byte(&mut self, addr: u32) -> u8 {
        let word = self.read_vdp_word(addr & EVEN_LANE);
        if addr & 1 == 0 {
            (word >> 8) as u8
        } else {
            word as u8
        }
    }

    /// VDP port word read: the data port, the status register behind a control
    /// port read, and the HV counter are the three ports with a read, and each of
    /// the three is now the chip's own function.
    ///
    /// The data port is the chip's own [`Vdp::read_data`], which decides which
    /// memory the code register points at, what the read is masked by, and how the
    /// address register moves on (M4-03, #81).
    ///
    /// The control port is the chip's own [`Vdp::read_status`] (M4-04, #82), and
    /// neither read is passive: reading either port clears the write-pending flag
    /// (genvdp.txt section 7), so a status read in the middle of a command word
    /// abandons it. That is what `status_read_word_and_bytes` and
    /// [`Self::status_read_clears_the_command_latch`] hold still.
    ///
    /// The HV counter is the chip's own [`Vdp::hv_counter`] (M4-05, #83): the
    /// `VC7-VC0` / `HC8-HC1` word of genvdp.txt section 5, which needs no byte
    /// fix-up on the way because the even lane of that word is the V counter and
    /// the odd lane the H one ("For 8-bit reads, the even byte (e.g. C00008h)
    /// returns the V counter, and the odd byte (e.g. C00009h) returns the H
    /// counter"). A VDP that has been reset has not been told where its beam is,
    /// so the counter reads 0 until M4-06 (#84) starts walking the position.
    ///
    /// The rest of the area answers the float, because nothing can produce
    /// anything else: the PSG and the debug pair have no read, which is the
    /// next paragraph.
    ///
    /// The PSG and the debug pair have no read in any case: plutiedev "Memory map"
    /// gives the PSG as write-only and the debug ports no function, and Genesis
    /// Plus GX's `vdp_read_word` answers `$18` and `$1C` with `m68k_read_bus_16`,
    /// which is its open bus - the answer Kabuto's hardware notes v1.5 observed
    /// on exactly these reads: `$C00018`/`$C0001C` "returns the following opcode
    /// as the 68k prefetches it before doing the actual read access". That is
    /// [`Bus::open_bus_word`] here (#508).
    fn read_vdp_word(&mut self, addr: u32) -> u16 {
        match vdp_port(addr) {
            Some(VdpPort::Data) => self.vdp.read_data(),
            // The status read, which clears the write-pending flag with the rest
            // of what it does (genvdp.txt section 7: "It is cleared when the
            // control port is read").
            Some(VdpPort::Control) => self.vdp.read_status(),
            // The beam position, as the `VC7-VC0` / `HC8-HC1` word of genvdp.txt
            // section 5 (`Vdp::hv_counter`, M4-05 #83).
            Some(VdpPort::HvCounter) => self.vdp.hv_counter(),
            Some(VdpPort::Psg) | Some(VdpPort::Debug) => self.open_bus_word(),
            // The corner of the area that decodes to nothing never reaches
            // here: the [`m68k::Bus`] dispatch hangs the bus in front of these
            // functions ([`Bus::vdp_hang`]). The arm stays for exhaustiveness.
            None => self.open_bus_word(),
        }
    }

    /// VDP port byte write: the byte reaches the port as the same byte in both
    /// halves of the word (genvdp.txt section 7 "8-bit port access"; Genesis Plus
    /// GX's `vdp_write_byte` is `vdp_68k_data_w(data << 8 | data)` for the data
    /// port and `vdp_68k_ctrl_w(data << 8 | data)` for the control one).
    ///
    /// That duplication is the whole story on the chip's side: [`Vdp::write_data`]
    /// and [`Vdp::write_control`] are word functions and the VDP never learns that
    /// the 68k only sent eight of the sixteen bits.
    ///
    /// A data port write is queued rather than performed - it is a write FIFO
    /// entry on its way to a memory (`vdp::fifo`, M4-21a #99) - and this does not
    /// wait for it. [`Vdp::data_port_stall_mclk`] is the wait, and it is charged to
    /// the 68k by [`Bus::vdp_data_write`].
    fn write_vdp_byte(&mut self, addr: u32, value: u8) {
        match vdp_port(addr) {
            Some(VdpPort::Data) => self.vdp_data_write(u16::from_be_bytes([value, value])),
            Some(VdpPort::Control) => self.vdp_control_write(u16::from_be_bytes([value, value])),
            // The PSG is one byte wide and it is the odd lane of the pair, so an
            // even-lane byte is not the PSG at all (plutiedev "Memory map": "PSG
            // is normally accessed by byte, so you'll generally see $C00011
            // instead"; Genesis Plus GX's `vdp_write_byte` takes the `$10`/`$14`
            // case and writes `if (address & 1)`).
            Some(VdpPort::Psg) => {
                if addr & 1 != 0 {
                    self.psg_write(value);
                }
            }
            // The counter takes no writes, and nothing models the debug pair.
            Some(VdpPort::HvCounter) | Some(VdpPort::Debug) => {}
            None => {}
        }
    }

    /// VDP port word write: the chip's two ports, and the PSG, which lives in the
    /// same block and is one byte wide.
    fn write_vdp_word(&mut self, addr: u32, value: u16) {
        match vdp_port(addr) {
            Some(VdpPort::Data) => self.vdp_data_write(value),
            Some(VdpPort::Control) => self.vdp_control_write(value),
            // A word write to the PSG pair feeds the PSG the low byte of the word.
            // That byte is the one at `$C00011`, the lane the port lives on, so a
            // `move.w` sends the same byte to the chip as the `move.b` of the same
            // command would (Genesis Plus GX's `vdp_write_word` is
            // `psg_write(cycles, data & 0xFF)`).
            Some(VdpPort::Psg) => self.psg_write(value as u8),
            Some(VdpPort::HvCounter) | Some(VdpPort::Debug) => {}
            None => {}
        }
    }

    /// One word to the control port, and then the DMA that write asked for.
    ///
    /// [`Vdp::write_control`] records the request and cannot run it, because the
    /// chip has no bus to read a source through; the glue that has one is what
    /// calls [`Vdp::run_dma`] (#85 says so on the method), and that call is a
    /// no-op touching nothing whenever the write asked for no DMA. So it follows
    /// every control port write, and nothing else.
    ///
    /// One data port word, and the wait the write FIFO charges for a write that
    /// finds it full.
    ///
    /// [`Vdp::write_data`] queues the word, and [`Vdp::data_port_stall_mclk`] is
    /// read on the state the port is in as the write arrives, which is before it is
    /// queued: it is the wait until a write made now has a place among the entries
    /// (`vdp::fifo::Fifo::slots_until_accepted`), and reading it after the queueing
    /// charges this write for itself as well - a slot more than the chip would have
    /// held for, on every write past the fourth (#545). The write that *fills* the
    /// FIFO is still taken and holds nothing, so only the fifth write of a burst is
    /// charged: what the full bit of genvdp.txt section 6 and Nemesis's VDP FIFO
    /// research on SpritesMind are describing.
    fn vdp_data_write(&mut self, value: u16) {
        let stall = self.vdp.data_port_stall_mclk();
        self.vdp.write_data(value);
        if stall != 0 {
            // TODO(#489): `data_port_stall_mclk` is beam-relative like the transfer
            // freeze - it is `mclk_to_slots(slots_until_accepted)`, master clocks
            // counted *from the beam* (`vdp::fifo`) - so by rights it belongs in
            // `pending_68k_hold` next to the DMA. It stays an additive stall here
            // because that is what the `md_test_pack` palette goldens are calibrated
            // on: charging the same wait as a deadline moves `960pal`, `512pal`,
            // `shltest`, `shltest2`, `md1536` and `mdtrans`, and reblessing six
            // goldens is a decision this change does not make. #517 is the ticket
            // where the two are finally spent the same way.
            self.stall_68k(stall);
        }
    }

    /// One word to the control port, and then the DMA that write asked for.
    ///
    /// [`Vdp::write_control`] records the request and cannot run it, because the
    /// chip has no bus to read a source through; the glue that has one is what
    /// calls [`Vdp::run_dma`] (#85 says so on the method), and that call is a
    /// no-op touching nothing whenever the write asked for no DMA. So it follows
    /// every control port write, and nothing else.
    ///
    /// A write that started a 68k -> VDP transfer also freezes the 68000 for the
    /// whole of it - "The 68000 is frozen during 68k -> VDP transfers" (genvdp.txt
    /// section 6) - and [`Vdp::dma_68k_stall_mclk`] is for how long, charged here
    /// for the scheduler to spend. It is read after every control write rather than
    /// only the one that starts a transfer, and that is safe rather than merely
    /// easy: the 68000 cannot make a second control write while it is frozen for
    /// the first, so by the time another arrives the beam has been walked to the
    /// end of the transfer and the number is back to 0. A fill and a copy report 0
    /// and leave the 68000 running, which is what genvdp.txt means by the busy bit
    /// being "only useful for fills and copies".
    ///
    /// Taking the fields apart rather than calling through `self` is the point of
    /// this function being a function: `run_dma` holds `&mut self.vdp` for the
    /// whole transfer, and [`VdpDmaSource`] holds `rom`, `ram` and `mapper` across
    /// it, which only separate field borrows allow at the same time.
    fn vdp_control_write(&mut self, value: u16) {
        let Bus {
            vdp,
            rom,
            ram,
            mapper,
            svp,
            prefetched_word,
            pending_68k_hold,
            ..
        } = self;
        let mut source = VdpDmaSource {
            rom,
            ram,
            mapper,
            svp: svp.as_ref(),
            prefetched_word: *prefetched_word,
        };
        vdp.write_control(value);
        vdp.run_dma(&mut source);
        // The freeze the transfer just started, if it started one (#101 made the
        // VDP report it, M3-16 #77 is the bus spending it). A hold and not an
        // additive wait, and maxed rather than summed: the figure is the transfer's
        // end counted from the beam, so a control write that arrives while a transfer
        // is running reports the same instant again, and summing it is what #489 found
        // running the chip clocks away from the raster.
        let held = vdp.dma_68k_stall_mclk();
        *pending_68k_hold = (*pending_68k_hold).max(held);
    }

    /// One byte to the PSG, from the VDP's PSG port at `$C00011`.
    ///
    /// Into [`Audio::psg_write`], which catches the chip up to the current master
    /// clock first: the Z80's own `0x7F11` and the 68k's `0xA07F11` window have
    /// been writing the same PSG since M3-07 (#67) and M3-08 (#68), and this port
    /// is a third spelling of the same wire rather than a second PSG.
    fn psg_write(&mut self, value: u8) {
        self.audio.psg_write(value);
    }

    /// Z80 bus byte read: one of the sound side's devices, or open bus if the 68k does
    /// not hold the Z80's bus. See [`Bus::z80_bus_granted`] for the gate and
    /// [`SoundSide`] for what each address is.
    /// The wait state of [`Z80_AREA_WAIT_MCLK`] is charged before the grant is
    /// asked, because the 68000 has spent the bus cycle either way: the access is
    /// slow because the device on the other end is 8-bit, not because the sound
    /// CPU happened to be holding it.
    ///
    /// The PSG lane is behind the TMSS gate as well as the bus gate: `0xA07F11` is
    /// the port the 68k also owns at `0xC00011`, so [`Bus::vdp_access_denied`]
    /// answers for it here and a locked read freezes the 68k like a locked write
    /// (M3-10 #70, #262). Reads of the lane are [`OPEN_BUS`] either way - the PSG
    /// is write-only - so the freeze is the only difference TMSS makes to a read.
    fn read_z80_byte(&mut self, addr: u32) -> u8 {
        self.stall_68k(Z80_AREA_WAIT_MCLK);
        if !self.z80_bus_granted() {
            // Not the 68k's bus to read, and no device on it is asked on the
            // 68k's behalf: the same answer Genesis Plus GX gives an access to
            // an area the running Z80 owns (`z80_read_byte` answers its
            // bus-held states with `m68k_read_bus_8`), which is the float.
            return self.open_bus_byte(addr);
        }
        match SoundSide::decode(addr) {
            SoundSide::Ram => self.z80.ram[z80_bus::ram_index(addr as u16)],
            // A read of the YM2612 is its status byte whichever port was used: the
            // chip has exactly one readable register, so the Z80's own view answers
            // the same way for all four.
            SoundSide::Ym => self.audio.ym_read_status(),
            // The bank register has no read handler in any reference, and the PSG is
            // write-only. The PSG arm above is the TMSS gate: same port, same answer
            // as 0xC00011 - see [`Bus::vdp_access_denied`]. What nothing drives here
            // answers the float, like every undriven read on this bus.
            SoundSide::Psg if self.vdp_access_denied() => self.open_bus_byte(addr),
            SoundSide::Bank | SoundSide::Psg | SoundSide::Nothing => self.open_bus_byte(addr),
        }
    }

    /// Z80 bus word read: the byte at `addr`, in both halves of the word.
    ///
    /// A 16 bit access to the sound side is one byte access, which is what both
    /// references that model it say: Genesis Plus GX's `z80_read_word` reads the byte
    /// once and mirrors it (`data | (data << 8)`), and MAME's `megadriv_68k_read_z80_ram`
    /// answers a word from the one Z80 byte the address names. plutiedev "Using the
    /// Z80" is blunt about it: "you must use byte accesses when touching Z80 RAM, word
    /// accesses won't work". So the low half of every word read here is a copy, not
    /// another byte of RAM, and [`Bus::write_z80_word`] drops the other half of a write
    /// for the same reason.
    fn read_z80_word(&mut self, addr: u32) -> u16 {
        z80_word(self.read_z80_byte(addr))
    }

    /// Z80 bus byte write: the sound side's device, dropped entirely unless the 68k
    /// holds the Z80's bus. See [`Bus::z80_bus_granted`] and [`SoundSide`], and
    /// [`Bus::read_z80_byte`] for the wait state charged before the grant either
    /// way. The PSG lane is behind the TMSS gate on top of the bus gate, as at its
    /// VDP-area spelling 0xC00011 - see [`Bus::vdp_access_denied`] for the sources
    /// (M3-10 #70, #262).
    fn write_z80_byte(&mut self, addr: u32, value: u8) {
        self.stall_68k(Z80_AREA_WAIT_MCLK);
        if !self.z80_bus_granted() {
            return;
        }
        match SoundSide::decode(addr) {
            SoundSide::Ram => self.z80.ram[z80_bus::ram_index(addr as u16)] = value,
            SoundSide::Ym => self.audio.ym_write(z80_bus::ym_port(addr as u16), value),
            SoundSide::Bank => self.z80.shift_in(value),
            SoundSide::Psg if self.vdp_access_denied() => {}
            SoundSide::Psg => self.audio.psg_write(value),
            SoundSide::Nothing => {}
        }
    }

    /// Z80 bus word write: the high byte, to the address the access names; the low half
    /// of the word is ignored. See [`Bus::read_z80_word`] for where that comes from -
    /// Genesis Plus GX's `z80_write_word` is `z80_write_byte(address, data >> 8)` and
    /// MAME's write handler says "for WORD access only the MSB is used, LSB is
    /// ignored".
    ///
    /// It is one rule for the whole area, RAM, YM2612, bank register and PSG alike,
    /// which is what Genesis Plus GX does by routing every word access through the byte
    /// handler; BlastEm's `io_write_w` for the area is the same `value >> 8`.
    fn write_z80_word(&mut self, addr: u32, value: u16) {
        self.write_z80_byte(addr, (value >> 8) as u8);
    }

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
            self.io
                .read(addr)
                .unwrap_or_else(|| self.open_bus_byte(addr))
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
    /// `value = value | (value << 8)`. Where BlastEm has no register at all its
    /// handler falls through to `get_open_bus_value()` - the float word whole,
    /// which is what this function answers a gap with ([`Bus::open_bus_word`],
    /// #508).
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
        if is_version_register(addr) {
            return io_word(self.version);
        }
        match self.io.read(addr) {
            Some(byte) => io_word(byte),
            // A gap in the register block is an undriven word read, which is
            // the float word whole (BlastEm's `io_read_w` falls through to
            // `get_open_bus_value()` where it has no register, #508).
            None => self.open_bus_word(),
        }
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

    /// Z80 bus control byte read: the bus request register, and nothing else.
    ///
    /// Bit 0 answers 0 only while the 68k holds the bus ([`Bus::z80_bus_granted`]:
    /// requested *and* out of reset) and 1 while the Z80 runs, which is what
    /// plutiedev "Using the Z80" polls (`btst #0, (Z80BusReq)`) between requesting
    /// the bus and touching the sound side. With the reset line asserted the answer
    /// is "running" even though the request is on the line: MAME's
    /// `megadriv_68k_check_z80_bus` and Genesis Plus GX both report the Z80 running
    /// while reset is held, and `!` of the grant gives exactly that (#260). The
    /// reset register has no read handler in any of the three references, so its
    /// read is float like every undriven read, and so are the seven bits of the
    /// bus request byte its flag does not drive.
    fn read_z80_ctrl_byte(&mut self, addr: u32) -> u8 {
        match Z80Ctrl::decode(addr) {
            Some(Z80Ctrl::Busreq) => busreq_byte(!self.z80_bus_granted(), self.open_bus_byte(addr)),
            Some(Z80Ctrl::Reset) | None => self.open_bus_byte(addr),
        }
    }

    /// Z80 bus control word read: the flag in bit 8 of the word, and the
    /// 68000's prefetched instruction word in the other fifteen bits.
    ///
    /// Bit 8 is the bit the byte register drives on a 16 bit access: Genesis Plus GX's
    /// `ctrl_io_read_word` clears or sets `0x0100` in an otherwise floating word and
    /// MAME's `megadriv_68k_check_z80_bus` does `nextvalue | 0x0100`. The rest of the
    /// word is the "unused bits return prefetched bus data" case - which both
    /// references answer with the queue, and which [`busreq_word`] is where that is
    /// argued; Time Killers reads both halves of it (#479).
    fn read_z80_ctrl_word(&mut self, addr: u32) -> u16 {
        match Z80Ctrl::decode(addr) {
            Some(Z80Ctrl::Busreq) => busreq_word(!self.z80_bus_granted(), self.prefetched_word),
            Some(Z80Ctrl::Reset) | None => self.open_bus_word(),
        }
    }

    /// Z80 bus control byte write: bit 0 of the byte written, at either address of the
    /// register pair - the decode drops A0, so `0xA11101` moves the same bus request
    /// flag and is not a second one.
    ///
    /// BlastEm takes `value & 1` from anywhere in the 0x100 block and MAME reads bit 0
    /// of whichever lane arrives; Genesis Plus GX only accepts the even lane. Every
    /// documented use is the word write at `0xA11100` (plutiedev "Using the Z80"), where
    /// all three agree.
    fn write_z80_ctrl_byte(&mut self, addr: u32, value: u8) {
        match Z80Ctrl::decode(addr) {
            Some(Z80Ctrl::Busreq) => self.z80_busreq = value & 1 != 0,
            // The reset line is inverted: 0 asserts, 1 releases.
            Some(Z80Ctrl::Reset) => self.set_z80_reset(value & 1 == 0),
            None => {}
        }
    }

    /// Z80 bus control word write: bit 8 of `value`, which is bit 0 of the byte at the
    /// even address of the pair (Genesis Plus GX's `ctrl_io_write_word` is
    /// `(data >> 8) & 1`, MAME's is `data & 0x0100`, and BlastEm's `io_write_w` hands
    /// the byte handler `value >> 8`).
    fn write_z80_ctrl_word(&mut self, addr: u32, value: u16) {
        self.write_z80_ctrl_byte(addr, (value >> 8) as u8);
    }

    /// Cartridge slot extra byte read: nothing in the block is readable.
    ///
    /// The save RAM control at [`SRAM_CONTROL`] is a write-only register - MAME
    /// installs a write handler for the pair and no read handler - and a cart with
    /// no save RAM has no register to read either. Genesis Plus GX's cart
    /// registers do read back; no game has ever needed this one, which is why
    /// `stub_regions_read_open_bus_until_their_ticket` can keep the whole area at
    /// [`OPEN_BUS`] whatever lives here.
    ///
    /// What did turn out to live here, and does answer, is the Radica board's
    /// game-select latch below: a read is how that board is banked (#492).
    fn read_cart_extra_byte(&mut self, addr: u32) -> u8 {
        if self.radica_reads_the_block() {
            self.mapper.set_radica_bank(addr);
            return 0x00;
        }
        self.open_bus_byte(addr)
    }

    /// Cartridge slot extra word read: the same latch on the same lane, and
    /// otherwise open bus, as [`Bus::read_cart_extra_byte`]. MAME's `read_a13`
    /// answers a word of 0 for the whole block, which is what the boot stubs'
    /// `MOVE.W $A130xx,D0` of #492 reads and discards.
    fn read_cart_extra_word(&mut self, addr: u32) -> u16 {
        if self.radica_reads_the_block() {
            self.mapper.set_radica_bank(addr);
            return 0x0000;
        }
        self.open_bus_word()
    }

    /// Is the cart of [`Self::mapper`] a Radica board, whose `$A13000` block is
    /// the game-select latch? Everything else that could answer the block - the
    /// save RAM control and the Sega mapper's registers - lives in it too, and
    /// a cart with any of those is a cart [`Mapper::detect`] did not name
    /// Radica.
    ///
    /// The latch itself is not a value but an event: MAME's `megadriv_rad.cpp`
    /// `read_a13` sets the bank from the address the program read and answers
    /// 0. The answer of 0 is MAME's: no source says what the board drives the
    /// bus with, and the boot stubs store the read into a D0 nothing else
    /// reads, so 0 is what the one reference that models the board answers.
    fn radica_reads_the_block(&self) -> bool {
        matches!(self.mapper, Mapper::Radica { .. })
    }

    /// Cartridge slot extra byte write: the save RAM control register, for a cart
    /// that has save RAM for it to control, and the Sega mapper's bank registers,
    /// for a cart that has a mapper.
    ///
    /// The two decodes are independent, and nothing gates the block: no unlock
    /// byte makes either register answer. Every reference that decodes it writes
    /// straight through, and plutiedev "Saving progress with SRAM" requires it of
    /// `0xA130F1` - see "Nothing unlocks these registers" in
    /// [`crate::cart::mapper`] for what each one does. A cart with neither has
    /// nothing here, and the write is dropped like any other to the block.
    fn write_cart_extra_byte(&mut self, addr: u32, value: u8) {
        if (addr & EVEN_LANE) == SRAM_CONTROL {
            if let Some(save) = self.sram.as_mut() {
                save.write_control(value);
            }
        } else if let Some(slot) = mapper::bank_slot(addr) {
            self.mapper.set_bank(slot, value);
        }
    }

    /// Cartridge slot extra word write: the low byte of `value`, which is the byte
    /// at `addr + 1` - the odd lane of the register pair, where every register of
    /// this block lives, as [`Bus::write_io_word`] takes for its own.
    ///
    /// That is one rule for the whole block: `0xA130F0` as a word is the save RAM
    /// control at `0xA130F1`, and `0xA130F2` as a word is the bank register at
    /// `0xA130F3`. An odd `addr` is an address error the 68k never completes, so
    /// the pair the access names is all that is left to decode.
    fn write_cart_extra_word(&mut self, addr: u32, value: u16) {
        self.write_cart_extra_byte(addr + 1, value as u8);
    }

    /// TMSS port byte read: nothing here is readable.
    ///
    /// TODO(M3-16 #77): reading [`TMSS_SECURITY`] back returns the bytes written to
    /// it (BlastEm answers from its `tmss_lock` pair, Genesis Plus GX from its
    /// `tmss[4]` image) and reading [`TMSS_ROM_SELECT`] back returns its bit 0
    /// (Genesis Plus GX's `gen_bankswitch_r`). Neither is asked for by a game - the
    /// security register exists for the 68k to write, and the TMSS boot ROM that
    /// would read the select bit does not ship. The fixed `0xFF` placeholder stays
    /// here rather than the float on purpose: this is a register the I/O controller
    /// answers, not an undriven bus, so #508's model does not reach it - its
    /// read-back is register work, and it stays on #77's list.
    fn read_tmss_byte(&mut self, _addr: u32) -> u8 {
        OPEN_BUS
    }

    /// TMSS port word read. Nothing is readable; see [`Bus::read_tmss_byte`].
    fn read_tmss_word(&mut self, _addr: u32) -> u16 {
        OPEN_BUS_WORD
    }

    /// TMSS port byte write: one more byte of the security string, or the ROM select
    /// bit.
    ///
    /// `0xA14101` bit 0 says whether the cartridge or the TMSS boot ROM is mapped at
    /// 0x000000 (segaretro "TradeMark Security System"; Genesis Plus GX's
    /// `gen_bankswitch_w`). This emulator ships no boot ROM, so **the cart stays
    /// mapped whatever this bit says** and writing it only stores it, which is what
    /// lets a program that has already written `SEGA` carry straight on. Without TMSS
    /// neither register is there, and the write is dropped.
    fn write_tmss_byte(&mut self, addr: u32, value: u8) {
        if (addr & TMSS_SECURITY_MASK) == TMSS_SECURITY {
            self.feed_tmss(value);
        } else if (addr & EVEN_LANE) == TMSS_ROM_SELECT {
            // The pair, not just 0xA14101: like the I/O registers these decode
            // without A0, which is how BlastEm takes them too (`location == 0xA14100
            // || location == 0xA14101`).
            if self.tmss_enabled() {
                self.tmss_cart_selected = value & 1 != 0;
            }
        }
    }

    /// TMSS port word write: both halves of the word are data, high byte first.
    ///
    /// Unlike every other word write on this bus, which takes one of its two bytes,
    /// this one is a 32 bit register arriving as two 16 bit writes: the security
    /// register is a longword and `move.l #"SEGA", (A14000).l` is the documented way
    /// to write it (segaretro "TradeMark Security System"). The core issues the high
    /// word first, so the bytes reach [`Bus::feed_tmss`] in `SEGA` order.
    ///
    /// A 16 bit write to the [`TMSS_ROM_SELECT`] pair goes through both lanes too, and
    /// the one that lands on 0xA14101 - the low byte, the lane the register is
    /// documented at - is the one that ends up stored, the same rule as
    /// [`Bus::write_io_word`].
    fn write_tmss_word(&mut self, addr: u32, value: u16) {
        self.write_tmss_byte(addr, (value >> 8) as u8);
        self.write_tmss_byte(addr + 1, value as u8);
    }

    /// SVP register page byte read, [`Region::Svp`]: the lane of the one
    /// register the address names ([`Svp::read_register_byte`] has the
    /// per-lane decode and the PM0 read side effect), or [`OPEN_BUS`] for the
    /// undecoded lanes - and for the whole page on a cart without the board,
    /// exactly what [`Region::Unmapped`] answered before it was decoded.
    fn read_svp_byte(&mut self, addr: u32) -> u8 {
        match self.svp.as_mut() {
            // `None` is the undecoded lanes, which float like every other
            // undriven read on this bus - the value [`Svp::read_register_byte`]
            // handed back as a fixed `0xFF` before #508.
            Some(svp) => svp
                .read_register_byte(addr)
                .unwrap_or_else(|| self.open_bus_byte(addr)),
            None => self.open_bus_byte(addr),
        }
    }

    /// SVP register page word read. The registers are 16-bit, so a word read
    /// is the register whole rather than two byte lanes - the rule the device
    /// regions of this dispatch follow.
    fn read_svp_word(&mut self, addr: u32) -> u16 {
        match self.svp.as_mut() {
            Some(svp) => svp
                .read_register_word(addr)
                .unwrap_or_else(|| self.open_bus_word()),
            None => self.open_bus_word(),
        }
    }

    /// SVP register page byte write: dropped, whatever the cart. The
    /// registers are 16-bit and the reference drops every byte write to the
    /// page (`m68k_unused_8_w` on its `case 0x50` write path).
    fn write_svp_byte(&mut self, addr: u32, value: u8) {
        if let Some(svp) = self.svp.as_mut() {
            svp.write_register_byte(addr, value);
        }
    }

    /// SVP register page word write: the command register takes it on a cart
    /// that has the board, and everything else is dropped like the
    /// reference's `m68k_unused_16_w`.
    fn write_svp_word(&mut self, addr: u32, value: u16) {
        if let Some(svp) = self.svp.as_mut() {
            svp.write_register_word(addr, value);
        }
    }

    /// Clock the SVP's DSP alongside the 68000: one SSP cycle per 68000
    /// cycle clocked (the ratio the references tie their SVP loops to, and
    /// within svpdoc section 4's "10-12M instructions/sec (or possibly less)
    /// is sufficient" against the 68k's 7.67 MHz). Nothing here on a cart
    /// without the board.
    pub(crate) fn svp_run_dsp(&mut self, m68k_cycles: u32) {
        if m68k_cycles == 0 {
            return;
        }
        let Bus { svp, rom, .. } = self;
        if let Some(svp) = svp {
            svp.run_dsp(rom, m68k_cycles);
        }
    }

    /// Cartridge slot extra byte read: a Radica board's game-select latch, on
    /// the cart that carries the board, and open bus otherwise.
    ///
    /// The latch is the one readable thing in the block, and it is not a value
    /// Undecoded byte read: the float, like every undriven read (#508).
    fn read_unmapped_byte(&mut self, addr: u32) -> u8 {
        self.open_bus_byte(addr)
    }

    /// Undecoded word read: the float, like every undriven read (#508).
    fn read_unmapped_word(&mut self, _addr: u32) -> u16 {
        self.open_bus_word()
    }

    /// Undecoded byte write: nothing but a Realtec cart's paging registers.
    ///
    /// The expansion slot at `0x400000-0x7FFFFF` is otherwise nothing's, but a
    /// Realtec board keeps the three registers that page its image out there rather
    /// than in the `0xA13000` block of [`crate::cart::mapper`] - see
    /// [`mapper::realtec_register`] for the decode, and
    /// [`Mapper::set_realtec_register`] for what a write to one does. A cart that is
    /// not a Realtec cart drops the write, exactly as [`Bus::write_cart_extra_byte`]
    /// drops a bank write on a cart with no Sega mapper.
    fn write_unmapped_byte(&mut self, addr: u32, value: u8) {
        if let Some(reg) = mapper::realtec_register(addr) {
            self.mapper.set_realtec_register(reg, value);
        }
    }

    /// Undecoded word write: the high byte, which is the byte at the even lane.
    ///
    /// The opposite way round from [`Bus::write_cart_extra_word`], and deliberately:
    /// the Realtec registers live on the even lane and take a word write from its own
    /// byte (BlastEm's `realtec_write_w` hands `realtec_write_b` the value shifted
    /// right by 8, and `realtec_write_b` then rejects odd addresses outright), while
    /// the `0xA13000` block keeps its registers on the odd lane. Reusing one decode
    /// for the other would take every Realtec write one byte late.
    fn write_unmapped_word(&mut self, addr: u32, value: u16) {
        self.write_unmapped_byte(addr, (value >> 8) as u8);
    }
}

/// Is `addr`, masked and already decoded as [`Region::Io`], one of the two bytes
/// of the hardware version register at [`IO_VERSION`]?
fn is_version_register(addr: u32) -> bool {
    (addr & EVEN_LANE) == IO_VERSION
}

/// A decoded part of [`Region::Z80`]: which sound-side device a 68k access lands on.
///
/// The 68k has no separate map for the sound side; it drives the Z80's address lines,
/// so this is [`crate::z80_bus`]'s own map applied to the low 15 bits of the address
/// ([`Z80_AREA_MASK`]) and the devices are the same objects, not copies of them: one
/// 8 KiB RAM, one bank register, one YM2612 log and one PSG log for both CPUs. The
/// boundaries are worth reading next to the Z80 side's:
///
/// - BlastEm's `io_write` masks the address with `0x7FFF` and then takes `0x0000`-
///   `0x3FFF` for the RAM (`& 0x1FFF`, so the mirror at `0x2000` is the Z80's mirror),
///   `0x4000`-`0x5FFF` for the YM2612 and `0x6000` for the bank register;
/// - Genesis Plus GX's `z80_read_byte`/`z80_write_byte` decode the same area with
///   `(address >> 13) & 3` and `(address >> 8) & 0x7F`;
/// - MAME maps `0xa00000-0xa01fff` (plus the `0xa02000` write mirror), `0xa04000`-
///   `0xa04003` and `0xa06000-0xa06001`.
enum SoundSide {
    /// The 8 KiB of sound RAM, mirrored at `0x2000` inside the window.
    Ram,
    /// The YM2612: four ports at `addr & 3`, into [`Bus::audio`].
    Ym,
    /// The bank shift register: `0x6000-0x60FF`, in [`Z80Side::bank`].
    ///
    /// The whole block, because that is what the Z80 side accepts (MAME maps
    /// `0xa06000-0xa06001` for the 68k, Genesis Plus GX the whole `0x60` page) and
    /// there is one register to accept it in.
    Bank,
    /// The PSG port: `0xA07F11` and its `0x13`/`0x15`/`0x17` mirrors, which are the
    /// port the Z80 sees at `0x7F11` and the 68k sees at `0xC00011`. One port, one chip,
    /// three spellings - see [`z80_bus::is_psg_port`]. The TMSS gate covers all three
    /// spellings, so a 68k write through this window is refused and freezes while the
    /// security string is missing, like its `0xC00011` spelling (M3-10 #70, #262).
    Psg,
    /// Anything else in the area: the gaps of the Z80 map, and the rest of the VDP
    /// window at `0xA07F00-0xA07FFF`.
    ///
    /// No reference drives those from the 68k - Genesis Plus GX's `z80_write_byte` treats
    /// the VDP lanes as a bus lockup and MAME and BlastEm leave them unmapped - because
    /// the 68k has the VDP on its own bus at `0xC00000` and would not go through the
    /// sound side to reach it. Reads are [`OPEN_BUS`] and writes are dropped; M3-09
    /// (#69) does not change this, it puts a VDP behind `Region::Vdp`.
    Nothing,
}

impl SoundSide {
    /// Decode `addr`, masked and already decoded as [`Region::Z80`].
    fn decode(addr: u32) -> Self {
        match addr & Z80_AREA_MASK {
            0x0000..=SND_RAM_END => Self::Ram,
            SND_YM_START..=SND_YM_END => Self::Ym,
            SND_BANK_START..=SND_BANK_END => Self::Bank,
            offset @ SND_VDP_START..=SND_VDP_END if z80_bus::is_psg_port(offset as u16) => {
                Self::Psg
            }
            _ => Self::Nothing,
        }
    }
}

/// One of the two Z80 control registers of [`Region::Z80Ctrl`].
enum Z80Ctrl {
    /// `0xA11100`: ask the Z80 for its bus, and read back who has it.
    Busreq,
    /// `0xA11200`: hold the Z80 in reset, or let it go. The YM2612 hangs off the same
    /// line, so this resets it too.
    Reset,
}

impl Z80Ctrl {
    /// Decode `addr`, masked and already decoded as [`Region::Z80Ctrl`]. Each register
    /// owns a 0x100 block and neither looks at A0-A7, which is why there is one flag per
    /// block and not one per address; see [`Z80_CTRL_MASK`] for the sources.
    fn decode(addr: u32) -> Option<Self> {
        match addr & Z80_CTRL_MASK {
            Z80_BUSREQ => Some(Self::Busreq),
            Z80_RESET_REG => Some(Self::Reset),
            _ => None,
        }
    }
}

/// The one word rule of [`Region::Z80`]: a 16 bit access is a single byte access
/// answered in both halves. See [`Bus::read_z80_word`] for the sources.
fn z80_word(byte: u8) -> u16 {
    u16::from_be_bytes([byte, byte])
}

/// What a byte read of the bus request register reports: `running` in bit 0 and the
/// float's lane in the other seven bits.
///
/// The unused bits are documented as returning whatever the previous bus transfer left
/// there, and the emulators keep them nonzero on purpose: MAME's comment on this
/// register is that ddragon, beast, superoff and Time Killers poll it expecting a zero
/// that never comes, and Genesis Plus GX says the same ("Unused bits return prefetched
/// bus data (Time Killers)"). The float is that answer: the lane of the prefetched
/// word ([`Bus::open_bus_byte`]), ones from the pull-up before the first fetch, and
/// never zero while the queue word has any bit set - which is the property those
/// games actually need.
///
/// The word read keeps the same rule for its own flag bit; see [`busreq_word`] for why
/// the low byte of that one is not held at a fixed 1 too.
fn busreq_byte(running: bool, open_bus: u8) -> u8 {
    (open_bus & !1) | u8::from(running)
}

/// What a word read of the bus request register reports: `running` in bit 8 - the bit
/// the byte register drives on a 16 bit access - and the 68000's prefetched
/// instruction word in every other bit, [`OPEN_BUS`] ones until the CPU publishes its
/// first fetch. See [`Bus::read_z80_ctrl_word`].
///
/// Bit 0 of a word read is unclaimed territory. `0xA11100` is a byte register driven on
/// D8 (plutiedev "Using the Z80"), so on a word read bits 0-7 are exactly the "unused
/// bits return prefetched bus data" case plutiedev describes - and both references that
/// model an answer model it as the queue, not as anything the register drives:
///
/// - Genesis Plus GX's `ctrl_io_read_word`, case 0x11, returns the word at the 68k's
///   PC with the flag lane forced - `& 0xFEFF` granted, `| 0x0100` running - under the
///   comment `/* Unused bits return prefetched bus data (Time Killers) */`. The comment
///   names this cart: the read is the case the modelling exists for.
/// - BlastEm's `io_read_w` keeps the flag in bit 8 of its byte path and fills the low
///   byte from `get_open_bus_value() & 0xFF`, the word at the end of the current
///   instruction (`read_dma_value(last_prefetch_address / 2)`).
///
/// Which word that is decides Time Killers' boot (#479). Bit 0 of the answer is bit 0
/// of the opcode that follows the poll's `MOVE.W`, i.e. of the `BTST` itself: the boot
/// poll `move.w $A11100,d0` / `btst #0,d0` / `bne.s` reads `$0810` (bit 0 clear) and
/// exits; the game loop's `move.w $A11100,d1` / `btst #0,d1` / `beq.s` reads `$0811`
/// (bit 0 set) and exits too. Both polls are effectively single-iteration on hardware,
/// which is also what the blastem oracle says the machine does: display on at frames
/// 100 and 200. A flag mirrored into bit 0 - what this read answered between #478 and
/// this change - cannot serve both polls: it reads 0 for exactly as long as the cart
/// holds the request, and the `beq.s` waits for a 1 that never comes, so the cart
/// parks there from frame ~126 with the screen off. #478's own doc comment called the
/// mirror "a cheap approximation, not a claim about what drives D0"; this is the
/// modelled prefetch latch it said would replace it.
fn busreq_word(running: bool, prefetched: u16) -> u16 {
    // GPGX's two halves: the flag lane forced, everything else the queue's word.
    (prefetched & 0xFEFF) | (u16::from(running) << 8)
}

/// The lane of `word` that a byte read at `addr` answers with: the even byte of
/// an access is the high byte of the word and the odd byte the low one, the
/// whole 68k data bus through this file (genvdp.txt section 7 "8-bit port
/// access"; [`Bus::read_vdp_byte`] applies the same rule to a VDP port's word).
///
/// A free function rather than a [`Bus`] method because one caller of it is
/// [`VdpDmaSource`], which cannot hold the bus (the borrow the struct docs
/// explain); it carries [`Bus::prefetched_word`] itself.
pub(crate) fn open_bus_lane(word: u16, addr: u32) -> u8 {
    if addr & 1 == 0 {
        (word >> 8) as u8
    } else {
        word as u8
    }
}

/// The one word rule of [`Region::Io`]: a register answers a 16 bit access to its
/// pair by repeating its byte into both halves. See [`Bus::read_io_word`] for the
/// sources and for the alternative that was not taken.
fn io_word(byte: u8) -> u16 {
    u16::from_be_bytes([byte, byte])
}

/// The 68000's view of the bus. `int_ack` is the one method that is not a memory
/// access, and since M3-14 (#75) it is the acknowledge the VDP waits for; the module
/// docs say what both halves of it are for.
impl m68k::Bus for Bus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.read_rom_byte(addr),
            Region::Ram => self.read_ram_byte(addr),
            // The TMSS gate: asking costs the access on a locked console. It lives in
            // the dispatch rather than in `read_vdp_byte` so that M3-09 (#69) cannot
            // fill the stub in and lose it - see [`Bus::vdp_access_denied`].
            Region::Vdp if self.vdp_access_denied() => self.open_bus_byte(addr),
            Region::Vdp if self.vdp_hang(addr) => self.open_bus_byte(addr),
            Region::Vdp => {
                self.vdp_accesses_68k += 1;
                self.read_vdp_byte(addr)
            }
            Region::Z80 => self.read_z80_byte(addr),
            Region::Io => self.read_io_byte(addr),
            Region::Z80Ctrl => self.read_z80_ctrl_byte(addr),
            Region::CartExtra => self.read_cart_extra_byte(addr),
            Region::Tmss => self.read_tmss_byte(addr),
            Region::Svp => self.read_svp_byte(addr),
            Region::Unmapped => self.read_unmapped_byte(addr),
        }
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.read_rom_word(addr),
            Region::Ram => self.read_ram_word(addr),
            // TMSS, and see the note on [`Bus::read_byte`].
            Region::Vdp if self.vdp_access_denied() => self.open_bus_word(),
            Region::Vdp if self.vdp_hang(addr) => self.open_bus_word(),
            Region::Vdp => {
                self.vdp_accesses_68k += 1;
                self.read_vdp_word(addr)
            }
            Region::Z80 => self.read_z80_word(addr),
            Region::Io => self.read_io_word(addr),
            Region::Z80Ctrl => self.read_z80_ctrl_word(addr),
            Region::CartExtra => self.read_cart_extra_word(addr),
            Region::Tmss => self.read_tmss_word(addr),
            Region::Svp => self.read_svp_word(addr),
            Region::Unmapped => self.read_unmapped_word(addr),
        }
    }

    fn write_byte(&mut self, addr: u32, value: u8) {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.write_rom_byte(addr, value),
            Region::Ram => self.write_ram_byte(addr, value),
            // TMSS, and see the note on [`Bus::read_byte`].
            Region::Vdp if self.vdp_access_denied() => {}
            Region::Vdp if self.vdp_hang(addr) => {}
            Region::Vdp => {
                self.vdp_accesses_68k += 1;
                self.write_vdp_byte(addr, value);
            }
            Region::Z80 => self.write_z80_byte(addr, value),
            Region::Io => self.write_io_byte(addr, value),
            Region::Z80Ctrl => self.write_z80_ctrl_byte(addr, value),
            Region::CartExtra => self.write_cart_extra_byte(addr, value),
            Region::Tmss => self.write_tmss_byte(addr, value),
            Region::Svp => self.write_svp_byte(addr, value),
            Region::Unmapped => self.write_unmapped_byte(addr, value),
        }
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        let addr = addr & ADDR_MASK;
        match Region::decode(addr) {
            Region::Rom => self.write_rom_word(addr, value),
            Region::Ram => self.write_ram_word(addr, value),
            // TMSS, and see the note on [`Bus::read_byte`].
            Region::Vdp if self.vdp_access_denied() => {}
            Region::Vdp if self.vdp_hang(addr) => {}
            Region::Vdp => {
                self.vdp_accesses_68k += 1;
                self.write_vdp_word(addr, value);
            }
            Region::Z80 => self.write_z80_word(addr, value),
            Region::Io => self.write_io_word(addr, value),
            Region::Z80Ctrl => self.write_z80_ctrl_word(addr, value),
            Region::CartExtra => self.write_cart_extra_word(addr, value),
            Region::Tmss => self.write_tmss_word(addr, value),
            Region::Svp => self.write_svp_word(addr, value),
            Region::Unmapped => self.write_unmapped_word(addr, value),
        }
    }

    /// Interrupt acknowledge: the 68000 has taken a level, so the VDP stops offering
    /// it, and the answer is `None` because a Genesis always autovectors.
    ///
    /// The acknowledge is not optional on the VDP's side. genvdp.txt section 6 says of
    /// status bit 7 that "Reading the control port does not clear this bit", so
    /// nothing the 68k does to read the chip takes the interrupt away, and
    /// [`Vdp::ack_irq`] is the only thing that does - which is what stops a level 6
    /// being offered again the moment the handler enables interrupts (M3-14, #75).
    ///
    /// Levels the VDP has no business clearing are ignored by [`Vdp::ack_irq`] itself,
    /// which is why level 2 - the TH pin of the I/O registers - can be passed straight
    /// through here too.
    fn int_ack(&mut self, level: u8) -> Option<u8> {
        self.vdp.ack_irq(level);
        None
    }

    fn note_prefetched_word(&mut self, word: u16) {
        self.prefetched_word = word;
    }

    /// The `RESET` instruction restarts the console, so the I/O chip's registers
    /// go back to their power-on values. Work RAM does not, and cannot: this is the
    /// console's `RESET`, not a reinitialisation of everything the emulator holds.
    /// TMSS is restarted too - the security string is forgotten and the VDP locked
    /// again, while any freeze goes with the CPU that `RESET` has just restarted.
    ///
    /// Neither do the Z80's two control lines: they go back the way [`Bus::new`] leaves
    /// them, under reset and with the bus the Z80's own, taking the grant the 68k was
    /// holding with them. Genesis Plus GX's `gen_reset` does this (`zstate = 0`, on the
    /// line commented "Z80 bus is released & Z80 is reseted", plus `zbank = 0`) and
    /// MAME's `machine_reset` sets `z80_is_reset = 1; z80_has_bus = 1`. The bank register
    /// is cleared here rather than by [`crate::z80_bus`] because this is the reset that
    /// owns the sound side's control state; the sound RAM survives like work RAM does.
    ///
    /// Asserting the reset line is not the edge that restarts the Z80, so no pulse is
    /// raised, but the YM2612 goes into reset with the line - and `gen_reset` calls
    /// `fm_reset` for the console's own reset whether or not the line had to move, which
    /// is why the YM is reset unconditionally here instead of through
    /// [`Bus::set_z80_reset`].
    fn reset_devices(&mut self) {
        self.io.reset();
        // The VDP goes back to the state a chip that has never been talked to is
        // in: its registers, its code and address registers, its write-pending
        // flag, and its memories. `RESET` is the console's own reset and both
        // references that model one reset the VDP with it: Genesis Plus GX's
        // `gen_reset` reaches `VDP_reset`, which zeroes VRAM, CRAM and VSRAM, and
        // MAME's `machine_reset` resets the VDP device with the rest of the
        // machine. Work RAM is the opposite case and stays: what a reset clears is
        // the chip's own state, not everything this bus holds.
        //
        // Rebuilding the chip is what stands in for a reset of the VDP's own,
        // which `crates/vdp` does not have yet: [`Vdp::new`] is exactly the state
        // its docs call "a VDP that has never been talked to". The `pal` it is
        // rebuilt with is the one this bus was built with, because `RESET` does not
        // change the crystal.
        self.vdp = Vdp::new(version_is_pal(self.version));
        // ...except for where that chip's beam is: `Vdp::new` sits it at line 0, a
        // line of active display 40 master clocks deep before anything may be latched
        // for it, and the 68000 this reset is about to restart starts writing its VDP
        // registers at its own first instruction. Parked in VBlank instead, that write
        // is in time for line 0 of the first frame after the reset - which is what
        // `crate::Genesis::new` does with a machine it has just built, and for the
        // same reason (#310).
        self.vdp.start_in_vblank();
        self.z80_busreq = false;
        self.z80_reset = true;
        self.z80_reset_pulse = false;
        self.audio.ym_reset();
        self.z80.bank = 0;
        // TMSS: `RESET` restarts the console, and a console that restarts asks the
        // security question again. Genesis Plus GX's `gen_reset` clears its TMSS
        // register and reinstalls the lockup handlers for exactly this ("clear TMSS
        // register", "VDP access is locked by default"), so a game has to write
        // `SEGA` once more before it may touch the VDP. The cart select bit goes back
        // to the boot ROM with it, and the detector goes back to the start of `SEGA`.
        //
        // The freeze goes the other way. It belongs to the 68k, and `RESET` has just
        // restarted the 68k: a machine that stayed frozen through a reset could not be
        // restarted by one, so clearing the flag is the only thing that makes the
        // instruction mean what it says.
        self.tmss_seek = 0;
        self.tmss_unlocked = false;
        self.locked_up = false;
        self.tmss_cart_selected = false;
        // And so does any wait that had not reached a clock yet. The transfer or
        // held write it was waiting for is gone with the chip, and a stall carried
        // across a `RESET` would freeze a 68000 that `RESET` has just restarted.
        self.pending_68k_stall = 0;
        self.pending_z80_stall = 0;
        self.pending_68k_hold = 0;
        self.pending_z80_hold = 0;
        // The cart slot's `0xA130F1` goes back to 0 with the rest of the register
        // state, but the save itself and its dirty flag do not: `RESET` is not a
        // power cycle, and the battery is still holding those bytes. See
        // [`crate::cart::sram::Sram::reset`].
        if let Some(save) = self.sram.as_mut() {
            save.reset();
        }
        // And the serial EEPROM's transfer in hand is abandoned with it: a reset
        // is the 68k stopping in the middle of walking the two lines, and a chip
        // that kept a half-taken byte would answer the next cart access with it.
        if let Some(chip) = self.eeprom.as_mut() {
            chip.reset();
        }
        // The mapper's bank registers go back to slot `n` on page `n` with them:
        // a cart boots from a reset assuming nothing is switched out from under
        // it, which is why slot 0 is pinned in the first place. Genesis Plus GX
        // asks for the same with `cart.hw.bankshift` on the carts that have the
        // mapper - "cartridge ROM mapping is reinitialized on /VRES".
        self.mapper.reset();
        // And the J-CART register's TH line goes back high with them (Genesis
        // Plus GX's `gamepad_reset`, "default state ... 0x40"). The EEPROM's
        // lines and array are the chip's business and [`Eeprom::reset`] has
        // already seen to them.
        if let Some(cart) = self.jcart.as_mut() {
            cart.reset();
        }
        // And the SVP's DSP goes back to its entry point with ST and the PMC
        // state clear (GPGX's `svp_reset` + `ssp1601_reset`): the DRAM, the
        // IRAM and the handshake latches survive like the save RAM does, but
        // the core itself restarts - a reset is a power-cycle for the cart's
        // processor, whatever the RAMs were doing.
        if let Some(svp) = self.svp.as_mut() {
            svp.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::z80_bus::{Z80View, ZRAM_SIZE};
    use m68k::Bus as _;
    use z80::Z80Bus as _;

    /// An image of `len` bytes with no word of it reading as a plausible
    /// default, so a test cannot pass by accident.
    fn rom_image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// What the version register at 0xA10001 reports for these tests: an export
    /// NTSC machine without TMSS ([`crate::Region::Usa`]). Deliberately not
    /// [`OPEN_BUS`], so a stub read cannot pass on the version register's value.
    const TEST_VERSION: u8 = 0xA0;

    /// Master clocks in one VDP scanline, for the tests that walk the beam with
    /// [`Vdp::advance`] rather than through the scheduler.
    const TEST_VDP_LINE: u32 = 3420;

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

        // 0xFFFFFE is the highest word-aligned address of the window. The odd 0xFFFF
        // is a word too and its second byte wraps, which #488 pins below.
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

    /// A word access at an odd address is its two byte accesses, so the half of it
    /// that falls past the top of the 64 KiB mirror continues at the bottom of the
    /// mirror (#488).
    ///
    /// The access is reachable, and was a host panic: `crates/m68k` writes
    /// exception frames with the unchecked `_raw` accessors precisely because the
    /// stack pointer is what it is (see `crates/m68k/src/address_error.rs`, "the
    /// exception path must never fault-check its own accesses"), so an
    /// address-error frame on a cart whose SP is odd asks the bus for the word at
    /// `0xFFFFFF`. `Star Odyssey (USA) (Super Fighter Team)` does exactly that, and
    /// the old `ram[index..index + 2]` died with `range end index 65537 out of
    /// range for slice of length 65536`.
    #[test]
    fn ram_word_write_straddling_the_top_of_the_mirror_wraps() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        bus.write_word(RAM_END, 0xBEEF);
        assert_eq!(bus.ram[0xFFFF], 0xBE);
        assert_eq!(
            bus.ram[0], 0xEF,
            "the second byte is the byte at 0x1000000, and only the low 16 bits name a byte"
        );
        assert_eq!(bus.read_word(RAM_END), 0xBEEF);

        // The same two bytes through the window's own mirrors.
        assert_eq!(bus.read_byte(0xE0_FFFF), 0xBE);
        assert_eq!(bus.read_byte(0xE1_0000), 0xEF);
    }

    /// The read side of [#488]: `read_ram_word` has the same shape one line above
    /// [`Bus::write_ram_word`], so it walks off the array the same way.
    #[test]
    fn ram_word_read_straddling_the_top_of_the_mirror_wraps() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.ram[0xFFFF] = 0xCA;
        bus.ram[0] = 0xFE;

        assert_eq!(bus.read_word(RAM_END), 0xCAFE);
        assert_eq!(bus.read_word(0xE1_FFFF), 0xCAFE);

        // A word is its two bytes, even across the wrap. The second byte's address,
        // 0x1000000, masks to 0x000000 and so decodes as the cartridge slot; the
        // mirror at 0xE10000 is the same byte inside the work-RAM window.
        assert_eq!(
            bus.read_word(RAM_END),
            (u16::from(bus.read_byte(0xE0_FFFF)) << 8) | u16::from(bus.read_byte(0xE1_0000))
        );
    }

    /// Away from the top of the mirror the same rule puts the two halves of an odd
    /// word write in the two bytes the address names - what `ram[3..5]` did before
    /// #488 as well, pinned so that the wrap above cannot be reached by shifting
    /// every odd write down to the aligned pair instead.
    #[test]
    fn ram_word_access_at_an_odd_address_is_its_two_byte_accesses() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        bus.write_word(RAM_START + 3, 0x1234);
        assert_eq!(&bus.ram[3..5], &[0x12, 0x34]);
        assert_eq!(bus.read_word(RAM_START + 3), 0x1234);
        assert_eq!(bus.read_byte(RAM_START + 3), 0x12);
        assert_eq!(bus.read_byte(RAM_START + 4), 0x34);
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

    /// The answer above the end of the image is filler, not the image repeated:
    /// the decision recorded on [`Bus::read_rom_byte`], pinned at the addresses
    /// `QuackShot Starring Donald Duck (World) (En,Ja) (Rev A)` really asks for.
    #[test]
    fn reads_above_the_image_are_filler_and_not_a_mirror_of_it() {
        // 0x80000 of the 1.25 MB cart, which is that dump exactly. `rom_image`
        // fills by index modulo 251, so no byte of it is 0xFF and no byte repeats
        // at two offsets a mirror would use: a wrap cannot pass this test by
        // accident, and neither can an off-by-one at the top of the image.
        let image = rom_image(0x08_0000);
        let mut bus = Bus::new(image.clone(), TEST_VERSION);

        // The last byte of the cart, and the word that ends on it: both real.
        assert_eq!(bus.read_byte(0x07_FFFF), image[0x07_FFFF]);
        assert_eq!(
            bus.read_word(0x07_FFFE),
            u16::from_be_bytes([image[0x07_FFFE], image[0x07_FFFF]]),
            "a word straddling the end keeps the half that is there",
        );

        // One past it, at the top of the slot, and at the three addresses that
        // modulo mirroring on this length would hand back `image[0]` for: `$80000`
        // and `$100000` are the wrap points, and `$13FFFF` is the end of the sum
        // the cart runs.
        for addr in [
            0x08_0000_u32,
            0x08_0002,
            0x10_0000,
            0x13_FFFF,
            0x3F_FFFE,
            ROM_END,
        ] {
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "byte at {addr:#08x}");
            assert_eq!(
                bus.read_word(addr & !1),
                OPEN_BUS_WORD,
                "word at {addr:#08x}"
            );
        }
    }

    /// The SVP's IRAM window is the game image, read at the same offsets.
    ///
    /// notaz *SVP doc* section 3 "Memory map" gives `0x000000`-`0x0007FF` to the
    /// SVP's program IRAM and `0x000800`-`0x1FFFFF` to the SVP program ROM, read
    /// from the same offsets of the 2 MB game ROM, and names that first mapping as
    /// the one descriptions get wrong ("There were reports that SVP has internal
    /// ROM, but fortunately they were wrong"). Nothing SVP-shaped is needed to
    /// hold it: the window is the cart area, so the [`Region::Rom`] decode above is
    /// exactly what the SVP reads. The document's other two windows - the DRAM at
    /// `0x300000` and the status registers at `0xA15000` - are not satisfied this
    /// way, and are #433.
    #[test]
    fn svp_iram_window_reads_the_game_image_at_the_same_offset() {
        let rom = rom_image(0x200_000);
        let mut bus = Bus::new(rom.clone(), TEST_VERSION);

        for offset in [0x000000_u32, 0x000001, 0x000100, 0x0007FE, 0x0007FF] {
            let at = offset as usize;
            assert_eq!(bus.read_byte(offset), rom[at], "byte at {offset:#08x}");
        }
        for offset in [0x000000_u32, 0x000002, 0x0007FC] {
            let at = offset as usize;
            assert_eq!(
                bus.read_word(offset),
                u16::from_be_bytes([rom[at], rom[at + 1]]),
                "word at {offset:#08x}",
            );
        }
    }

    /// The SVP cart's shared DRAM: a write-then-read-back at both widths
    /// ([`Svp::covers_dram`]), through the [`m68k::Bus`] dispatch. The window
    /// starts at `0x300000` and runs to `0x31FFFF`, 16-bit, big-endian lanes
    /// at byte width.
    #[test]
    fn svp_dram_window_write_then_read_back() {
        let mut bus = Bus::new(svp_cart_rom(), TEST_VERSION);

        bus.write_word(0x30_0000, 0x1234);
        assert_eq!(bus.read_word(0x30_0000), 0x1234, "word round trip");
        assert_eq!(bus.read_byte(0x30_0000), 0x12, "high lane");
        assert_eq!(bus.read_byte(0x30_0001), 0x34, "low lane");

        bus.write_byte(0x31_0001, 0xAB);
        assert_eq!(bus.read_byte(0x31_0001), 0xAB, "odd lane byte write");
        bus.write_byte(0x31_0000, 0xCD);
        assert_eq!(bus.read_word(0x31_0000), 0xCDAB, "both lanes of one word");

        // The last word of the window, and the wrap-free decode: 0x31FFFE is
        // DRAM word 0xFFFF, the last of the array.
        bus.write_word(0x31_FFFE, 0xCAFE);
        assert_eq!(bus.read_word(0x31_FFFE), 0xCAFE);
        assert_eq!(bus.read_byte(0x31_FFFF), 0xFE);
    }

    /// The DRAM window answers the DRAM and not the image underneath it -
    /// which is what it answered before the board was decoded - and the
    /// registers answer the board and not open bus, at exactly the lanes the
    /// decode gives them.
    #[test]
    fn svp_windows_answer_the_board_and_not_the_image_or_open_bus() {
        let rom = svp_cart_rom();
        let mut bus = Bus::new(rom.clone(), TEST_VERSION);

        // The image held 0x00 at 0x300000; the window reads that before any
        // write, and holds the written value after it, where a write onto the
        // image would have been dropped.
        assert_eq!(bus.read_word(0x30_0002), 0x0000, "DRAM words come up clear");
        bus.write_word(0x30_0002, 0x5AA5);
        assert_eq!(bus.read_word(0x30_0002), 0x5AA5);

        // Registers: a word write to the command register, XST reads back at
        // the register and the 0xA15002 mirror, PM0 carries the "68k has
        // written" bit and no answer bit, and nothing in the write dropped.
        bus.write_word(0xA1_5000, 0xBEEF);
        assert_eq!(bus.read_word(0xA1_5000), 0xBEEF);
        assert_eq!(bus.read_word(0xA1_5002), 0xBEEF, "the mirror");
        assert_eq!(
            bus.read_word(0xA1_5004),
            0x0002,
            "PM0 bit 1: the command was taken"
        );
        assert_eq!(bus.read_byte(0xA1_5000), 0xBE, "byte lanes of XST");
        assert_eq!(bus.read_byte(0xA1_5001), 0xEF);
    }

    /// A cart without the board: the register page reads open bus and drops
    /// writes exactly as [`Region::Unmapped`] did, and the DRAM window is
    /// still the image - which is the detection-negative half of the stage.
    /// The image is 4 MiB so the window at $300000 overlaps it, the way an
    /// ordinary big cart's would; a 2 MiB image ends at $1FFFFF and answers
    /// the window open bus above its end, as every cart does (see
    /// [`Bus::read_rom_byte`]'s docs on reading past the end).
    #[test]
    fn a_cart_without_the_board_answers_the_svp_windows_as_before() {
        let rom = rom_image(0x400_000);
        let mut bus = Bus::new(rom.clone(), TEST_VERSION);

        // The page: open bus, reads and writes.
        for addr in [0xA1_5000, 0xA1_5002, 0xA1_5004, 0xA1_5008] {
            assert_eq!(bus.read_word(addr), OPEN_BUS_WORD, "word at {addr:#08x}");
            bus.write_word(addr, 0x2222);
            assert_eq!(bus.read_word(addr), OPEN_BUS_WORD, "write dropped");
            assert_eq!(bus.read_byte(addr), OPEN_BUS, "byte at {addr:#08x}");
        }

        // The window: image bytes at the same offsets, and writes dropped.
        for offset in [0x300_000_u32, 0x300_002, 0x310_000] {
            let at = offset as usize;
            assert_eq!(
                bus.read_word(offset),
                u16::from_be_bytes([rom[at], rom[at + 1]]),
                "image at {offset:#08x}",
            );
        }
        bus.write_word(0x300_000, 0x1234);
        assert_eq!(
            bus.read_word(0x300_000),
            u16::from_be_bytes([rom[0x300_000], rom[0x300_001]]),
            "write dropped onto image"
        );
    }

    /// A VDP transfer sourcing from the DRAM reads the DRAM and not the image:
    /// [`Bus::read_svp_dram_word`] is [`VdpDmaSource`]'s view, and Virtua
    /// Racing moves the DSP's output into VRAM through exactly that source.
    #[test]
    fn svp_dram_answers_a_vdp_dma_source() {
        let mut svp = Svp::new();
        svp.write_dram_word(0x30_0004, 0x7F3E);
        assert_eq!(
            Bus::read_svp_dram_word(Some(&svp), 0x30_0004),
            Some(0x7F3E),
            "inside the window"
        );
        assert_eq!(
            Bus::read_svp_dram_word(Some(&svp), 0x32_0000),
            None,
            "outside the window"
        );
        assert_eq!(
            Bus::read_svp_dram_word(None, 0x30_0004),
            None,
            "no board, no answer: the image arm takes it"
        );
    }

    /// A synthetic SVP cart image: a 2 MiB image whose overseas title names
    /// Virtua Racing and whose 0x810 carries the DSP program probe, the two
    /// halves [`Svp::detect`] asks for. Nothing else is set, which is close
    /// enough to the real carts for a bus test.
    fn svp_cart_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x200_000];
        rom[0x150..0x15D].copy_from_slice(b"Virtua Racing");
        rom[0x810..0x814].copy_from_slice(b"OHMP");
        rom
    }

    #[test]
    fn unmapped_reads_return_open_bus_placeholder() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        for addr in [
            // 0xA15100, not 0xA15000: the SVP's register page below it is
            // [`Region::Svp`] since #433, whose handlers answer open bus for
            // everything they do not decode - the gap itself starts above it.
            0x400000, 0x7FFFFE, 0x800000, 0x9FFFFE, 0xA12000, 0xA15100, 0xBFFFFE,
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
        // the tests below cover. The Z80 area and its two control registers left
        // this list with #68 and are covered by the arbitration tests below, and
        // the VDP data port left it with #69 for the same reason: the VDP is what
        // answers it now, and the port tests at the end of this module cover it.
        for addr in [0xA10020, 0xA130F1, 0xA14000, 0xA14101] {
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

    /// `int_ack` has both halves since M3-14 (#75), and this pins both: the level
    /// goes to [`Vdp::ack_irq`] so the VDP stops offering it, and the answer is
    /// still `None` because a Genesis always autovectors and the bus never gives the
    /// CPU a vector number.
    #[test]
    fn int_ack_clears_the_vdp_and_keeps_autovectoring() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert_eq!(
            bus.int_ack(2),
            None,
            "level 2 is the I/O chip's, not the VDP's"
        );

        // A VINT that has actually happened, so there is something to clear.
        bus.vdp.write_register(1, 0x60);
        bus.vdp.advance(225 * TEST_VDP_LINE);
        assert_eq!(bus.vdp.pending_irq(), Some(6), "the VINT is being offered");
        assert_eq!(
            bus.int_ack(4),
            None,
            "and a level 4 acknowledge is None too"
        );
        assert_eq!(
            bus.vdp.pending_irq(),
            Some(6),
            "which leaves the level 6 the level 4 was not about"
        );

        assert_eq!(bus.int_ack(6), None, "autovector, so no vector number");
        assert_eq!(
            bus.vdp.pending_irq(),
            None,
            "and the VDP stopped offering it"
        );
    }

    // ---- Z80 bus arbitration, Z80 reset, and the 68k's way into the sound side ----
    //
    // The addresses and the register writes are plutiedev "Using the Z80", which is
    // what a game's own init code does:
    //
    //     Z80Ram:     equ $A00000
    //     Z80BusReq:  equ $A11100
    //     Z80Reset:   equ $A11200

    /// Take the Z80's bus the way a game does - `move.w #$100, (Z80BusReq)` - so a
    /// test can reach the hardware behind `0xA00000`. The bus is released but the
    /// reset line is *asserted* at power on (see [`Bus::new`]), so reaching the
    /// sound side needs this *and* [`release_the_z80_reset`]: the init order of
    /// plutiedev "Using the Z80" is reset, request, then release reset, because
    /// the area stays dark while the reset line is held (#260).
    fn request_the_z80_bus(bus: &mut Bus) {
        bus.write_word(Z80_BUSREQ, 0x0100);
    }

    /// Release it again, so the Z80 can run.
    fn release_the_z80_bus(bus: &mut Bus) {
        bus.write_word(Z80_BUSREQ, 0x0000);
    }

    /// Status bit 7 of the YM2612: still busy with the last write, which only a
    /// data-port write starts (`ym2612::Ym2612::write`, and the YM2608 application
    /// manual's status description).
    const YM_BUSY: u8 = 0x80;

    /// Has the PSG taken no write at all?
    ///
    /// Its power-on state is every channel at attenuation 15 and every tone period
    /// at 0 ([`sn76489::Sn76489::new`]), and none of the bytes these tests aim at a
    /// locked port, an even lane or the HV counter leaves that. This is what
    /// "nothing reached the PSG" means now that the chip is a chip rather than the
    /// log M3-07 (#67) left in its place (M5-13, #118).
    fn psg_untouched(bus: &Bus) -> bool {
        (0..4).all(|channel| {
            bus.audio.psg.attenuation(channel) == 15 && bus.audio.psg.tone_period(channel) == 0
        })
    }

    /// Release the Z80's reset line - `move.w #$100, (Z80Reset)`.
    fn release_the_z80_reset(bus: &mut Bus) {
        bus.write_word(Z80_RESET_REG, 0x0100);
    }

    /// The Z80 owns the bus until a 68k request says otherwise, so the bus register
    /// reads back "the Z80 is running" from the moment the bus exists: bit 0 of the
    /// byte and bit 8 of the word are both 1 (plutiedev "Using the Z80", whose
    /// `PauseZ80` macro polls `btst #0, (Z80BusReq)` until it reads 0).
    #[test]
    fn busreq_read_reports_running_by_default() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert_eq!(bus.read_byte(Z80_BUSREQ) & 1, 1, "byte: Z80 running");
        assert_eq!(
            bus.read_word(Z80_BUSREQ) & 0x0100,
            0x0100,
            "word: Z80 running"
        );

        // Every other bit is open bus, and open bus here means 1. That is also what
        // the polling games need: MAME's note on this register is that Time Killers
        // and friends loop until the read is nonzero, and Genesis Plus GX returns
        // prefetched bus data for the unused bits for the same reason.
        assert_eq!(bus.read_byte(Z80_BUSREQ), OPEN_BUS);
        assert_eq!(bus.read_word(Z80_BUSREQ), OPEN_BUS_WORD);
    }

    /// A granted bus reads back as granted: writing bit 8 of the word at
    /// `0xA11100` takes the bus from the Z80, and the same bit reads back 0.
    #[test]
    fn busreq_grant_after_request() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.write_word(Z80_BUSREQ, 0x0100);
        release_the_z80_reset(&mut bus);
        assert_eq!(
            bus.read_word(Z80_BUSREQ) & 0x0100,
            0,
            "word read of the grant"
        );
        assert_eq!(bus.read_byte(Z80_BUSREQ) & 1, 0, "byte read of the grant");
        assert_ne!(
            bus.read_byte(Z80_BUSREQ),
            0,
            "the unused bits are never all-zero"
        );

        // Writing 0 releases it and the Z80 is running again; nothing latches.
        bus.write_word(Z80_BUSREQ, 0x0000);
        assert_eq!(bus.read_word(Z80_BUSREQ) & 0x0100, 0x0100);
    }

    /// The read-back of `0xA11100` answers "the Z80 is running" (bit 0 of the byte,
    /// bit 8 and bit 0 of the word) in every state of the two lines but the one
    /// that grants the area: bus requested *and* reset released. The corner this
    /// pins is (reset asserted, bus requested), which both references answer with
    /// "running": MAME's `megadriv_68k_check_z80_bus` and Genesis Plus GX report
    /// the Z80 running while the reset line is held even though the 68k's request
    /// is on the line, so a sound driver that polls the grant back before its
    /// reset release reads the not-granted state a real console gives it (#260).
    /// BlastEm is the dissenting reference: it tests `z80_get_busack` alone, so it
    /// would answer "granted" in that state.
    #[test]
    fn busreq_readback_tracks_reset_and_busreq() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);

        // (reset asserted, bus released) - power on: running.
        assert_eq!(bus.read_byte(Z80_BUSREQ) & 1, 1);
        assert_eq!(bus.read_word(Z80_BUSREQ) & 0x0100, 0x0100);

        // (reset asserted, bus requested): still "running" - the reset line wins
        // over the request, byte read and word read alike.
        bus.write_word(Z80_BUSREQ, 0x0100);
        assert_eq!(
            bus.read_byte(Z80_BUSREQ) & 1,
            1,
            "reset held: the poll sees the Z80 running"
        );
        assert_eq!(
            bus.read_word(Z80_BUSREQ) & 0x0100,
            0x0100,
            "word: the same answer"
        );

        // (reset released, bus requested): granted, byte read and word read.
        release_the_z80_reset(&mut bus);
        assert_eq!(bus.read_byte(Z80_BUSREQ) & 1, 0);
        assert_eq!(bus.read_word(Z80_BUSREQ) & 0x0100, 0);
        // Bit 0 of the word read is not the flag but the 68000's prefetched
        // instruction word ([`busreq_word`]); what this bus has been told is
        // checked in [`busreq_word_unused_bits_are_the_prefetched_word`].

        // (reset released, bus released): running again.
        release_the_z80_bus(&mut bus);
        assert_eq!(bus.read_byte(Z80_BUSREQ) & 1, 1);
        assert_eq!(bus.read_word(Z80_BUSREQ) & 0x0100, 0x0100);
    }

    /// The unused bits of a **word** read are the 68000's prefetched instruction
    /// word, not a copy of the flag and not a fixed pattern.
    ///
    /// Both surviving references answer the word read this way: Genesis Plus GX's
    /// `ctrl_io_read_word` case 0x11 returns the word at the 68k's PC with the flag
    /// lane forced (`& 0xFEFF` when granted, `| 0x0100` running) under the comment
    /// "Unused bits return prefetched bus data (Time Killers)", and BlastEm's
    /// `io_read_w` fills the low byte from `get_open_bus_value() & 0xFF`, the word at
    /// the end of the current instruction. Which word that is decides Time Killers'
    /// boot (#479): the opcode following the poll's `MOVE.W` is `BTST #$0,D0` =
    /// `$0810` at boot, whose bit 0 lets the `bne.s` poll exit, and `BTST #$0,D1` =
    /// `$0811` in the game loop, whose bit 0 is set, letting the later `beq.s`
    /// poll exit. A flag mirrored into bit 0 (#478) cannot serve both: the game loop
    /// poll waits for bit 0 to read 1 in a state where the bus is granted, and spins
    /// there from frame ~126 for good.
    #[test]
    fn busreq_word_unused_bits_are_the_prefetched_word() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.write_word(Z80_BUSREQ, 0x0100);
        release_the_z80_reset(&mut bus);

        // The game loop's poll: the word following its MOVE.W is $0811, bit 0 set,
        // so the read falls out of the poll while the bus is granted.
        bus.prefetched_word = 0x0811;
        assert_eq!(bus.read_word(Z80_BUSREQ), 0x0811);

        // The boot poll: the word following its MOVE.W is $0810, bit 0 clear, so
        // the same composition leaves bit 0 at 0 and the bne.s falls through.
        bus.prefetched_word = 0x0810;
        assert_eq!(bus.read_word(Z80_BUSREQ), 0x0810);

        // Without a grant the flag lane is the one bit that moves.
        bus.write_word(Z80_BUSREQ, 0x0000);
        bus.prefetched_word = 0x0811;
        assert_eq!(
            bus.read_byte(Z80_BUSREQ) & 1,
            1,
            "running: the byte read says so"
        );
        assert_eq!(bus.read_word(Z80_BUSREQ), 0x0911);
    }

    /// With nothing published yet the word read is the open-bus ones with the
    /// flag lane carrying the grant, the same never-all-zero rule the byte read
    /// keeps ([`busreq_grant_after_request`]).
    #[test]
    fn busreq_word_before_any_prefetch_is_open_bus_with_the_flag() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert_eq!(
            bus.read_word(Z80_BUSREQ) & 0x0100,
            0x0100,
            "running by default"
        );
        assert_eq!(
            bus.read_word(Z80_BUSREQ),
            0xFFFF,
            "no queue word yet: every lane is the open-bus 1s"
        );

        bus.write_word(Z80_BUSREQ, 0x0100);
        release_the_z80_reset(&mut bus);
        assert_eq!(
            bus.read_word(Z80_BUSREQ),
            0xFEFF,
            "granted: flag lane clear"
        );
    }

    /// Every undriven read on the 68k side answers the 68000's prefetched
    /// instruction word, the lane of it the byte read sits on.
    ///
    /// The model, and who says it: an undriven data bus holds the last value a
    /// transfer drove onto the lines, the last cycle before a typical undriven
    /// read is the fetch of the next opcode, and the one place hardware has been
    /// observed is Kabuto's VDP-port note ("SEGA Mega Drive / Genesis hardware
    /// notes" v1.5): reading `$C00018`/`$C0001C` "returns the following opcode
    /// as the 68k prefetches it before doing the actual read access". Both
    /// references implement exactly that as their whole open-bus answer:
    /// Genesis Plus GX's `m68k_read_bus_16` answers the word at `m68k.pc`, and
    /// BlastEm's `get_open_bus_value` answers the queue word, lane-selected by
    /// address parity for byte reads (`value = location & 1 ? word : word >> 8`).
    /// The word itself is what the CPU publishes on every queue refill
    /// ([`m68k::Bus::note_prefetched_word`], #479), and `$FFFF` before the first
    /// fetch is the pull-up state (Kabuto: "a value of `$FFFF` is read (due to
    /// bus pull-up)"). #508 is the ticket; the honest limit on it is that no
    /// source gives a decay model or the residue a data write leaves, so this is
    /// the prefetch word and nothing else.
    #[test]
    fn undriven_reads_answer_the_prefetched_word() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.note_prefetched_word(0x1234);

        // The undecoded gaps of [`Region::Io`], byte lanes and word.
        assert_eq!(bus.read_byte(0xA10020), 0x12, "even lane");
        assert_eq!(bus.read_byte(0xA10021), 0x34, "odd lane");
        assert_eq!(bus.read_word(0xA10020), 0x1234);

        // Above the end of a cart image.
        assert_eq!(bus.read_byte(0x08_0000), 0x12);
        assert_eq!(bus.read_word(0x08_0000), 0x1234);

        // The VDP debug pair, the ports Kabuto's note is about, and the PSG's
        // read lane in the same block - write-only, so nothing drives a read.
        assert_eq!(bus.read_word(0xC00018), 0x1234);
        assert_eq!(bus.read_byte(0xC00018), 0x12);
        assert_eq!(bus.read_byte(0xC00019), 0x34);
        assert_eq!(bus.read_word(0xC0001C), 0x1234);
        assert_eq!(bus.read_word(0xC00010), 0x1234);
        assert_eq!(bus.read_byte(0xC00011), 0x34);

        // The reset register has no read handler in any reference, so its
        // unclaimed read is float; the bank register and the sound side's
        // undriven lanes answer the same way.
        assert_eq!(bus.read_word(Z80_RESET_REG), 0x1234);
        assert_eq!(bus.read_byte(Z80_RESET_REG), 0x12);
        assert_eq!(bus.read_byte(0xA06000), 0x12);
        assert_eq!(bus.read_word(0xA07F10), 0x1212, "one byte access, twice");

        // The sram control block of [`Region::CartExtra`].
        assert_eq!(bus.read_byte(SRAM_CONTROL), 0x12);
        assert_eq!(bus.read_word(SRAM_CONTROL), 0x1234);

        // [`Region::Unmapped`].
        assert_eq!(bus.read_byte(0xA1307E), 0x12);
        assert_eq!(bus.read_word(0xA1307E), 0x1234);
    }

    /// Before the CPU publishes its first fetch the float reads as ones: the
    /// data lines have pull-ups, and `0xFF`/`0xFFFF` is what a bus nothing has
    /// driven reads as. This is what the pre-#508 placeholder answered on every
    /// read forever; now it is the one state of the latch it names.
    #[test]
    fn the_float_reads_as_pull_up_ones_before_anything_is_fetched() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert_eq!(bus.read_byte(0xA10020), OPEN_BUS);
        assert_eq!(bus.read_word(0xA10020), OPEN_BUS_WORD);
        assert_eq!(bus.read_byte(0x08_0000), OPEN_BUS);
        assert_eq!(bus.read_word(0xC00018), OPEN_BUS_WORD);
    }

    /// A TMSS-refused VDP read freezes the 68k and answers the float, the way
    /// Genesis Plus GX's `m68k_lockup_r_8`/`m68k_lockup_r_16` halt the CPU and
    /// answer the word at `m68k.pc` (its `mem68k.c`, "Illegal areas"). The
    /// freeze itself is pinned in [`tmss_on_vdp_read_before_unlock_sets_locked_flag`].
    #[test]
    fn a_tmss_refused_read_answers_the_float() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        bus.note_prefetched_word(0x1234);
        assert_eq!(bus.read_word(0xC00000), 0x1234, "access dropped");
        assert!(bus.locked_up(), "and the access still froze the 68k");
    }

    /// The dead half of a half-width save window: the window owns the address,
    /// so the image does not answer, and nothing drives the byte - which is the
    /// float ([`crate::cart::sram::Sram::read`]'s own half of this is the
    /// `None` it hands back so the bus can answer). The placeholder read as
    /// `0xFF` here before #508; the pinned half is what the window wires up.
    #[test]
    fn the_dead_half_of_a_half_width_save_answers_the_float() {
        let mut bus = odd_sram_bus();
        bus.note_prefetched_word(0x1234);
        bus.write_byte(0x20_0001, 0xAB);
        assert_eq!(bus.read_byte(0x20_0002), 0x12, "even lane of the float");
        assert_eq!(bus.read_word(0x20_0000), 0x12AB, "save byte 0 under it");
    }

    /// The SVP register page's undecoded lanes float like every other undriven
    /// read, on a cart with the board and without it alike.
    #[test]
    fn the_svp_register_page_undecoded_lanes_answer_the_float() {
        let mut bus = Bus::new(rom_image(0x400_000), TEST_VERSION);
        bus.note_prefetched_word(0x1234);
        assert_eq!(bus.read_word(0xA1_5008), 0x1234, "no board: whole page");

        let mut bus = Bus::new(svp_cart_rom(), TEST_VERSION);
        bus.note_prefetched_word(0x1234);
        assert_eq!(
            bus.read_word(0xA1_5008),
            0x1234,
            "with the board: undecoded"
        );
    }

    /// A DMA sourcing past the end of its image reads the float too: a
    /// transfer's source cycles are bus cycles like any other (BlastEm's
    /// `get_open_bus_value` is literally `read_dma_value` of the prefetch
    /// address, which is the same answer from the other side).
    #[test]
    fn a_dma_source_read_answers_the_float() {
        // An empty image, so the transfer's source at $000000 is already past
        // the end of it. R19 the word count, R21-R23 the source address in its
        // 7-9-8 split; the rest is the set-up of
        // `dma_68k_to_vram_via_control_write`.
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        bus.note_prefetched_word(0x1234);
        set_reg(&mut bus, 1, 0x10);
        set_reg(&mut bus, 15, 2);
        set_reg(&mut bus, 19, 1);
        set_reg(&mut bus, 20, 0);
        set_reg(&mut bus, 21, 0);
        set_reg(&mut bus, 22, 0);
        set_reg(&mut bus, 23, 0);
        let (first, second) = dma_cmd(0x0000);
        bus.write_word(0xC00004, first);
        bus.write_word(0xC00004, second);
        bus.vdp.advance(TEST_VDP_LINE);
        assert_eq!(
            &bus.vdp.vram[0..2],
            &[0x12, 0x34],
            "the source word is the float"
        );
    }

    /// The byte write does the same thing, from bit 0 of the byte rather than bit 8
    /// of the word, and `0xA11101` is the same register and not a second flag: the
    /// decode drops A0 (BlastEm decodes the `0xA11100` block with `location &
    /// 0xFFF00` and takes `value & 1`; MAME maps `0xA11100-0xA11101` as one
    /// register, reading bit 0 of whichever byte lane arrives).
    #[test]
    fn byte_write_busreq() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.write_byte(Z80_BUSREQ, 0x01);
        // The reset line is asserted from power on and the read-back answers
        // "running" while it is, so the release has to come before the poll.
        release_the_z80_reset(&mut bus);
        assert_eq!(bus.read_byte(Z80_BUSREQ) & 1, 0, "$01 requests the bus");

        // It is the one flag: the grant the byte write took is the grant the sound
        // side checks.
        bus.write_byte(0xA00010, 0x5A);
        assert_eq!(bus.z80.ram[0x0010], 0x5A, "byte write requested for real");
        bus.write_byte(0xA11101, 0x00);
        assert_eq!(
            bus.read_word(Z80_BUSREQ) & 0x0100,
            0x0100,
            "$00 at 0xA11101 releases"
        );

        // Bits 1-7 of a byte write are ignored, like the bank register before it.
        bus.write_byte(Z80_BUSREQ, 0xFE);
        assert_eq!(bus.read_byte(Z80_BUSREQ) & 1, 1, "0xFE asked for nothing");
    }

    /// With the bus granted, `0xA00000-0xA01FFF` is the sound RAM: the 68k reads and
    /// writes it, the `0xA02000` mirror answers the same bytes, and the whole area
    /// repeats every `0x8000` because only the low 15 address bits reach the Z80 bus
    /// (BlastEm masks a 68k access to the area with `0x7FFF`).
    #[test]
    fn z80_ram_access_from_68k_when_bus_granted() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);

        bus.write_byte(0xA00010, 0x5A);
        assert_eq!(bus.read_byte(0xA00010), 0x5A);
        assert_eq!(bus.z80.ram[0x0010], 0x5A);

        // The mirror the 8 KiB sits behind, in both directions.
        assert_eq!(bus.read_byte(0xA02010), 0x5A, "mirror at 0xA02000");
        bus.write_byte(0xA03FFF, 0x77);
        assert_eq!(bus.read_byte(0xA01FFF), 0x77);
        assert_eq!(
            bus.read_byte(0xA08010),
            0x5A,
            "the area repeats every 0x8000"
        );
    }

    /// Without the grant there is nothing behind `0xA00000`: reads return open bus
    /// and writes vanish. Real hardware is nastier than that - Genesis Plus GX
    /// installs a bus lockup for the whole area while the Z80 owns it, and BlastEm
    /// pays a wait state and then reads garbage - so a game that pokes the sound side
    /// without requesting the bus first hangs or reads junk rather than getting a
    /// clean 0xFF. This is the default state, because the bus is released at power on.
    #[test]
    fn z80_ram_access_blocked_without_bus() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert_eq!(bus.read_byte(0xA00010), OPEN_BUS);
        assert_eq!(bus.read_word(0xA00010), OPEN_BUS_WORD);

        bus.write_byte(0xA00010, 0x5A);
        bus.write_word(0xA00010, 0x5A5A);
        assert!(
            bus.z80.ram.iter().all(|&b| b == 0),
            "dropped writes reached the RAM"
        );
        assert_eq!(
            Z80View::new(&mut bus).read(0x0010),
            0,
            "the Z80 sees nothing either"
        );
    }

    /// With the bus requested but the Z80 still held in reset, the sound side is
    /// still not there. plutiedev "Using the Z80", "Loading a Z80 program": "we
    /// reset, then request the bus then release reset (this is because for some
    /// reason we can't access Z80 RAM while Z80 is reset)", and the copy into Z80
    /// RAM only starts after the reset release. MAME's `megadriv_68k_read_z80_ram`
    /// / `megadriv_68k_write_z80_ram` allow the access only when `z80_has_bus == 0`
    /// && `z80_is_reset == 0`, and Genesis Plus GX installs the 0xa0 page handlers
    /// only with the reset line released (`gen_zbusreq_w` when `zstate == 1`,
    /// `gen_zreset_w` when `zstate == 2`), so there a 68k read under reset gets
    /// prefetched bus data and its writes go nowhere. "Refuse" is the shape every
    /// other unclaimed area of this bus has (#508's placeholder): reads answer
    /// [`OPEN_BUS`] and writes are dropped, never queued.
    #[test]
    fn z80_area_refused_while_reset_held() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        request_the_z80_bus(&mut bus);
        // What this test does *not* do is the point: the reset line is still
        // asserted from power on, as it is in a game between its bus request and
        // its reset release.

        // The sound RAM is dark: reads open bus, writes dropped.
        bus.write_byte(0xA00010, 0x5A);
        bus.write_word(0xA00010, 0x5A5A);
        assert_eq!(bus.read_byte(0xA00010), OPEN_BUS);
        assert_eq!(bus.read_word(0xA00010), OPEN_BUS_WORD);
        assert!(
            bus.z80.ram.iter().all(|&b| b == 0),
            "dropped writes reached the RAM"
        );

        // The bank register takes no shift...
        for _ in 0..9 {
            bus.write_byte(0xA06000, 0x01);
        }
        assert_eq!(
            bus.z80.bank, 0,
            "the bank register is behind the reset gate"
        );

        // ... and neither does the PSG.
        bus.write_byte(0xA07F11, 0x9F);
        bus.write_byte(0xA07F13, 0x13);
        assert!(psg_untouched(&bus), "the PSG is behind the reset gate too");

        // A YM2612 data-port write under reset is dropped, and the BUSY it would
        // have set proves it once the reset line is out of the way: the status is
        // read after the release, so a held-for-later write would read busy.
        bus.write_byte(0xA04007, 0x00);
        release_the_z80_reset(&mut bus);
        assert_eq!(
            bus.read_byte(0xA04000) & YM_BUSY,
            0,
            "the write under reset was dropped, not held for later"
        );

        // The release of the plutiedev init is what opens the area: the same
        // writes land now.
        bus.write_byte(0xA00010, 0x5A);
        assert_eq!(bus.read_byte(0xA00010), 0x5A);
        bus.write_byte(0xA07F11, 0x9F);
        bus.write_byte(0xA07F13, 0x13);
        assert_eq!(
            bus.audio.psg.attenuation(0),
            3,
            "the PSG answers after release"
        );
        bus.write_byte(0xA04007, 0x00);
        assert_eq!(
            bus.read_byte(0xA04000) & YM_BUSY,
            YM_BUSY,
            "and so does the YM2612"
        );
    }

    /// The Z80-RAM word quirk, kept because both references that model it have it:
    /// a word read answers the byte at the even address twice, and a word write only
    /// writes that byte and ignores the other half. Genesis Plus GX's `z80_read_word`
    /// is literally the byte read once and mirrored, `z80_write_word` is
    /// `z80_write_byte(address, data >> 8)`, and MAME's write handler says "for WORD
    /// access only the MSB is used, LSB is ignored". plutiedev "Using the Z80" puts
    /// it as "you must use byte accesses when touching Z80 RAM, word accesses won't
    /// work". See [`Bus::read_z80_word`] before changing any of this in M5.
    #[test]
    fn word_read_z80_ram_duplicates_byte() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);

        bus.write_byte(0xA00010, 0x3C);
        assert_eq!(bus.read_word(0xA00010), 0x3C3C);

        // The other half of the quirk.
        bus.write_byte(0xA00011, 0xC3);
        bus.write_word(0xA00010, 0xABCD);
        assert_eq!(
            bus.z80.ram[0x0010], 0xAB,
            "the high byte of the word was written"
        );
        assert_eq!(
            bus.z80.ram[0x0011], 0xC3,
            "the low byte of the word was ignored"
        );
    }

    /// The Z80 runs only when the 68k has neither held it in reset nor taken its bus,
    /// which is the ticket's `!busreq && !reset`. Reset is what makes it false at
    /// power on: both references that set the lines up start with reset asserted and
    /// the bus released (MAME's `machine_reset` sets `z80_is_reset = 1` and
    /// `z80_has_bus = 1`, Genesis Plus GX's `gen_reset` sets `zstate = 0`, commented
    /// "Z80 bus is released & Z80 is reseted").
    #[test]
    fn reset_held_means_z80_does_not_run() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert!(!bus.z80_can_run(), "held in reset at power on");

        release_the_z80_reset(&mut bus);
        assert!(bus.z80_can_run(), "released and on its bus");

        bus.write_word(Z80_RESET_REG, 0x0000);
        assert!(!bus.z80_can_run(), "reset asserted again");

        // The two lines are independent: a bus request stops a Z80 that is out of
        // reset, and releasing the request starts it back up.
        release_the_z80_reset(&mut bus);
        request_the_z80_bus(&mut bus);
        assert!(!bus.z80_can_run(), "the 68k has the bus");
        release_the_z80_bus(&mut bus);
        assert!(bus.z80_can_run());
    }

    /// Coming out of reset is the event that resets the Z80's PC to 0 (plutiedev
    /// "Using the Z80": release reset, and the program in RAM starts at `$0000`;
    /// Genesis Plus GX's `gen_zreset_w` calls `z80_reset()` on exactly that edge).
    /// The bus cannot touch the CPU itself, so it raises a pulse the scheduler takes
    /// with [`Bus::take_z80_reset`] and answers with `z80::Z80::reset`.
    #[test]
    fn reset_release_resets_z80_pc() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        let mut cpu = z80::Z80::new();
        assert!(
            !bus.take_z80_reset(),
            "power on asserts the line, it does not release it"
        );

        // Asserting an already asserted line is neither a release nor an edge, so the
        // YM2612 hanging off the same pin is held in reset rather than reset again.
        let ym_resets = bus.audio.ym_resets;
        bus.write_word(Z80_RESET_REG, 0x0000);
        assert!(!bus.take_z80_reset());
        assert_eq!(bus.audio.ym_resets, ym_resets, "no edge, no YM2612 reset");

        // The release edge, and the scheduler's side of the handshake: the bus raises
        // the pulse, the scheduler answers it with the CPU's own reset.
        release_the_z80_reset(&mut bus);
        cpu.set_pc(0x1234);
        if bus.take_z80_reset() {
            cpu.reset();
        }
        assert_eq!(
            cpu.pc(),
            0,
            "the release the bus reported is the one that clears PC"
        );
        assert!(!bus.take_z80_reset(), "the pulse is consumed once");

        // Asserting it for real now takes the YM2612 with it: BlastEm's reset handler
        // calls `ym_reset` in the same branch that asserts the Z80's reset.
        bus.write_word(Z80_RESET_REG, 0x0000);
        assert_eq!(
            bus.audio.ym_resets,
            ym_resets + 1,
            "asserting reset reaches the YM2612"
        );
        assert!(
            !bus.take_z80_reset(),
            "and asserting is still not a release"
        );
    }

    /// The `0xA00000` window and the Z80's `0x0000` RAM are one and the same 8 KiB:
    /// the 68k writes, the Z80 reads it through its own map, and the other way round.
    #[test]
    fn z80_ram_is_one_array_seen_by_both_cpus() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);

        bus.write_byte(0xA00020, 0x67);
        assert_eq!(Z80View::new(&mut bus).read(0x0020), 0x67);
        assert_eq!(
            Z80View::new(&mut bus).read(0x2020),
            0x67,
            "the Z80's own mirror is the same byte"
        );

        Z80View::new(&mut bus).write(0x1234, 0x1E);
        assert_eq!(bus.read_byte(0xA01234), 0x1E);
        assert_eq!(bus.z80.ram.len(), ZRAM_SIZE, "one 8 KiB array, not two");
    }

    /// The 68k can move the Z80's bank register - `0xA06000` is the same shift
    /// register the Z80 sees at `0x6000`, which is why it sits behind the bus grant
    /// too. Every reference that decodes the area gives the 68k the write: BlastEm's
    /// `io_write` shifts `z80_bank_reg` from `0xA00000`-side addresses, Genesis Plus
    /// GX's `z80_write_byte` takes the `0x60` block into `gen_zbank_w`, and MAME maps
    /// `0xA06000-0xA06001` to `megadriv_68k_z80_bank_write`.
    #[test]
    fn bus_write_from_68k_moves_the_z80_bank() {
        let mut rom = vec![0u8; 0x1_0000];
        rom[0x8010] = 0x6C;
        let mut bus = Bus::new(rom, TEST_VERSION);

        // No grant, no shift.
        for _ in 0..9 {
            bus.write_byte(0xA06000, 0x01);
        }
        assert_eq!(bus.z80.bank, 0);

        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);
        // Nine writes, least significant bit first: each write enters at the top of the
        // 9 bit register, so a 1 written first shifts down to bit 0.
        bus.write_byte(0xA06000, 0x01);
        for _ in 0..8 {
            bus.write_byte(0xA06000, 0x00);
        }
        assert_eq!(bus.z80.bank, 0x001);

        // The Z80's window follows the register the 68k moved: 0x001 << 15 is
        // 0x008000, so its `0x8010` is the 68k's 0x8010.
        assert_eq!(Z80View::new(&mut bus).read(0x8010), 0x6C);
    }

    /// `0xA04000-0xA05FFF` is the YM2612, the same four ports at `addr & 3` that #67
    /// gave the Z80: bank 0 at `0xA04000/1` and bank 1 at `0xA04002/3` (plutiedev
    /// "YM2612 register reference", and BlastEm's `io_write`, which picks the port
    /// from bits 0 and 1 of the area offset).
    ///
    /// The chip cannot be read back, so the ports are told apart by what they do:
    /// only a data-port write raises BUSY ([`crate::Audio::ym_write`], and
    /// `ym2612::Ym2612::write`), and a register the Z80 loaded is visible in the
    /// status the 68k reads.
    #[test]
    fn ym2612_ports_are_reachable_from_the_68k() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);

        // `0xA04006` is `0xA04002` again - the four repeat every 4 bytes across the
        // whole area - and that is part II's *address* port, which is not a write to
        // a register and so does not hold the chip busy. `0xA04007` is the data port
        // beside it, and does.
        assert_eq!(
            bus.read_byte(0xA04000) & YM_BUSY,
            0,
            "idle before any write"
        );
        bus.write_byte(0xA04006, 0x27);
        assert_eq!(
            bus.read_byte(0xA04000) & YM_BUSY,
            0,
            "0xA04006 is the mirrored part II address port, not a data write"
        );
        bus.write_byte(0xA04007, 0x00);
        assert_ne!(
            bus.read_byte(0xA04000) & YM_BUSY,
            0,
            "0xA04007 is the mirrored part II data port, and only a data write is busy"
        );

        // One chip, not one per CPU: the timer the Z80 loads through its own
        // `0x4000`-`0x4003` is the one whose overflow the 68k reads at `0xA04000`.
        Z80View::new(&mut bus).write(0x4000, 0x24);
        Z80View::new(&mut bus).write(0x4001, 0xFF);
        Z80View::new(&mut bus).write(0x4000, 0x25);
        Z80View::new(&mut bus).write(0x4001, 0x03);
        Z80View::new(&mut bus).write(0x4000, 0x27);
        Z80View::new(&mut bus).write(0x4001, 0x05);
        bus.audio.advance_to(crate::audio::YM_SAMPLE_MCLK);
        assert_ne!(
            bus.read_byte(0xA04000) & 0x01,
            0,
            "timer A loaded by the Z80 overflows into the status the 68k reads"
        );
    }

    /// `0xA07F11` is the PSG port the Z80 sees at `0x7F11`, which #67 settled as one
    /// port with several spellings, so this is a third address into the same log and
    /// not a third view: the `0x11/0x13/0x15/0x17` lanes of the VDP window, never the
    /// even lane of the pair.
    #[test]
    fn psg_write_from_68k_at_a07f11() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);

        // `0x9F` is channel 0's volume at 15 and `0x13` a data byte under the same
        // latch, which is channel 0's volume at 3 (SMS Power, "SN76489", register
        // writes). Both bytes have to have arrived for that to be the answer.
        bus.write_byte(0xA07F11, 0x9F);
        bus.write_byte(0xA07F13, 0x13);
        assert_eq!(bus.audio.psg.attenuation(0), 3);

        // The even byte of the pair is not the port, and neither is any other lane
        // of the VDP window; the 68k has its own VDP ports for those.
        bus.write_byte(0xA07F10, 0x06);
        bus.write_byte(0xA07F00, 0x07);
        assert_eq!(bus.audio.psg.attenuation(0), 3);

        Z80View::new(&mut bus).write(0x7F11, 0x22);
        assert_eq!(
            bus.audio.psg.attenuation(0),
            2,
            "one PSG, whichever of the three spellings a byte arrived by"
        );

        // The PSG is write-only, so a read of its port is nothing's.
        assert_eq!(bus.read_byte(0xA07F11), OPEN_BUS);
        assert_eq!(bus.read_word(0xA07F10), OPEN_BUS_WORD);
    }

    /// The console's `RESET` puts the two control lines back to their power-on
    /// values, which is what Genesis Plus GX's `gen_reset` does ("Z80 bus is released
    /// & Z80 is reseted", and `zbank = 0`) and MAME's `machine_reset`
    /// (`z80_is_reset = 1; z80_has_bus = 1`). The sound RAM survives, like work RAM.
    #[test]
    fn reset_devices_parks_the_z80_and_clears_the_bank() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);
        bus.write_byte(0xA06000, 0x01);
        bus.write_byte(0xA00010, 0x5A);
        release_the_z80_bus(&mut bus);
        assert!(bus.z80_can_run(), "the setup let the Z80 have its bus back");

        bus.reset_devices();

        assert!(!bus.z80_can_run(), "back under reset");
        assert!(!bus.take_z80_reset(), "asserting is not a release");
        assert_eq!(
            bus.read_byte(Z80_BUSREQ) & 1,
            1,
            "the 68k lost its grant too"
        );
        assert_eq!(bus.z80.bank, 0, "the bank register belongs to this reset");
        assert_eq!(bus.z80.ram[0x0010], 0x5A, "sound RAM survives RESET");
    }

    // ---- TMSS: the trademark security lock on VDP access (M3-10 #70) ----
    //
    // A TMSS console holds the VDP off until the 68k has written the long "SEGA" to
    // 0xA14000. segaretro "TradeMark Security System", "Design": "Write a long word
    // containing "SEGA" (all caps) to address 00 A14000 before accessing the VDP data
    // port (00 C00000). If the game fails to do this, the next VDP data port access
    // will cause the 68000 to lock up - this is performed by the I/O controller."
    // The port addresses and their mirroring are plutiedev "Memory map".

    /// A machine that reports no TMSS: [`crate::Region::Usa`] without the bit, which
    /// is [`TEST_VERSION`] and what [`crate::Config::default`] asks for.
    const NO_TMSS: u8 = 0xA0;
    /// The same machine with the TMSS bit of the version register set, which is how
    /// [`crate::Region::version_register`] says "I have a TMSS".
    const WITH_TMSS: u8 = 0xA1;

    /// Write the security string the way a game's `move.l #"SEGA", (A14000).l` does:
    /// two word writes, high word first, because the 68k splits a long into two word
    /// accesses in that order (`m68k::cpu`, "a long is two word accesses, high word
    /// first"). So `0x5345` then `0x4741` - the bytes of `SEGA` in order.
    fn write_sega(bus: &mut Bus) {
        bus.write_word(TMSS_SECURITY, 0x5345);
        bus.write_word(TMSS_SECURITY + 2, 0x4741);
    }

    /// One of every kind of access this bus offers, across the VDP window: data,
    /// control and PSG ports, the hang-up corner, a mirror and the top of the area.
    /// Reads and writes, both widths. What the VDP answers is M3-09 (#69)'s; what
    /// TMSS does about it is what these tests are about, and that shows as
    /// [`Bus::locked_up`].
    fn touch_vdp(bus: &mut Bus) {
        for addr in [
            0xC00000,
            0xC00004,
            0xC00008,
            0xC00011,
            // Not 0xC00020: the hang-up corner is not a port and hangs any
            // console that touches it, TMSS state aside - that is the corner's
            // own tests' business (`the_hang_covers_the_corner_and_every_mirror_of_it`).
            0xC00040,
            // The far end of the area, at its last mirror's data-port lane -
            // the last byte of the region (VDP_END - 1 = $DFFFFF) is inside
            // that mirror's hang corner since #508's model.
            VDP_END - 0x3F,
        ] {
            let _ = bus.read_byte(addr);
            let _ = bus.read_word(addr & EVEN_LANE);
            bus.write_byte(addr, 0x5A);
            bus.write_word(addr & EVEN_LANE, 0x5A5A);
        }
    }

    /// With TMSS off - what [`crate::Config::default`] asks for and so what most
    /// games on most emulated consoles get - the VDP is open from the first cycle:
    /// no access freezes anything, and the security register is not there to write
    /// to (BlastEm's `write_tmss` only accepts the string when the version register
    /// claims TMSS).
    #[test]
    fn tmss_off_vdp_access_works() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        touch_vdp(&mut bus);
        assert!(!bus.locked_up(), "no TMSS, so no lock-up");

        write_sega(&mut bus);
        touch_vdp(&mut bus);
        assert!(
            !bus.tmss_unlocked,
            "a console with nothing to unlock has nothing unlocked either"
        );
        assert!(!bus.locked_up(), "and still nothing froze");
    }

    /// A TMSS console refuses the VDP until the string arrives: the access is
    /// dropped, which for reads is the same [`OPEN_BUS`] every other refused access
    /// here returns and for writes is nothing at all, and the lock-up flag goes set
    /// for the scheduler to freeze the 68k with. There is no recovery short of a
    /// reset, which is why the flag is set rather than the access answered.
    #[test]
    fn tmss_on_vdp_access_before_unlock_sets_locked_flag() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        assert!(!bus.locked_up(), "nothing has happened yet");

        // A game's usual first write: `move.w #$3FA0, (VDPControl)`, before it has
        // thought about TMSS at all.
        bus.write_word(0xC00004, 0x3FA0);
        assert!(
            bus.locked_up(),
            "VDP write before the unlock freezes the 68k"
        );
    }

    /// Reads lock up too: segaretro says "the next VDP data port access", and
    /// Genesis Plus GX maps the read handlers to its lockup handlers along with the
    /// write ones, so a probe of the HV counter is as fatal as a write to control.
    #[test]
    fn tmss_on_vdp_read_before_unlock_sets_locked_flag() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        assert_eq!(bus.read_word(0xC00000), OPEN_BUS_WORD, "access dropped");
        assert!(
            bus.locked_up(),
            "VDP read before the unlock freezes the 68k"
        );
    }

    /// `SEGA` written as the 68k writes a long - two words, high word first - gets
    /// past the check, and after that the VDP is the 68k's.
    #[test]
    fn tmss_unlock_with_sega_write() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);

        bus.write_word(TMSS_SECURITY, 0x5345);
        assert!(!bus.tmss_unlocked, "half the string is not the string");
        bus.read_byte(0xC00000);
        assert!(bus.locked_up(), "still locked on half");

        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        write_sega(&mut bus);
        assert!(bus.tmss_unlocked, "0x5345 then 0x4741 is SEGA");
        touch_vdp(&mut bus);
        assert!(!bus.locked_up(), "the VDP is released");
    }

    /// The same check fed one byte at a time, which is what a game doing
    /// `move.b #$53, (A14000).l` and friends produces: the byte lane says which byte
    /// of the string each write is, so four bytes in address order are the string.
    #[test]
    fn tmss_unlock_with_sega_bytes() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        for (offset, byte) in TMSS_SECURITY_STRING.iter().enumerate() {
            bus.write_byte(TMSS_SECURITY + offset as u32, *byte);
            let written = offset + 1;
            assert_eq!(
                bus.tmss_unlocked,
                written == TMSS_SECURITY_STRING.len(),
                "{written} byte(s) of SEGA"
            );
        }
        touch_vdp(&mut bus);
        assert!(!bus.locked_up(), "byte writes release the VDP too");
    }

    /// A wrong byte fails the check without doing anything else to the machine:
    /// the attempt is abandoned and the detector starts again, so the real string
    /// written afterwards unlocks. Genesis Plus GX gets the same out of keeping the
    /// 4-byte register image and comparing it against `"SEGA"` on every write.
    #[test]
    fn wrong_string_does_not_unlock() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        // "SEGB", wrong in the last byte only.
        bus.write_word(TMSS_SECURITY, 0x5345);
        bus.write_word(TMSS_SECURITY + 2, 0x4742);
        assert!(!bus.tmss_unlocked, "SEGB is not SEGA");

        bus.write_word(0xC00000, 0x0000);
        assert!(bus.locked_up(), "and the VDP is still held");
    }

    /// ... and a wrong string followed by the right one ends unlocked: the detector
    /// restarting must not latch on the mistake.
    #[test]
    fn wrong_string_then_sega_unlocks() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        bus.write_word(TMSS_SECURITY, 0x4E41); // "NA"
        bus.write_byte(TMSS_SECURITY + 2, b'X');
        assert!(!bus.tmss_unlocked);

        write_sega(&mut bus);
        assert!(bus.tmss_unlocked, "the detector recovers");
        touch_vdp(&mut bus);
        assert!(!bus.locked_up());
    }

    /// `RESET` restarts the console, and a console that restarts asks the security
    /// question again: the string is forgotten. Genesis Plus GX's `gen_reset` clears
    /// the TMSS register and reinstalls its lockup handlers for exactly this
    /// ("clear TMSS register", "VDP access is locked by default"). The freeze goes
    /// the other way: it belongs to the CPU that `RESET` has just restarted, so a
    /// machine still frozen after a reset could not be restarted by one.
    #[test]
    fn tmss_reset_rearms_the_lock() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        write_sega(&mut bus);
        touch_vdp(&mut bus);
        assert!(!bus.locked_up(), "unlocked and working");

        bus.reset_devices();

        assert!(!bus.tmss_unlocked, "RESET asks the question again");
        assert!(!bus.locked_up, "and clears the freeze it caused");
        bus.write_word(0xC00004, 0x3FA0);
        assert!(bus.locked_up, "the VDP is locked up for its answer");
    }

    /// The lock covers the whole [`Region::Vdp`] window, not just the two ports a
    /// game usually touches: Genesis Plus GX's `gen_tmss_w` answers the security
    /// register by mapping all of 0xC0-0xDF to its lockup handlers, which is the
    /// data, control, HV counter, **PSG** and debug ports, their mirrors every 0x40
    /// and the 0xC00020-0xC0003F hang-up corner of plutiedev "Memory map". The PSG
    /// is in there because it lives in the VDP's own port pair (plutiedev "Memory
    /// map", "PSG is normally accessed by byte, so you'll generally see $C00011
    /// instead") and reaches the bus through this region, not through the Z80 side.
    #[test]
    fn tmss_lock_covers_the_whole_vdp_window() {
        let vdp_addresses = [
            0xC00000, // data port
            0xC00004, // control port
            0xC00008, // HV counter
            0xC00011, // PSG port
            0xC00018, // debug address port
            0xC00040, // the every-0x40 mirror
            // The far end of the area, its last mirror's data-port lane: the
            // last byte of the region is inside that mirror's hang corner
            // (#508), which the corner's own tests cover.
            VDP_END - 0x3F,
        ];

        for addr in vdp_addresses {
            let mut bus = Bus::new(Vec::new(), WITH_TMSS);
            bus.write_byte(addr, 0x5A);
            assert!(bus.locked_up, "{addr:#08x} is inside the lock");
            assert!(
                psg_untouched(&bus),
                "{addr:#08x} must not reach the PSG while locked"
            );
        }

        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        write_sega(&mut bus);
        for addr in vdp_addresses {
            bus.write_byte(addr, 0x5A);
            let _ = bus.read_word(addr & EVEN_LANE);
            assert!(!bus.locked_up, "{addr:#08x} is open once unlocked");
        }

        // The hang-up corner at 0xC00020 is in the pages GPGX's gen_tmss_w locks
        // with the rest, but it has a gate of its own now (#508): a locked console
        // freezes at the TMSS arm of the dispatch first, and an unlocked one
        // freezes at the hang arm - it is not a port and never answers.
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        bus.write_byte(0xC00020, 0x5A);
        assert!(bus.locked_up, "the corner is inside the lock while locked");

        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        write_sega(&mut bus);
        bus.write_byte(0xC00020, 0x5A);
        assert!(bus.locked_up, "and hangs the unlocked console too");
    }

    /// The corner of [`Region::Vdp`] that decodes to nothing does not answer at
    /// all: plutiedev "Memory map", verbatim, "Accessing address range $C00020 to
    /// $C0003F hangs up". A hang is a machine state, not a return value - no
    /// DTACK comes back, so the 68000 stops - and the closest model the crate has
    /// is the TMSS freeze: [`Bus::locked_up`], cleared by nothing but
    /// [`m68k::Bus::reset_devices`]. Genesis Plus GX answers the same corner with
    /// its `m68k_lockup_w_8`/`m68k_lockup_r_8` handlers, which halt the CPU
    /// (https://github.com/ekeeke/Genesis-Plus-GX/blob/master/core/mem68k.c), and
    /// its lockup reads return the prefetched word, which is what the float half
    /// of this ticket answers here too (#508).
    #[test]
    fn an_access_to_the_hang_corner_locks_up_the_68k() {
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        assert!(!bus.locked_up(), "setup: nothing has frozen yet");

        bus.write_byte(0xC00020, 0x5A);
        assert!(bus.locked_up(), "the write hung the bus");

        // No recovery short of a reset - the same rule as the TMSS freeze.
        bus.write_byte(0xC00004, 0x80);
        bus.reset_devices();
        assert!(!bus.locked_up(), "reset_devices is the one clear");

        // A read hangs the same way.
        let _ = bus.read_byte(0xC00022);
        assert!(bus.locked_up(), "the read hung the bus too");
        bus.reset_devices();

        // Word accesses hang; the ports on the other side of A5 do not.
        bus.write_word(0xC00020, 0x0000);
        assert!(bus.locked_up(), "the word write hung the bus");
        bus.reset_devices();
        let _ = bus.read_word(0xC00020);
        assert!(bus.locked_up(), "the word read hung the bus");
        bus.reset_devices();
        bus.write_word(0xC00004, 0x3FA0);
        assert!(!bus.locked_up(), "the control port still answers");
    }

    /// The hang is the decoder, not one address: A5 is the only line that
    /// separates the ports from the corner, so the whole 0x20-byte span hangs
    /// and every 0x40 mirror of it hangs with it (plutiedev "Memory map": the
    /// ports run to $C0001F, the corner is $C00020-$C0003F, and "VDP mirrors
    /// every $40 up to $DFFFFF" is the same decoder one line later). The Z80's
    /// bank window is the one route of its own that reaches the corner - its
    /// `$7F00`-`$7FFF` port window is masked to the 0x20-byte port block and
    /// cannot - so a bank-window read of the corner hangs the bus through the
    /// same dispatch.
    #[test]
    fn the_hang_covers_the_corner_and_every_mirror_of_it() {
        let hang_addresses = [
            0xC00020, 0xC00021, 0xC0002E, 0xC0003F, // the span itself
            0xC00060, 0xC000A0, 0xDFFFE0, // the every-0x40 mirrors
        ];
        for addr in hang_addresses {
            let mut bus = Bus::new(Vec::new(), TEST_VERSION);
            bus.write_byte(addr, 0x5A);
            assert!(bus.locked_up, "{addr:#08x} is inside the hang");
        }

        // The Z80's bank window is the other route into the VDP area, and it is
        // not masked down to the port block: the register selects the top 9 bits
        // of the 68k space ([`z80_bus::Z80Side::window_addr`]), so bank `$180`
        // puts `$8020`-`$803F` on the corner. The shift protocol that loads the
        // register has its own tests; the value is poked here.
        let mut bus = Bus::new(Vec::new(), TEST_VERSION);
        bus.z80.bank = 0x180;
        Z80View::new(&mut bus).read(0x8020);
        assert!(
            bus.locked_up(),
            "the Z80's bank-window read of $C00020 hung the bus"
        );
    }

    /// The PSG has a second spelling on the 68k side, the Z80 window's `0xA07F11`
    /// ([`SoundSide::Psg`]), and it is behind the TMSS gate with the VDP area's
    /// `0xC00011`: one port, so one answer. Genesis Plus GX's `gen_tmss_w` locks
    /// the VDP pages 0xC0-0xDF in *both* bus maps that can reach them while the
    /// register does not read `"SEGA"` - the 68k's own `m68k.memory_map`, and the
    /// Z80's bank-window view `zbank_memory_map`, whose unlocked handler
    /// `zbank_write_vdp` answers the PSG lane (its case 0x10) with `psg_write` -
    /// so a locked PSG write freezes whoever asked, whichever spelling they used.
    #[test]
    fn tmss_on_psg_write_via_z80_window_is_refused() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);
        assert!(!bus.locked_up(), "taking the bus alone freezes nothing");

        // The write `psg_write_from_68k_at_a07f11` lands on an unlocked console:
        // a data byte under the power-on latch, which moves channel 0's tone
        // period the moment it arrives (SMS Power, "SN76489").
        bus.write_byte(0xA07F11, 0x5A);
        assert!(psg_untouched(&bus), "the locked PSG takes no write");
        assert!(
            bus.locked_up(),
            "the write froze the 68k, as it would at 0xC00011"
        );

        // Reads refuse the same way: the PSG is write-only, so the answer is the
        // [`OPEN_BUS`] it always gives, and the lock-up flag is what says the gate
        // saw the access at all. A fresh machine, so this is the read's own doing.
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);
        assert_eq!(bus.read_byte(0xA07F11), OPEN_BUS);
        assert!(bus.locked_up(), "the read froze the 68k too");
    }

    /// ... and the unlock opens this spelling with the rest: after `SEGA`, the
    /// same two bytes land that a locked console refused, unchanged from
    /// `psg_write_from_68k_at_a07f11` (M3-08 #68) - and without a freeze.
    #[test]
    fn tmss_unlock_opens_the_z80_window_psg() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);
        write_sega(&mut bus);
        assert!(bus.tmss_unlocked, "setup: the string got through");

        // The two bytes of `psg_write_from_68k_at_a07f11`: channel 0's volume
        // latch, then its data byte (SMS Power, "SN76489").
        bus.write_byte(0xA07F11, 0x9F);
        bus.write_byte(0xA07F13, 0x13);
        assert_eq!(bus.audio.psg.attenuation(0), 3, "the write lands");
        assert!(!bus.locked_up(), "nothing froze: the port is open");
    }

    /// The gate covers the PSG lane and nothing else of the Z80 window: Genesis
    /// Plus GX's `gen_tmss_w` maps 0xC0-0xDF to its lockup handlers and leaves the
    /// rest of the maps alone, so the YM2612 at 0xA04000 (its `z80_write_byte`
    /// case 2, which runs whether or not TMSS holds) and the even lane of the PSG
    /// pair (no reference drives it at all, [`SoundSide::Nothing`]) keep the
    /// answers they give on a console without a TMSS.
    #[test]
    fn tmss_lock_leaves_the_rest_of_the_z80_window_alone() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);

        // A YM2612 data-port write under the lock still reaches the chip, which
        // the BUSY it sets proves (the same probe `z80_area_refused_while_reset_held`
        // uses for the reset gate).
        bus.write_byte(0xA04007, 0x00);
        assert_eq!(
            bus.read_byte(0xA04000) & YM_BUSY,
            YM_BUSY,
            "the YM2612 is not behind the TMSS gate"
        );
        assert!(!bus.locked_up(), "and it freezes nothing");

        // The even lane of the PSG pair is not the port, so it is dropped as ever
        // - without a lock-up, which is the one thing that would make it part of
        // the gate.
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        request_the_z80_bus(&mut bus);
        release_the_z80_reset(&mut bus);
        bus.write_byte(0xA07F10, 0x5A);
        assert!(
            psg_untouched(&bus),
            "the even lane reaches nothing, as ever"
        );
        assert!(!bus.locked_up(), "only the PSG lane itself freezes the 68k");
    }

    /// 0xA14101 bit 0 is "what sits behind the cartridge slot": 1 the cartridge, 0
    /// the TMSS boot ROM (segaretro "TradeMark Security System"; Genesis Plus GX's
    /// `gen_bankswitch_w` takes `data & 1` the same way and reads it back in
    /// `gen_bankswitch_r`). This emulator ships no boot ROM, so the cart stays
    /// mapped whatever the bit says and the bit is only stored - and only a TMSS
    /// console has the register to store it in (BlastEm ignores the write without
    /// TMSS, as it does the security register).
    #[test]
    fn tmss_rom_select_bit_is_stored() {
        let mut bus = Bus::new(Vec::new(), WITH_TMSS);
        assert!(!bus.tmss_cart_selected, "power on asks for the boot ROM");

        bus.write_byte(TMSS_ROM_SELECT + 1, 0xFF);
        assert!(bus.tmss_cart_selected, "bit 0 of the byte written");
        bus.write_byte(TMSS_ROM_SELECT, 0x02);
        assert!(!bus.tmss_cart_selected, "only bit 0 means anything");

        bus.write_byte(TMSS_ROM_SELECT + 1, 0x01);
        bus.reset_devices();
        assert!(
            !bus.tmss_cart_selected,
            "RESET puts the boot ROM back, as Genesis Plus GX maps it at 0x000000 on reset"
        );

        let mut plain = Bus::new(Vec::new(), NO_TMSS);
        plain.write_byte(TMSS_ROM_SELECT + 1, 0x01);
        assert!(!plain.tmss_cart_selected, "no TMSS, no ROM select");
    }

    /// The acceptance criterion "default config keeps TMSS off", end to end: the
    /// version byte [`crate::Config::default`] produces claims no TMSS, so a bus
    /// built the way [`Bus::new`] tells its caller to leaves the VDP open from the
    /// first access.
    #[test]
    fn default_config_leaves_tmss_off() {
        let config = crate::Config::default();
        let version = crate::Region::Usa.version_register(config.tmss);
        assert_eq!(
            version & TMSS_VERSION_MASK,
            0,
            "bit 0 of the version is TMSS"
        );

        let mut bus = Bus::new(Vec::new(), version);
        touch_vdp(&mut bus);
        assert!(!bus.locked_up(), "a default console does not lock its VDP");
    }

    // Save RAM: the header at 0x1B0, the window it declares inside the cartridge
    // area, the `0xA130F1` switch in front of it, and the view the front end
    // persists. The decoding itself is `cart::sram`'s own tests; these are the
    // ones about what the bus does with it.

    /// A ROM image `len` bytes long declaring save RAM the way plutiedev "Saving
    /// progress with SRAM" tells authors to: `RA`, the type byte `kind`, the
    /// blank, and the window. Every other byte of the image is `fill`, so a test
    /// can tell ROM data from save data at a glance.
    fn sram_rom(len: usize, kind: u8, start: u32, end: u32, fill: u8) -> Vec<u8> {
        let mut rom = vec![fill; len];
        rom[0x1B0..0x1B4].copy_from_slice(&[b'R', b'A', kind, 0x20]);
        rom[0x1B4..0x1B8].copy_from_slice(&start.to_be_bytes());
        rom[0x1B8..0x1BC].copy_from_slice(&end.to_be_bytes());
        rom
    }

    /// The usual cart: the 32 KiB odd-only window of plutiedev's example, on a
    /// 2 MB image that stops short of it, so that `0xA130F1` is not needed to
    /// reach it and one test can hold that fact.
    fn odd_sram_bus() -> Bus {
        Bus::new(
            sram_rom(0x20_0000, 0xF8, 0x20_0001, 0x20_FFFF, 0x5A),
            TEST_VERSION,
        )
    }

    /// A cart whose header says nothing about save RAM has none: `sram()` is
    /// `None`, the cartridge area keeps answering as the image, and the control
    /// register has nothing to control.
    #[test]
    fn no_header_means_no_sram() {
        let image = rom_image(0x40_0000);
        let rom_byte = image[0x20_0001];
        let mut bus = Bus::new(image, TEST_VERSION);
        assert_eq!(bus.sram(), None);
        assert_eq!(bus.read_byte(0x20_0001), rom_byte);

        bus.write_byte(0xA130F1, 3);
        bus.write_byte(0x20_0001, 0x00);
        assert_eq!(bus.read_byte(0x20_0001), rom_byte, "a cart with no save");
        assert_eq!(bus.sram(), None, "has no save to map either");
        assert!(!bus.take_dirty());
    }

    /// [`rom_image`] is `i % 251` rather than zeros, so the header sniff
    /// [`Bus::new`] does runs over arbitrary bytes. Pin that nothing the tests
    /// here build decodes as save RAM: at 0x1B0 the image has 0xB5 0xB6, which is
    /// the first thing [`crate::cart::sram::SramInfo::from_extra_memory`] looks
    /// at and is not `RA`.
    #[test]
    fn no_bus_test_rom_decodes_as_sram() {
        let image = rom_image(0x200);
        assert_ne!(&image[0x1B0..0x1B2], &b"RA"[..]);
        // The lengths the bus and Z80-view tests build images at: `z80_bus` uses
        // 0x10000 of the same generator, which is long enough for the sniff to run
        // over.
        for len in [
            0, 1, 4, 0x1FF, 0x200, 0x1000, 0x1_0000, 0x20_0000, 0x40_0000,
        ] {
            let mut bus = Bus::new(rom_image(len), TEST_VERSION);
            assert!(
                bus.sram().is_none(),
                "a {len} byte image declares no save RAM"
            );
            bus.write_byte(0xA130F1, 3);
            assert!(bus.sram().is_none(), "and 0xA130F1 cannot give it one");
        }
    }

    /// The odd-only window of plutiedev's example, with no `0xA130F1` write
    /// anywhere: 0x200001 is save byte 0 and 0x200003 is save byte 1, packed
    /// rather than a stride, and the last address of the window is its last byte.
    #[test]
    fn odd_sram_byte_mapping() {
        let mut bus = odd_sram_bus();
        assert_eq!(bus.sram().map(<[u8]>::len), Some(32 * 1024));

        bus.write_byte(0x20_0001, 0xAB);
        bus.write_byte(0x20_0003, 0xCD);
        assert_eq!(bus.read_byte(0x20_0001), 0xAB);
        assert_eq!(bus.read_byte(0x20_0003), 0xCD);
        assert_eq!(&bus.sram().unwrap()[0..2], &[0xAB, 0xCD]);
        assert_eq!(
            bus.sram().unwrap()[0x7FFF],
            0xFF,
            "0x20FFFF is save byte 0x7FFF"
        );
    }

    /// An even address inside an odd-only window is the half the save RAM is not
    /// wired to. The window is the save's, so the image does not answer there
    /// either: nothing drives the bus, and a write to it does nothing.
    #[test]
    fn even_address_in_odd_sram_reads_open_bus() {
        let mut bus = odd_sram_bus();
        bus.write_byte(0x20_0001, 0xAB);
        assert!(bus.take_dirty(), "the live half took its write");
        assert_eq!(bus.read_byte(0x20_0002), OPEN_BUS);
        bus.write_byte(0x20_0002, 0x11);
        assert_eq!(bus.read_byte(0x20_0002), OPEN_BUS);
        assert_eq!(
            &bus.sram().unwrap()[0..2],
            &[0xAB, 0xFF],
            "and nothing was stored"
        );
        assert!(!bus.take_dirty(), "and nothing was changed either");

        // A word is its two bytes here, so the even address below the first save
        // byte is the open bus past the end of the image over save byte 0.
        assert_eq!(bus.read_word(0x20_0000), 0xFF_AB);
        bus.write_byte(0x20_0003, 0x34);
        assert_eq!(bus.read_word(0x20_0002), 0xFF_34);
    }

    /// A 4 MB cart fills the whole cartridge area, so its own ROM is what answers
    /// the save RAM's addresses until the program writes `0xA130F1` (plutiedev
    /// "Games larger than 2MB": "when we write 1 to it, SRAM shows up in the
    /// upper 2MB of the cartridge, and when we write 0 the upper part of the ROM
    /// shows up instead").
    #[test]
    fn a130f1_maps_sram_over_rom_for_large_rom() {
        let mut bus = Bus::new(
            sram_rom(0x40_0000, 0xF8, 0x20_0001, 0x20_FFFF, 0x5A),
            TEST_VERSION,
        );
        assert_eq!(
            bus.sram().map(<[u8]>::len),
            Some(32 * 1024),
            "the cart declares a save"
        );
        assert_eq!(
            bus.read_byte(0x20_0001),
            0x5A,
            "with the ROM in front of it"
        );

        bus.write_byte(0x20_0001, 0x11);
        assert_eq!(bus.read_byte(0x20_0001), 0x5A);
        assert!(
            !bus.take_dirty(),
            "and that write went to ROM, which is read-only"
        );

        bus.write_byte(0xA130F1, 1);
        assert_eq!(
            bus.read_byte(0x20_0001),
            0xFF,
            "switched, and nothing saved yet"
        );
        bus.write_byte(0x20_0001, 0x11);
        assert_eq!(bus.read_byte(0x20_0001), 0x11);
        assert_eq!(bus.sram().unwrap()[0], 0x11);
        assert!(bus.take_dirty());

        bus.write_byte(0xA130F1, 0);
        assert_eq!(bus.read_byte(0x20_0001), 0x5A, "and back to the ROM");
        assert_eq!(bus.sram().unwrap()[0], 0x11, "the save is still there");
    }

    /// `0xA130F0` and `0xA130F1` are the two lanes of one byte register, so they
    /// answer as one, and a 16 bit write to the pair carries its data in the byte
    /// at the odd lane - the rule of the I/O registers ([`Bus::write_io_word`]).
    /// Genesis Plus GX accepts the whole 0xA13060-0xA130F1 range for the
    /// register; nothing decodes the rest of the block.
    #[test]
    fn sram_control_is_a_pair_and_a_word_write_uses_its_odd_lane() {
        let mut bus = Bus::new(
            sram_rom(0x40_0000, 0xF8, 0x20_0001, 0x20_FFFF, 0x5A),
            TEST_VERSION,
        );
        bus.write_byte(0xA130F0, 1);
        assert_eq!(
            bus.read_byte(0x20_0001),
            0xFF,
            "the even lane is the register too"
        );
        bus.write_byte(0xA130F1, 0);
        assert_eq!(bus.read_byte(0x20_0001), 0x5A);

        bus.write_word(0xA130F0, 0x0001);
        assert_eq!(bus.read_byte(0x20_0001), 0xFF, "the odd byte of the word");
        bus.write_word(0xA130F0, 0x0100);
        assert_eq!(bus.read_byte(0x20_0001), 0x5A, "and not the even one");

        for addr in [0xA13001, 0xA13060, 0xA130F2, 0xA130FF] {
            bus.write_byte(addr, 1);
            assert_eq!(
                bus.read_byte(0x20_0001),
                0x5A,
                "{addr:#08x} is not the register"
            );
        }
    }

    /// The control register is write-only: reading it back is open bus, which is
    /// what MAME installs (a write handler for the pair, nothing for reads) and
    /// what [`stub_regions_read_open_bus_until_their_ticket`] already pins for a
    /// cart with no save RAM. Genesis Plus GX's cart registers are readable; no
    /// game reads this one back.
    #[test]
    fn sram_control_register_reads_open_bus() {
        let mut bus = odd_sram_bus();
        bus.write_byte(0xA130F1, 3);
        assert_eq!(bus.read_byte(0xA130F1), OPEN_BUS);
        assert_eq!(bus.read_byte(0xA130F0), OPEN_BUS);
        assert_eq!(bus.read_word(0xA130F0), OPEN_BUS_WORD);
    }

    /// Bit 1 of `0xA130F1` is the write protect: reads go on answering and the
    /// save stops changing. Genesis Plus GX's `mapper_sega_w` takes it from the
    /// same write as the map bit, which is how they are read here too.
    #[test]
    fn write_protect_bit_blocks_writes() {
        let mut bus = odd_sram_bus();
        bus.write_byte(0x20_0001, 0x11);
        assert!(bus.take_dirty());

        bus.write_byte(0xA130F1, 2);
        assert_eq!(bus.read_byte(0x20_0001), 0x11, "reads are unaffected");
        bus.write_byte(0x20_0001, 0x22);
        bus.write_word(0x20_0002, 0x3344);
        assert_eq!(bus.read_byte(0x20_0001), 0x11, "the byte write is refused");
        assert_eq!(
            bus.sram().unwrap()[0..2],
            [0x11, 0xFF],
            "and so is the word one"
        );
        assert!(!bus.take_dirty(), "a refused write is not a change");

        bus.write_byte(0xA130F1, 0);
        bus.write_byte(0x20_0001, 0x22);
        assert_eq!(bus.read_byte(0x20_0001), 0x22);
        assert!(bus.take_dirty());
    }

    /// The save reads 0xFF before the game has written it, through the bus as
    /// well as through [`Bus::sram`]: the choice [`crate::cart::sram::Sram::new`]
    /// documents, that emulators fill save RAM as if nothing were driving it
    /// rather than with the garbage real SRAM powers up to.
    #[test]
    fn sram_powers_up_as_ff_through_the_bus() {
        let mut bus = odd_sram_bus();
        assert_eq!(bus.read_byte(0x20_0001), 0xFF);
        assert_eq!(bus.read_word(0x20_0002), OPEN_BUS_WORD);
        assert!(bus.sram().unwrap().iter().all(|&byte| byte == 0xFF));
        assert!(!bus.take_dirty(), "and filling it in is not a change");
    }

    /// The even-only window, $F0 of segaretro's table: `start + 2n` is storage,
    /// and the odd bytes of the window are the dead half. Acceptance criterion
    /// "odd, even and word layouts".
    #[test]
    fn even_sram_layout() {
        let mut bus = Bus::new(
            sram_rom(0x20_0000, 0xF0, 0x20_0000, 0x20_FFFF, 0x5A),
            TEST_VERSION,
        );
        assert_eq!(bus.sram().map(<[u8]>::len), Some(32 * 1024));

        bus.write_byte(0x20_0000, 0xAA);
        bus.write_byte(0x20_0002, 0xBB);
        assert_eq!(bus.read_byte(0x20_0000), 0xAA);
        assert_eq!(bus.read_byte(0x20_0002), 0xBB);
        assert_eq!(&bus.sram().unwrap()[0..2], &[0xAA, 0xBB]);
        assert_eq!(
            bus.read_byte(0x20_0001),
            OPEN_BUS,
            "the odd half of the window is dead"
        );
        bus.write_byte(0x20_0001, 0x11);
        assert_eq!(
            &bus.sram().unwrap()[0..2],
            &[0xAA, 0xBB],
            "and takes no write"
        );

        // The window starts at 0x200000, so the image ends where it begins.
        assert_eq!(bus.read_byte(0x1F_FFFF), 0x5A);
    }

    /// The 16-bit window, $E0: every byte of it is storage, in address order, and
    /// a word is a word like anywhere else on the bus.
    #[test]
    fn word_sram_layout() {
        let mut bus = Bus::new(
            sram_rom(0x20_0000, 0xE0, 0x20_0000, 0x20_1FFF, 0x5A),
            TEST_VERSION,
        );
        assert_eq!(bus.sram().map(<[u8]>::len), Some(8 * 1024));

        bus.write_byte(0x20_0000, 0xAA);
        bus.write_byte(0x20_0001, 0xBB);
        assert_eq!(bus.read_byte(0x20_0000), 0xAA);
        assert_eq!(bus.read_byte(0x20_0001), 0xBB);
        assert_eq!(
            bus.read_word(0x20_0000),
            0xAA_BB,
            "big endian, as every word here is"
        );

        bus.write_word(0x20_0002, 0xCC_DD);
        assert_eq!(&bus.sram().unwrap()[0..4], &[0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(bus.read_word(0x20_0002), 0xCC_DD);
    }

    /// `dirty` is the front end's reason to persist: set by any write the save
    /// takes, cleared by `take_dirty` alone, and set by nothing else at all.
    #[test]
    fn sram_dirty_flag_tracks_accepted_writes() {
        let mut bus = odd_sram_bus();
        assert!(!bus.take_dirty());

        bus.read_byte(0x20_0001);
        bus.read_word(0x20_0002);
        assert!(!bus.take_dirty(), "reads are not changes");

        bus.write_byte(0x20_0001, 0x11);
        assert!(bus.take_dirty());
        assert!(
            !bus.take_dirty(),
            "and reading back what was written is not either"
        );
        bus.write_byte(0x20_0001, 0x11);
        assert!(
            bus.take_dirty(),
            "even a write of the byte that was already there"
        );

        bus.load_sram(&vec![0x00; 32 * 1024]);
        assert!(!bus.take_dirty(), "a restore is not a change to persist");

        bus.write_byte(0x20_0002, 0x11);
        assert!(
            !bus.take_dirty(),
            "nor is a write to the dead half of the window"
        );

        bus.write_byte(0xA130F1, 2);
        bus.write_byte(0x20_0001, 0x22);
        assert!(!bus.take_dirty(), "nor one the protect bit refused");
    }

    /// `load_sram` puts a save back where it came from. A blob of any other length
    /// is another cart's save, so it is refused whole rather than half applied,
    /// and a cart with no save RAM has nowhere for one to land.
    #[test]
    fn load_sram_restores_contents() {
        let mut bus = odd_sram_bus();
        bus.write_byte(0x20_0001, 0x11);
        let saved: Vec<u8> = bus.sram().unwrap().to_vec();
        assert_eq!(saved.len(), 32 * 1024);

        for wrong in [saved.len() - 1, saved.len() + 1, 0] {
            bus.load_sram(&vec![0x00; wrong]);
            assert_eq!(
                bus.sram().unwrap(),
                &saved[..],
                "a {wrong} byte load changed nothing"
            );
        }

        let mut restored = odd_sram_bus();
        restored.load_sram(&saved);
        assert_eq!(
            restored.read_byte(0x20_0001),
            0x11,
            "the save is where the cart reads it"
        );
        assert!(
            !restored.take_dirty(),
            "and restoring it is not a change of our own"
        );

        let mut plain = Bus::new(Vec::new(), TEST_VERSION);
        plain.load_sram(&[0xFF]);
        assert_eq!(plain.sram(), None);
        assert!(!plain.take_dirty());
    }

    /// `RESET` restarts the console, and with it the cart slot's register: the
    /// write protect is off and the map bit is 0 again. The save itself survives -
    /// `RESET` is not a power cycle, and the battery is still holding it - as does
    /// the fact that it changed.
    #[test]
    fn reset_clears_the_sram_control_register() {
        let mut bus = Bus::new(
            sram_rom(0x40_0000, 0xF8, 0x20_0001, 0x20_FFFF, 0x5A),
            TEST_VERSION,
        );
        bus.write_byte(0xA130F1, 1);
        bus.write_byte(0x20_0001, 0x11);
        bus.write_byte(0xA130F1, 3);
        bus.write_byte(0x20_0001, 0x22);
        assert_eq!(bus.read_byte(0x20_0001), 0x11);

        bus.reset_devices();
        assert_eq!(bus.read_byte(0x20_0001), 0x5A, "the map bit is back to 0");
        bus.write_byte(0xA130F1, 1);
        assert_eq!(
            bus.read_byte(0x20_0001),
            0x11,
            "the save survived the reset"
        );
        bus.write_byte(0x20_0001, 0x33);
        assert_eq!(bus.read_byte(0x20_0001), 0x33, "and it is writable again");
    }

    // The Sega mapper: an image larger than the cartridge area, the seven bank
    // registers of 0xA130F3-0xA130FF in front of it, and the pinning of slot 0.
    // The translation itself is [`crate::cart::mapper`]'s own tests; these are
    // the ones about the register block and the seam in
    // [`Bus::read_rom_byte`].

    /// The 5 MB cart of the ticket: every byte of each 512 KiB page is its own
    /// page number, so any read of it says which page answered - at the top of a
    /// page, which is what the ticket asks for, or anywhere else in one.
    ///
    /// No header, so no save RAM: the bytes at 0x1B0 are zero, which is not `RA`.
    fn banked_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x50_0000];
        for page in 0..10u8 {
            let start = usize::from(page) * 0x8_0000;
            rom[start..start + 0x8_0000].fill(page);
        }
        rom
    }

    /// The bus of the 5 MB cart, mapper and all.
    fn banked_bus() -> Bus {
        Bus::new(banked_rom(), TEST_VERSION)
    }

    /// A cart that fits the cartridge area and says nothing about the mapper has
    /// no mapper: writes to the bank registers do nothing at all, and the image
    /// keeps answering in address order.
    #[test]
    fn mapper_disabled_for_small_rom() {
        let image = rom_image(0x40_0000);
        let mut bus = Bus::new(image.clone(), TEST_VERSION);

        // The 4 MiB cart is the common case, and 0xA130F3 is where the mapper
        // would be if this cart had one.
        for (offset, addr) in (0xA130F3..=0xA130FF).step_by(2).enumerate() {
            bus.write_byte(addr, (offset + 1) as u8);
        }
        bus.write_word(0xA130FE, 0x0009);

        for addr in [0, 0x08_0000, 0x20_0001, 0x3F_FFFE, ROM_END] {
            assert_eq!(bus.read_byte(addr), image[addr as usize], "{addr:#08x}");
        }
        assert_eq!(
            bus.read_word(0x1_2340),
            u16::from_be_bytes([image[0x1_2340], image[0x1_2341]])
        );

        // And under 4 MiB, where the same is truer still.
        let small = rom_image(0x10_0000);
        let mut small_bus = Bus::new(small.clone(), TEST_VERSION);
        for addr in 0xA130F3..=0xA130FF {
            small_bus.write_byte(addr, 9);
        }
        assert_eq!(small_bus.read_byte(0x08_0000), small[0x08_0000]);
    }

    /// A 5 MB cart comes up with slot `n` on page `n`, which is the first 4 MiB
    /// read exactly as a cart that size would be: the ticket's case, `0x080000`
    /// answering 1 because page 1 starts with a 1.
    #[test]
    fn default_banks_identity() {
        let rom = banked_rom();
        let mut bus = banked_bus();

        assert_eq!(bus.read_byte(0x00_0000), 0, "slot 0 on page 0");
        assert_eq!(bus.read_byte(0x08_0000), 1, "slot 1 on page 1");
        assert_eq!(bus.read_byte(0x38_0000), 7, "slot 7 on page 7");

        // Every slot, and not just at the top of it: the translation is the
        // whole 512 KiB, so the byte within the slot survives too.
        for slot in 0..8usize {
            for offset in [0usize, 1, 0x7_FFFE, 0x7_FFFF] {
                let addr = (slot * 0x8_0000 + offset) as u32;
                assert_eq!(
                    bus.read_byte(addr),
                    rom[slot * 0x8_0000 + offset],
                    "{addr:#08x}"
                );
            }
        }
        // Words too, and across a page boundary: 0x080000 is page 1's first byte
        // followed by its second, and 0x07FFFE is the last word of page 0.
        assert_eq!(bus.read_word(0x08_0000), 0x0101);
        assert_eq!(bus.read_word(0x28_0000), 0x0505);
        assert_eq!(bus.read_byte(0x3F_FFFF), rom[0x3F_FFFF]);
    }

    /// The ticket's case: `move.b #9, (MapperBank7)`, and 0x380000 - which was
    /// page 7 - is page 9: the top megabyte of the 5 MB cart, which nothing else
    /// can reach.
    #[test]
    fn switch_slot_7_to_bank_9() {
        let mut bus = banked_bus();
        assert_eq!(bus.read_byte(0x38_0000), 7);

        bus.write_byte(0xA130FF, 9);
        assert_eq!(bus.read_byte(0x38_0000), 9, "page 9 answers slot 7 now");
        assert_eq!(
            bus.read_byte(0x3F_FFFF),
            9,
            "the whole 512 KiB moved, not just its first byte"
        );
        assert_eq!(bus.read_byte(0x30_0000), 6, "slot 6 is where it was");

        // The other six registers, each into its own slot: page 0 is what makes
        // the move visible, since identity has slot `n` on page `n`.
        for (offset, addr) in (0xA130F3..0xA130FF).step_by(2).enumerate() {
            let slot = 1 + offset as u32;
            bus.write_byte(addr, 0);
            assert_eq!(
                bus.read_byte(slot * 0x8_0000),
                0,
                "{addr:#010x} is the register of slot {slot}"
            );
        }
        assert_eq!(
            bus.read_byte(0x38_0000),
            9,
            "and slot 7 stayed where it was put"
        );
    }

    /// Slot 0 is fixed to page 0: the cart has to boot, and there is no register
    /// for it - the lane its register would be on is the save RAM control at
    /// `0xA130F0`, so every write in the block leaves slot 0 alone.
    #[test]
    fn slot_0_fixed() {
        let mut bus = banked_bus();

        for addr in (0xA130F0..=0xA130FF).step_by(2) {
            bus.write_byte(addr, 9);
            bus.write_word(addr, 9);
        }
        bus.write_byte(0xA130F1, 0xFF);

        assert_eq!(bus.read_byte(0x00_0000), 0, "slot 0 is still page 0");
        assert_eq!(bus.read_byte(0x07_FFFF), 0, "the whole of it");
        assert_eq!(bus.read_byte(0x08_0000), 9, "and slot 1 moved");
    }

    /// A bank number past the end of the image is nothing driving the bus, not a
    /// panic and not a mirror back into the cart: `0xA130FF` holds a byte, so a
    /// 5 MB cart has nine pages and 200 is not one of them.
    #[test]
    fn bank_past_end_of_image_reads_open_bus() {
        let mut bus = banked_bus();
        bus.write_byte(0xA130FF, 200);

        assert_eq!(bus.read_byte(0x38_0000), OPEN_BUS);
        assert_eq!(bus.read_word(0x3F_FFFE), OPEN_BUS_WORD);
        assert_eq!(
            bus.read_byte(0x30_0000),
            6,
            "the other slots are unaffected"
        );

        // Page 9 is the last one the cart has, and it does answer.
        bus.write_byte(0xA130FF, 9);
        assert_eq!(bus.read_byte(0x38_0000), 9);
    }

    /// A mapped page that runs off the end of the cart is the same case as a
    /// plain read above the image, and the mapper is not where the length lives:
    /// [`crate::cart::mapper::Mapper::map`] returns an offset and the image decides
    /// whether it exists, so a word straddling the end of a *mapped* page keeps
    /// the half that is in the cart - which is what the two separate lookups in
    /// [`Bus::read_rom_word`] are for.
    #[test]
    fn word_read_straddling_a_mapped_page_past_the_image_end() {
        // The 5 MB cart, cut to nine whole pages and one byte of the tenth, so
        // that page 9 - the last one slot 7 can be pointed at - ends one byte
        // inside the cart rather than on a page boundary.
        let mut image = banked_rom();
        image.truncate(0x48_0001);
        let mut bus = Bus::new(image, TEST_VERSION);
        bus.write_byte(0xA130FF, 9);

        assert_eq!(bus.read_byte(0x38_0000), 9, "page 9 answers slot 7");
        assert_eq!(
            bus.read_byte(0x38_0001),
            OPEN_BUS,
            "and one byte in it stops"
        );
        assert_eq!(
            bus.read_word(0x38_0000),
            0x09FF,
            "the word keeps the mapped half and fills the other",
        );
    }

    /// The registers go back to slot `n` on page `n` with the rest of the
    /// console's register state, because `RESET` is the 68k starting over and the
    /// cart boots from a reset on the assumption that nothing is switched.
    #[test]
    fn reset_devices_restores_the_bank_identity() {
        let mut bus = banked_bus();
        bus.write_byte(0xA130F3, 8);
        bus.write_byte(0xA130FF, 9);
        assert_eq!(bus.read_byte(0x08_0000), 8);
        assert_eq!(bus.read_byte(0x38_0000), 9);

        bus.reset_devices();
        assert_eq!(bus.read_byte(0x08_0000), 1, "slot 1 is back on page 1");
        assert_eq!(bus.read_byte(0x38_0000), 7, "and slot 7 on page 7");
    }

    /// A bank register is a byte register on the odd lane of its pair, so a word
    /// write to the even lane carries its data in the low byte - the rule of
    /// [`Bus::write_io_word`] and of [`Bus::write_cart_extra_word`] for the save
    /// RAM control - and 0xA130F2 as a word agrees with 0xA130F3 as a byte.
    #[test]
    fn word_write_to_a_bank_register_uses_its_odd_lane() {
        let mut bus = banked_bus();

        bus.write_word(0xA130F2, 0x0005);
        assert_eq!(bus.read_byte(0x08_0000), 5, "the low byte, at 0xA130F3");

        bus.write_byte(0xA130F3, 6);
        assert_eq!(
            bus.read_byte(0x08_0000),
            6,
            "the odd lane is the same register"
        );

        bus.write_word(0xA130F2, 0x0900);
        assert_eq!(
            bus.read_byte(0x08_0000),
            0,
            "and so the high byte is not the register's data"
        );

        // The save RAM control keeps its pair: a word write to 0xA130F0 is a
        // write to 0xA130F1, and neither is a bank register.
        bus.write_word(0xA130F0, 0x0009);
        assert_eq!(bus.read_byte(0x08_0000), 0, "unchanged by the SRAM pair");
    }

    /// The mapper is at the seam in [`Bus::read_rom_byte`] and nowhere else, so a
    /// cart with a save RAM window and a mapper - which no shipped cart is, since
    /// the carts big enough for the mapper declare no save - keeps the save in
    /// front of the banks rather than behind them.
    #[test]
    fn sram_keeps_priority_over_the_mapper() {
        // 5 MB of image, and a header at 0x1B0 declaring the odd window of
        // plutiedev's example inside it.
        let mut rom = banked_rom();
        rom[0x1B0..0x1B4].copy_from_slice(&[b'R', b'A', 0xF8, 0x20]);
        rom[0x1B4..0x1B8].copy_from_slice(&0x20_0001u32.to_be_bytes());
        rom[0x1B8..0x1BC].copy_from_slice(&0x20_FFFFu32.to_be_bytes());
        let mut bus = Bus::new(rom, TEST_VERSION);

        // Slot 4 is 0x200000-0x27FFFF, so 0x200001 is where the save window and
        // a bank of the mapper overlap. With the image reaching past the window -
        // as any image over 4 MB must - the save is not mapped by default, and
        // the page answers.
        assert_eq!(bus.read_byte(0x20_0001), 4, "identity: slot 4 on page 4");
        bus.write_byte(0xA130F9, 8);
        assert_eq!(bus.read_byte(0x20_0001), 8, "and the mapper's answer");

        bus.write_byte(0xA130F1, 1);
        assert_eq!(
            bus.read_byte(0x20_0001),
            0xFF,
            "the save is in front of the bank the mapper is on"
        );
        assert_eq!(
            bus.read_byte(0x20_0000),
            8,
            "and only in front of the window it declared"
        );
        bus.write_byte(0x20_0001, 0x77);
        assert_eq!(bus.read_byte(0x20_0001), 0x77);
        assert_eq!(bus.sram().unwrap()[0], 0x77, "stored in the save");
        assert!(bus.take_dirty());

        bus.write_byte(0xA130F1, 0);
        assert_eq!(
            bus.read_byte(0x20_0001),
            8,
            "and the bank is back, where the mapper put it"
        );
    }

    // Radica: the plug-n-play boards of #492, whose bank latch is set by the
    // read that names it. The translation arithmetic is
    // [`crate::cart::mapper`]'s own tests; these are the ones about the seam in
    // [`Bus::read_cart_extra_byte`] and the read's answer.

    /// A synthetic Radica unit: a SEGA header, the patched reset vector
    /// `(0x3F << 16) | 0x0300`, and the launcher page's own vector table at
    /// `0x3F0000`, the fingerprint [`Mapper::detect`] keys on. Every byte of
    /// each 64 KiB block is that block's number, so a read names the flash byte
    /// it reached.
    fn radica_cart() -> Vec<u8> {
        let mut rom = vec![0u8; 0x40_0000];
        for block in 0..64u8 {
            let size = 64 * 1024;
            rom[usize::from(block) * size..(usize::from(block) + 1) * size].fill(block);
        }
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[4..8].copy_from_slice(&0x3F_0300u32.to_be_bytes());
        rom[0x3F_0000..0x3F_0008]
            .copy_from_slice(&[0x00, 0xFF, 0xFF, 0xF0, 0x00, 0x00, 0x04, 0x00]);
        rom
    }

    /// A read of the `$A13000` block is what banks a Radica board, and the read
    /// itself answers 0 (MAME `read_a13`): the two boot stubs' `MOVE.W
    /// $A130xx,D0` reads and discards it, and `$000400` then answers the byte
    /// the bank shifted in rather than the one the image has there.
    #[test]
    fn a_radica_read_banks_the_board_and_answers_zero() {
        let rom = radica_cart();
        let mut bus = Bus::new(rom.clone(), TEST_VERSION);

        // Bank 0: the plain image, which is what `$000400` answers before any
        // read of the block - block 0's fill byte.
        assert_eq!(bus.read_byte(0x00_0400), 0, "identity before any latch");

        // The unit cart 1's boot read: banks $3F in, and the answer is 0, not
        // open bus and not a register value.
        assert_eq!(bus.read_byte(0xA1_307E), 0x00);
        assert_eq!(
            bus.read_byte(0x00_0400),
            0x3F,
            "the launcher page shifted in"
        );
        assert_eq!(bus.read_word(0x00_0400), 0x3F3F, "and it wraps the window");
        // The shift runs off the end of the flash and comes back around it:
        // MAME's `read` masks with the flash size.
        assert_eq!(bus.read_byte(0x01_0000), 0x00);

        // A word read latches the same way, which is what cart 2's boot does:
        // `MOVE.W $A13078,D0`.
        assert_eq!(bus.read_word(0xA1_3078), 0x0000);
        assert_eq!(bus.read_byte(0x00_0400), 0x3C, "banked by the word read");

        // The block's upper half folds into the same 64 banks.
        assert_eq!(bus.read_byte(0xA1_30FE), 0x00);
        assert_eq!(bus.read_byte(0x00_0400), 0x3F);
    }

    /// A cart that is not a Radica cart keeps the block at open bus, and the
    /// read moves nothing: the latch is the cart's board, not a console decode.
    #[test]
    fn a_non_radica_read_of_the_block_is_open_bus_and_banks_nothing() {
        let image = radica_cart();
        // Cut the image down to a plain 4 MiB cart by putting the reset vector
        // back where a game's entry is: detection fails, the block is open bus.
        let mut plain = image.clone();
        plain[4..8].copy_from_slice(&0x0000_04B0u32.to_be_bytes());
        let mut bus = Bus::new(plain, TEST_VERSION);

        assert_eq!(bus.read_byte(0xA1_307E), OPEN_BUS);
        assert_eq!(bus.read_word(0xA1_307E), OPEN_BUS_WORD);
        assert_eq!(
            bus.read_byte(0x00_0400),
            image[0x00_0400],
            "and the image did not move"
        );
    }

    /// A reset puts a Radica board's latch back on bank 0 with the rest of the
    /// register state: the patched reset vector has to reach the launcher
    /// again, which only bank 0's plain image can do.
    #[test]
    fn reset_devices_banks_a_radica_board_zero() {
        let mut bus = Bus::new(radica_cart(), TEST_VERSION);
        bus.read_byte(0xA1_307E);
        assert_eq!(bus.read_byte(0x00_0400), 0x3F, "banked by the read");

        bus.reset_devices();
        assert_eq!(
            bus.read_byte(0x00_0400),
            0x00,
            "bank 0, the plain image again"
        );
    }

    // Realtec: a 512 KiB cart paged over itself by three registers out in the
    // expansion slot. The slice arithmetic is [`crate::cart::mapper`]'s own tests;
    // these are the ones about the decode in [`Bus::write_unmapped_byte`] and the
    // fact that a cart without Realtec never sees it.

    /// A 512 KiB Realtec cart: each 8 KiB block filled with its own number, plus
    /// the `SEGA` at `0x7E100` that says the image is paged.
    fn realtec_cart() -> Vec<u8> {
        let mut rom = vec![0u8; 512 * 1024];
        for block in 0..64u8 {
            let size = 8 * 1024;
            rom[usize::from(block) * size..(usize::from(block) + 1) * size].fill(block);
        }
        rom[0x7E100..0x7E104].copy_from_slice(b"SEGA");
        rom
    }

    /// A Realtec cart answers its own header at `0x000100` before anything is
    /// written, because the window is the image's last 8 KiB mirrored - and the
    /// registers that move it are in the expansion slot, not the `0xA13000` block.
    #[test]
    fn a_realtec_cart_is_paged_from_the_expansion_slot() {
        let mut bus = Bus::new(realtec_cart(), TEST_VERSION);
        assert_eq!(bus.read_byte(0x00_0100), b'S', "its own header, mirrored");

        // 128 KiB at 256 KiB: block 32 answers the bottom of the window.
        bus.write_byte(0x40_2000, 1);
        bus.write_byte(0x40_4000, 2);
        assert_eq!(bus.read_byte(0x00_0000), 32);
        assert_eq!(
            bus.read_byte(0x02_0000),
            32,
            "mirrored, not banked per slot"
        );

        // A write to 0xA130F3, the Sega mapper's first bank register, does nothing
        // to a Realtec cart: the two carts are decoded apart.
        bus.write_byte(0xA130F3, 5);
        assert_eq!(bus.read_byte(0x00_0000), 32, "unchanged");
    }

    /// The Realtec registers are read on the even lane and take a word write from
    /// its high byte - the opposite of the `0xA13000` block, which keeps its
    /// registers on the odd lane. Getting this wrong moves every write one byte.
    #[test]
    fn realtec_registers_answer_the_even_lane_and_the_high_byte() {
        let mut bus = Bus::new(realtec_cart(), TEST_VERSION);

        bus.write_byte(0x40_2001, 1);
        assert_eq!(
            bus.read_byte(0x00_0000),
            63,
            "an odd lane is not a register, so nothing paged"
        );

        // A word write to the start register takes its value from the high byte:
        // 0x0200 is a write of 2, which is 2 * 128 KiB, and 32 blocks in.
        bus.write_word(0x40_4000, 0x0200);
        assert_eq!(
            bus.read_byte(0x00_0000),
            32,
            "the high byte of a word write is the register"
        );

        // And the low byte of that same word is not: 0x00FF would have left the
        // start where it was, so a write whose value is in the low lane does nothing.
        let mut bus = Bus::new(realtec_cart(), TEST_VERSION);
        bus.write_word(0x40_4000, 0x0002);
        assert_eq!(
            bus.read_byte(0x00_0000),
            63,
            "the low byte of a word write is not the register"
        );
    }

    /// A cart that is not Realtec drops writes to the expansion slot, so the new
    /// decode cannot disturb an ordinary cart: `0x400000` is the expansion slot and
    /// answers nothing at all.
    #[test]
    fn an_ordinary_cart_ignores_the_realtec_registers() {
        let mut rom = realtec_cart();
        rom[0x7E100..0x7E104].copy_from_slice(b"sega");
        let mut bus = Bus::new(rom, TEST_VERSION);

        assert_eq!(
            bus.read_byte(0x00_0100),
            0,
            "no mirror: the image answers itself"
        );
        bus.write_byte(0x40_2000, 1);
        bus.write_byte(0x40_4000, 2);
        assert_eq!(bus.read_byte(0x00_0000), 0, "and the write went nowhere");
    }
    // The serial EEPROM: the other save shape the cartridge slot answers, with
    // its two lines on bits of the cart data bus. The chip's own protocol is
    // `cart::eeprom`'s tests; these are about what the bus does with it.

    /// A cart whose save is a serial EEPROM: an `$E8` and the single address
    /// 0x200001, which is what a cart that means it declares, on an image `len`
    /// bytes long with `serial` for its name.
    fn eeprom_rom(len: usize, serial: &str) -> Vec<u8> {
        let mut rom = vec![0u8; len];
        rom[0x180..0x180 + serial.len()].copy_from_slice(serial.as_bytes());
        rom[0x1B0..0x1B4].copy_from_slice(&[b'R', b'A', 0xE8, 0x20]);
        rom[0x1B4..0x1B8].copy_from_slice(&0x20_0001u32.to_be_bytes());
        rom[0x1B8..0x1BC].copy_from_slice(&0x20_0001u32.to_be_bytes());
        rom
    }

    /// A J-CART-family cart the way the real dumps look: a serial, a checksum
    /// and a boot longword, and **no** extra-memory record - the board's
    /// windows are its own, which is exactly why the family is matched by the
    /// table rather than by the header.
    fn jcart_rom(serial: &str, checksum: u16, boot: u32) -> Vec<u8> {
        let mut rom = vec![0u8; 0x10_0000];
        rom[0x180..0x180 + serial.len()].copy_from_slice(serial.as_bytes());
        rom[0x18E..0x190].copy_from_slice(&checksum.to_be_bytes());
        rom[0..4].copy_from_slice(&boot.to_be_bytes());
        rom
    }

    /// A cart board's two lines, walked through the cart slot one access at a
    /// time: which address carries which line, at which bit, and which bit of the
    /// answer the chip drives. Every cart that declares a window declares the odd
    /// byte 0x200001, and only Acclaim's 32 Mbit board puts the clock somewhere
    /// else.
    struct EepromLines {
        scl_addr: u32,
        scl: u8,
        sda_addr: u32,
        sda: u8,
        out: u8,
        /// The byte last written to each half of the cart's pair, so that moving
        /// one line leaves the other where it was.
        written: [u8; 2],
        /// The address the line is read back from, which is the data address
        /// for every board but the Codemasters one, whose answer lives a
        /// quadrant away.
        out_addr: u32,
    }

    impl EepromLines {
        fn new(scl_addr: u32, scl: u8, sda_addr: u32, sda: u8, out: u8) -> Self {
            Self {
                scl_addr,
                scl,
                sda_addr,
                sda,
                out,
                written: [0xFF; 2],
                out_addr: sda_addr,
            }
        }

        /// Sega's boards: D1 is the clock and D0 the data, on the odd byte of the
        /// pair, and the answer comes back on D0.
        fn sega() -> Self {
            Self::new(0x20_0001, 2, 0x20_0001, 1, 1)
        }

        /// The Acclaim 16 Mbit board, which is the same until the answer, on D1.
        fn acclaim_16m() -> Self {
            Self::new(0x20_0001, 2, 0x20_0001, 1, 2)
        }

        /// The Acclaim 32 Mbit board: D0 of the even byte is the clock and D0 of
        /// the odd byte is the data.
        fn acclaim_32m() -> Self {
            Self::new(0x20_0000, 1, 0x20_0001, 1, 1)
        }

        /// The Codemasters J-CART board: both lines on either byte lane of the
        /// write quadrant - this walks them from $300007 - and the answer read
        /// back on bit 7 of the odd byte at the top of the next quadrant.
        fn codemasters() -> Self {
            let mut lines = Self::new(0x30_0007, 2, 0x30_0007, 1, 0x80);
            lines.out_addr = 0x38_0001;
            lines
        }

        fn scl(&mut self, bus: &mut Bus, level: bool) {
            let (addr, mask) = (self.scl_addr, self.scl);
            self.drive(bus, addr, mask, level);
        }

        fn sda(&mut self, bus: &mut Bus, level: bool) {
            let (addr, mask) = (self.sda_addr, self.sda);
            self.drive(bus, addr, mask, level);
        }

        fn drive(&mut self, bus: &mut Bus, addr: u32, mask: u8, level: bool) {
            let byte = &mut self.written[(addr & 1) as usize];
            *byte = if level { *byte | mask } else { *byte & !mask };
            bus.write_byte(addr, *byte);
        }

        fn start(&mut self, bus: &mut Bus) {
            self.scl(bus, true);
            self.sda(bus, true);
            self.sda(bus, false);
            self.scl(bus, false);
        }

        fn stop(&mut self, bus: &mut Bus) {
            self.sda(bus, false);
            self.scl(bus, true);
            self.sda(bus, true);
        }

        fn clock(&mut self, bus: &mut Bus, level: bool) -> bool {
            self.sda(bus, level);
            self.scl(bus, true);
            let answer = bus.read_byte(self.out_addr) & self.out != 0;
            self.scl(bus, false);
            answer
        }

        fn send(&mut self, bus: &mut Bus, byte: u8) -> bool {
            for bit in (0..8).rev() {
                self.clock(bus, byte & (1 << bit) != 0);
            }
            !self.clock(bus, true)
        }

        fn recv(&mut self, bus: &mut Bus, ack: bool) -> u8 {
            let mut byte = 0u8;
            for bit in (0..8).rev() {
                if self.clock(bus, true) {
                    byte |= 1 << bit;
                }
            }
            self.clock(bus, !ack);
            byte
        }
    }

    /// The save of a cart with a serial EEPROM is the chip's array, through the
    /// one view of a cart's save that [`Bus::sram`] is: [`Bus::load_sram`] puts
    /// one back and [`Bus::take_dirty`] says when the game has changed it.
    #[test]
    fn eeprom_contents_exposed_via_sram_api() {
        let mut bus = Bus::new(eeprom_rom(0x20_0000, "GM T-123456-00"), TEST_VERSION);
        // 128 bytes, because a cart that declares `$E8` and nothing else has the
        // X24C01, and the array is the size of the chip.
        assert_eq!(bus.sram().map(<[u8]>::len), Some(128));
        assert!(bus.sram().unwrap().iter().all(|&byte| byte == 0xFF));

        let mut lines = EepromLines::sega();
        // 0x5A at word 1: the word address and the write bit in the first byte,
        // which is the whole of what the X24C01 takes as addressing.
        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0x02), "the chip acknowledges");
        assert!(lines.send(&mut bus, 0x5A));
        lines.stop(&mut bus);
        assert_eq!(bus.sram().unwrap()[1], 0x5A, "and it is the save");
        assert!(bus.take_dirty(), "which the front end is told about");
        assert!(!bus.take_dirty(), "once, and once only");

        // And what it stored reads back through the bus as well as through the
        // view: word 1 again, this time with the read bit.
        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0x03));
        assert_eq!(lines.recv(&mut bus, false), 0x5A);
        lines.stop(&mut bus);

        bus.load_sram(&[0x11; 128]);
        assert!(bus.sram().unwrap().iter().all(|&byte| byte == 0x11));
        assert!(!bus.take_dirty(), "and restoring is not a change");
    }

    /// The chip's line is what the window answers, in front of the bank the mapper
    /// has switched in and of the image under it - which is the one thing a cart
    /// that is too big for the cartridge area *and* has a save on two bits of its
    /// bus can get wrong.
    #[test]
    fn eeprom_keeps_priority_over_the_sram_and_mapper() {
        // A 5 MB cart, its pages filled with their own numbers, declaring an
        // EEPROM at 0x200001.
        let mut rom = vec![0u8; 0x50_0000];
        for page in 0..10u8 {
            let start = usize::from(page) * 0x8_0000;
            rom[start..start + 0x8_0000].fill(page);
        }
        rom[0x180..0x18E].copy_from_slice(b"GM T-123456-00");
        rom[0x1B0..0x1B4].copy_from_slice(&[b'R', b'A', 0xE8, 0x20]);
        rom[0x1B4..0x1B8].copy_from_slice(&0x20_0001u32.to_be_bytes());
        rom[0x1B8..0x1BC].copy_from_slice(&0x20_0001u32.to_be_bytes());
        let mut bus = Bus::new(rom, TEST_VERSION);
        // One save and not two: `$E8` is not save RAM, so `0xA130F1` has nothing
        // to map and the chip's 128 bytes are what `sram()` reports.
        assert_eq!(bus.sram().map(<[u8]>::len), Some(128));

        // Slot 4 is 0x200000-0x27FFFF, so the window lies over a bank of the
        // mapper: the image answers 4 there, and 8 once the bank is switched.
        assert_eq!(bus.read_byte(0x20_0000), 4);
        bus.write_byte(0xA130F9, 8);
        assert_eq!(bus.read_byte(0x20_0000), 8, "and the mapper moved");

        // The chip owns the one bit it drives, over the bank's own 8, and it owns
        // nothing else: the byte below the window is the mapper's alone.
        let mut lines = EepromLines::sega();
        lines.sda(&mut bus, false);
        assert_eq!(bus.read_byte(0x20_0001) & 1, 0, "the line is held low");
        assert_eq!(bus.read_byte(0x20_0001) & !1, 8, "over the bank under it");
        assert_eq!(
            bus.read_byte(0x20_0000),
            8,
            "and no other cart address is touched by it"
        );
    }

    /// A cart's word accesses are its two byte accesses, which is what lets a
    /// board that puts its two lines on two halves of the bus work at all, and
    /// what makes the low half of a word write the half that counts on a board
    /// that looks at neither.
    #[test]
    fn eeprom_word_accesses_are_two_byte_accesses() {
        let mut rom = eeprom_rom(0x20_0004, "GM T-081326 00");
        rom[0x20_0000] = 0x4E;
        let mut bus = Bus::new(rom, TEST_VERSION);
        // NBA Jam is on Acclaim's 16 Mbit board: the X24C02, and the answer on D1.
        assert_eq!(bus.sram().map(<[u8]>::len), Some(256));
        let mut lines = EepromLines::acclaim_16m();

        // A word write to the pair drives both halves, and the same byte in both
        // of them leaves the lines where the board would have put them with the
        // one access it is.
        bus.write_word(0x20_0000, 0x0303);
        bus.write_word(0x20_0000, 0x0302);
        lines.scl(&mut bus, false);
        // Which is a start condition - D0 fell while D1 was up - so the chip is
        // listening, and the bytes that follow a byte at a time are a write. The
        // X24C02 takes mode 2 addressing: the device address, then the word.
        assert!(lines.send(&mut bus, 0xA0), "the device address");
        assert!(lines.send(&mut bus, 0x02), "and word 2");
        assert!(lines.send(&mut bus, 0xC3));
        lines.stop(&mut bus);
        assert_eq!(bus.sram().unwrap()[2], 0xC3);

        // And the word read of the pair is its two byte reads: the even half is
        // the image's own byte, and the chip's line is the one bit of the odd
        // half its board reads back on.
        let word = bus.read_word(0x20_0000);
        assert_eq!(word >> 8, 0x4E, "the even half is the image's, untouched");
        assert_eq!(
            (word as u8) & 2,
            2,
            "and the line, high since the stop, is in the odd half's D1"
        );
    }

    /// The 32 Mbit board's split, the one mapping that puts the two lines on two
    /// different addresses: D0 of the even byte is the clock, D0 of the odd byte
    /// is the data, and a whole transfer walks the split.
    #[test]
    fn acclaim_32m_board_splits_the_two_lines_over_the_bus() {
        let mut bus = Bus::new(eeprom_rom(0x20_0000, "GM T-081276 00"), TEST_VERSION);
        assert_eq!(bus.sram().map(<[u8]>::len), Some(256), "the 24C02");

        // The board's read-back rides on the window latch its word write
        // arms, and the games arm it at the top of every EEPROM operation and
        // restore it on the way out (NBA Jam TE's wrapper, `move.w #0,(A0)`
        // at entry and `move.w #1,(A0)` at exit): walk the board the way a
        // game does, latch first.
        bus.write_word(0x20_0000, 0x0000);
        let mut lines = EepromLines::acclaim_32m();
        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0xA0), "the device address");
        assert!(lines.send(&mut bus, 0x05), "and word 5");
        assert!(lines.send(&mut bus, 0x5C));
        lines.stop(&mut bus);
        assert_eq!(bus.sram().unwrap()[5], 0x5C);

        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0xA0));
        assert!(lines.send(&mut bus, 0x05));
        lines.stop(&mut bus);
        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0xA1), "and the same word, reading");
        assert_eq!(lines.recv(&mut bus, false), 0x5C);
        lines.stop(&mut bus);
    }

    /// On the bus, the latch is the word write and nothing else: it arms and
    /// restores the quadrant's read window, it does not drive the two lines
    /// the way a word write decomposed into byte writes would, and a word
    /// write outside the quadrant is the ordinary two byte writes it always
    /// was (Genesis Plus GX's `mapper_acclaim_32M_write16` against its
    /// `mapper_i2c_generic_read8`).
    #[test]
    fn acclaim_32m_word_write_is_the_window_latch() {
        let mut rom = eeprom_rom(0x20_0004, "GM T-081276 00");
        rom[0x20_0002] = 0xA4;
        let mut bus = Bus::new(rom, TEST_VERSION);
        assert_eq!(bus.read_byte(0x20_0002), 0xA4, "the image byte, ROM window");

        // Arm: bit 0 clear. The line is idle-high, so every byte in the
        // quadrant now carries it in bit 0.
        bus.write_word(0x20_0000, 0x0000);
        assert_eq!(bus.read_byte(0x20_0002), 0xA5);
        assert_eq!(bus.read_byte(0x20_0000), 0x01);

        // And the word write drove no line: were it the two byte writes of a
        // board without the latch, the odd half's $00 would have pulled /SDA
        // down and this would read 0xA4 again.
        bus.write_word(0x20_0000, 0x0002);
        assert_eq!(bus.read_byte(0x20_0002), 0xA5, "still armed, lines high");

        // A byte write to the pair moves a line and never the latch.
        bus.write_byte(0x20_0001, 0x00);
        assert_eq!(bus.read_byte(0x20_0002), 0xA4, "the line, low, in bit 0");
        bus.write_byte(0x20_0001, 0x01);

        // Restore: bit 0 set, and the quadrant is the image again.
        bus.write_word(0x20_0000, 0x0001);
        assert_eq!(bus.read_byte(0x20_0002), 0xA4);
        assert_eq!(bus.read_byte(0x20_0000), 0x00);

        // Outside the quadrant the word write is no latch: it decomposes as
        // it always has, and the window it finds there stays where it was.
        bus.write_word(0x20_0000, 0x0000);
        bus.write_word(0x10_0000, 0x0000);
        assert_eq!(bus.read_byte(0x20_0002), 0xA5, "still armed");
    }

    /// The J-CART register answers the top quadrant for the carts that carry
    /// it, both lanes, as a word and as bytes, and nothing outside it moves:
    /// the register is $380000-$3FFFFF and nothing else (Genesis Plus GX's
    /// `mapper_i2c_jcart_init`, "serial EEPROM (read) & J-CART (read/write)
    /// mapped to $380000-$3fffff").
    #[test]
    fn the_jcart_register_answers_the_top_quadrant() {
        let mut bus = Bus::new(
            jcart_rom("GM T-120096-50", 0x7E50, 0xFFFF_FFFC),
            TEST_VERSION,
        );
        // Port 2 in the even lane, bit 14 tied low; port 5 in the odd lane
        // with TH in bit 6 and the line high since nothing has pulled it.
        assert_eq!(bus.read_byte(0x38_0000), 0x3F);
        assert_eq!(bus.read_byte(0x38_0001), 0xFF);
        assert_eq!(bus.read_byte(0x3F_FFFE), 0x3F, "the whole quadrant");
        assert_eq!(bus.read_byte(0x3F_FFFF), 0xFF);
        // A word read is its two byte reads, which is GPGX's read16 answer:
        // `(eeprom_i2c_out() << 7) | jcart_read(address)`.
        assert_eq!(bus.read_word(0x38_0000), 0x3FFF);
        // Below the quadrant nothing changed: the image, and open bus past it.
        assert_eq!(bus.read_byte(0x37_FFFF), 0xFF, "open bus, the 1 MiB image");
    }

    /// The ticket's ownership test, one direction: a write to the J-CART
    /// register moves TH, and the EEPROM's line is not disturbed by it - bit 7
    /// stays released, and the chip took no byte.
    #[test]
    fn a_jcart_write_moves_th_and_leaves_the_lines_alone() {
        let mut bus = Bus::new(
            jcart_rom("GM T-120096-50", 0x7E50, 0xFFFF_FFFC),
            TEST_VERSION,
        );
        assert_eq!(
            bus.read_byte(0x38_0001),
            0xFF,
            "TH high, line released high"
        );

        // Bit 0 of the byte is TH, from either lane (GPGX's write8 answers
        // both; BlastEm's only the odd one - a word write settles the same).
        bus.write_byte(0x38_0001, 0x00);
        assert_eq!(bus.read_byte(0x38_0001), 0xBF, "TH down, line still high");
        bus.write_byte(0x38_0000, 0x01);
        assert_eq!(bus.read_byte(0x38_0001), 0xFF, "TH up again, even lane");
        // A word write is its two byte writes: the even half's bit 0 is word
        // bit 8, the odd half's is word bit 0, and the odd half is written
        // last - so the word settles at GPGX's `jcart_write` answer, bit 0 of
        // the word, one transient aside (the documented behaviour of every
        // Lane::Both board, `docs/eeprom-games.md`).
        bus.write_word(0x38_0000, 0x0100);
        assert_eq!(bus.read_byte(0x38_0001), 0xBF, "word bit 0 low: TH down");
        bus.write_word(0x38_0000, 0x0000);
        assert_eq!(bus.read_byte(0x38_0001), 0xBF, "word bit 0 is TH, and 0");
        bus.write_word(0x38_0000, 0x0001);
        assert_eq!(bus.read_byte(0x38_0001), 0xFF, "word bit 0 set, TH up");

        // And the line never moved: bit 7 is high through every answer above,
        // and the chip stored nothing.
        assert!(!bus.take_dirty());
    }

    /// The ticket's ownership test, the other direction: an EEPROM clock/data
    /// write drives the lines and does not move TH. Bit 6 of the register's
    /// answer is where the register's state is, bit 7 where the chip's is, and
    /// neither write touches the other's bit.
    #[test]
    fn an_eeprom_line_write_does_not_move_th() {
        let mut bus = Bus::new(
            jcart_rom("GM T-120096-50", 0x7E50, 0xFFFF_FFFC),
            TEST_VERSION,
        );
        // TH down first, so the test cannot pass by a line write coincidentally
        // restoring it.
        bus.write_byte(0x38_0001, 0x00);
        assert_eq!(bus.read_byte(0x38_0001), 0xBF);

        // A start condition at $300001: D0 falls while D1 is up, and the chip
        // is listening. The register sees the line in bit 7 and never TH.
        let mut lines = EepromLines::codemasters();
        lines.start(&mut bus);
        assert_eq!(
            bus.read_byte(0x38_0001),
            0x3F,
            "TH down, line low: two owners, two bits"
        );

        // A byte to the chip, and a stop: the line goes back up, TH stays down.
        assert!(lines.send(&mut bus, 0xA0), "the chip acknowledges");
        assert!(lines.send(&mut bus, 0x04), "and word 4");
        assert!(lines.send(&mut bus, 0x5A), "and the byte for it");
        lines.stop(&mut bus);
        assert_eq!(bus.read_byte(0x38_0001), 0xBF, "TH never moved");
        assert!(bus.take_dirty(), "the chip took the byte");
        assert_eq!(bus.sram().unwrap()[4], 0x5A);
    }

    /// A full byte to the chip, walked through the board: the write quadrant
    /// drives the lines from any address of itself - this one from $300007 -
    /// and the answer is read back a quadrant up. This is the AT24C16 of
    /// Micro Machines 2, mode 2 addressing, on the bus.
    #[test]
    fn codemasters_line_writes_are_window_wide() {
        let mut bus = Bus::new(
            jcart_rom("GM T-120096-50", 0x7E50, 0xFFFF_FFFC),
            TEST_VERSION,
        );
        assert_eq!(bus.sram().map(<[u8]>::len), Some(2048), "the AT24C16");

        let mut lines = EepromLines::codemasters();
        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0xA0), "the device address");
        assert!(lines.send(&mut bus, 0x03), "and word 3");
        assert!(lines.send(&mut bus, 0xC3));
        lines.stop(&mut bus);
        assert_eq!(bus.sram().unwrap()[3], 0xC3);

        // Reading it back, through the answer quadrant the same way.
        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0xA0));
        assert!(lines.send(&mut bus, 0x03));
        lines.stop(&mut bus);
        lines.start(&mut bus);
        assert!(lines.send(&mut bus, 0xA1), "and the same word, reading");
        assert_eq!(lines.recv(&mut bus, false), 0xC3);
        lines.stop(&mut bus);
    }

    /// The cricket carts - `T-120106` and `T-120146` - shipped without the
    /// register (GPGX: "support for Brian Lara Cricket games without J-CART"):
    /// their chip owns the answer quadrant alone, the even lane stays the
    /// image's, and a write up there is dropped.
    #[test]
    fn the_cricket_cart_reads_its_chip_in_the_top_quadrant() {
        let mut bus = Bus::new(
            jcart_rom("GM T-120146-50", 0x9A5C, 0x00FF_FFFC),
            TEST_VERSION,
        );
        assert_eq!(bus.sram().map(<[u8]>::len), Some(8192), "the AT24C65");

        // The line, released, in bit 7 of the odd byte; the even lane is the
        // open bus past the image, untouched by any board bit.
        assert_eq!(bus.read_byte(0x38_0001), 0xFF);
        assert_eq!(bus.read_byte(0x38_0000), 0xFF);
        let mut lines = EepromLines::codemasters();
        lines.start(&mut bus);
        assert_eq!(bus.read_byte(0x38_0001), 0x7F, "the line, low, in bit 7");
        assert_eq!(bus.read_byte(0x38_0000), 0xFF, "no register on this cart");

        // A write to the answer quadrant is nothing: no register to move, and
        // the chip's write window is the quadrant below.
        bus.write_byte(0x38_0001, 0x01);
        bus.write_word(0x38_0000, 0xFFFF);
        assert_eq!(bus.read_byte(0x38_0001), 0x7F, "unchanged by the writes");
    }

    /// The two zero-serial Micro Machines carts get the chip and the register
    /// together: GPGX's checksum-plus-boot-longword match is what names them.
    #[test]
    fn an_mm_cart_gets_chip_and_ports_together() {
        let mut bus = Bus::new(
            jcart_rom("GM 00000000-00", 0x165E, 0x444E_4C44),
            TEST_VERSION,
        );
        assert_eq!(bus.sram().map(<[u8]>::len), Some(2048));
        assert_eq!(bus.read_byte(0x38_0000), 0x3F, "the register answers too");
    }

    /// The original Micro Machines (Europe) revisions, which share the zero
    /// serial and the $165E checksum with Turbo Tournament 96, are ordinary
    /// carts: the boot longword is the guard, and nothing of the board is
    /// theirs.
    #[test]
    fn the_original_micro_machines_gets_neither() {
        let mut bus = Bus::new(
            jcart_rom("GM 00000000-00", 0x165E, 0xFFFF_FFFC),
            TEST_VERSION,
        );
        assert_eq!(bus.sram(), None, "no chip");
        assert_eq!(
            bus.read_byte(0x38_0001),
            0xFF,
            "open bus past the image, as before any of this"
        );
    }

    /// A cart that declares neither save has neither, and a cart that declares an
    /// EEPROM has no `0xA130F1` switch for anything to answer with: the window
    /// keeps answering as the image, and `sram()` says nothing.
    #[test]
    fn no_eeprom_means_the_window_is_the_image() {
        let mut rom = rom_image(0x20_0004);
        rom[0x1B0..0x1B4].copy_from_slice(b"    ");
        // Set so that a chip holding the line low for an acknowledge, which is
        // what a cart that had one would do here, would be noticed.
        rom[0x20_0001] = 0xFF;
        let (image, above) = (rom[0x20_0001], rom[0x20_0003]);
        let mut bus = Bus::new(rom, TEST_VERSION);
        assert_eq!(bus.sram(), None);
        assert_eq!(bus.read_byte(0x20_0001), image);

        let mut lines = EepromLines::sega();
        lines.start(&mut bus);
        lines.send(&mut bus, 0x02);
        lines.stop(&mut bus);
        assert_eq!(bus.read_byte(0x20_0001), image, "the image answers");
        assert_eq!(bus.read_byte(0x20_0003), above);
        assert!(!bus.take_dirty());
    }

    // ---- The VDP ports on the bus (M3-09 #69) ----
    //
    // The port addresses, their mirrors and the byte rules are plutiedev "Memory
    // map" and genvdp.txt section 7 "VDP ports"; the chip behind them is
    // `crates/vdp`, reached through [`Bus::vdp`].
    //
    // Two helpers set the chip up the way a game does, through the ports rather
    // than through `Vdp::write_register`, because that is what the tests are
    // about: they only ever assert on what a port access did.

    /// One VDP register write: `100r rrrr dddd dddd` to the control port
    /// (genvdp.txt section 7 "Programming VDP registers").
    fn set_reg(bus: &mut Bus, reg: u8, value: u8) {
        let word = 0x8000 | ((u16::from(reg) & 0x1F) << 8) | u16::from(value);
        bus.write_word(0xC00004, word);
    }

    /// The two control port words that point the data port at `addr` for a VRAM
    /// write (genvdp.txt section 7: the first word is CD1-CD0 = `01` and A13-A0,
    /// the second CD5-CD2 and A15-A14).
    fn vram_write_cmd(addr: u16) -> (u16, u16) {
        (0x4000 | (addr & 0x3FFF), (addr >> 14) & 0x0003)
    }

    /// The same command with CD5 set, which is what "Setting CD5 will trigger a
    /// DMA operation" means (genvdp.txt section 7): the trigger is bit 7 of the
    /// second word.
    fn dma_cmd(addr: u16) -> (u16, u16) {
        (0x4000 | (addr & 0x3FFF), 0x0080 | ((addr >> 14) & 0x0003))
    }

    /// Point the data port at `addr` for a VRAM write, both halves through the
    /// control port at 0xC00004.
    fn vram_write_at(bus: &mut Bus, addr: u16) {
        let (first, second) = vram_write_cmd(addr);
        bus.write_word(0xC00004, first);
        bus.write_word(0xC00004, second);
    }

    /// A word read of the data port is the VDP's data port read: the VRAM word
    /// the address register points at, big endian, and R15 steps the address on
    /// for the next read (M4-03, #81).
    ///
    /// This is the read half of `word_write_data_port_reaches_vdp`, seen from
    /// 68k side: `move.w $C00000, d0` after a command word with CD1-CD0 = `00`.
    #[test]
    fn word_read_data_port_reads_vdp() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        set_reg(&mut bus, 15, 2);
        vram_write_at(&mut bus, 0x0000);
        bus.write_word(0xC00000, 0xDEAD);
        bus.write_word(0xC00000, 0xBEEF);

        // The VRAM read command for address $0000: CD1-CD0 and CD5-CD2 are all
        // zero, so both halves are the address alone.
        bus.write_word(0xC00004, 0x0000);
        bus.write_word(0xC00004, 0x0000);
        assert_eq!(bus.read_word(0xC00000), 0xDEAD);
        assert_eq!(
            bus.read_word(0xC00002),
            0xBEEF,
            "the read of a port is a data port access, so R15 moved the address on"
        );
    }

    /// A word write to the data port reaches the VRAM the code register points
    /// at, and R15 steps the address register on for the next one.
    #[test]
    fn word_write_data_port_reaches_vdp() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        set_reg(&mut bus, 15, 2);
        vram_write_at(&mut bus, 0x0000);

        bus.write_word(0xC00000, 0xDEAD);
        // Big endian into the two bytes of the word, which is what makes a
        // `move.w` of a sprite entry land in the order genvdp.txt section 15
        // documents (see `ports::vram_word_bytes`).
        assert_eq!(&bus.vdp.vram[0..2], &[0xDE, 0xAD]);

        bus.write_word(0xC00000, 0xBEEF);
        assert_eq!(
            &bus.vdp.vram[2..4],
            &[0xBE, 0xEF],
            "the write incremented the address register"
        );
    }

    /// An 8-bit write to the control port reaches the VDP as the byte in both
    /// halves of the word (genvdp.txt section 7 "8-bit port access"; Genesis Plus
    /// GX's `vdp_write_byte` calls `vdp_68k_ctrl_w(data << 8 | data)`).
    ///
    /// `0x8F` is the test value because it is only a register write once it has
    /// been duplicated: `0x8F8F` is `100r rrrr dddd dddd` for R15, while `0x008F`
    /// and `0x8F00` are half of a command word and write nothing.
    #[test]
    fn byte_write_control_duplicates_byte() {
        // Either lane of the word, and either of the two words the control port
        // is at (plutiedev "Memory map": "all VDP ports are mirrored every other
        // word, e.g. control port is at both $C00004 and $C00006").
        for addr in [0xC00004u32, 0xC00005, 0xC00006, 0xC00007] {
            let mut bus = Bus::new(Vec::new(), NO_TMSS);
            bus.write_byte(addr, 0x8F);
            assert_eq!(bus.vdp.reg(15), 0x8F, "{addr:#08x} is the control port");
        }
    }

    /// A read of the control port is the status read, not a control port write,
    /// and the status read is not free: it ends whatever command word was
    /// half-written into the latch.
    ///
    /// The value is the chip's own status register (M4-04, #82). For a VDP that
    /// has been built and never run that is `0x3608`: the d15-d10 constants the
    /// VDP drives (`0x3400`, genvdp.txt section 6), FIFO empty (d9, and a VDP
    /// that has never been written to has nothing on its write FIFO), and VB
    /// (d3) because the display is off
    /// until a game turns it on. What this pins down is the decode, the two byte
    /// lanes of the word, and the latch.
    #[test]
    fn status_read_word_and_bytes() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);

        let word = bus.read_word(0xC00004);
        assert_eq!(
            word, 0x3608,
            "the control port is the status register now, not the open bus it \
             read until #82: 0x3400 of driven constants | FIFO empty | the VB \
             bit a switched-off display reads"
        );
        assert_eq!(
            bus.read_byte(0xC00004),
            (word >> 8) as u8,
            "the even lane is the high byte of the port's word"
        );
        assert_eq!(
            bus.read_byte(0xC00005),
            (word & 0xFF) as u8,
            "the odd lane is the low byte of the same word"
        );

        // Half a command word down, then the status read every VBL handler does
        // somewhere. "It is cleared when the control port is read" (genvdp.txt
        // section 7), so the pending half is gone before the second half is sent:
        // `second` is a first half in its own right, the code register it leaves
        // is a read code, and the data port word that follows is ignored rather
        // than landing at $0004.
        set_reg(&mut bus, 15, 2);
        let (first, second) = vram_write_cmd(0x0004);
        bus.write_word(0xC00004, first);
        let _ = bus.read_word(0xC00004);
        let _ = bus.read_byte(0xC00005);
        bus.write_word(0xC00004, second);
        bus.write_word(0xC00000, 0x1234);
        assert_eq!(
            &bus.vdp.vram[4..6],
            &[0x00, 0x00],
            "a status read abandons the command word in the latch"
        );
    }

    /// The 68k-side proof of the one thing a status read does to the chip besides
    /// answering: "It is cleared when the control port is read" (genvdp.txt
    /// section 7), so the control port judges the next word from scratch again.
    ///
    /// A register write is the observable case, because "You cannot write to a
    /// VDP register if the pending flag is set to one": with the latch still set
    /// the register write is swallowed as the second half of the command, and
    /// with the status read in between it reaches the register.
    #[test]
    fn status_read_clears_the_command_latch() {
        let (first, _second) = vram_write_cmd(0x0004);

        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        bus.write_word(0xC00004, first);
        let status = bus.read_word(0xC00004);
        bus.write_word(0xC00004, 0x8F04);
        assert_eq!(
            bus.vdp.reg(15),
            4,
            "R15 was written, so the status read of {status:#06x} ended the command word"
        );

        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        bus.write_word(0xC00004, first);
        bus.write_word(0xC00004, 0x8F04);
        assert_eq!(
            bus.vdp.reg(15),
            0,
            "without the read in between 0x8F04 is the second half of the command"
        );
    }

    /// The HV counter block is read-only, reads as the counter on either lane,
    /// and is not any other port of the area.
    ///
    /// The value is the VDP's own counter since M4-05 (#83), so this is a
    /// decode test, a byte-lane test, a value test and a write-does-nothing test.
    #[test]
    fn hv_counter_read() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);

        // A VDP that has never been told where its beam is sits at the start of
        // line 0, which is 0x00 for both counters (M4-06 #84 is what walks the
        // position; see [`Vdp::set_position`]).
        assert_eq!(
            bus.read_word(0xC00008),
            0x0000,
            "not the open-bus placeholder"
        );

        // The block is 0xC00008-0xC0000F: four words of counter, all of them the
        // same one (plutiedev "Memory map" lists $C00008 and $C0000C, and every
        // port mirrors every other word).
        bus.vdp.set_position(0xEA, 640);
        for addr in [0xC00008u32, 0xC0000A, 0xC0000C, 0xC0000E] {
            let word = bus.read_word(addr);
            assert_eq!(word, 0xEA20, "{addr:#08x} is the counter");
            assert_eq!(bus.read_byte(addr), (word >> 8) as u8, "{addr:#08x}");
            assert_eq!(bus.read_byte(addr + 1), (word & 0xFF) as u8, "{addr:#08x}");
        }
        // And the two halves of the word are the two counters of genvdp.txt
        // section 5: V on the even lane, H on the odd one.
        assert_eq!(bus.read_byte(0xC00008), 0xEA, "V counter");
        assert_eq!(bus.read_byte(0xC00009), 0x20, "H counter");

        for addr in [0xC00008u32, 0xC0000C] {
            bus.write_word(addr, 0xDEAD);
            bus.write_byte(addr + 1, 0x5A);
        }
        assert!(
            bus.vdp.vram.iter().all(|&b| b == 0),
            "the counter block is not the data port"
        );
        assert_eq!(bus.vdp.reg(15), 0, "and not the control port");
        assert!(psg_untouched(&bus), "and not the PSG");
    }

    /// Which addresses of [`Region::Vdp`] are the data port.
    ///
    /// Every mirror of plutiedev "Memory map" has to reach the chip - the every
    /// other word one that lets a longword be two word accesses, and "VDP mirrors
    /// every $40 up to $DFFFFF" - and the one address that page says is not a
    /// port at all must not reach it: `$C00020-$C0003F` hangs up, which this bus
    /// answers as nothing-there until #508 models the hang. The mask that
    /// decides it, and why the one #69 suggested is not it, is on [`vdp_port`].
    #[test]
    fn mirror_address_hits_vdp() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        set_reg(&mut bus, 15, 2);
        vram_write_at(&mut bus, 0x0000);

        // Each write is one word at the address R15 has stepped to, so each
        // address below has to have been taken for the data port in turn.
        let ports: [u32; 8] = [
            0xC0_0000, // the port itself
            0xC0_0002, // its every-other-word mirror
            0xC0_0040, // the three $40 mirrors of the port and of its mirror
            0xC0_0042, 0xC0_0080, 0xC0_0100, // A8 is not part of the decode either
            0xC1_0000, // nor are A16-A18
            0xDF_FF80, // the top of the area is still the VDP: $80 of any 0x100
        ];
        for (index, addr) in ports.iter().enumerate() {
            let value = 0x0101 * (index as u16 + 1);
            bus.write_word(*addr, value);
        }
        let expected: Vec<u8> = (1..=8).flat_map(|i| [i, i]).collect();
        assert_eq!(&bus.vdp.vram[0..16], expected.as_slice(),);

        // And the hang-up corner of the same page: no device answers it, so no
        // word is the data port. The address register has stopped moving, so the
        // next two bytes of VRAM are still nothing.
        for addr in [0xC0_0020u32, 0xC0_0022, 0xC0_003E, 0xC0_0060, 0xC0_00A0] {
            bus.write_word(addr, 0xFF00);
            bus.write_byte(addr + 1, 0xFF);
        }
        assert_eq!(
            &bus.vdp.vram[16..18],
            &[0, 0],
            "0xC00020-0xC0003F is not a port of the VDP"
        );
    }

    /// The PSG port of the 68k side is the odd lane of `$C00010` and its
    /// `$C00014` mirror, and one byte of it is one write to the sound chip
    /// (plutiedev "Memory map": "PSG is normally accessed by byte, so you'll
    /// generally see $C00011 instead").
    ///
    /// The chip behind the port is the real `sn76489::Sn76489` of M5-13 (#118), so
    /// what a port write has to show is the register it landed on: `0x8n` latches
    /// channel 0's tone register with its low nibble (SMS Power, "SN76489", register
    /// writes), and the nibble says which of the four mirrors delivered it.
    #[test]
    fn psg_write_logged() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        for (nibble, lane) in [0xC00011u32, 0xC00013, 0xC00015, 0xC00017]
            .iter()
            .enumerate()
        {
            bus.write_byte(*lane, 0x80 | nibble as u8);
            assert_eq!(
                bus.audio.psg.tone_period(0),
                nibble as u16,
                "{lane:#08x} is the PSG port, and its byte arrived"
            );
        }

        // A word write to the pair is the PSG too, and the byte it sends is the
        // low one: the same byte a `move.b` at $C00011 leaves there (Genesis Plus
        // GX's `vdp_write_word` does `psg_write(cycles, data & 0xFF)`).
        bus.write_word(0xC00010, 0x002A);
        assert_eq!(
            bus.audio.psg.tone_period(0),
            0x2A3,
            "the low byte 0x2A is the data half of channel 0's tone under the latch, \n             on top of the 3 the last mirror left"
        );

        // The even lane of the pair, on its own, is nothing: the PSG only has the
        // odd byte of it (`is_psg_port`, and Genesis Plus GX's
        // `vdp_write_byte` writing its $10/$14 case `if (address & 1)`).
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        bus.write_byte(0xC00010, 0x8F);
        assert!(psg_untouched(&bus), "the even lane is not the PSG");

        // None of this is the VDP: the PSG lives in the VDP's port pair but the
        // PSG is not the VDP.
        assert!(bus.vdp.vram.iter().all(|&b| b == 0), "not the data port");
        assert_eq!(bus.vdp.reg(15), 0, "not the control port");
    }

    /// A DMA command word through the control port moves the block.
    ///
    /// This is the test that proves the bus calls `Vdp::run_dma`: a control port
    /// write on its own only sets the request flag ([`Vdp::dma_requested`]), so
    /// VRAM only holds the transferred words if the glue ran the transfer after
    /// the write. #85 says this on the method, and that the call is a no-op
    /// whenever the write asked for no DMA.
    ///
    /// "Ran" is not "finished" since #101: the transfer is timed by the VDP's
    /// access slots, so the words arrive as the beam goes round and the test has
    /// to move it.
    ///
    /// The registers are genvdp.txt section 11 and plutiedev "$93xx" to "$97xx":
    /// R1 M1 (bit 4) allows DMA, R19/R20 the word count, R21/R22/R23 the source
    /// address, and R15 is the destination's own step, set to 2 like every DMA
    /// set-up does.
    #[test]
    fn dma_68k_to_vram_via_control_write() {
        /// R19, R20, R21, R22 and R23 in the five writes they take.
        fn regs(bus: &mut Bus, from: u8, values: &[u8]) {
            for (offset, value) in values.iter().enumerate() {
                set_reg(bus, from + offset as u8, *value);
            }
        }

        // Work RAM as the source: four words at 0xE00000.
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        for (index, word) in [0x1234u16, 0x5678, 0x9ABC, 0xDEF0].iter().enumerate() {
            bus.write_word(RAM_START + 2 * index as u32, *word);
        }
        set_reg(&mut bus, 1, 0x10);
        set_reg(&mut bus, 15, 2);
        // R19-R20 = 4 words, R21-R23 = 0x000000 of the 0xE00000 block.
        regs(&mut bus, 19, &[4, 0, 0, 0, 0x70]);

        let (first, second) = dma_cmd(0x0000);
        bus.write_word(0xC00004, first);
        bus.write_word(0xC00004, second);
        assert!(
            !bus.vdp.dma_requested(),
            "the request was run rather than left standing"
        );
        assert!(
            bus.vdp.vram[0..8].iter().all(|&byte| byte == 0),
            "and it is a transfer of access slots, not of this call: nothing has landed yet (#101)"
        );
        // Eight slots for the four VRAM words, and a line of the display-off VDP
        // these tests run has 167 of them: one line of beam is the whole transfer.
        bus.vdp.advance(TEST_VDP_LINE);
        assert_eq!(
            &bus.vdp.vram[0..8],
            &[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0],
            "the four words of work RAM are in VRAM"
        );

        // The cartridge as the source, which is the other memory a DMA reads and
        // the reason the DMA's view of the bus is not just work RAM.
        let mut rom = vec![0; 0x200];
        rom[0x100..0x108].copy_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE, 0x11, 0x22, 0x33, 0x44]);
        let mut bus = Bus::new(rom, NO_TMSS);
        set_reg(&mut bus, 1, 0x10);
        set_reg(&mut bus, 15, 2);
        // R19-R20 = 4 words, R21-R23 = 0x00100: 0x80 of the block's first 512
        // bytes, so R21 (source bits 8-1) is 0x80 and the other two are 0.
        regs(&mut bus, 19, &[4, 0, 0x80, 0, 0]);

        let (first, second) = dma_cmd(0x0000);
        bus.write_word(0xC00004, first);
        bus.write_word(0xC00004, second);
        bus.vdp.advance(TEST_VDP_LINE);
        assert_eq!(
            &bus.vdp.vram[0..8],
            &[0xCA, 0xFE, 0xBA, 0xBE, 0x11, 0x22, 0x33, 0x44],
            "the four words of the cart are in VRAM"
        );
    }

    // ---- Who pays for an access (M3-16, #77) --------------------------------
    //
    // [`Bus::take_68k_stall`] and [`Bus::take_z80_stall`] are the bus's side of an
    // additive wait: what the scheduler adds to a chip's clock without running it
    // (`crate::Scheduler::charge_mclk`). [`Bus::take_68k_hold`] and
    // [`Bus::take_z80_hold`] are the other kind of time - a hold whose length is
    // measured *from the beam*, which the scheduler spends as a deadline rather
    // than as a duration (#489). These measure the time in the place it is
    // decided; the scheduler's own tests (`crate::scheduler`) hold the two together
    // on a running machine.

    /// The set-up of a 68k -> VDP transfer of `words` words from the 0xE00000
    /// block to VRAM 0, through the ports a game uses: R1 M1 allows DMA, R15 is
    /// the step, R19/R20 the length and R21-R23 the source (genvdp.txt section 11,
    /// plutiedev "$93xx" to "$97xx"), the `0x70` of R23 being address bits 23-17 of
    /// the work RAM block.
    fn start_dma(bus: &mut Bus, words: u16) {
        set_reg(bus, 1, 0x10);
        set_reg(bus, 15, 2);
        set_reg(bus, 19, words as u8);
        set_reg(bus, 20, (words >> 8) as u8);
        set_reg(bus, 21, 0);
        set_reg(bus, 22, 0);
        set_reg(bus, 23, 0x70);
        let (first, second) = dma_cmd(0x0000);
        bus.write_word(0xC00004, first);
        bus.write_word(0xC00004, second);
    }

    /// The three constants, pinned to the sentences that set them.
    ///
    /// Every other timing test in this area - [`a00000_access_adds_wait_state`],
    /// [`z80_window_access_costs_both_cpus`], and the scheduler pair
    /// `z80_bank_access_slows_z80` / `z80_bank_access_steals_68k_cycles` - works
    /// out its expectation *from* the constants. That is right for a model test and
    /// it has one consequence worth naming: none of them fails when a number
    /// changes, which is how three unquoted figures sat here through four tests
    /// that all looked like they were checking them. So this is the one place the
    /// values are stated as numbers, next to where #308 got them: Kabuto, "SEGA
    /// Mega Drive / Genesis hardware notes" v1.5, "The bus system" ("The average
    /// Z80 delay is around 3.3 Z80 cycles, whereas the average 68k delay is around
    /// 11 68k cycles"), and `docs/ARCHITECTURE.md` for the two divisors.
    ///
    /// Moving one is allowed. Moving one now means editing a line that says where
    /// the number came from, and the sound-side one says it is unquoted.
    #[test]
    fn the_bus_timing_constants_are_the_quoted_ones() {
        assert_eq!(MCLK_PER_68K, 7, "the 68000 runs at master / 7");
        assert_eq!(MCLK_PER_Z80, 15, "the Z80 runs at master / 15");
        assert_eq!(
            Z80_WINDOW_WAIT_MCLK, 50,
            "3.3 Z80 cycles is 49.5 master clocks, paid up to 50"
        );
        assert_eq!(
            Z80_WINDOW_STEAL_MCLK, 77,
            "11 68000 clocks, the same chip's loss on the same arbitration being larger"
        );
        assert_eq!(
            Z80_AREA_WAIT_MCLK, 7,
            "one 68000 clock: the count is unquoted, see the constant's doc comment"
        );
    }

    /// One 68000 access to the sound side at `0xA00000` costs one wait state beyond
    /// the access itself, and one wait state per *access*: the word forms are one
    /// trip to the slow 8-bit side rather than two (see [`Bus::read_z80_word`]), so
    /// they pay once.
    #[test]
    fn a00000_access_adds_wait_state() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);

        // Z80 RAM, the YM2612 and the bank register: the three devices of the
        // area, all three behind the same width conversion.
        for addr in [Z80_START, Z80_START + 0x4000, Z80_START + 0x6000] {
            bus.read_byte(addr);
            assert_eq!(
                bus.take_68k_stall(),
                Z80_AREA_WAIT_MCLK,
                "one wait state for a byte read of {addr:#08x}"
            );

            bus.write_byte(addr, 0x00);
            assert_eq!(
                bus.take_68k_stall(),
                Z80_AREA_WAIT_MCLK,
                "and for a byte write to {addr:#08x}"
            );

            bus.read_word(addr);
            assert_eq!(
                bus.take_68k_stall(),
                Z80_AREA_WAIT_MCLK,
                "and for the one word access {addr:#08x} is"
            );
        }

        // A 16 bit device of the same machine pays nothing, and so does the
        // cartridge: the wait state is the sound side's, not the 68000's.
        bus.read_word(RAM_START);
        assert_eq!(bus.take_68k_stall(), 0, "work RAM is a 16 bit device");
        bus.read_word(ROM_START);
        assert_eq!(bus.take_68k_stall(), 0, "and so is the cart");
    }

    /// Six back-to-back data port writes cost the 68000 nothing until the four
    /// entries of the write FIFO are taken, and what they cost after that depends
    /// on where the beam is: clearing a head entry means waiting for two free
    /// access slots, and two of the 16 an active H32 line offers is a much longer
    /// wait than two of the 167 of a blank one.
    ///
    /// The two numbers are the slot tables' (`vdp::slots`): 310 master clocks into
    /// an active line the next free slots are slot 22 and slot 30 of `ACTIVE_H32`
    /// at 440 and 600, and 310 into a blank line they are the two next slots of
    /// `BLANK_H32` at 320 and 340. The writes are made at a fixed beam either way,
    /// so the two lines differ by the schedule and nothing else.
    #[test]
    fn fifo_full_stalls_68k() {
        /// What `writes` VRAM words pushed at a beam `mclk` into `line` cost the
        /// 68000, in the master clocks the scheduler will spend.
        fn burst(line: u32, mclk: u32, writes: u32) -> u32 {
            let mut bus = Bus::new(Vec::new(), NO_TMSS);
            // Display enable, R1 bit 6: with the display off the memories are free
            // for the port the moment it asks (#99's rule), and there is no wait
            // here to measure.
            set_reg(&mut bus, 1, 0x40);
            bus.vdp.advance(line * TEST_VDP_LINE + mclk);
            vram_write_at(&mut bus, 0x0000);
            assert_eq!(bus.take_68k_stall(), 0, "a command word holds nobody");

            (0..writes)
                .map(|_| {
                    bus.write_word(0xC0_0000, 0x1234);
                    bus.take_68k_stall()
                })
                .sum()
        }

        // The FIFO is four entries deep (`vdp::fifo::FIFO_DEPTH`), and a write that
        // finds room is not held at all.
        assert_eq!(burst(0, 310, 4), 0, "four writes fit");

        let active = burst(0, 310, 6);
        let blank = burst(230, 310, 6);
        assert!(
            blank > 0,
            "the fifth and sixth writes are held even in VBlank"
        );
        assert!(
            active > 5 * blank,
            "two slots of an active line is not two slots of a blank one: {active} vs {blank}"
        );
    }

    /// The control word that starts a 68k -> VDP transfer freezes the 68000 for the
    /// whole of it (genvdp.txt section 6), and the bus charges for it on that write
    /// alone: [`vdp::Vdp::dma_68k_stall_mclk`] is the number, and the command word
    /// is the moment it is known.
    #[test]
    fn dma_command_word_charges_the_68k() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        set_reg(&mut bus, 1, 0x10);
        set_reg(&mut bus, 15, 2);
        for (reg, value) in [(19, 64), (20, 0), (21, 0), (22, 0), (23, 0x70)] {
            set_reg(&mut bus, reg, value);
            assert_eq!(bus.take_68k_stall(), 0, "a register write holds nobody");
        }

        let (first, _) = dma_cmd(0x0000);
        bus.write_word(0xC0_0004, first);
        assert_eq!(
            bus.take_68k_stall(),
            0,
            "the first half of the command does not start the transfer"
        );

        let (_, second) = dma_cmd(0x0000);
        bus.write_word(0xC0_0004, second);
        // The freeze is a hold, not an additive wait: it is the instant the last
        // access slot of the transfer arrives, counted *from the beam*
        // (`vdp::slots::Vdp::dma_mclk_to_slots`), and #489 is the ticket that says
        // the scheduler cannot spend that as a duration.
        assert_eq!(
            bus.take_68k_stall(),
            0,
            "a freeze is not an additive per-access wait"
        );
        let stall = bus.take_68k_hold();
        assert!(stall > 0, "and the second freezes the 68000 for something");
        assert_eq!(
            stall,
            bus.vdp.dma_68k_stall_mclk(),
            "the VDP's own number, and charged once"
        );
    }

    /// The freeze a 68k -> VDP transfer puts on the 68000 is one instant on the beam,
    /// so a control write that arrives while the transfer is still running records the
    /// same instant again rather than a second freeze on top of it. A cart's DMA loop
    /// does make that write, and [`Bus::vdp_control_write`] reads the level on purpose
    /// on *every* control write.
    ///
    /// Summed, the two records are the runaway of #489: the beam has not moved, so
    /// the number is the whole of the transfer twice, and each control write in a
    /// loop adds another whole transfer to a clock that is already ahead of the
    /// raster. Maxed, they are the one deadline the transfer ends at.
    #[test]
    fn two_records_of_one_transfer_are_one_hold() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        set_reg(&mut bus, 1, 0x10);
        set_reg(&mut bus, 15, 2);
        for (reg, value) in [(19, 64), (20, 0), (21, 0), (22, 0), (23, 0x70)] {
            set_reg(&mut bus, reg, value);
        }
        let (first, second) = dma_cmd(0x0000);
        bus.write_word(0xC0_0004, first);
        bus.write_word(0xC0_0004, second);
        let held = bus.vdp.dma_68k_stall_mclk();
        assert!(held > 0, "a transfer is running at an unmoved beam");

        // A register write - not a command, so not a second transfer - made while
        // the first is still running. The beam has not moved and neither has the
        // end of the transfer.
        set_reg(&mut bus, 1, 0x10);
        assert_eq!(bus.take_68k_stall(), 0, "a register write adds no wait");
        assert_eq!(
            bus.take_68k_hold(),
            held,
            "two records of one transfer are the one instant it ends at, not twice its length"
        );
    }

    /// A Z80 access to the 68k bus window costs both chips: the sound CPU pays for
    /// the width conversion and the 68000 loses the bus behind it. The Z80's own
    /// RAM, the YM2612 and the PSG are on its side of the arbitration and cost
    /// neither anything.
    #[test]
    fn z80_window_access_costs_both_cpus() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);

        for _ in 0..4 {
            Z80View::new(&mut bus).write(0x0100, 0x12);
            assert_eq!(Z80View::new(&mut bus).read(0x0100), 0x12);
            assert_eq!(bus.take_z80_stall(), 0, "sound RAM is not contention");
            assert_eq!(bus.take_68k_stall(), 0, "and it steals no 68000 clock");
        }

        // The window at 0x8000, at bank 0: the bottom of the 68k address space.
        Z80View::new(&mut bus).read(0x8000);
        assert_eq!(bus.take_z80_stall(), Z80_WINDOW_WAIT_MCLK);
        assert_eq!(bus.take_68k_stall(), Z80_WINDOW_STEAL_MCLK);

        Z80View::new(&mut bus).write(0x8000, 0x34);
        assert_eq!(bus.take_z80_stall(), Z80_WINDOW_WAIT_MCLK);
        assert_eq!(bus.take_68k_stall(), Z80_WINDOW_STEAL_MCLK);
    }

    /// The other side of [`z80_window_access_costs_both_cpus`], which #532 measured
    /// rather than assumed: over 480 frames of `Sonic & Knuckles (World)` the sound
    /// CPU made 6,825,388 accesses to its own RAM, 106,984 to the YM2612 and 1,921
    /// PSG writes, and not one of them charged the 68000 a clock. Those are the ports
    /// below, and they have to stay free for [`Z80_WINDOW_STEAL_MCLK`] to keep
    /// meaning "the 68k bus": the same cart reads the window 64,827 times across the
    /// same 480 frames, and it is the reading of the window, not any of this, that is
    /// supposed to cost the 68000 a cycle.
    #[test]
    fn z80_side_accesses_steal_nothing() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);

        // Sound RAM, the YM2612 address and data ports, the bank register, and the
        // PSG behind the VDP ports at 0x7F11.
        for addr in [0x0100u16, 0x4000, 0x4001, 0x6000, 0x7F11] {
            Z80View::new(&mut bus).read(addr);
            Z80View::new(&mut bus).write(addr, 0x2A);
            assert_eq!(bus.take_z80_stall(), 0, "{addr:#06x} is not the 68k bus");
            assert_eq!(bus.take_68k_stall(), 0, "and it steals no 68000 clock");
        }
    }

    /// The 68000's transfer owns the whole of its side of the bus, including the
    /// part the Z80 is asking through: a window access made while a 68k -> VDP DMA
    /// is running waits for the rest of it as well as paying its own T-states.
    #[test]
    fn z80_window_access_waits_for_a_running_dma() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        // The beam unmoved, so the transfer the command word started is running.
        start_dma(&mut bus, 256);
        let dma = bus.vdp.dma_68k_stall_mclk();
        assert!(dma > 0, "and it is a transfer that takes time");
        bus.take_68k_stall();
        bus.take_68k_hold();

        Z80View::new(&mut bus).read(0x8000);
        // The two kinds of time the window costs the sound CPU come back apart:
        // its own access, additive, and the rest of the 68000's transfer, which is
        // an instant on the beam and therefore a hold (#489). Their sum is the
        // figure this test used to ask the bus for in one number.
        let wait = bus.take_z80_stall();
        let held = bus.take_z80_hold();
        assert_eq!(wait, Z80_WINDOW_WAIT_MCLK, "the access's own T-states");
        assert_eq!(held, dma, "and the wait for the 68000's transfer");
        assert_eq!(
            wait + held,
            Z80_WINDOW_WAIT_MCLK + dma,
            "both together are still what the window costs"
        );
    }

    /// `RESET` resets the VDP with the rest of the console, memories and all: see
    /// [`m68k::Bus::reset_devices`] for the references that reset the chip on a
    /// `RESET` and for the work RAM that deliberately survives one.
    #[test]
    fn reset_devices_resets_the_vdp() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        set_reg(&mut bus, 15, 2);
        vram_write_at(&mut bus, 0x0000);
        bus.write_word(0xC00000, 0xDEAD);
        assert_eq!(&bus.vdp.vram[0..2], &[0xDE, 0xAD], "and not before");

        bus.reset_devices();

        assert_eq!(bus.vdp.reg(15), 0, "the registers go back to zero");
        assert!(
            bus.vdp.vram.iter().all(|&b| b == 0),
            "and so does the memory they point at"
        );
    }

    /// `RESET` restarts the VDP's beam in VBlank rather than at line 0, so whatever
    /// the restarted 68000 writes to the chip from its first instruction is written
    /// during blanking and is in time for line 0 of the first frame after the reset
    /// (#310). The console's own reset button reaches here through
    /// [`crate::Genesis::reset`], which is why it does not have to park the beam a
    /// second time.
    #[test]
    fn reset_devices_starts_the_beam_in_vblank() {
        let mut bus = Bus::new(Vec::new(), NO_TMSS);
        // Somewhere else first, so the reset has a position to change: a chip whose
        // beam has been walked a hundred lines.
        bus.vdp.advance(100 * TEST_VDP_LINE);
        assert_ne!(bus.vdp.hv_counter() >> 8, 0, "and not already there");

        bus.reset_devices();

        // The V half of the HV counter is the line the beam is on (genvdp.txt
        // section 5), and `E0` is where section 4 puts the first line of VBlank.
        assert_eq!(
            bus.vdp.hv_counter() >> 8,
            0xE0,
            "the rebuilt chip comes back on the first line of VBlank"
        );
    }
}
