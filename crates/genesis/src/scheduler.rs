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
//! ## What the line loop does not do yet
//!
//! The VDP is not in this ticket: no rendering, no HV counter, no VINT/HINT, and no
//! Z80 interrupt line. `// TODO(M3-14 #75)` marks where each of those joins, which is
//! M3-14 (#75), the VDP half of the scheduler. So is freezing the 68k on
//! [`Bus::locked_up`], which has to move the 68k's clock rather than skip it, for
//! exactly the reason the stopped Z80's clock moves.

use crate::bus::Bus;
use crate::config::Region;
use crate::z80_bus::Z80View;
use m68k::M68k;
use z80::Z80;

/// Master clocks in one scanline: 3420 of them, in both regions
/// (docs/ARCHITECTURE.md, "Clocks and timing").
const MASTER_CLOCKS_PER_LINE: u64 = 3420;
/// Master clocks per 68000 clock: the 68000 runs at master / 7.
const MASTER_CLOCKS_PER_68K: u64 = 7;
/// Master clocks per Z80 T-state: the Z80 runs at master / 15.
const MASTER_CLOCKS_PER_Z80: u64 = 15;
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
const LINES_PER_FRAME_PAL: u16 = 313;

/// The line loop of the console: `docs/ARCHITECTURE.md`, "Clocks and timing".
pub struct Scheduler {
    /// Scanline now running, 0 at the top of the frame.
    pub line: u16,
    /// Master clocks elapsed at the end of the last completed line.
    pub master_clock: u64,
    m68k_clock: u64,
    z80_clock: u64,
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
                if bus.z80_can_run() {
                    Self::charge(
                        &mut self.z80_clock,
                        step_z80(z80, bus),
                        MASTER_CLOCKS_PER_Z80,
                        line_end,
                    );
                } else {
                    // Stopped, but still clocked. The chip runs no instructions while
                    // the 68k holds it in reset or on its bus, yet the hardware is
                    // still being clocked, so the position moves to the end of the
                    // line: leaving it where it was would owe the Z80 those clocks and
                    // `run_line` would pay them back as a burst of instructions the
                    // moment the 68k let go.
                    self.z80_clock = line_end;
                }
            } else {
                // TODO(M3-14 #75): freeze the 68k while `bus.locked_up()` is set, by
                // taking its clock to the end of the line rather than by skipping it.
                let cycles = cpu.step(bus);
                Self::charge(
                    &mut self.m68k_clock,
                    cycles,
                    MASTER_CLOCKS_PER_68K,
                    line_end,
                );
                bus.io.set_time(self.m68k_clock / MASTER_CLOCKS_PER_68K);
            }
        }

        // TODO(M3-14 #75): the VDP joins here - `vdp.advance(MASTER_CLOCKS_PER_LINE)`,
        // the HV counter from `self.line`, and VINT/HINT out to `M68k::set_ipl` and
        // `Z80::set_int_line`.
        self.master_clock += MASTER_CLOCKS_PER_LINE;
        self.line = (self.line + 1) % self.lines_per_frame;
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
        let mut bus = Bus::new(rom_image(), VERSION);
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

    /// One frame of `s`'s region.
    fn run_frame(s: &mut Scheduler, cpu: &mut M68k, z80: &mut Z80, bus: &mut Bus) {
        let lines = s.lines_per_frame;
        run_lines(s, cpu, z80, bus, lines);
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
}
