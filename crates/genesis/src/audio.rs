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
//! after it. [`Audio::frame`] hands them to [`Mixer`], which is what
//! [`crate::Genesis::drain_audio`] runs them through, and the mixer's own buffers
//! are reserved once for the same reason: a frame of audio must not allocate.

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
    /// Raw and unresampled, in `ym2612`'s own units: what turns them into the
    /// host's rate and levels is [`Mixer`], reached through [`Audio::frame`] and
    /// [`crate::Genesis::drain_audio`] (M5-14, #119).
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
    /// Called by [`crate::Scheduler::run_frame`] as a frame starts, so the frame
    /// a caller wants is the one that has just run and is still in here: this is
    /// what [`Audio::frame`] and [`crate::Genesis::drain_audio`] read before the
    /// next frame's samples begin arriving.
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

// The mixer: two chips, one host rate ---------------------------------------

use crate::config::{Config, LowPass};
use std::f32::consts::PI;

/// What an FM sample is multiplied by on its way into the mix.
///
/// `ym2612::output` adds six converters' 9-bit codes, so one channel at full
/// scale is 255 there, where the PSG's own volume table puts one channel at full
/// scale at 8191 and says the mixer scales later. The 32 is the `>> 5` of
/// `ym2612`'s output stage put back: it lands the FM in the 14-bit-per-channel domain
/// that Genesis Plus GX sums its `out_fm` in (its own `core/sound/ym2612.c`
/// clamps each channel to +-8191 and adds those, read 2026-07-31), which is the
/// domain [`PSG_LEVEL_REFERENCE`] was measured against.
const YM_OUTPUT_GAIN: f32 = 32.0;

/// What a PSG sample is multiplied by at `Config::psg_level` 100: 4200/8191.
///
/// Genesis Plus GX's PSG core puts attenuation code 0 at `PSG_MAX_VOLUME` of
/// 2800 (`core/sound/psg.c`, read 2026-07-31), a number whose own comment in
/// that file says is "roughly adjusted to match VA4 MD1 PSG/FM balance with 1.5x
/// amplification of PSG output", and that core's default preamp is 150 percent
/// against the FM's 100 (`libretro/libretro.c`: `config.psg_preamp = 150` and
/// `config.fm_preamp = 100`). So the balance being copied here is 2800 * 1.5 =
/// 4200 where an FM channel at full scale is 8191, and this is that ratio.
///
/// Verified rather than trusted, at both ends: the PSG's table in this repository
/// does reach 8191 per channel at attenuation 0, so four channels at full tilt
/// arrive as 4 * 8191 * 0.5128 = 16 797 against Genesis Plus GX's 4 * 4200 =
/// 16 800; and its FM at full tilt arrives as 6 * 255 * 32 = 48 960 against that
/// core's 6 * 8191 = 49 146, both of which clip in an `i16` and both of which
/// clip at the same place.
const PSG_LEVEL_REFERENCE: f32 = 4_200.0 / 8_191.0;

/// The PSG resampler's kernel length, in PSG ticks: 64.
///
/// The ticket offers 16 to 32 taps, and for the FM that is about right; at the
/// PSG's 223722 Hz it is not enough to hold up `square_wave_aliasing_low`, and
/// the arithmetic is worth writing down because it is the reason the PSG gets
/// twice the FM. A windowed sinc of `taps` taps at `fs` has a transition band
/// about `4 * fs / taps` wide (the Hamming main lobe), so 32 taps at 223722 Hz
/// is a transition of 28 kHz: such a filter is still passing 27 kHz, and 27 kHz
/// is exactly the frequency that folds down onto 20.5 kHz when the output is 48
/// kHz, which is the bin that test reads. 64 taps bring the transition to 14 kHz
/// and put 27.5 kHz - the PSG's first harmonic above the output's Nyquist, and
/// the loudest of the aliased ones at a twelfth of the fundamental - far enough
/// into the stop band for the Hamming window's own 43 dB.
const PSG_TAPS: usize = 64;
/// The FM's kernel length, in FM samples: 24, which is the same 9 kHz transition
/// at its own rate. The FM is only being taken from 53267 Hz to 48000 Hz, so what
/// has to be suppressed is the image of its own sampling rate, which starts at
/// 26634 Hz and is past this filter's stop band.
const YM_TAPS: usize = 24;

/// Kernel rows per stream: one for each position of an output sample inside one
/// input sample.
///
/// The ratio of the two rates is not a whole number and never repeats, so there
/// is no exact polyphase grid to build; 256 rows is the quantisation the kernel
/// is cut into instead. The error is a phase error of at most half a row, a
/// five-hundredth of an input sample, which is 0.2 % of a period at the top of
/// the pass band and 46 dB down - well under everything else this file is
/// measuring, and cheap enough to keep in one 256-coefficient row per output
/// sample.
const PHASES: usize = 256;

/// The corner of the DC-blocking high-pass, in Hz: 20.
///
/// The two chips do not sit at zero. The discrete YM2612's ladder gives a silent
/// chip a level of six converters' worth of offset (`ym2612`'s output stage, the
/// `+4`/`-3` step) and the PSG's own sum is bipolar around zero but not centred
/// when a channel is held at one level. 20 Hz is below the bottom of the music
/// and far above the rate the filters have to run at to get rid of it in a frame
/// or two.
const DC_BLOCK_HZ: f32 = 20.0;

/// The corner of [`LowPass::Model1`], in Hz: 3390.
///
/// The Model 1's output stage is a resistor and a capacitor, and this is the
/// corner the ticket gives for a VA3-ish board. It agrees with the only other
/// number available: Genesis Plus GX's default one-pole coefficient is 0.6 at
/// 48 kHz (`libretro/libretro.c`, `config.lp_range = 0x9999`), which is a corner
/// of `-ln(1 - 0.6) * 48000 / 2 PI` = 3.9 kHz, so the two sources put the same
/// board's roll-off within half a kilohertz of each other and on it being one
/// pole.
const MODEL1_LOW_PASS_HZ: f32 = 3_390.0;

/// One frame's worth of what the two chips made, and where in each chip's own
/// sample count it starts.
///
/// Made by [`Audio::frame`] and read once by [`Mixer::mix`]. The positions
/// matter as much as the samples: the mixer lays both streams on the master
/// clock, and it can only do that if it knows the absolute number of the first
/// entry of the frame it has been handed. A caller that has just run a frame
/// takes this from [`Audio`] and does not build it.
pub struct ChipFrame<'a> {
    /// The FM's `(left, right)` pairs, one every [`YM_SAMPLE_MCLK`].
    pub ym: &'a [(i16, i16)],
    /// The absolute index of `ym[0]`.
    pub ym_first: u64,
    /// The PSG's one-pin sums, one every [`PSG_TICK_MCLK`].
    pub psg: &'a [i16],
    /// The absolute index of `psg[0]`.
    pub psg_first: u64,
}

impl Audio {
    /// This frame's chip output, ready for [`Mixer::mix`].
    ///
    /// The two positions are read off the clocks rather than from a counter of
    /// what has been drained: `ym_pos` is the master clock the *next* FM sample
    /// is due at, so it is the count of samples the chip has ever made times
    /// their spacing, and the first sample of the frame is that count less what
    /// the frame still holds.
    pub fn frame(&self) -> ChipFrame<'_> {
        ChipFrame {
            ym: &self.ym_samples,
            ym_first: self.ym_pos / YM_SAMPLE_MCLK - self.ym_samples.len() as u64,
            psg: &self.psg_samples,
            psg_first: self.psg_pos / PSG_TICK_MCLK - self.psg_samples.len() as u64,
        }
    }
}

/// One chip's path from its own clock onto the host's: the samples it has made
/// that the mixer has not spent yet, and the kernel that puts them on a grid they
/// are not on.
///
/// Every position here is absolute and every one of them is a master clock:
/// input sample `i` belongs to master clock `i * period`, and an output sample at
/// master clock `t` reads the `taps` input samples that end at the one it falls
/// in. Nothing is rounded once a frame, because there is nothing per frame to
/// round - the count of output samples a stretch of master clock is worth
/// follows from the two step sizes, which is what makes
/// `output_count_exact_over_one_second` exact rather than close.
///
/// The kernel is causal - it reads the sample the output lies in and the ones
/// before it, never the ones after - so a frame can be mixed as soon as it is
/// complete and the mixer never has to wait for input it does not have. The cost
/// is that the last output sample of a frame is short of the taps that would
/// belong to it, and it is one output sample (the kernel of the FM spans one FM
/// sample past the last one held, which is 0.9 of an output sample), whose
/// missing weights are the smallest in the window.
struct Stream {
    /// One output sample of master clocks - the stream's `period` (`YM_SAMPLE_MCLK`
    /// or `PSG_TICK_MCLK`) times the host rate - in the units [`Mixer`] counts
    /// time in. A whole number because the host rate is a whole number of samples
    /// per second and the master clock is counted in whole ticks: this is the
    /// integer rate bookkeeping that keeps `output_count_exact_over_one_second`
    /// exact instead of nearly.
    step: u64,
    /// Kernel length in input samples.
    taps: usize,
    /// Gain applied to every input sample as it arrives, so that by the time the
    /// two streams are added they are in one domain.
    gain: f32,
    /// 1 for the PSG's single output pin, 2 for the FM's left and right.
    channels: usize,
    /// `PHASES` rows of `taps` coefficients, oldest tap first, one row per
    /// position of an output sample inside one input sample. See
    /// [`Stream::build_kernel`].
    kernel: Vec<f32>,
    /// The samples themselves, interleaved by channel, oldest first.
    samples: Vec<f32>,
    /// The absolute index of `samples[0]`, which [`Stream::trim`] moves along.
    first: i64,
}

impl Stream {
    /// An empty stream of `channels` channels, samples `period` master clocks
    /// apart, `taps` long, at `gain`, resampled to `rate` from `master_hz`, and
    /// reserved for `frame` samples of one frame so that no frame ever
    /// reallocates it.
    fn new(
        period: u64,
        taps: usize,
        gain: f32,
        channels: usize,
        rate: u32,
        master_hz: u64,
        frame: usize,
    ) -> Self {
        Self {
            step: period * u64::from(rate),
            taps,
            gain,
            channels,
            kernel: Self::build_kernel(period, taps, rate, master_hz),
            samples: Vec::with_capacity((frame + taps) * channels),
            first: 0,
        }
    }

    /// The resampling kernel: `PHASES` Hamming-windowed sinc low-passes of
    /// `taps` taps, one for each of the `PHASES` places an output sample can sit
    /// inside an input sample.
    ///
    /// The corner is 0.45 of whichever rate is lower. Of the output, because that
    /// is what would alias: anything above half the output rate folds down on top
    /// of the band being kept. Of the input, because a sampled stream carries
    /// nothing above its own Nyquist anyway and there is no sense paying for a
    /// pass band that ends at a frequency the chip cannot reach.
    ///
    /// Tap `k` of phase `p` sits `k - taps / 2 - p / PHASES` samples away from
    /// the output position, which is what makes the window and the sinc share a
    /// centre: the samples the output needs straddle it, half before and half
    /// after, and a window that is not centred on the sinc it is cutting makes a
    /// filter that is not a low-pass at all. [`Stream::sum`] walks the row and
    /// the samples in the same order, oldest sample against `row[0]`.
    ///
    /// Each row is then normalised to sum to 1, which makes the filter exactly
    /// unity at DC: a constant in has to be a constant out, and that is what
    /// lets [`Mixer`] put the DC-blocking high-pass after the resampler and still
    /// call it a DC blocker.
    fn build_kernel(period: u64, taps: usize, rate: u32, master_hz: u64) -> Vec<f32> {
        let in_rate = master_hz as f64 / period as f64;
        let cutoff = (f64::from(rate) * 0.45 / in_rate).min(0.45);
        let mut kernel = vec![0.0_f32; PHASES * taps];
        for phase in 0..PHASES {
            let offset = phase as f64 / PHASES as f64;
            let row = &mut kernel[phase * taps..phase * taps + taps];
            let mut sum = 0.0;
            for (k, tap) in row.iter_mut().enumerate() {
                // Tap `k` reads the input sample `oldest + k`, and `oldest` is
                // `taps / 2` before the sample the output position lies in, so
                // this tap is `k - taps / 2` along from that sample and the
                // output is `offset` further on still.
                let distance = k as f64 - (taps / 2) as f64 - offset;
                let ideal = if distance == 0.0 {
                    2.0 * cutoff
                } else {
                    let angle = PI64 * distance;
                    (2.0 * cutoff * angle).sin() / angle
                };
                let window = 0.54 - 0.46 * (2.0 * PI64 * k as f64 / (taps - 1) as f64).cos();
                *tap = (ideal * window) as f32;
                sum += f64::from(*tap);
            }
            for tap in row.iter_mut() {
                *tap /= sum as f32;
            }
        }
        kernel
    }

    /// How many input samples are held.
    fn count(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// The master-clock position of the first input sample that is *not* held, in
    /// the units [`Mixer`] times by: this is how far the mixer is allowed to mix.
    fn end_num(&self) -> u64 {
        (self.first as u64 + self.count() as u64) * self.step
    }

    /// Make the stream ready for `count` samples whose first is absolute index
    /// `at`, and say how many of them the stream already has.
    ///
    /// Time may not run backwards and may not jump. Backwards is the harmless
    /// case of the same frame being offered twice - a `drain_audio` call with no
    /// `run_frame` in front of it - and those samples are dropped here rather
    /// than mixed twice. A jump means frames ran without being drained at all:
    /// the gap is filled with silence, which loses exactly the sound of the
    /// frames that were dropped and nothing else, and keeps every sample after
    /// it on its own master clock so that the output count stays exact.
    fn align(&mut self, at: u64, count: usize) -> usize {
        let next = self.first as u64 + self.count() as u64;
        if at >= next {
            let gap = usize::try_from(at - next).unwrap_or(usize::MAX) * self.channels;
            self.samples.resize(self.samples.len() + gap, 0.0);
            0
        } else {
            usize::try_from(next - at).unwrap_or(usize::MAX).min(count)
        }
    }

    /// Add the PSG's frame, `new[0]` being absolute index `at`.
    fn push_mono(&mut self, new: &[i16], at: u64) {
        let skip = self.align(at, new.len());
        self.samples
            .extend(new[skip..].iter().map(|&v| f32::from(v) * self.gain));
    }

    /// Add the FM's frame, `new[0]` being absolute index `at`, both pins in one
    /// pass so that the two channels of one sample stay together.
    fn push_stereo(&mut self, new: &[(i16, i16)], at: u64) {
        let skip = self.align(at, new.len());
        self.samples.extend(new[skip..].iter().flat_map(|&(left, right)| {
            [f32::from(left) * self.gain, f32::from(right) * self.gain]
        }));
    }

    /// The kernel row for output position `pos`, and the absolute index of the
    /// oldest input sample it reads.
    ///
    /// Split out of [`Stream::sum`] because the FM's two channels share one row:
    /// the position is worked out once per output sample per stream rather than
    /// once per channel.
    fn tap_row(&self, pos: u64) -> (&[f32], i64) {
        let index = pos / self.step;
        let phase = (pos % self.step * PHASES as u64 / self.step) as usize;
        (
            &self.kernel[phase * self.taps..phase * self.taps + self.taps],
            index as i64 - (self.taps / 2) as i64,
        )
    }

    /// The input sample at absolute index `index` of `channel`, or silence
    /// outside what is held.
    ///
    /// Silence at both ends and for the same reason: before the first sample
    /// there is genuinely nothing - the machine had not been clocked - and
    /// [`Stream::tap_row`] never asks for a sample past the last one, because
    /// the mixer does not mix past the end of what is held.
    fn sample(&self, index: i64, channel: usize) -> f32 {
        let offset = index - self.first;
        if offset < 0 {
            return 0.0;
        }
        let i = (offset as usize) * self.channels + channel;
        self.samples.get(i).copied().unwrap_or(0.0)
    }

    /// One channel's contribution to one output sample: the window of input
    /// samples from `oldest` on, weighted by `row`.
    ///
    /// A window that runs off either end of what the stream holds is divided back
    /// up by the weight it did get. Zero-padding alone would be right at the
    /// start of time, where there genuinely is nothing before sample 0, and wrong
    /// at the end of a frame, where the samples simply have not been made yet:
    /// half the weights of a kernel centred on the output are missing there, so
    /// an uncorrected window would lose 6 dB of level on the last few output
    /// samples of every frame - a click 60 times a second, and the reason a
    /// band-limited resampler has to say what it does at a boundary. Dividing by
    /// the surviving weight keeps the gain at 1 and costs only a wider
    /// transition, for the fraction of a millisecond it applies to.
    fn sum(&self, row: &[f32], oldest: i64, channel: usize) -> f32 {
        let mut total = 0.0;
        let mut held = 0.0;
        for (k, &weight) in row.iter().enumerate() {
            let index = oldest + k as i64;
            if !self.held(index) {
                continue;
            }
            total += self.sample(index, channel) * weight;
            held += weight;
        }
        if held == 0.0 {
            0.0
        } else {
            total / held
        }
    }

    /// Is the input sample at `index` held? Only ever false at the two ends of a
    /// frame, which is what lets [`Stream::sum`] ask once per tap.
    fn held(&self, index: i64) -> bool {
        let offset = index - self.first;
        offset >= 0 && (offset as usize) < self.count()
    }

    /// Drop the input samples up to `to` that no future output sample can reach,
    /// keeping the `taps - 2` before `to` that the next one still reads.
    ///
    /// This is what keeps the buffer one frame plus a kernel wide rather than
    /// growing for ever: `Vec::drain` moves the tail down and keeps the
    /// capacity, so after the first frame no call to this file allocates.
    fn trim(&mut self, to: u64) {
        let keep_from = to as i64 - (self.taps / 2) as i64;
        if keep_from <= self.first {
            return;
        }
        let drop = (keep_from - self.first) as usize * self.channels;
        self.samples.drain(..drop.min(self.samples.len()));
        self.first = keep_from;
    }
}

/// One output channel's half-remembered past: what the DC blocker held, the
/// sample before it, and what the board's low-pass held.
#[derive(Clone, Copy, Default)]
struct FilterState {
    /// The blocker's pole.
    blocked: f32,
    /// The blocker's previous input, the one it subtracts.
    previous: f32,
    /// The low-pass's pole.
    low: f32,
}

/// The mixer: the two chips' streams, resampled band-limited onto the host's
/// sample grid, summed, and put through the console's own output filters.
///
/// ## What it is for
///
/// The chips run at their own rates - the FM about 53267 Hz and the PSG about
/// 223722 Hz (docs/ARCHITECTURE.md, "Clocks and timing") - and the host wants
/// `config.sample_rate`. docs/ARCHITECTURE.md ("Audio chips") asks for a
/// band-limited resampler rather than an interpolating one, and the difference
/// is the PSG: a PSG sample is a square wave's edge, whose harmonics run up past
/// half the output rate and come back down through it when the stream is
/// decimated by anything less than a filter. Linear interpolation leaves them
/// there, which is `square_wave_aliasing_low`.
///
/// ## Time, and why it is counted in master clocks
///
/// Both streams and the output are positions on the same master clock, and the
/// mixer keeps time as one number: `out_pos` is the master clock of the
/// next output sample, in units of `1 / sample_rate` so that it stays whole. The
/// output rate is therefore exactly `sample_rate` of the host's clock by
/// construction rather than by averaging, and a frame produces however many
/// samples the master clock it ran says it produces - which is not the same
/// number every frame, and is not supposed to be.
///
/// ## Allocation
///
/// None after the first frame. Both streams are reserved at [`Mixer::new`] and
/// then only `extend`ed and `drain`ed; the kernels are built once. The output
/// `Vec` belongs to the caller and grows by whatever `Vec` growth means.
pub struct Mixer {
    /// The machine's master clock, Hz: the console's `Region::master_clock_hz`.
    master_hz: u64,
    /// The master clock of the next output sample to produce, in units of
    /// `1 / sample_rate`, so that it is a whole number.
    out_pos: u64,
    /// The FM's two channels.
    ym: Stream,
    /// The PSG's one.
    psg: Stream,
    /// The `a` of the board's low-pass, or `None` for [`LowPass::Off`].
    low_pass: Option<f32>,
    /// What the DC-blocking high-pass keeps of its own previous output: one less
    /// the `a` of the same one-pole the low-pass uses, because the blocker is
    /// that filter's other zero - see [`Mixer::shape`].
    dc_keep: f32,
    /// One filter state per output channel.
    state: [FilterState; 2],
}

impl Mixer {
    /// A mixer for a machine clocked at `master_hz`, taking its rate, its PSG
    /// level and its output filter from `config`.
    ///
    /// The master clock is a separate argument because `Config` does not carry
    /// it and cannot: it is a property of the console the cartridge turned out to
    /// want ([`crate::Config::resolve_region`]), and by the time a mixer is built
    /// that has been decided.
    pub fn new(master_hz: u64, config: &Config) -> Self {
        let rate = config.sample_rate.max(1);
        Self {
            master_hz,
            out_pos: 0,
            ym: Stream::new(
                YM_SAMPLE_MCLK,
                YM_TAPS,
                YM_OUTPUT_GAIN,
                2,
                rate,
                master_hz,
                YM_FRAME_CAPACITY,
            ),
            psg: Stream::new(
                PSG_TICK_MCLK,
                PSG_TAPS,
                PSG_LEVEL_REFERENCE * config.psg_level as f32 / 100.0,
                1,
                rate,
                master_hz,
                PSG_FRAME_CAPACITY,
            ),
            low_pass: match config.lowpass {
                LowPass::Off => None,
                LowPass::Model1 => Some(one_pole(MODEL1_LOW_PASS_HZ, rate)),
            },
            dc_keep: 1.0 - one_pole(DC_BLOCK_HZ, rate),
            state: [FilterState::default(); 2],
        }
    }

    /// Mix one frame's chip output and append the result to `out` as interleaved
    /// stereo `i16`.
    ///
    /// Everything the frame holds is mixed - the mixer never holds output back
    /// for a later frame - and everything it mixes is spent, so handing the same
    /// frame over twice appends nothing the second time
    /// (`a_frame_is_mixed_once`). What is not mixed is whatever the streams do
    /// not have yet: the mix stops at whichever chip is further behind, because
    /// 1008 and 240 do not divide each other and a frame's two streams end on
    /// different master clocks.
    pub fn mix(&mut self, frame: &ChipFrame<'_>, out: &mut Vec<i16>) {
        self.ym.push_stereo(frame.ym, frame.ym_first);
        self.psg.push_mono(frame.psg, frame.psg_first);

        let end = self.ym.end_num().min(self.psg.end_num());
        while self.out_pos < end {
            let (ym_row, ym_oldest) = self.ym.tap_row(self.out_pos);
            let (psg_row, psg_oldest) = self.psg.tap_row(self.out_pos);
            let psg = self.psg.sum(psg_row, psg_oldest, 0);
            let left = self.ym.sum(ym_row, ym_oldest, 0) + psg;
            let right = self.ym.sum(ym_row, ym_oldest, 1) + psg;
            out.push(self.shape(0, left));
            out.push(self.shape(1, right));
            self.out_pos += self.master_hz;
        }

        self.ym.trim(self.out_pos / self.ym.step);
        self.psg.trim(self.out_pos / self.psg.step);
    }

    /// One mixed sample of one channel through the console's two filters and into
    /// an `i16`.
    ///
    /// The DC blocker goes first and the board's low-pass second: the offsets it
    /// is there for are at 0 Hz, where the low-pass is doing nothing, and taking
    /// them out before the low-pass means its pole never holds a level that is
    /// not sound. The low-pass is the `y += a * (x - y)` one-pole form of
    /// [`one_pole`]; the blocker is the same pole with the difference of the
    /// input in place of the input, `y = (1 - a) * y_prev + x - x_prev`, which is
    /// the backward-Euler discretisation of `s / (s + wc)` - unity where the
    /// low-pass is unity and zero at DC, which is the whole of what it is for.
    ///
    /// The sum of six FM channels and four PSG channels can ask for more than an
    /// `i16` can say - Genesis Plus GX, whose levels this copies, has the same
    /// headroom problem and clips at the same place (`audio_update` of its
    /// `core/system.c` clamps to 32767 and -32768 after filtering) - so the
    /// answer is clamped rather than wrapped.
    fn shape(&mut self, channel: usize, value: f32) -> i16 {
        let (low_pass, dc_keep) = (self.low_pass, self.dc_keep);
        let state = &mut self.state[channel];

        let blocked = dc_keep * state.blocked + value - state.previous;
        state.previous = value;
        state.blocked = blocked;

        let mut shaped = blocked;
        if let Some(a) = low_pass {
            // `y += a * (x - y)`: the new level moves `a` of the way from the
            // level it was towards the one it is being told. Doing this the other
            // way round - `x + a * (y - x)` - is a filter with `1 - a`, a corner
            // three times higher, and it is very easy to write by accident.
            shaped = state.low + a * (blocked - state.low);
            state.low = shaped;
        }
        shaped.clamp(-32_768.0, 32_767.0).round() as i16
    }
}

/// The `a` of a one-pole filter with a corner of `freq` at `rate`.
///
/// `1 - exp(-2 PI f / fs)`, the exact discrete-time version of `y += a * (x - y)`
/// rather than the `2 PI f / fs` it is often written as: at 3.39 kHz of 48 kHz
/// the two differ by 6 % in `a` and by a third of a decibel at the corner, which
/// is more than the difference between the two sources for the corner itself.
fn one_pole(freq: f32, rate: u32) -> f32 {
    let step = 2.0 * PI * freq / rate.max(1) as f32;
    1.0 - (-step).exp()
}

/// `PI` at the width the kernels are built at: they are built in `f64` and
/// applied in `f32`, and this is what [`Stream::build_kernel`] counts in.
const PI64: f64 = std::f64::consts::PI;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, LowPass, Region};
    use std::f64::consts::PI;

    /// The NTSC master clock, which every mixer test runs on
    /// (docs/ARCHITECTURE.md, "Clocks and timing").
    const NTSC_HZ: u64 = 53_693_175;
    /// Master clocks in one NTSC raster: 262 lines of [`MASTER_CLOCKS_PER_LINE`].
    const NTSC_FRAME: u64 = 262 * MASTER_CLOCKS_PER_LINE;
    /// The default host rate every test mixes to.
    const RATE: u32 = 48_000;
    /// The samples at the end of a mixed run that [`steady`] does not measure:
    /// the last few output samples of a mix are read from a kernel that runs off
    /// the end of the frame, which is the right thing to do and the wrong place
    /// to measure a filter from.
    const TAIL_GUARD: usize = 32;

    /// A mixer on the NTSC master clock with `config`.
    fn mixer(config: Config) -> Mixer {
        assert_eq!(Region::Usa.master_clock_hz(), NTSC_HZ);
        Mixer::new(NTSC_HZ, &config)
    }

    /// How many samples each chip produces in `frames` NTSC rasters.
    fn counts(frames: u64) -> (usize, usize) {
        let mclk = frames * NTSC_FRAME;
        (
            (mclk / YM_SAMPLE_MCLK) as usize,
            (mclk / PSG_TICK_MCLK) as usize,
        )
    }

    /// The `i`th master clock of a stream whose samples are `period` apart.
    fn mclk_of(index: usize, period: u64) -> f64 {
        index as f64 * period as f64 / NTSC_HZ as f64
    }

    /// A `value` Hz sine at `rate` sampled every `period` master clocks, at
    /// `amplitude` and phase-aligned so that it reaches its peak on a sample.
    fn sine(count: usize, period: u64, value: f64, amplitude: f64) -> Vec<i16> {
        (0..count)
            .map(|i| {
                let phase = 2.0 * PI * value * mclk_of(i, period);
                (amplitude * phase.sin()).round() as i16
            })
            .collect()
    }

    /// The left channel of an interleaved output buffer.
    fn left(out: &[i16]) -> Vec<f64> {
        out.iter().step_by(2).map(|&v| f64::from(v)).collect()
    }

    /// The magnitude of the `value` Hz component of `signal`, as a Hann-windowed
    /// DFT, over a signal that holds a whole number of cycles of it.
    fn bin(signal: &[f64], value: f64) -> f64 {
        let n = signal.len() as f64;
        let step = 2.0 * PI * value / RATE as f64;
        let (mut re, mut im, mut weight) = (0.0, 0.0, 0.0);
        for (i, sample) in signal.iter().enumerate() {
            let window = 0.5 - 0.5 * (2.0 * PI * i as f64 / n).cos();
            re += sample * window * (i as f64 * step).cos();
            im -= sample * window * (i as f64 * step).sin();
            weight += window;
        }
        (re * re + im * im).sqrt() * 2.0 / weight
    }

    /// `x` relative to `reference`, in decibels.
    fn decibels(x: f64, reference: f64) -> f64 {
        20.0 * (x / reference).log10()
    }

    /// A window of `want` samples from the steady state: past the first half,
    /// where the DC blocker is still settling after the first sample of machine
    /// time, and off the last samples, where the resampler is reading past the
    /// end of the frame it has been given (see [`Stream::sum`]).
    fn steady(signal: &[f64], want: usize) -> &[f64] {
        let end = signal.len() - TAIL_GUARD;
        assert!(end > want, "the signal is shorter than the window");
        &signal[end - want..end]
    }

    /// The root mean square of `signal`.
    fn rms(signal: &[f64]) -> f64 {
        (signal.iter().map(|v| v * v).sum::<f64>() / signal.len() as f64).sqrt()
    }

    /// The one test that goes through the real [`Audio`] rather than through
    /// samples made in this file: 60 NTSC rasters of silence have to come out as
    /// exactly the number of output samples that much master clock is worth.
    ///
    /// The count is the whole of what "no drift" means: an output rate that is
    /// right on average but wrong on one frame either gaps or duplicates a frame
    /// of host audio every time it runs, and a rate that is kept by rounding the
    /// per-frame count is off by the fraction it threw away every frame for ever.
    #[test]
    fn output_count_exact_over_one_second() {
        let mut audio = Audio::new();
        let mut mixer = mixer(Config::default());
        let mut out: Vec<i16> = Vec::new();
        let mut clock = 0_u64;
        for _ in 0..60 {
            audio.start_frame();
            for _ in 0..262 {
                clock += MASTER_CLOCKS_PER_LINE;
                audio.advance_to(clock);
            }
            mixer.mix(&audio.frame(), &mut out);
        }

        let got = (out.len() / 2) as f64;
        let want = 60.0 * NTSC_FRAME as f64 / NTSC_HZ as f64 * RATE as f64;
        assert!(
            (got - want).abs() <= 1.0,
            "{} output frames for {} frames of master clock ({want} expected)",
            got,
            clock,
        );
    }

    /// Nothing goes in, nothing comes out - including no `NaN`, no DC from the
    /// filters and no first-frame transient.
    #[test]
    fn silence_in_silence_out() {
        let (ym_count, psg_count) = counts(4);
        let ym = vec![(0_i16, 0_i16); ym_count];
        let psg = vec![0_i16; psg_count];
        let mut mixer = mixer(Config::default());
        let mut out = Vec::new();
        mixer.mix(
            &ChipFrame {
                ym: &ym,
                ym_first: 0,
                psg: &psg,
                psg_first: 0,
            },
            &mut out,
        );
        assert!(
            out.iter().all(|&v| v == 0),
            "{:?}",
            &out[..8.min(out.len())]
        );
        assert!(!out.is_empty(), "four frames have to produce something");
    }

    /// A 1 kHz sine at the FM's own rate keeps its amplitude through the
    /// resampler: within 1%.
    ///
    /// Mixed with the low-pass off, because what is under test is the resampler;
    /// [`lowpass_attenuates_10khz`] is the board filter's own test.
    #[test]
    fn sine_through_resampler_keeps_amplitude() {
        let (ym_count, psg_count) = counts(4);
        let amplitude = 400.0;
        let ym: Vec<(i16, i16)> = sine(ym_count, YM_SAMPLE_MCLK, 1_000.0, amplitude)
            .into_iter()
            .map(|v| (v, v))
            .collect();
        let psg = vec![0_i16; psg_count];
        let config = Config {
            lowpass: LowPass::Off,
            ..Config::default()
        };
        assert_eq!(config.sample_rate, RATE);

        let mut mixer = mixer(config);
        let mut out = Vec::new();
        mixer.mix(
            &ChipFrame {
                ym: &ym,
                ym_first: 0,
                psg: &psg,
                psg_first: 0,
            },
            &mut out,
        );

        // Measured in the steady state, and as a peak because the signal is at 48
        // of the host's samples per cycle. The first half of a mixed run is not
        // steady: a DC blocker with a 20 Hz corner takes eight milliseconds to
        // settle after the first sample of machine time, and the measurement is
        // of the filter, not of that.
        let signal = left(&out);
        let peak = steady(&signal, 1_024)
            .iter()
            .fold(0.0_f64, |best, &v| best.max(v.abs()));
        let want = amplitude * YM_OUTPUT_GAIN as f64;
        let error = (peak / want - 1.0).abs();
        assert!(
            error <= 0.01,
            "peak {peak} against {want}: {:.2}% off",
            error * 100.0
        );
    }

    /// A PSG square wave does not alias down into the output.
    ///
    /// The square is at 2.5 kHz rather than the 3 kHz of the ticket because at
    /// 3 kHz every alias of an odd harmonic lands exactly on another odd harmonic
    /// (48000 / 3000 is a whole number), so the test could not tell an alias from
    /// the signal. At 2.5 kHz the harmonics are at 2.5, 7.5, 12.5, 17.5 and 22.5
    /// kHz, and the alias of the 11th, 13th, 15th and 17th land on 20.5, 15.5,
    /// 10.5 and 5.5 kHz where nothing else is. Those four bins are what
    /// naive interpolation leaves ringing, and they have to sit at least 40 dB
    /// under the fundamental.
    #[test]
    fn square_wave_aliasing_low() {
        let (ym_count, psg_count) = counts(8);
        let square = |t: f64| -> i16 {
            if (t * 2_500.0).fract() < 0.5 {
                8_191
            } else {
                -8_191
            }
        };
        let psg: Vec<i16> = (0..psg_count)
            .map(|i| square(mclk_of(i, PSG_TICK_MCLK)))
            .collect();
        let ym = vec![(0_i16, 0_i16); ym_count];
        let config = Config {
            lowpass: LowPass::Off,
            ..Config::default()
        };
        let mut mixer = mixer(config);
        let mut out = Vec::new();
        mixer.mix(
            &ChipFrame {
                ym: &ym,
                ym_first: 0,
                psg: &psg,
                psg_first: 0,
            },
            &mut out,
        );

        // A window of exactly 0.064 s is 160 cycles of the fundamental and 3072
        // host samples, so every bin below is an exact bin of it.
        let signal = left(&out);
        let window = &signal[signal.len() - 3_072..];
        let fundamental = bin(window, 2_500.0);
        assert!(fundamental > 1.0, "the square did not come through at all");
        for alias in [5_500.0, 10_500.0, 15_500.0, 20_500.0] {
            let level = decibels(bin(window, alias), fundamental);
            assert!(
                level <= -40.0,
                "{alias} Hz holds {level:.1} dB against the fundamental"
            );
        }
    }

    /// The Model 1 low-pass does what it is there for, and `LowPass::Off` does
    /// not do it.
    ///
    /// A first-order filter at 3.39 kHz is 9.4 dB down at 10 kHz, and that is
    /// what a VA3-ish board's output resistor and capacitor are. Measured against
    /// the same signal through `Off` so that the resampler's own answer at 10 kHz
    /// cancels out of the comparison.
    #[test]
    fn lowpass_attenuates_10khz() {
        let (ym_count, psg_count) = counts(8);
        let ym: Vec<(i16, i16)> = sine(ym_count, YM_SAMPLE_MCLK, 10_000.0, 400.0)
            .into_iter()
            .map(|v| (v, v))
            .collect();
        let psg = vec![0_i16; psg_count];
        let frame = ChipFrame {
            ym: &ym,
            ym_first: 0,
            psg: &psg,
            psg_first: 0,
        };

        let mut through = Vec::new();
        mixer(Config {
            lowpass: LowPass::Model1,
            ..Config::default()
        })
        .mix(&frame, &mut through);
        let mut bypassed = Vec::new();
        mixer(Config {
            lowpass: LowPass::Off,
            ..Config::default()
        })
        .mix(&frame, &mut bypassed);

        // 640 whole cycles of the tone each, out of the steady state for the same
        // reason as [`sine_through_resampler_keeps_amplitude`]: the start of a
        // mixed run is full of the DC blocker's own settling, which is not the
        // board's low-pass and would be counted as part of its answer.
        let window = 3_072;
        let with = rms(steady(&left(&through), window));
        let without = rms(steady(&left(&bypassed), window));
        let loss = decibels(with, without);
        assert!(loss <= -9.0, "Model 1 left {loss:.2} dB on a 10 kHz tone");
    }

    /// A constant is not a sound: the ladder's offset and the PSG's are gone by
    /// the end of a tenth of a second.
    #[test]
    fn dc_removed() {
        let (ym_count, psg_count) = counts(10);
        let ym = vec![(500_i16, 500_i16); ym_count];
        let psg = vec![0_i16; psg_count];
        let mut mixer = mixer(Config::default());
        let mut out = Vec::new();
        mixer.mix(
            &ChipFrame {
                ym: &ym,
                ym_first: 0,
                psg: &psg,
                psg_first: 0,
            },
            &mut out,
        );

        let signal = left(&out);
        let peak = signal.iter().fold(0.0_f64, |best, &v| best.max(v.abs()));
        assert!(
            peak > 8_000.0,
            "the DC level never reached the output: {peak}"
        );
        let tail = signal[signal.len() - 8..]
            .iter()
            .fold(0.0_f64, |best, &v| best.max(v.abs()));
        assert!(
            tail < peak / 100.0,
            "tail {tail} against a peak of {peak}: the DC is still there"
        );
    }

    /// Mixing the same frame twice is the caller's mistake and must not
    /// duplicate the frame: what `mix` consumed is what it consumed.
    #[test]
    fn a_frame_is_mixed_once() {
        let (ym_count, psg_count) = counts(2);
        let ym = vec![(0_i16, 0_i16); ym_count];
        let psg = vec![0_i16; psg_count];
        let frame = ChipFrame {
            ym: &ym,
            ym_first: 0,
            psg: &psg,
            psg_first: 0,
        };
        let mut mixer = mixer(Config::default());
        let mut once = Vec::new();
        mixer.mix(&frame, &mut once);
        let mut twice = Vec::new();
        mixer.mix(&frame, &mut twice);
        assert!(
            twice.is_empty(),
            "the second call mixed {} halves again",
            twice.len()
        );
    }
}
