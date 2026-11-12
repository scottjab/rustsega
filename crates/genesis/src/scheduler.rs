//! The master clock, and the per-line interleave of the two CPUs (M3-14a #74).
//!
//! ## One clock, and two derived positions in it
//!
//! Everything here counts in master clocks. The 68000 runs at master / 7 and the Z80
//! at master / 15 (docs/ARCHITECTURE.md, "Clocks and timing"), so an instruction that
//! costs `c` of its own clocks moves its clock by `c * 7` or `c * 15`: the
//! multiplication goes *into* the master domain and nothing is ever divided back out
//! of it. That matters because a scanline is 3420 master clocks and 3420 / 7 is
//! 488.57, so there is no whole number of 68000 clocks per line to count.
//!
//! So [`Scheduler::master_clock`] is the time the machine agrees on, advanced exactly
//! [`MASTER_CLOCKS_PER_LINE`] by each [`Scheduler::run_line`], and [`Scheduler::line`]
//! is its scanline index. `m68k_clock` and `z80_clock` are positions in the same
//! master-clock scale: how far each chip has actually run. They are never reset, at a
//! line boundary or a frame boundary, and that is where the per-line remainder lives -
//! the amount by which a chip has run past `master_clock` when the line ends is the
//! fraction of a clock the line could not pay for, and the next line starts owing it
//! nothing. Dropping it would lose 4 sevenths of a 68000 clock 262 times a frame,
//! which is what `no_drift_over_100_frames` would notice.
//!
//! `u64` master clocks never overflow: the count moves at the master clock rate, so
//! 2^64 of them is 2^64 / 53_693_175 s, about ten thousand years of running
//! (NTSC, and PAL is slower still).
//!
//! ## The VDP's position in it
//!
//! [`Bus::vdp`] is a third chip on the same master clock. [`Scheduler::sync_vdp`] walks
//! it after every slice, to `min(m68k_clock, z80_clock)` - the time the two CPUs agree
//! has passed - so the beam never runs ahead of the instructions the bus was answering,
//! and [`Scheduler::update_interrupts`] takes what the walk produced: the VDP's
//! VINT/HINT level onto the 68000's `ipl` pins and the VBlank pulse onto the Z80's
//! `/INT`. Rendering is inside [`vdp::Vdp::advance`] and never called from here.
//!
//! ## Waiting, which is also clocked
//!
//! ## The sound chips are a third pair of positions
//!
//! [`Bus::audio`] holds the YM2612 and the PSG, and neither of them runs on the
//! master clock or on either CPU's: one YM2612 sample is 1008 master clocks and
//! one PSG tick 240 of them ([`crate::audio::YM_SAMPLE_MCLK`],
//! [`crate::audio::PSG_TICK_MCLK`]). They are not stepped from here beyond being
//! told what time it is - [`crate::Audio::set_now`] before each slice, clamped to
//! the end of the line, and [`crate::Audio::advance_to`] at the end of every line -
//! because each chip runs itself up to that time when it is written to. See
//! [`crate::audio`] for why catching up is exact rather than an approximation.
//!
//! Three of the bus's timing effects are a chip being held rather than running: a
//! 68k -> VDP DMA, a data port write to a full FIFO, and an access on the wrong
//! side of the arbitration between the two CPUs. None of them changes what the
//! machine does, only what it spends, and they arrive here as master clocks
//! through [`Bus::take_68k_stall`] and [`Bus::take_z80_stall`] (M3-16, #77) to be
//! paid by [`Scheduler::charge_mclk`] - the same rule the stopped Z80 and the
//! TMSS-frozen 68k ([`Bus::locked_up`]) already follow: move the clock, do not
//! skip it, because a chip that is owed the time will be paid it back as a burst
//! of instructions the moment it is let go.

use serde::{Deserialize, Serialize};

use crate::bus::Bus;
use crate::config::Region;
use crate::z80_bus::Z80View;
use m68k::M68k;
use z80::Z80;

/// Master clocks in one scanline: 3420 of them, in both regions
/// (docs/ARCHITECTURE.md, "Clocks and timing").
pub(crate) const MASTER_CLOCKS_PER_LINE: u64 = 3420;
/// Master clocks per 68000 clock: the 68000 runs at master / 7.
pub(crate) const MASTER_CLOCKS_PER_68K: u64 = 7;
/// Master clocks per Z80 T-state: the Z80 runs at master / 15.
pub(crate) const MASTER_CLOCKS_PER_Z80: u64 = 15;
/// The most slices one line may take, which is a guard on the line loop rather than a
/// limit on it: 3420 slices is more than four times the most a line can legitimately
/// need.
///
/// The loop takes one instruction per slice, and 4 is the cheapest either chip has
/// (`NOP` on the 68000 - `ops::flow`: "every one of `NOP.json.gz`'s 8065 cases is
/// billed 4 clocks" - and `INC r` on the Z80, `crates/z80/src/alu8.rs`), so a line is
/// at most `3420 / 7 + 3420 / 15` = 717 slices. Reaching this bound means a slice
/// advanced a clock by nothing and the loop is asking the same chip again forever -
/// which is the failure mode of a stopped Z80 whose clock was left where it was, and
/// worth a panic naming the line over a test suite that hangs.
const MAX_SLICES_PER_LINE: u32 = MASTER_CLOCKS_PER_LINE as u32;
/// Scanlines in one NTSC frame.
const LINES_PER_FRAME_NTSC: u16 = 262;
/// Scanlines in one PAL frame.
pub(crate) const LINES_PER_FRAME_PAL: u16 = 313;

/// The line loop of the console: `docs/ARCHITECTURE.md`, "Clocks and timing".
///
/// `Serialize`/`Deserialize` are for the save state of M6-07 (#127): the four
/// clocks are the machine's position in its own frame, and a state that did not
/// carry them would resume a frame from its start.
#[derive(Serialize, Deserialize)]
pub struct Scheduler {
    /// Scanline now running, 0 at the top of the frame.
    pub line: u16,
    /// Master clocks elapsed at the end of the last completed line.
    pub master_clock: u64,
    m68k_clock: u64,
    z80_clock: u64,
    /// How far [`Bus::vdp`] has been advanced, in the same master clocks as the two
    /// CPU positions. It is never ahead of either of them, and it is never reset, so
    /// the VDP sees every clock the machine runs and sees it once.
    vdp_clock: u64,
    lines_per_frame: u16,
}

impl Scheduler {
    /// A scheduler for `region`: how many scanlines a frame has, and that is all the
    /// region changes about the line loop. A line is 3420 master clocks in either
    /// region, and the frequencies themselves - 53_693_175 Hz NTSC, 53_203_424 PAL -
    /// only turn master clocks into seconds, which is a host pacing question and not a
    /// line budget one (docs/ARCHITECTURE.md, "Clocks and timing").
    ///
    /// The caller hands over an already-resolved [`Region`].
    /// [`crate::Config::resolve_region`] is the only thing that turns a cartridge
    /// header into a region ([`crate::config`]), so `RegionSetting::Auto` has been
    /// decided before a scheduler exists and this never has to guess - the same reason
    /// [`Bus::new`] takes the version byte rather than a `Config`.
    pub fn new(region: Region) -> Self {
        Self {
            line: 0,
            master_clock: 0,
            m68k_clock: 0,
            z80_clock: 0,
            vdp_clock: 0,
            lines_per_frame: if region.is_pal() {
                LINES_PER_FRAME_PAL
            } else {
                LINES_PER_FRAME_NTSC
            },
        }
    }

    /// Run one scanline: `cpu` and `z80` are stepped in slices of one instruction
    /// each, whichever of the two is behind, until both have reached the end of the
    /// line - so each gets exactly the clocks the line holds it, and neither runs the
    /// whole line before the other starts (docs/ARCHITECTURE.md, "Clocks and timing").
    ///
    /// The sound CPU also obeys the two lines the 68k drives it with, taken from
    /// `bus` at every slice boundary: it runs only while [`Bus::z80_can_run`], and
    /// [`Bus::take_z80_reset`] reporting the release edge is answered with
    /// [`z80::Z80::reset`] (plutiedev, "Using the Z80").
    ///
    /// Every 68k slice stamps [`Bus::io`] with the time, since a 6-button pad measures
    /// its `TH` timeout in 68000 clocks ([`crate::Io::set_time`]). A Z80 slice does not
    /// need to: the 68000 clock does not move during one.
    ///
    /// Every path through the loop moves one of the two clocks, which is what makes it
    /// end, and [`MAX_SLICES_PER_LINE`] is the panic that says so if it turns out not
    /// to.
    pub fn run_line(&mut self, cpu: &mut M68k, z80: &mut Z80, bus: &mut Bus) {
        let line_end = self.master_clock + MASTER_CLOCKS_PER_LINE;
        let mut slices = 0_u32;

        while self.m68k_clock < line_end || self.z80_clock < line_end {
            slices += 1;
            Self::slice_bound(slices, line_end);

            // Both lines of the Z80 handshake are driven by the 68k from inside its
            // own step, so the edge is looked for at every slice boundary rather than
            // once per chip turn - and looked for whether or not the Z80 may run, the
            // way the hardware resets the chip on the release regardless of BUSACK.
            if bus.take_z80_reset() {
                z80.reset();
            }

            if self.z80_is_behind(line_end) {
                // The sound chips are told what time it is before the chip runs,
                // not after, because a write made by this instruction is charged
                // to the position it started from: an instruction cannot be
                // stopped half way (see [`crate::Audio::set_now`]).
                bus.audio.set_now(self.z80_clock.min(line_end));
                if bus.z80_can_run() {
                    Self::charge(
                        &mut self.z80_clock,
                        step_z80(z80, bus),
                        MASTER_CLOCKS_PER_Z80,
                        line_end,
                    );
                    Self::charge_mclk(&mut self.z80_clock, bus.take_z80_stall());
                } else {
                    // Stopped, but still clocked. The chip runs no instructions while
                    // the 68k holds it in reset or on its bus, yet the hardware is
                    // still being clocked, so the position moves to the end of the
                    // line: leaving it where it was would owe the Z80 those clocks and
                    // `run_line` would pay them back as a burst of instructions the
                    // moment the 68k let go.
                    self.z80_clock = line_end;
                }
            } else if bus.locked_up() {
                // The 68k is frozen by the TMSS lock-up: like the stopped Z80 it
                // runs nothing and is still clocked, so the position goes to the
                // end of the line rather than being left where it was. Skipping it
                // is what the hardware cannot be doing, and what would hand the
                // frozen chip every clock of the frame as a burst of instructions
                // at the reset that frees it (M3-16, #77).
                self.m68k_clock = line_end;
                bus.io.set_time(self.m68k_clock / MASTER_CLOCKS_PER_68K);
            } else {
                bus.audio.set_now(self.m68k_clock.min(line_end));
                let cycles = cpu.step(bus);
                Self::charge(
                    &mut self.m68k_clock,
                    cycles,
                    MASTER_CLOCKS_PER_68K,
                    line_end,
                );
                // The bus waits are paid in the same master clocks the instruction
                // was just charged in: the access is over, and it is the chip that
                // is held.
                Self::charge_mclk(&mut self.m68k_clock, bus.take_68k_stall());
                bus.io.set_time(self.m68k_clock / MASTER_CLOCKS_PER_68K);
            }

            self.sync_vdp(bus);
            Self::update_interrupts(cpu, z80, &bus.vdp);
        }

        // The line is over, so every clock it held is now payable to the sound
        // chips: run them to the end of it here rather than leaving the whole
        // line's samples for the next write or the next frame. `line_end` is the
        // exact end of the raster, and the per-slice positions above were clamped
        // to it, which is what makes a frame's sample count a whole number of
        // frame lengths rather than a function of where the instructions fell.
        bus.audio.advance_to(line_end);

        self.master_clock += MASTER_CLOCKS_PER_LINE;
        self.line = (self.line + 1) % self.lines_per_frame;
    }

    /// Restart the raster the scheduler is counting, because something outside
    /// it has restarted the beam: the console's reset button reaches
    /// [`m68k::Bus::reset_devices`], which rebuilds [`Bus::vdp`] as a chip that has
    /// never been advanced, its own clock unwound to nothing (M3-15, #76) and its
    /// beam parked back into blanking by the same reset (#310).
    ///
    /// Two positions have to follow it. [`Scheduler::line`] goes back to 0, and it
    /// is the scheduler's own count of lines since the frame boundary rather than
    /// the VDP's line number: the two are only required to wrap together, and since
    /// #310 they sit a fixed distance apart, because a console that is started puts
    /// its beam on the VBlank line the VDP raises `frame_done` on rather than at
    /// line 0. Leaving this counter where the reset left it would make the first
    /// frame after one up to a raster longer than every one after it.
    ///
    /// And `vdp_clock` goes to the time the two CPUs agree on, which is where
    /// [`Scheduler::sync_vdp`] starts from: it is a position in the master clock
    /// rather than a chip, so it is *not* reset with the VDP, and letting the new
    /// chip be walked by everything the old one had already run would hand it
    /// every clock since power on in one [`vdp::Vdp::advance`] call - a frame
    /// counter tens of thousands of lines past the VBlank it is meant to be
    /// waiting for, and a first frame after a reset that never ends.
    pub fn restart_frame(&mut self) {
        self.line = 0;
        self.vdp_clock = self.m68k_clock.min(self.z80_clock);
    }

    /// Run one whole frame: [`Scheduler::run_line`] until the VDP says the frame is
    /// drawn, and then the rest of the raster too.
    ///
    /// [`vdp::Vdp::frame_done`] is raised at VBlank start, which on NTSC is line 224
    /// of 262, and once per frame after that. Ending the frame there would make the
    /// first call 224 lines long and every later one 262 - which is not a difference
    /// the VDP can be blamed for, since it is simply answering a question about where
    /// its own beam is. So the flag is what ends the waiting, and the line counter
    /// wrapping is what ends the frame: one `run_frame` is one raster of
    /// [`Scheduler::lines_per_frame`] lines, and it takes exactly one `frame_done`
    /// with it (`run_frame_ends_at_vblank` holds the two together).
    pub fn run_frame(&mut self, cpu: &mut M68k, z80: &mut Z80, bus: &mut Bus) {
        // A frame of sound starts with the frame of picture: the buffers hold what
        // this raster produces and nothing older (see [`crate::Audio::start_frame`]).
        bus.audio.start_frame();
        let mut lines = 0_u32;
        while !bus.vdp.frame_done() {
            lines += 1;
            // The line counter is what ends the frame below, so a VDP that never
            // raises the flag would spin here forever: the same kind of guard
            // [`Scheduler::slice_bound`] is, on the outer loop instead of the inner
            // one, and a panic naming itself rather than a hung test.
            assert!(
                lines <= u32::from(self.lines_per_frame),
                "{} lines and the VDP has not raised `frame_done`",
                lines,
            );
            self.run_line(cpu, z80, bus);
        }
        while self.line != 0 {
            self.run_line(cpu, z80, bus);
        }
    }

    /// Walk the VDP to the time the two CPUs agree has passed, so that the beam and
    /// the interrupts that fall out of it never run ahead of the instructions the bus
    /// has been answering.
    ///
    /// Both CPU positions are only ever raised, so this is a monotone clock too and
    /// the subtraction cannot underflow. The amount is bounded by one slice - the
    /// call happens at every slice boundary - which is why it fits the `u32` of
    /// [`vdp::Vdp::advance`] without a thought beyond the cast.
    fn sync_vdp(&mut self, bus: &mut Bus) {
        let now = self.m68k_clock.min(self.z80_clock);
        bus.vdp.advance((now - self.vdp_clock) as u32);
        self.vdp_clock = now;
    }

    /// The two wires from the VDP to the two CPUs, re-read after every slice.
    ///
    /// `ipl` is a pin, not a queue ([`m68k::M68k::ipl`]): the 68000 samples it between
    /// instructions, so holding the VDP's current offer on it is the whole of the
    /// delivery, and [`Bus::int_ack`] - which the CPU calls as it takes the level -
    /// is what makes the next read of [`vdp::Vdp::pending_irq`] come back without it.
    ///
    /// The Z80's `/INT` is a level too ([`z80::Z80::set_int_line`]), and
    /// [`vdp::Vdp::z80_int_line`] is the pulse it is driven with: one stretch of the
    /// first line of VBlank, once per frame.
    fn update_interrupts(cpu: &mut M68k, z80: &mut Z80, vdp: &vdp::Vdp) {
        cpu.ipl = vdp.pending_irq().unwrap_or(0);
        z80.set_int_line(vdp.z80_int_line());
    }

    /// Is the sound CPU the one that is behind, so that its slice comes next? The 68k
    /// wins a tie: it is the chip that owns the arbitration the Z80 is waiting on.
    fn z80_is_behind(&self, line_end: u64) -> bool {
        self.z80_clock < line_end
            && (self.m68k_clock >= line_end || self.z80_clock < self.m68k_clock)
    }

    /// Stop a line that will never end: `slices` is how many the line finishing at
    /// `line_end` has taken, and it has run out.
    ///
    /// A guard rather than a limit - see [`MAX_SLICES_PER_LINE`] for how far past any
    /// legitimate count it is, and [`Scheduler::charge`] for why a slice that charged
    /// nothing is the only way to reach it.
    fn slice_bound(slices: u32, line_end: u64) {
        assert!(
            slices <= MAX_SLICES_PER_LINE,
            concat!(
                "{} slices and the line ending at master clock {} has not finished: ",
                "a slice advanced a CPU clock by nothing"
            ),
            slices,
            line_end,
        );
    }

    /// Move one chip's clock on by a bus wait of `mclk` master clocks.
    ///
    /// The 68000 and the Z80 are held off the bus by things they are not running:
    /// a 68k -> VDP DMA and a data port write to a full FIFO
    /// ([`vdp::Vdp::dma_68k_stall_mclk`], [`vdp::Vdp::data_port_stall_mclk`]), the
    /// wait state of an access to the sound side at `0xA00000`, and the two
    /// contended halves of a Z80 access to the 68k bus. All of it arrives here in
    /// master clocks through [`Bus::take_68k_stall`] and [`Bus::take_z80_stall`],
    /// which is what keeps the CPU cores unaware of the bus underneath them, and it
    /// is spent the way a frozen chip's time always is: the clock moves and the
    /// chip does not.
    fn charge_mclk(clock: &mut u64, mclk: u32) {
        *clock += u64::from(mclk);
    }

    /// Move one chip's clock on by a slice of `cycles` at its own rate,
    /// `masters_per_cycle`. The slice may run past `line_end`, and does: an
    /// instruction cannot be stopped half way, and what it ran past is carried into
    /// the next line rather than thrown away (see the module docs).
    ///
    /// A slice that cost nothing is taken straight to `line_end`. `run_line` only ever
    /// steps a chip that is behind the end of the line, so a slice that advanced
    /// nothing and was charged nothing would be asked again forever; letting the line
    /// finish without that chip is the only honest thing left to do with it.
    fn charge(clock: &mut u64, cycles: u32, masters_per_cycle: u64, line_end: u64) {
        let masters = u64::from(cycles).saturating_mul(masters_per_cycle);
        *clock = if masters == 0 {
            line_end
        } else {
            *clock + masters
        };
    }
}

/// One slice of the sound CPU: a view of the machine made for this slice and thrown
/// away again, which is what [`Z80View`] is for - the bank register and the sound RAM
/// live on the bus, so nothing outlives the instruction (see [`crate::z80_bus`]).
fn step_z80(z80: &mut Z80, bus: &mut Bus) -> u32 {
    let mut view = Z80View::new(bus);
    z80.step(&mut view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Z80_WINDOW_STEAL_MCLK, Z80_WINDOW_WAIT_MCLK};
    // The 68k's own view of the machine, for the writes that drive the Z80's two
    // control lines the way a program would.
    use m68k::Bus as _;

    /// A plain Genesis: no TMSS, so nothing below has to unlock a VDP to run.
    const VERSION: u8 = 0xA0;
    /// `Z80BusReq: equ $A11100` and `Z80Reset: equ $A11200` (plutiedev, "Using the
    /// Z80"): the two lines the 68k drives to stop the sound CPU.
    const Z80_BUSREQ: u32 = 0xA11_100;
    const Z80_RESET: u32 = 0xA11_200;
    /// Where the cartridge program below starts.
    const CODE: u32 = 0x00_0010;
    /// The `AF` pair a reset Z80 reports: `Z80::reset` leaves it at 0xFFFF, and the
    /// test program counts its own executions in `A`.
    const A_RESET: u8 = 0xFF;

    /// The 68k's program: `ADDQ.L #$1,D0` (`$5280`) then `BRA.B` back to it
    /// (`$60FC`, a displacement of -4 from the address of the displacement word,
    /// which `ops::flow::tests::bra_byte_backwards` pins for `$60FE`). Both are
    /// hand-assembled from M68000UM section 8 "ADDQ" and section 1 "BRA";
    /// `runs_expected_68k_cycles_per_frame` is what proves they are the pair the
    /// comments say they are.
    fn rom_image() -> Vec<u8> {
        let mut rom = vec![0_u8; 0x20];
        // Reset vector table (M68000UM section 5 "Reset"): SSP at 0, PC at 4.
        rom[0..4].copy_from_slice(&0x00FF_0000_u32.to_be_bytes());
        rom[4..8].copy_from_slice(&CODE.to_be_bytes());
        rom[0x10..0x12].copy_from_slice(&0x5280_u16.to_be_bytes());
        rom[0x12..0x14].copy_from_slice(&0x60FC_u16.to_be_bytes());
        rom
    }

    /// The console the tests run on: the 68k looping above in a cartridge image, a
    /// Z80 looping `INC A; JR -3` (`3C 18 FD`) at the bottom of sound RAM, and the
    /// Z80's reset line released so both chips have something to do.
    fn machine() -> (M68k, Z80, Bus) {
        machine_at(VERSION)
    }

    /// [`machine`] on a console whose version register reports `version`, for the
    /// one test that needs the TMSS of [`Bus`] as well as its clocks.
    fn machine_at(version: u8) -> (M68k, Z80, Bus) {
        let mut bus = Bus::new(rom_image(), version);
        // `INC A; JR -3` at $0000 of sound RAM, where a reset Z80 starts (`Z80::reset`
        // puts PC at 0, and plutiedev "Using the Z80" loads its program there).
        bus.z80.ram[0..3].copy_from_slice(&[0x3C, 0x18, 0xFD]);
        release_z80_reset(&mut bus);
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        let mut z80 = Z80::new();
        z80.reset();
        (cpu, z80, bus)
    }

    /// `move.w #$100, (Z80Reset)` - let the sound CPU go.
    fn release_z80_reset(bus: &mut Bus) {
        bus.write_word(Z80_RESET, 0x0100);
    }

    /// The 68k takes the Z80's bus: `move.w #$100, (Z80BusReq)`.
    fn hold_z80_bus(bus: &mut Bus) {
        bus.write_word(Z80_BUSREQ, 0x0100);
    }

    /// `move.w #0, (Z80BusReq)` - give it back.
    fn release_z80_bus(bus: &mut Bus) {
        bus.write_word(Z80_BUSREQ, 0x0000);
    }

    /// The Z80's `A`, which is how many `INC A`s the test program has run (mod 256).
    fn z80_a(z80: &Z80) -> u8 {
        (z80.af() >> 8) as u8
    }

    fn run_lines(s: &mut Scheduler, cpu: &mut M68k, z80: &mut Z80, bus: &mut Bus, lines: u16) {
        for _ in 0..lines {
            s.run_line(cpu, z80, bus);
        }
    }

    /// One frame of `s`'s region, by the scheduler's own rule rather than by a line
    /// count, so that every test above is really testing [`Scheduler::run_frame`].
    fn run_frame(s: &mut Scheduler, cpu: &mut M68k, z80: &mut Z80, bus: &mut Bus) {
        s.run_frame(cpu, z80, bus);
    }

    // ---- The VDP in the loop (M3-14, #75) ------------------------------------
    //
    // The 68k in these tests has one program - `BRA.B $` - and the VDP it talks to is
    // set up through the bus's own VDP port arm, which is the same `write_word` the
    // 68k would reach with a `move.w`. What is under test here is the scheduler and
    // the chip, not the 68000's instruction decoding, which `crates/m68k` owns.

    /// The VDP's two 68000 ports: `$C00000` data and `$C00004` control, both
    /// mirrored across the whole `$C00000-$DFFFFF` area (plutiedev "Memory map").
    const VDP_DATA: u32 = 0xC0_0000;
    const VDP_CONTROL: u32 = 0xC0_0004;
    /// The 68000's autovector slots for the two VDP levels: `AUTO_VECTOR_BASE + 6`
    /// is vector 30 and `+ 4` is vector 28 (`crates/m68k/src/interrupts.rs`), so
    /// the longs live at `4 * 30` and `4 * 28`.
    const VINT_VECTOR: usize = 4 * 30;
    const HINT_VECTOR: usize = 4 * 28;
    /// Where the two handlers go, clear of the init code at [`CODE`].
    const VINT_HANDLER: u32 = 0x0000_0100;
    const HINT_HANDLER: u32 = 0x0000_0110;
    /// `BRA.B $`, the loop the 68k waits in while the interrupts arrive.
    const BRA_SELF: u16 = 0x60_FE;
    /// Supervisor mode with the interrupt mask at 0. `M68k::reset` leaves the mask
    /// at 7 (M68000UM section 5 "Reset"), which would mask every VINT and HINT
    /// from the bus; these tests are about the delivery path from the VDP, which
    /// `crates/m68k/src/interrupts.rs` already covers for the mask itself.
    const MASKLESS: u16 = 0x2000;
    /// Master clocks in a VDP line, the same 3420 [`MASTER_CLOCKS_PER_LINE`] is.
    const VDP_LINE: u32 = MASTER_CLOCKS_PER_LINE as u32;
    /// The first line of VBlank: 224, the `E0h` of genvdp.txt section 4 "Vertical
    /// interrupts", which is the line `timing` raises `frame_done` on and - the
    /// NTSC V counter being one count a line up to 255 - its own V counter. This
    /// is where the console parks a beam it has just started (#310).
    const VBLANK_LINE: u16 = 224;

    /// The console `rom` runs on. The sound CPU stays in the reset [`Bus::new`]
    /// leaves it in, so these tests have the 68k and the VDP and nothing else.
    fn console(rom: Vec<u8>) -> (M68k, Z80, Bus) {
        let mut bus = Bus::new(rom, VERSION);
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        let mut z80 = Z80::new();
        z80.reset();
        (cpu, z80, bus)
    }

    /// A cartridge image for the interrupt tests: the reset table with
    /// [`VINT_HANDLER`] in the level 6 slot and [`HINT_HANDLER`] in the level 4 one,
    /// and a program of `BRA.B $` to sit in while they fire.
    ///
    /// The handler bodies are `ADDQ.L #1,D0` / `ADDQ.L #1,D1` and `RTE`
    /// (`$5280`/`$5281` and `$4E73`: the first is the encoding
    /// `runs_expected_68k_cycles_per_frame` proves, the second `decode`'s
    /// `0x4E73 => Op::Rte`). Each counts its own entries and neither saves the
    /// register it bumps, which is enough for a counter and nothing else.
    fn interrupt_rom() -> Vec<u8> {
        let mut rom = vec![0_u8; 0x200];
        rom[0..4].copy_from_slice(&0x00FF_0000_u32.to_be_bytes());
        rom[4..8].copy_from_slice(&CODE.to_be_bytes());
        rom[VINT_VECTOR..VINT_VECTOR + 4].copy_from_slice(&VINT_HANDLER.to_be_bytes());
        rom[HINT_VECTOR..HINT_VECTOR + 4].copy_from_slice(&HINT_HANDLER.to_be_bytes());
        rom[CODE as usize..CODE as usize + 2].copy_from_slice(&BRA_SELF.to_be_bytes());

        for (handler, addq) in [(VINT_HANDLER, 0x5280_u16), (HINT_HANDLER, 0x5281)] {
            let at = usize::try_from(handler).expect("a 24 bit address fits a usize");
            rom[at..at + 2].copy_from_slice(&addq.to_be_bytes());
            rom[at + 2..at + 4].copy_from_slice(&0x4E73_u16.to_be_bytes());
        }
        rom
    }

    /// [`console`] running the interrupt program, with the 68k's mask down - the
    /// only way a VDP level 4 or level 6 reaches a handler at all - and the VDP
    /// registers in `regs` written through the control port the way the 68k's own
    /// [`m68k::Bus::write_word`] would write them.
    fn interrupt_machine(regs: &[(u8, u8)]) -> (M68k, Z80, Bus) {
        let (mut cpu, z80, mut bus) = console(interrupt_rom());
        cpu.set_sr(MASKLESS);
        for &(reg, value) in regs {
            bus.write_word(
                VDP_CONTROL,
                0x8000 | (u16::from(reg) << 8) | u16::from(value),
            );
        }
        (cpu, z80, bus)
    }

    /// One frame is 262 lines of 3420 master clocks, and the 68k must have been run
    /// for all of it: 3420 / 7 is 488.57 68k clocks a line, so the line's budget can
    /// only be met by carrying the remainder into the next line rather than rounding
    /// it. The slack on the assertion is one instruction: `run_line` stops stepping a
    /// CPU once it has reached the end of the line, so the slice that got it there may
    /// overshoot, but by no more than one instruction ever.
    #[test]
    fn runs_expected_68k_cycles_per_frame() {
        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);
        run_frame(&mut s, &mut cpu, &mut z80, &mut bus);

        let frame = u64::from(s.lines_per_frame) * MASTER_CLOCKS_PER_LINE;
        assert_eq!(s.lines_per_frame, 262, "this is the NTSC machine");
        assert!(
            s.m68k_clock >= frame && s.m68k_clock - frame <= 10 * MASTER_CLOCKS_PER_68K,
            "{} master clocks of 68k, wanted {frame} plus at most one instruction",
            s.m68k_clock
        );
        // And the clock it was charged is the clock it spent: one `ADDQ.L #1,D0` and
        // the `BRA.B` back to it cost 8 + 10 = 18 68000 clocks (`ops::arith`: "ADDQ.L
        // #,Dn is the long add"; `ops::flow`: "`Bcc`/`BRA` taken 10"), so `D0` counts
        // the loop iterations the master clock pays for. A line that clamped the 68k
        // back to the line end instead of carrying its overshoot would lose two master
        // clocks a line and fit 7336 iterations into the frame instead of 7112.
        let iteration = 18 * MASTER_CLOCKS_PER_68K;
        let iterations = u64::from(cpu.d[0]) * iteration;
        assert!(
            iterations.abs_diff(s.m68k_clock) <= iteration,
            "{} loop iterations of {iteration} master clocks is not {}",
            cpu.d[0],
            s.m68k_clock
        );
    }

    /// The same for the sound CPU: 3420 / 15 is 228 T-states a line, exactly, so a
    /// Z80 that ran all of a 262 line frame has `262 * 228` T-states on the clock.
    /// One instruction of slack again, the largest the test program can produce being
    /// the 12 T-state `JR` (`crates/z80/src/flow.rs`, "JR e").
    #[test]
    fn runs_expected_z80_cycles_per_frame() {
        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);
        run_frame(&mut s, &mut cpu, &mut z80, &mut bus);

        let frame = u64::from(s.lines_per_frame) * MASTER_CLOCKS_PER_LINE;
        assert!(
            s.z80_clock >= frame && s.z80_clock - frame <= 12 * MASTER_CLOCKS_PER_Z80,
            "{} master clocks of Z80, wanted {frame} plus at most one instruction",
            s.z80_clock
        );
        assert_ne!(z80_a(&z80), A_RESET, "and the Z80 ran its program");
    }

    /// While the 68k holds the Z80's bus the chip runs no instructions, but its clock
    /// does not stop: a stopped clock would have to be caught up, and the catch-up
    /// would run the sound CPU for ten lines of instructions in one go the moment the
    /// 68k let go.
    #[test]
    fn z80_stopped_while_busreq() {
        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);
        s.run_line(&mut cpu, &mut z80, &mut bus);
        let a_ran = z80_a(&z80);
        assert_ne!(a_ran, A_RESET, "the sound CPU ran its program first");

        hold_z80_bus(&mut bus);
        run_lines(&mut s, &mut cpu, &mut z80, &mut bus, 10);
        assert_eq!(
            z80_a(&z80),
            a_ran,
            "no Z80 instruction runs while the 68k holds its bus"
        );
        assert_eq!(
            s.z80_clock, s.master_clock,
            "but its clock moved with the line, so it owes itself nothing"
        );

        release_z80_bus(&mut bus);
        let before = s.z80_clock;
        let a_before = z80_a(&z80);
        s.run_line(&mut cpu, &mut z80, &mut bus);
        let moved = s.z80_clock - before;
        assert!(
            moved <= MASTER_CLOCKS_PER_LINE + 12 * MASTER_CLOCKS_PER_Z80,
            "one line of Z80 after the release, not ten lines of catch-up: {moved}"
        );
        // 228 T-states of budget over a 16 T-state loop is fourteen `INC A`s; ten
        // lines of catch-up would be ten times that.
        let incs = z80_a(&z80).wrapping_sub(a_before);
        assert!(
            incs > 0 && incs <= 20,
            "one line of Z80 work: {incs} instructions"
        );
    }

    /// Releasing the Z80's reset line is the event that puts its `PC` back to 0
    /// (plutiedev, "Using the Z80"; Genesis Plus GX's `gen_zreset_w` calls
    /// `z80_reset()` on exactly that edge), and the bus reports it with
    /// [`Bus::take_z80_reset`] for the scheduler to answer.
    #[test]
    fn z80_reset_release_resets_pc() {
        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);
        s.run_line(&mut cpu, &mut z80, &mut bus);
        assert_ne!(z80.pc(), 0, "the program moves PC");

        // Driving reset asserted stops the chip where it stands.
        bus.write_word(Z80_RESET, 0x0000);
        let pc = z80.pc();
        let a = z80_a(&z80);
        s.run_line(&mut cpu, &mut z80, &mut bus);
        assert_eq!(z80.pc(), pc, "held in reset, it runs nothing");
        assert_eq!(z80_a(&z80), a);

        // Releasing it is the edge that resets PC. The 68k asks for the Z80's bus
        // first so that nothing runs after the reset and `PC` is still 0 to look at:
        // the two lines are independent (`Bus::z80_can_run`).
        hold_z80_bus(&mut bus);
        release_z80_reset(&mut bus);
        s.run_line(&mut cpu, &mut z80, &mut bus);
        assert_eq!(z80.pc(), 0, "the release resets PC to the reset vector");
        assert_eq!(
            z80_a(&z80),
            A_RESET,
            "the reset the scheduler took is the Z80's own, which is AF at 0xFFFF too"
        );

        // Let it have its bus back and it starts over from 0.
        release_z80_bus(&mut bus);
        s.run_line(&mut cpu, &mut z80, &mut bus);
        assert_ne!(z80.pc(), 0, "running again from the vector");
        let incs = z80_a(&z80).wrapping_sub(A_RESET);
        assert!(
            incs > 0 && incs <= 20,
            "counting from the top of the program, not from where it was stopped: {incs}"
        );
    }

    /// The line's 4 / 7 of a 68k clock has to be carried, not dropped: over 100 frames
    /// the master clock is exactly `100 * 262 * 3420`, and both CPU clocks are still
    /// within one instruction of it. Nothing here may count 68k cycles and multiply
    /// them back.
    #[test]
    fn no_drift_over_100_frames() {
        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);
        for _ in 0..100 {
            run_frame(&mut s, &mut cpu, &mut z80, &mut bus);
        }

        let expected = 100 * 262 * MASTER_CLOCKS_PER_LINE;
        assert_eq!(s.master_clock, expected, "exact master clock accounting");
        assert_eq!(
            s.line, 0,
            "and the line counter is back at the top of a frame"
        );
        assert!(
            s.m68k_clock >= expected && s.m68k_clock - expected <= 10 * MASTER_CLOCKS_PER_68K,
            "the 68k neither lost nor gained a line's clocks: {}",
            s.m68k_clock
        );
        assert!(
            s.z80_clock >= expected && s.z80_clock - expected <= 12 * MASTER_CLOCKS_PER_Z80,
            "nor did the Z80: {}",
            s.z80_clock
        );
    }

    /// PAL is the same line clock and the same 3420 master clocks a line, and 313
    /// lines to a frame instead of 262 (docs/ARCHITECTURE.md, "Clocks and timing").
    #[test]
    fn pal_has_313_lines() {
        assert_eq!(Scheduler::new(Region::Europe).lines_per_frame, 313);
        assert_eq!(Scheduler::new(Region::Japan).lines_per_frame, 262);
        assert_eq!(Scheduler::new(Region::Usa).lines_per_frame, 262);

        // And the line loop uses it, rather than it being a field nothing reads.
        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Europe);
        run_frame(&mut s, &mut cpu, &mut z80, &mut bus);
        assert_eq!(s.line, 0, "the PAL frame wraps at its 313th line");
        assert_eq!(s.master_clock, 313 * MASTER_CLOCKS_PER_LINE);
    }

    /// A slice that advanced nothing - a chip that is not executing, or a `step` that
    /// returned 0 - must move its clock to the end of the line. `run_line` only ever
    /// steps the CPU that is behind, so a slice that advances nothing and is charged
    /// nothing would be asked again forever.
    #[test]
    fn a_slice_that_advances_nothing_cannot_spin_the_line() {
        let mut clock = 100;
        Scheduler::charge(&mut clock, 0, MASTER_CLOCKS_PER_68K, 1_000);
        assert_eq!(clock, 1_000, "a 0 cycle slice goes to the line end");

        // Which is only the guard: a slice that does cost is charged its own clocks,
        // and may overshoot the line end rather than being clamped to it.
        let mut clock = 100;
        Scheduler::charge(&mut clock, 8, MASTER_CLOCKS_PER_68K, 1_000);
        assert_eq!(clock, 100 + 8 * MASTER_CLOCKS_PER_68K);
        Scheduler::charge(&mut clock, 8, MASTER_CLOCKS_PER_68K, 1_000);
        assert_eq!(clock, 100 + 16 * MASTER_CLOCKS_PER_68K, "past the line end");
    }

    /// The slice bound is a guard, so it is tested on its own: nothing in `run_line`
    /// reaches it today (see [`MAX_SLICES_PER_LINE`]), and what it guards against (a
    /// chip whose clock never moves) hangs rather than fails, which is exactly what the
    /// panic is for.
    #[test]
    #[should_panic(expected = "has not finished: a slice advanced a CPU clock by nothing")]
    fn a_line_that_makes_no_progress_panics_instead_of_spinning() {
        Scheduler::slice_bound(MAX_SLICES_PER_LINE + 1, MASTER_CLOCKS_PER_LINE);
    }

    /// A line at the bound is still legal: the check is on the count, not on the line.
    #[test]
    fn a_line_at_the_slice_bound_is_still_allowed() {
        Scheduler::slice_bound(MAX_SLICES_PER_LINE, MASTER_CLOCKS_PER_LINE);
    }

    /// The degenerate machine: a 68k with the HALT pin asserted and a Z80 still parked
    /// in reset by [`Bus::new`]. Neither chip has anything to run, and the line still
    /// has to finish.
    #[test]
    fn a_stalled_machine_still_finishes_its_line() {
        let (mut cpu, mut z80, mut bus) = machine();
        bus.write_word(Z80_RESET, 0x0000);
        cpu.halted = true;
        let mut s = Scheduler::new(Region::Usa);
        s.run_line(&mut cpu, &mut z80, &mut bus);

        assert_eq!(s.master_clock, MASTER_CLOCKS_PER_LINE);
        assert_eq!(
            s.z80_clock, MASTER_CLOCKS_PER_LINE,
            "reset, and still clocked"
        );
        assert!(
            s.m68k_clock >= MASTER_CLOCKS_PER_LINE,
            "halted, and still clocked"
        );
        assert_eq!(z80_a(&z80), A_RESET, "neither ran anything");
    }

    /// genvdp.txt section 4 "Vertical interrupts": "a level 6 interrupt will occur
    /// when the V counter is at line E0h", once per frame. With R1 bit 5 (IE0) set
    /// by the program and vector 30 aimed at a handler that counts, three frames is
    /// three counts.
    #[test]
    fn vint_reaches_68k() {
        // Register 1 = $60: display on (bit 6) and IE0 (bit 5).
        let (mut cpu, mut z80, mut bus) = interrupt_machine(&[(1, 0x60)]);
        let mut s = Scheduler::new(Region::Usa);

        for _ in 0..3 {
            s.run_frame(&mut cpu, &mut z80, &mut bus);
        }

        assert_eq!(
            cpu.d[0], 3,
            "one level 6 per frame, taken through vector 30"
        );
        assert_eq!(cpu.d[1], 0, "and no HINT: R0 bit 4 is clear");
    }

    /// genvdp.txt section 4 "Line interrupts": the counter is loaded from R10 on
    /// line 0 and decremented on every line that is not VBlank, so R10 = 15 is an
    /// interrupt every 16 lines. `timing.rs` puts the first on line 15 and the last
    /// on line 223 - the line VBlank starts on still decrements, and lines 225 to
    /// 261 reload - which is the 224 / 16 = 14 a frame holds.
    #[test]
    fn hint_fires_every_n_lines() {
        // R0 = $10 turns IE1 on and R10 = 15 is the line counter the ticket names.
        let (mut cpu, mut z80, mut bus) = interrupt_machine(&[(0, 0x10), (10, 15)]);
        let mut s = Scheduler::new(Region::Usa);

        let mut counted = Vec::new();
        for _ in 0..5 {
            s.run_frame(&mut cpu, &mut z80, &mut bus);
            counted.push(cpu.d[1]);
        }

        assert_eq!(
            counted,
            vec![14, 28, 42, 56, 70],
            "one level 4 every 16 lines of active display, and none in VBlank"
        );
        assert_eq!(cpu.d[0], 0, "and no VINT: R1 bit 5 is clear");
    }

    /// The acknowledge is the only thing that clears a VDP interrupt - "Reading the
    /// control port does not clear this bit" (genvdp.txt section 6) - so
    /// [`Bus::int_ack`] hands the level to [`vdp::Vdp::ack_irq`] and, taking both
    /// halves of it: the VDP stops offering the level, and the answer stays `None`
    /// so the 68k keeps autovectoring.
    #[test]
    fn int_ack_clears_vdp_pending() {
        let mut bus = Bus::new(Vec::new(), VERSION);
        bus.vdp.write_register(1, 0x60);
        bus.vdp.advance(225 * VDP_LINE);
        assert_eq!(
            bus.vdp.pending_irq(),
            Some(6),
            "a frame's worth of clocks, and the VINT is being offered"
        );

        assert_eq!(bus.int_ack(6), None, "autovector, as a Genesis always does");
        assert_eq!(
            bus.vdp.pending_irq(),
            None,
            "and the VDP stopped offering it"
        );

        // And the same thing through the loop: a frame in which the handler ran
        // leaves nothing pending, because the CPU acknowledged on entry.
        let (mut cpu, mut z80, mut bus) = interrupt_machine(&[(1, 0x60)]);
        let mut s = Scheduler::new(Region::Usa);
        s.run_frame(&mut cpu, &mut z80, &mut bus);
        assert_eq!(cpu.d[0], 1, "the level 6 handler ran");
        assert_eq!(
            bus.vdp.pending_irq(),
            None,
            "and entry to it cleared the VDP's flag"
        );
    }

    /// The Z80's VBlank interrupt: genvdp.txt never mentions it (see the `vdp`
    /// crate's `Z80_INT_MCLK` for what it is inferred from), but
    /// [`vdp::Vdp::z80_int_line`] is the wire and this scheduler is what drives it -
    /// so a frame gives the sound CPU exactly one interrupt, not one per instruction
    /// of the pulse that carries it.
    #[test]
    fn z80_int_on_vblank() {
        // `$0000`: `EI`, then `HALT` - and at `$38`, the IM 1 entry point, `INC A`,
        // 64 `NOP`, `EI`, `HALT`. Two choices there are doing specific work:
        //
        // - `HALT` rather than `RET` at the end. The chip is then waiting for the
        //   next pulse instead of unwinding a return address, so nothing about the
        //   stack or the address it came back to can be what the count depends on.
        // - the 64 `NOP` are not filler. `Z80_INT_MCLK` holds the line for 2560
        //   master clocks, about 170 T-states, and `/INT` is a level: a handler that
        //   reopened interrupts after 12 T-states would be re-entered several times
        //   before the VDP let go, which is exactly what this test must not do. 64
        //   `NOP` is 256 T-states, so `EI` lands after the pulse has gone and a frame
        //   is one count rather than sixty.
        let mut program = vec![0x76_u8; 0x7B];
        program[0] = 0xFB;
        program[0x38] = 0x3C;
        program[0x39..0x79].fill(0x00);
        program[0x79] = 0xFB;

        let mut bus = Bus::new(rom_image(), VERSION);
        bus.z80.ram[..program.len()].copy_from_slice(&program);
        release_z80_reset(&mut bus);
        // Releasing reset is the edge `run_line` answers with `Z80::reset`, and that
        // reset clears the interrupt mode armed just below, so take the pulse here
        // rather than leave it for the line loop to spend on a chip whose mode the
        // test has already chosen.
        assert!(
            bus.take_z80_reset(),
            "releasing reset is the pulse the scheduler answers"
        );
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        let mut z80 = Z80::new();
        z80.reset();
        // The mode a real program would ask for with `IM 1`, armed from the test
        // rather than by an opcode so this stays a test of the wire (#75 owns that)
        // and not of the ED decoder. For the record the decoder is not the obstacle:
        // `$ED $56` and `$ED $76` are `IM 1` here (`decode.rs`, and all 1000 cases of
        // the suite's `ed 56`/`ed 76` end with `im` = 1); executing one would just
        // put a second thing under test.
        z80.set_im(z80::IntMode::Mode1);

        let mut s = Scheduler::new(Region::Usa);
        let mut counted = Vec::new();
        for _ in 0..3 {
            s.run_frame(&mut cpu, &mut z80, &mut bus);
            counted.push(z80_a(&z80).wrapping_sub(A_RESET));
        }

        assert_eq!(
            counted,
            vec![1, 2, 3],
            "one Z80 interrupt per frame, not several per VBlank pulse"
        );
    }

    /// One [`Scheduler::run_frame`] is one whole raster.
    ///
    /// The VDP raises `frame_done` at VBlank start, 224 lines into a 262 line NTSC
    /// frame, and stops raising it until the next one: ending the frame there would
    /// make the first call 224 lines and every later one 262. So the flag is what
    /// ends the waiting and the rest of the raster is paid for as well, which keeps
    /// consecutive returns exactly `262 * 3420` master clocks apart and takes
    /// exactly one flag each time.
    #[test]
    fn run_frame_ends_at_vblank() {
        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);

        for frame in 1..=5_u64 {
            let before = s.master_clock;
            s.run_frame(&mut cpu, &mut z80, &mut bus);
            assert_eq!(
                s.master_clock - before,
                262 * MASTER_CLOCKS_PER_LINE,
                "frame {frame} is a whole raster of lines"
            );
            assert_eq!(s.line, 0, "and the line counter wraps with it");
            assert!(
                !bus.vdp.frame_done(),
                "frame {frame} took the VDP's flag with it rather than leaving it up"
            );
        }
    }

    /// The other start of frame: a console that parks the beam on the first line of
    /// VBlank before it runs anything (#310, `Genesis::new`) begins its rasters in
    /// blanking rather than at line 0, and a frame is still one whole raster.
    ///
    /// The VDP's line and [`Scheduler::line`] are a fixed distance apart in that
    /// machine - the scheduler counts lines since the frame boundary, the chip since
    /// VBlank - and all the two loops of [`Scheduler::run_frame`] have to keep is the
    /// wrap. Because the parked chip is parked without its frame flag up, the wait is
    /// what runs the whole raster, taking the flag the beam raises on arriving back at
    /// VBlank; the drain after it has nothing left to pay for.
    #[test]
    fn a_frame_that_starts_in_vblank_is_a_whole_raster() {
        let (mut cpu, mut z80, mut bus) = machine();
        // What `Genesis::new` does to the chip it has just built.
        bus.vdp.start_in_vblank();
        let mut s = Scheduler::new(Region::Usa);

        for frame in 1..=3_u64 {
            let before = s.master_clock;
            s.run_frame(&mut cpu, &mut z80, &mut bus);
            assert_eq!(
                s.master_clock - before,
                262 * MASTER_CLOCKS_PER_LINE,
                "frame {frame} of a machine started in VBlank is a whole raster"
            );
            assert_eq!(s.line, 0, "and its line counter wraps with it");
            assert_eq!(
                bus.vdp.hv_counter() >> 8,
                VBLANK_LINE,
                "and the beam is back at the line the frame started on"
            );
        }
    }

    /// R7's backdrop colour, set by a program writing a CRAM command and one data
    /// word, reaches the frame buffer through [`vdp::Vdp::advance`] alone - the
    /// scheduler never calls `render_line`.
    #[test]
    fn frame_buffer_updates() {
        let (mut cpu, mut z80, mut bus) = console(interrupt_rom());
        // $C000 is the first half of a command word whose CD1-CD0 make it a CRAM
        // write (genvdp.txt section 7) and $000E is CRAM entry 0 as `0000 BBB0 GGG0
        // RRR0` with R = 7: full red, which the frame buffer's `0xAABBGGRR` packing
        // is 0xFF00_00FF. Both go in over the ports, as a `move.w` would.
        bus.write_word(VDP_CONTROL, 0xC000);
        bus.write_word(VDP_DATA, 0x000E);
        let mut s = Scheduler::new(Region::Usa);
        s.run_frame(&mut cpu, &mut z80, &mut bus);

        let frame = bus.vdp.frame();
        let row_is_red = |y: usize| {
            (0..frame.width).all(|x| frame.pixels[frame.pixel_index(x, y)] == 0xFF00_00FF)
        };
        assert_eq!(frame.height, 224, "the active display, and no more");
        assert!(
            row_is_red(0) && row_is_red(100) && row_is_red(223),
            "the backdrop colour reaches every active line"
        );
        assert!(
            !row_is_red(224),
            "and no line past the active display was painted"
        );
    }

    // ---- Bus waits, on a running machine (M3-16, #77) -----------------------
    //
    // The bus decides what an access costs ([`Bus::take_68k_stall`] and
    // [`Bus::take_z80_stall`]); the line loop is what spends it, by moving a chip's
    // clock without running it. These tests hold the two together: each one
    // measures a chip's own work over a known number of lines, so a wait that was
    // never paid shows up as iterations that should not have been there, and one
    // paid twice as iterations that should have been.

    /// One `ADDQ.L #1,D0` and the `BRA.B` back to it - the loop [`machine`] runs -
    /// is 8 + 10 = 18 68000 clocks, so this is what one iteration of that loop costs
    /// in the master clocks the scheduler counts.
    const LOOP_68K: u64 = 18 * MASTER_CLOCKS_PER_68K;

    /// `bus.write_word(VDP_CONTROL, ..)` for one register, as a game's `move.w`
    /// writes it (genvdp.txt section 7 "Programming VDP registers").
    fn set_reg(bus: &mut Bus, reg: u8, value: u8) {
        bus.write_word(
            VDP_CONTROL,
            0x8000 | (u16::from(reg) & 0x1F) << 8 | u16::from(value),
        );
    }

    /// A 68k -> VDP transfer of `words` words of the 0xE00000 block to VRAM 0: R1
    /// M1 allows DMA, R19/R20 the length, R21-R23 the source, and the two command
    /// words with CD5 set are what start it (genvdp.txt section 11, plutiedev
    /// "$93xx" to "$97xx"). Started from the test because the 68k's program here is
    /// a counter, and `bus.write_word` to the control port is the `move.w` that a
    /// program would use.
    fn start_dma(bus: &mut Bus, words: u16) {
        set_reg(bus, 1, 0x10);
        set_reg(bus, 15, 2);
        set_reg(bus, 19, words as u8);
        set_reg(bus, 20, (words >> 8) as u8);
        set_reg(bus, 21, 0);
        set_reg(bus, 22, 0);
        set_reg(bus, 23, 0x70);
        // CD1-CD0 = `01` for a VRAM write at address 0, and CD5 for the transfer:
        // the second word is the one that triggers it.
        bus.write_word(VDP_CONTROL, 0x4000);
        bus.write_word(VDP_CONTROL, 0x0080);
    }

    /// How many times the 68k's loop ran over `lines` of the machine.
    fn iterations(
        s: &mut Scheduler,
        cpu: &mut M68k,
        z80: &mut Z80,
        bus: &mut Bus,
        lines: u16,
    ) -> u32 {
        let before = cpu.d[0];
        run_lines(s, cpu, z80, bus, lines);
        cpu.d[0].wrapping_sub(before)
    }

    /// `INC HL; LD A,(addr); JR -6`: one read of `addr` and one count of it round,
    /// 6 + 13 + 12 = 31 T-states (`crates/z80/src/alu16.rs` for `INC rr`, `ld8.rs`
    /// for `LD A,(nn)`, `flow.rs` for a taken `JR`). The displacement is -6 because
    /// a `JR` counts from the byte after it, which is where `-3` would land - on the
    /// operand of the load, not on the `INC`.
    fn z80_read_loop(addr: u16) -> [u8; 6] {
        let mut program = [0_u8; 6];
        program[0] = 0x23;
        program[1] = 0x3A;
        program[2..4].copy_from_slice(&addr.to_le_bytes());
        program[4] = 0x18;
        program[5] = 0xFA;
        program
    }

    /// The 68k counting out of ROM and the Z80 reading `addr` once a time round,
    /// both running: the machine the Z80 contention tests measure, the two counters
    /// being the two things a wait can take away.
    fn z80_contender(addr: u16) -> (M68k, Z80, Bus) {
        let mut bus = Bus::new(rom_image(), VERSION);
        let program = z80_read_loop(addr);
        bus.z80.ram[..program.len()].copy_from_slice(&program);
        release_z80_reset(&mut bus);
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        let mut z80 = Z80::new();
        z80.reset();
        (cpu, z80, bus)
    }

    /// One frame of [`z80_contender`]: the 68k's iterations, the Z80's, and the
    /// master clock the frame was.
    fn one_frame(addr: u16) -> (u32, u16, u64) {
        let (mut cpu, mut z80, mut bus) = z80_contender(addr);
        let mut s = Scheduler::new(Region::Usa);
        run_frame(&mut s, &mut cpu, &mut z80, &mut bus);
        (cpu.d[0], z80.hl(), s.master_clock)
    }

    /// "During 68k-to-VDP DMA the 68k is halted until the DMA ends" (genvdp.txt
    /// section 6), and [`vdp::Vdp::dma_68k_stall_mclk`] is for how long: over the
    /// same 20 lines the 68000 runs the clocks and none of its instructions while
    /// the transfer has the bus.
    ///
    /// The transfer is 1024 words, which at the 167 slots a H32 line with the
    /// display off spends on it is twelve lines and a bit - most of the twenty - so
    /// the loop's iterations go from several hundred to a handful. Time is not lost,
    /// only spent: the 68000's clock still covers the 20 lines, so what the freeze
    /// takes out of the counter is exactly the time it reports.
    #[test]
    fn dma_halts_68k_for_reported_time() {
        let lines = 20_u16;

        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);
        let free = iterations(&mut s, &mut cpu, &mut z80, &mut bus, lines);

        let (mut cpu, mut z80, mut bus) = machine();
        let mut s = Scheduler::new(Region::Usa);
        run_lines(&mut s, &mut cpu, &mut z80, &mut bus, 1);
        start_dma(&mut bus, 1024);
        // Deliberately not [`Bus::take_68k_stall`]: the scheduler is the thing that
        // has to spend it, and taking it here would spend it instead.
        // [`crate::bus::tests::dma_command_word_charges_the_68k`] is what pins the
        // number the bus records to this one.
        let reported = bus.vdp.dma_68k_stall_mclk();
        assert!(
            reported > 0,
            "the command word that starts the transfer freezes the 68000"
        );

        let held = iterations(&mut s, &mut cpu, &mut z80, &mut bus, lines);
        assert!(
            held < free / 2,
            "{held} iterations with a DMA, {free} without"
        );

        // The freeze and the loop share the line budget, so the iterations the
        // freeze cost are the freeze: `reported` master clocks, to within the one
        // instruction each of the two runs may have run past the last line.
        let lost = u64::from(free - held) * LOOP_68K;
        assert!(
            lost.abs_diff(u64::from(reported)) <= 3 * LOOP_68K,
            "{lost} master clocks of loop lost to a freeze of {reported}"
        );
        assert!(held > 0, "and the 68000 comes back when the transfer ends");
    }

    /// A Z80 reading the 68k bus window gets through less of its loop in a frame
    /// than the same Z80 reading its own RAM, by the extra T-states
    /// [`crate::bus::Z80_WINDOW_WAIT_MCLK`] charges it for the width conversion.
    #[test]
    fn z80_bank_access_slows_z80() {
        let (_, bank_reads, _) = one_frame(0x8000);
        let (_, ram_reads, frame) = one_frame(0x0100);
        assert!(
            bank_reads < ram_reads,
            "{bank_reads} reads through the window against {ram_reads} of sound RAM"
        );

        // Both against the model: the frame's master clocks over the T-states of a
        // turn round, with the wait on the 68k bus and without it.
        let loop_mclk = 31 * MASTER_CLOCKS_PER_Z80;
        let model = |wait: u64| frame / (loop_mclk + wait);
        assert!(
            u64::from(ram_reads).abs_diff(model(0)) <= 3,
            "uncontended: {ram_reads} against the model's {}",
            model(0)
        );
        assert!(
            u64::from(bank_reads).abs_diff(model(u64::from(Z80_WINDOW_WAIT_MCLK))) <= 3,
            "contended: {bank_reads} against the model's {}",
            model(u64::from(Z80_WINDOW_WAIT_MCLK))
        );
    }

    /// And the same loop's reads take 68000 iterations with them. The 68000 keeps
    /// its clock - the frame is still a frame - but loses the bus for
    /// [`crate::bus::Z80_WINDOW_STEAL_MCLK`] of it per read the sound CPU makes.
    #[test]
    fn z80_bank_access_steals_68k_cycles() {
        let (free, _, _) = one_frame(0x0100);
        let (held, accesses, _) = one_frame(0x8000);

        assert!(
            held < free,
            "{held} iterations of the 68k's loop while the Z80 hammers the window, {free} while it does not"
        );
        let lost = u64::from(free - held) * LOOP_68K;
        let expected = u64::from(accesses) * u64::from(Z80_WINDOW_STEAL_MCLK);
        assert!(
            lost.abs_diff(expected) <= 4 * LOOP_68K,
            "{lost} master clocks of 68000 lost to {accesses} window accesses of {Z80_WINDOW_STEAL_MCLK}, which is {expected}"
        );
    }

    /// A 68000 frozen by the TMSS lock-up runs nothing and is still clocked: the
    /// line ends, the beam goes on, and the chip is not owed the line back as a
    /// burst of instructions (M3-16, #77).
    #[test]
    fn a_locked_up_68k_is_frozen_and_still_clocked() {
        let (mut cpu, mut z80, mut bus) = machine_at(Region::Usa.version_register(true));

        // The first touch of a VDP held by TMSS is the lock-up, and this is the
        // `move.w $C00000, d0` that makes it.
        assert_eq!(bus.read_word(VDP_DATA), 0xFFFF, "refused, and frozen");
        assert!(bus.locked_up());

        let mut s = Scheduler::new(Region::Usa);
        s.run_line(&mut cpu, &mut z80, &mut bus);

        assert_eq!(cpu.d[0], 0, "the frozen 68000 ran no instructions");
        assert!(
            s.m68k_clock >= MASTER_CLOCKS_PER_LINE,
            "and it was still clocked: {}",
            s.m68k_clock
        );
        assert_ne!(z80_a(&z80), A_RESET, "and the rest of the machine ran");
    }
}
