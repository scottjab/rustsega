//! The two sound chips inside the console, and the master-clock positions that
//! decide when each one is asked for a sample (M5-13, #118).
//!
//! ## One clock, four rates
//!
//! [`crate::Scheduler`] keeps a single master clock, and every device is a
//! different distance along it: the VDP is walked in master clocks, the 68000
//! runs at master / 7 and the Z80 at master / 15 (docs/ARCHITECTURE.md, "Clocks
//! and timing"). The two sound chips are a further division down, and neither of
//! them is the master clock or either CPU's:
//!
//! * The YM2612 produces one output sample every 144 68000 clocks, which is 1008
//!   master clocks and about 53267 Hz ([`YM_SAMPLE_MCLK`]; docs/ARCHITECTURE.md,
//!   "Clocks and timing", and `ym2612::Ym2612::clock`, which is that one step).
//! * The PSG ticks at its own internal rate of the Z80 clock over 16, which is 240
//!   master clocks and about 223722 Hz ([`PSG_TICK_MCLK`]; SMS Power, "SN76489",
//!   tone generator, and [`sn76489::Sn76489::clock`]).
//!
//! ## Catching up rather than interleaving
//!
//! A chip cannot be stepped a bit at a time between two instructions, and it must
//! not be stepped at the end of a line as though everything happened there: a
//! volume write 200 lines into a frame is heard 200 lines before the frame ends.
//! So each chip carries the master clock it has been run to, and
//! [`Audio::sync_ym`] / [`Audio::sync_psg`] run it forward to [`Audio::now`] -
//! which [`crate::Scheduler`] stamps at every slice, the same way it stamps
//! [`crate::Io::set_time`] - immediately before anything writes that chip and
//! again at the end of every line.
//!
//! That is exact rather than approximate, because a chip's `clock` is a pure step:
//! running 300 ticks in one call at the moment they were owed and running them
//! one at a time as they pass produce the same samples. What matters is only that
//! a register write lands between the right two of them, and catching up first is
//! what puts it there. Reading is a catch-up too: the YM2612's BUSY flag counts
//! down with the sample clock, so a status read that did not advance the chip
//! would read a flag that had run out ages ago and a driver polling it would wait
//! forever.
//!
//! ## The frames, and not allocating them
//!
//! Each chip's output goes into its own buffer, and the two buffers are the state
//! of a frame rather than of the machine: [`Audio::start_frame`] `clear`s them,
//! which keeps whatever capacity the first frame needed and allocates nothing
//! after it. Nothing reads them yet - resampling, mixing and
//! [`crate::Genesis::drain_audio`] are M5-14 (#119) - but a buffer that grew for
//! ever would be a leak with no reader to notice it.

use sn76489::Sn76489;
use ym2612::Ym2612;

use crate::scheduler::{LINES_PER_FRAME_PAL, MASTER_CLOCKS_PER_LINE};

/// Master clocks between two YM2612 output samples: 144 68000 clocks at
/// [`crate::scheduler::MASTER_CLOCKS_PER_68K`] each, about 53267 Hz
/// (docs/ARCHITECTURE.md, "Clocks and timing").
pub const YM_SAMPLE_MCLK: u64 = 144 * 7;

/// Master clocks between two PSG internal ticks: the Z80 clock divided by 16, so
/// 16 T-states at [`crate::scheduler::MASTER_CLOCKS_PER_Z80`] each, about
/// 223722 Hz (SMS Power, "SN76489", tone generator; docs/ARCHITECTURE.md,
/// "Clocks and timing").
pub const PSG_TICK_MCLK: u64 = 16 * 15;

/// Samples one PAL frame can hold, which is the capacity the buffers are reserved
/// at: PAL is the slower frame but the longer one, and the chips run on the
/// master clock rather than on seconds, so it is the one that needs the most
/// entries. Rounding up is deliberate - the reservation is an upper bound, and a
/// push that had to grow is a silent allocation rather than a loud failure.
const YM_FRAME_CAPACITY: usize =
    (LINES_PER_FRAME_PAL as usize * MASTER_CLOCKS_PER_LINE as usize) / YM_SAMPLE_MCLK as usize + 1;
/// The same for the PSG, whose tick is a little over four times finer.
const PSG_FRAME_CAPACITY: usize =
    (LINES_PER_FRAME_PAL as usize * MASTER_CLOCKS_PER_LINE as usize) / PSG_TICK_MCLK as usize + 1;

/// The console's sound hardware: the two chips, how far each has been run, and
/// the samples they produced this frame.
///
/// Owned by [`crate::Bus`] for the same reason [`crate::Z80Side`] is: both CPUs
/// reach the same one YM2612 and the same one PSG (plutiedev, "Memory map" - the
/// 68k's `$A04000`-`$A04003` and the Z80's `$4000`-`$4003` are one chip's four
/// ports, and `$C00011` and `$7F11` are one PSG port), so neither side may own it
/// and a [`crate::Z80View`] cannot.
///
/// The chips are public because the machine has nothing else to hand a caller
/// that wants to know what a chip was told, and because the YM2612 has no
/// register readback of its own - `ym2612::Ym2612` deliberately keeps what state
/// it can show behind `#[cfg(test)]`, so the chip itself is the only thing left
/// to look at.
pub struct Audio {
    /// The FM synthesizer, reached by both CPUs' four YM ports.
    pub ym: Ym2612,
    /// The PSG, reached by both CPUs' one write-only port.
    pub psg: Sn76489,
    /// The YM2612's output for the frame now running, one `(left, right)` per
    /// sample at [`YM_SAMPLE_MCLK`] apart.
    ///
    /// Raw and unresampled: the resampler, the Model 1 low-pass and the mix down
    /// to `config.sample_rate` are M5-14 (#119), which is what will drain this.
    pub ym_samples: Vec<(i16, i16)>,
    /// The PSG's output for the frame now running, one sum per tick of
    /// [`PSG_TICK_MCLK`] (SMS Power, "SN76489": the chip has one output pin).
    pub psg_samples: Vec<i16>,
    /// The master clock the chips are being run to: where the machine says it is,
    /// stamped by [`crate::Scheduler`] at every slice boundary.
    ///
    /// Held here rather than read from the scheduler because the bus answers
    /// accesses from both CPUs and has no way to reach the loop that is running
    /// them.
    now: u64,
    /// Master clock the next YM2612 sample is due at. Always a whole multiple of
    /// [`YM_SAMPLE_MCLK`], which is what makes the sample count of a stretch of
    /// the master clock `stretch / YM_SAMPLE_MCLK` with the remainder carried on
    /// rather than rounded away each frame.
    ym_pos: u64,
    /// Master clock the next PSG tick is due at, likewise a whole multiple of
    /// [`PSG_TICK_MCLK`].
    psg_pos: u64,
    /// How many times this machine has driven the YM2612's reset line.
    ///
    /// The chip's `/RESET` pin is tied to the Z80's own reset line, so the 68k
    /// resets it with `0xA11200`: BlastEm's reset handler calls `ym_reset` in the
    /// branch that asserts Z80 reset, and Genesis Plus GX's `gen_zreset_w` calls
    /// `fm_reset` on both edges of the line and its `gen_reset` once more for the
    /// console's `RESET`. Counted per edge, not per level: [`crate::Bus::new`]
    /// starts with the line held asserted, and that is the Z80 not having been let
    /// go yet rather than a reset anyone performed.
    pub ym_resets: usize,
}

impl Audio {
    /// The two chips in their power-on states, both positioned at master clock 0
    /// with the frame buffers reserved but empty.
    ///
    /// The capacity is taken once, here, and after it the buffers only ever
    /// `clear` and `push`: see [`Audio::start_frame`] and the module docs.
    pub fn new() -> Self {
        Self {
            ym: Ym2612::new(),
            psg: Sn76489::new(),
            ym_samples: Vec::with_capacity(YM_FRAME_CAPACITY),
            psg_samples: Vec::with_capacity(PSG_FRAME_CAPACITY),
            now: 0,
            ym_pos: 0,
            psg_pos: 0,
            ym_resets: 0,
        }
    }

    /// Say what master clock the machine is at, so that a chip written between
    /// now and the next call is heard from here on.
    ///
    /// Set before every slice by [`crate::Scheduler::run_line`] and clamped to
    /// the end of the line being run: an instruction cannot be stopped half way,
    /// so a write inside one is attributed to the position the instruction
    /// started at, and a write in the instruction that straddles the line
    /// boundary is attributed to the boundary rather than past it. That is what
    /// keeps the sample count of a frame a whole number of frame lengths.
    pub fn set_now(&mut self, now: u64) {
        self.now = now;
    }

    /// The master clock the chips are being run to.
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Run both chips to `now` and remember it as the time of the machine.
    ///
    /// This is what [`crate::Scheduler::run_line`] calls at the end of every
    /// line, so that a frame's samples are all present when the frame ends and
    /// the buffers do not grow past one frame's worth of catch-up between two
    /// writes.
    pub fn advance_to(&mut self, now: u64) {
        self.set_now(now);
        self.sync_ym();
        self.sync_psg();
    }

    /// Discard the frame that just ran, keeping the capacity.
    ///
    /// Called by [`crate::Scheduler::run_frame`] as a frame starts. Nothing
    /// drains these buffers yet, and when M5-14 (#119) does it will be the frame
    /// that has just run that it takes.
    pub fn start_frame(&mut self) {
        self.ym_samples.clear();
        self.psg_samples.clear();
    }

    /// Write one YM2612 port, at the current time.
    ///
    /// The catch-up is the whole of what this adds to [`Ym2612::write`]: the
    /// samples up to this moment are generated first, so the register takes
    /// effect on the next sample and not on the one that has already been pushed.
    pub fn ym_write(&mut self, port: u8, value: u8) {
        self.sync_ym();
        self.ym.write(port, value);
    }

    /// The YM2612 status byte, read at the current time.
    ///
    /// A read is not passive: BUSY runs down with the sample clock, so the chip
    /// has to be caught up before it is asked (see the module docs and
    /// [`ym2612::Ym2612::read_status`]).
    pub fn ym_read_status(&mut self) -> u8 {
        self.sync_ym();
        self.ym.read_status()
    }

    /// Write the PSG's one data port, at the current time.
    pub fn psg_write(&mut self, value: u8) {
        self.sync_psg();
        self.psg.write(value);
    }

    /// Assert the YM2612's reset line, which the hardware ties to the Z80's.
    ///
    /// The chip is rebuilt, which is `ym2612`'s stand-in for a reset exactly as
    /// `crate::Bus` rebuilds the VDP: [`Ym2612::new`] is the power-on state and
    /// the crate offers no `reset`. The clock position is deliberately *not*
    /// wound back - the chip stops being the old one, it does not stop being
    /// clocked - and the samples it had already produced stay in the frame.
    pub fn ym_reset(&mut self) {
        self.ym = Ym2612::new();
        self.ym_resets += 1;
    }

    /// Run the YM2612 until the sample after `now` is the next one due.
    ///
    /// A step is taken for every whole [`YM_SAMPLE_MCLK`] between the chip's
    /// position and `now`, so a chip asked at 1008 has had exactly one sample and
    /// one at 1007 has had none. The position is left on the grid, which is what
    /// makes the count of samples over any stretch of the master clock that
    /// stretch's whole number of periods with the remainder carried, unaltered by
    /// how the stretches were cut up.
    fn sync_ym(&mut self) {
        while self.ym_pos + YM_SAMPLE_MCLK <= self.now {
            let sample = self.ym.clock();
            self.ym_samples.push(sample);
            self.ym_pos += YM_SAMPLE_MCLK;
        }
    }

    /// Run the PSG until the tick after `now` is the next one due.
    /// [`Audio::sync_ym`] is why the loop is shaped like this.
    fn sync_psg(&mut self) {
        while self.psg_pos + PSG_TICK_MCLK <= self.now {
            let sample = self.psg.clock();
            self.psg_samples.push(sample);
            self.psg_pos += PSG_TICK_MCLK;
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the two chips are in the master clock, and how much they have produced.
///
/// Hand-written because `ym2612::Ym2612` has no `Debug` - its state is two
/// register banks, 24 phase counters and 24 envelopes, and a [`crate::Bus`] dump
/// is for finding a stray byte rather than for that - so the chips are reported
/// by what the clock says about them instead.
impl std::fmt::Debug for Audio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Audio")
            .field("now", &self.now)
            .field("ym_pos", &self.ym_pos)
            .field("psg_pos", &self.psg_pos)
            .field("ym_samples", &self.ym_samples.len())
            .field("psg_samples", &self.psg_samples.len())
            .field("ym_resets", &self.ym_resets)
            .finish()
    }
}
