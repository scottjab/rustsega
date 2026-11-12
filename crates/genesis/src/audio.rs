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
        assert!(out.iter().all(|&v| v == 0), "{:?}", &out[..8.min(out.len())]);
        assert!(!out.is_empty(), "four frames have to produce something");
    }

    /// A 1 kHz sine at the FM's own rate keeps its amplitude through the
    /// resampler: within 1%.
    ///
    /// Mixed with the low-pass off, because what is under test is the resampler;
    /// [`lowpass_attenuates_10khz`] is the board filter's own test.
    #[test]
    fn sine_through_resampler_keeps_amplitude() {
        let (ym_count, psg_count) = counts(2);
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

        // Past the kernel's leading edge, and measured as a peak because the
        // signal is at 48 of the host's samples per cycle.
        let signal = left(&out);
        let peak = signal[64..]
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

        // The last 0.064 s of each: 640 whole cycles of the tone, past the
        // resampler's leading edge and past the transient of the DC blocker.
        let window = 3_072;
        let with = rms(&left(&through)[through.len() / 2 - window..through.len() / 2]);
        let without = rms(&left(&bypassed)[bypassed.len() / 2 - window..bypassed.len() / 2]);
        let loss = decibels(with, without);
        assert!(
            loss <= -9.0,
            "Model 1 left {loss:.2} dB on a 10 kHz tone"
        );
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
