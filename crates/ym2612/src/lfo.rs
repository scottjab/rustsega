//! The LFO: the global tremolo and vibrato of `&22`, and the two ways it reaches
//! the operators.
//!
//! One counter is the whole of the LFO. It is 7 bits - 128 steps - and it is
//! advanced once every [`STEP_PERIOD`] samples of the frequency `&22` bits 2-0
//! names, and `&22` bit 3 clear holds it at 0 rather than stopping it (YM2608
//! application manual `&22`). Everything else here is two read-outs of that one
//! counter:
//!
//! - **AM**, amplitude modulation, reads it as a triangle of attenuations: 126 at
//!   the ends of the cycle and 0 at the middle, which is added to an operator's
//!   envelope attenuation when that operator's `&60` bit 7 is set, shifted down
//!   by the channel's AMS - `&B4` bits 5-4 ([`AM_SHIFT`], [`Lfo::amplitude`]).
//! - **PM**, phase modulation, reads it a quarter of the way down as a triangle of
//!   frequency offsets: a signed offset added to the channel's F-Num before the
//!   phase increment is worked out, of a depth the channel's PMS - `&B4` bits 2-0
//!   - picks ([`PM_SHIFTS`], [`pm_offset`]).
//!
//! Both read-outs are of the counter as it stands, so AM and PM are in step with
//! each other: the two ends of the AM triangle are the two ends of the PM swing,
//! which is what makes a tremolo and a vibrato on the same `&22` land together.
//!
//! ## The AM depth, and the four AMS values
//!
//! The triangle is 0..126 with no shift, so AMS 3 - the deepest - adds 126 of the
//! 1024 of an octave-quantised attenuation, which is 11.8 dB, and the other three
//! are 5.9, 1.4 and 0 dB ([`AM_SHIFT`]). An operator with `&60` bit 7 clear gets
//! nothing at all whatever its channel's AMS says, and AMS 0 is nothing by
//! construction rather than by a special case.
//!
//! ## The PM depth, which is a shift rather than a sum
//!
//! PM is a *relative* frequency offset - the manual quotes it in cents, which is a
//! ratio - so the offset has to be a fraction of the F-Num. The chip takes the top
//! seven bits of the F-Num, `fnum >> 4`, adds two shifted copies of it together,
//! and shifts the pair down two ([`pm_offset`]); see [`PM_SHIFTS`] for the pairs
//! and the cents they come to. PM therefore moves the increment and nothing else:
//! the key code the key scaling and the detune are read at stays the key code of
//! the F-Num that was written, not of the wobbled one ([`crate::phase`]).
//!
//! Nothing here is copied. The step periods, the triangle, the four AM shifts and
//! the PM shift pairs are hardware constants: they were read off the YM2608
//! application manual's LFO section - the frequency table, the AM depth table and
//! the PM depth table, 0, 3.4, 6.7, 10, 14, 20, 40 and 80 cents - and each was
//! then cross-checked against Nuked-OPN2's `ym3438.c` and ymfm's `ymfm_opn.cpp`,
//! both read 12 Nov 2026, which are the only implementations that publish them:
//! Nuked's `lfo_cycles` is exactly [`STEP_PERIOD`], its `eg_am_shift` exactly
//! [`AM_SHIFT`], and its `pg_lfo_sh1`/`pg_lfo_sh2` pair-sum shifts to the cent
//! depths [`pm_offset`] is tested at. ymfm's independent table is the same series
//! one sample slower (`{109, 78, 72, 68, 63, 45, 9, 6}`), which is the ±1 that
//! the divider of [`Lfo::tick`] cannot avoid.

use crate::phase::CHANNELS;
use crate::regs::{self, PART_I};

/// Output samples per step of the LFO counter, by `&22` bits 2-0 (YM2608
/// application manual `&22`, LFO frequency table; the same eight numbers as
/// Nuked-OPN2's `lfo_cycles`).
///
/// The hardware divides with a mask rather than a counter - Nuked adds one to a
/// quotient every sample and steps when `quotient & period == period`, which
/// lands the first step one sample late - so this is one sample a period out on
/// a 128-step cycle. ymfm, which divides the manual's table by hand, needs 109
/// for the first of them.
const STEP_PERIOD: [u8; 8] = [108, 77, 71, 67, 62, 44, 8, 5];

/// The LFO counter is 7 bits: 128 steps, whatever the frequency that steps it
/// (YM2608 application manual `&22`, and the `&22` enable bit's own mask of
/// 0x7F in Nuked-OPN2).
const COUNTER_MASK: u8 = 0x7F;

/// The AM shift of the four AMS values of `&B4` bits 5-4: 0, 1.4, 5.9 and 11.8 dB
/// at the peak of the triangle (YM2608 application manual `&B4`, AM sensitivity
/// table; the same `{7, 3, 1, 0}` as Nuked-OPN2's `eg_am_shift`).
///
/// AMS 0 shifts the 0..126 triangle out of existence, which is how a depth of
/// zero is stored without a special case.
const AM_SHIFT: [u8; 4] = [7, 3, 1, 0];

/// The two shift counts of the PM offset, `[PMS][triangle step]`, where `PMS` is
/// `&B4` bits 2-0 and the step is the 0..7 half of the PM phase of
/// [`Lfo::pm_phase`].
///
/// The offset is `(fnum >> 4 >> a) + (fnum >> 4 >> b)`, two shifted copies of the
/// top seven bits of the F-Num added - which is the shape the chip's adder has,
/// and the reason this is a table of shift pairs rather than of offsets: the two
/// shifts truncate separately, and folding them into one fraction does not come
/// out the same at every F-Num. Two more shifts of the sum finish it, and PMS 6
/// and 7 share PMS 5's pairs and gain their extra depth by [`pm_offset`]'s left
/// shift instead.
///
/// A shift of 7 is the zero: `fnum >> 4` is seven bits at most, so that copy of
/// the pair contributes nothing. Read as the peak of the triangle, the rows come
/// to 0, 3.4, 6.7, 10, 13.4, 19.7, 39.4 and 77.9 cents at an F-Num of 0x400 -
/// the YM2608 application manual's PM depth table of 0, 3.4, 6.7, 10, 14, 20, 40
/// and 80 cents, to the cent its own rounding allows, which is what
/// [`pm_depth_matches_the_manual`] pins.
const PM_SHIFTS: [[(u8, u8); 8]; 8] = [
    [
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 7),
    ],
    [
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 2),
        (7, 2),
        (7, 2),
        (7, 2),
    ],
    [
        (7, 7),
        (7, 7),
        (7, 7),
        (7, 2),
        (7, 2),
        (7, 2),
        (1, 7),
        (1, 7),
    ],
    [
        (7, 7),
        (7, 7),
        (7, 2),
        (7, 2),
        (1, 7),
        (1, 7),
        (1, 2),
        (1, 2),
    ],
    [
        (7, 7),
        (7, 7),
        (1, 2),
        (1, 7),
        (1, 7),
        (1, 7),
        (1, 2),
        (0, 7),
    ],
    [
        (7, 7),
        (7, 7),
        (1, 7),
        (1, 7),
        (0, 7),
        (0, 7),
        (0, 2),
        (0, 1),
    ],
    [
        (7, 7),
        (7, 7),
        (1, 7),
        (1, 7),
        (0, 7),
        (0, 7),
        (0, 2),
        (0, 1),
    ],
    [
        (7, 7),
        (7, 7),
        (1, 7),
        (1, 7),
        (0, 7),
        (0, 7),
        (0, 2),
        (0, 1),
    ],
];

/// PMS 6 and 7 share [`PM_SHIFTS`]'s fifth row and multiply it instead, which is
/// what the hardware does with the offset before the last shift down.
const PM_TOP_PMS: u8 = 5;

/// The PM phase of the counter is the counter divided by four: 32 of them to the
/// cycle, 16 of triangle and a sign ([`pm_offset`]).
const PM_PHASE_SHIFT: u8 = 2;

/// The triangle of PM steps is 16 wide before the fold: `&4` says which way the
/// other three of the four low bits run.
const PM_STEPS: u8 = 0x0F;

/// The sign of the PM offset: bit 4 of the PM phase, which is bit 6 of the
/// counter - the same bit that turns the AM triangle around, so the two sides of
/// a `&22` frequency agree about which half of the cycle they are in.
const PM_NEGATIVE: u8 = 0x10;

/// The 7-bit LFO counter, and the per-channel `&B4` depths it is read at.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lfo {
    /// `&22` bit 3: the LFO runs. Clear holds [`Lfo::counter`] at 0.
    enable: bool,
    /// `&22` bits 2-0: which [`STEP_PERIOD`] divides the counter by.
    freq: u8,
    /// The counter, 0..127.
    counter: u8,
    /// Samples taken of the current [`STEP_PERIOD`], 0 to one short of it after
    /// every [`Lfo::tick`] - a lowered `&22` can leave it above the new period for
    /// one sample, which is what [`Lfo::tick`]'s `>=` is for.
    step: u8,
    /// `&B4` bits 5-4 per channel: how far down the AM triangle that channel's
    /// amplitude-modulated operators are read at.
    ams: [u8; CHANNELS],
    /// `&B4` bits 2-0 per channel: how deep that channel's PM swings.
    pms: [u8; CHANNELS],
}

impl Lfo {
    /// An LFO that has never been written to: stopped, at counter 0, with every
    /// channel at AMS 0 and PMS 0, which is what a chip answers `&22` and `&B4`
    /// with before anything has written them.
    pub(crate) fn new() -> Self {
        Self {
            enable: false,
            freq: 0,
            counter: 0,
            step: 0,
            ams: [0; CHANNELS],
            pms: [0; CHANNELS],
        }
    }

    /// A data port write of `value` to `register` of `part`, for the two
    /// registers the LFO reads: the global `&22` and the per-channel `&B4`.
    pub(crate) fn register_written(&mut self, part: usize, register: u8, value: u8) {
        if register == regs::REG_LFO {
            // The global registers live in part I alone.
            if part == PART_I {
                self.enable = value & 0x08 != 0;
                self.freq = value & 0x07;
            }
            return;
        }

        if let Some(channel) = regs::channel_of_register(part, regs::REG_AMS_PMS, register) {
            self.ams[channel] = (value >> 4) & 0x03;
            self.pms[channel] = value & 0x07;
        }
    }

    /// Advance by one output sample: one [`STEP_PERIOD`] at a time, and the
    /// counter does not run past its 128 steps.
    ///
    /// The recirculate compares `>=` rather than `==`, because `&22` bits 2-0 can
    /// be rewritten while [`Lfo::step`] is already past the new, smaller period -
    /// and an equality that can then never fire again leaves the divider counting
    /// forever: `step` climbs to 255 and the increment overflows the `u8`, which
    /// is a panic in a debug build and a permanently out-of-phase LFO in a
    /// release one (the #349 panic, 403 ROMs of the M6-05 compatibility sweep).
    /// The divider is a counter that recirculates, so the observable requirement
    /// is only that it never runs past the period in force and keeps stepping at
    /// it, with no way to get stuck: `>=` recovers on the first sample at or after
    /// the newly programmed period and the 7-bit counter keeps its place.
    ///
    /// `wrapping_add` on its own would not do. It silences the panic but leaves
    /// `step` to climb the rest of the old swing first - up to another 255 samples
    /// before the LFO moves again, against a 5-sample period at the fastest
    /// frequency - which is the same out-of-phase fault, made quiet.
    pub(crate) fn tick(&mut self) {
        self.step += 1;
        if self.step >= self.period() {
            self.step = 0;
            self.counter = (self.counter + 1) & COUNTER_MASK;
        }

        // The enable gates the counter, not the divider: `&22` bit 3 clear holds
        // the counter at 0 (YM2608 application manual `&22`), so re-enabling
        // restarts the wave rather than resuming it mid-swing.
        if !self.enable {
            self.counter = 0;
        }
    }

    /// What the LFO adds to the attenuation of one operator right now: the
    /// triangle at full depth, shifted down by that channel's AMS, and 0 for an
    /// operator whose `&60` bit 7 is clear (`am_on`).
    ///
    /// It is up to the caller to add it to the envelope attenuation, before the
    /// clamp at silence and in the order `crate::envelope` documents.
    pub(crate) fn amplitude(&self, channel: usize, am_on: bool) -> u8 {
        if !am_on {
            return 0;
        }
        let shift = AM_SHIFT[usize::from(self.ams[channel] & 0x03)];
        triangle(self.counter) >> shift
    }

    /// The LFO's phase as the phase modulation reads it: the counter divided by
    /// four, of which [`PM_NEGATIVE`] is the sign and the low four bits the step
    /// of the triangle. Read by [`crate::phase`], which turns it into an offset
    /// with [`pm_offset`] once it knows the channel's F-Num.
    pub(crate) fn pm_phase(&self) -> u8 {
        self.counter >> PM_PHASE_SHIFT
    }

    /// The PMS of each channel, for [`crate::phase`] to offset its F-Num by.
    pub(crate) fn pms(&self) -> &[u8; CHANNELS] {
        &self.pms
    }

    /// Samples per counter step of the frequency in force.
    fn period(&self) -> u8 {
        STEP_PERIOD[usize::from(self.freq & 0x07)]
    }

    /// The counter, which nothing but the model and its tests reads: the chip
    /// reads [`Lfo::amplitude`] and [`Lfo::pm_phase`] instead.
    #[cfg(test)]
    pub(crate) fn counter(&self) -> u8 {
        self.counter
    }
}

/// The AM triangle at `counter`: 126 at the two ends of the cycle, 0 at the two
/// middle steps.
///
/// The first half of the cycle runs the low six bits backwards and the second
/// forwards, so the wave starts at its peak - which is what makes a note keyed
/// on with the LFO at 0 start quiet rather than start loud, the way the manual's
/// own AM figure has it (YM2608 application manual `&22`, AM output figure).
fn triangle(counter: u8) -> u8 {
    let half = if counter & 0x40 == 0 {
        counter ^ 0x3F
    } else {
        counter & 0x3F
    };
    half * 2
}

/// The PM offset of a channel, signed, in halves of an F-Num: what to add to
/// `fnum << 1` before the block shift of [`crate::phase::increment_pm`].
///
/// `phase` is [`Lfo::pm_phase`] and `pms` is `&B4` bits 2-0. The 16 steps of the
/// phase fold onto the eight of [`PM_SHIFTS`] - the triangle goes up them and
/// back down again - and [`PM_NEGATIVE`] says which way round (YM2608
/// application manual `&B4`, PM depth table).
///
/// Halves of an F-Num are the unit the hardware works in, because the F-Num it
/// offsets is itself already doubled on the way to the phase increment; the
/// offset is masked to those 12 bits by the increment, as it is on the chip.
pub(crate) fn pm_offset(fnum: u16, pms: u8, phase: u8) -> i32 {
    let pms = pms & 0x07;
    let step = usize::from(fold_pm_step(phase & PM_STEPS));
    let (first, second) = PM_SHIFTS[usize::from(pms)][step];

    // The top seven bits of the F-Num, which is what a PM depth of "cents" - a
    // ratio - has to be read at.
    let fnum_high = u32::from(fnum & 0x07FF) >> 4;
    let mut offset = (fnum_high >> u32::from(first)) + (fnum_high >> u32::from(second));
    if pms > PM_TOP_PMS {
        offset <<= u32::from(pms - PM_TOP_PMS);
    }
    let offset = (offset >> 2) as i32;

    if phase & PM_NEGATIVE == 0 {
        offset
    } else {
        -offset
    }
}

/// The step of [`PM_SHIFTS`] a 0..15 PM phase is: 0 to 7 forwards and 7 to 0
/// backwards, so the offset rises to the depth of the row and falls again.
fn fold_pm_step(step: u8) -> u8 {
    if step & 0x08 == 0 {
        step
    } else {
        step ^ PM_STEPS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two `&22` writes of the tests: `&22` with its enable bit set, at the
    /// slowest and fastest of the eight frequencies.
    const LFO_ON: u8 = 0x08;

    /// A data port write of `register` on part I, for the LFO's own registers.
    fn write(lfo: &mut Lfo, register: u8, value: u8) {
        lfo.register_written(PART_I, register, value);
    }

    /// `&22` bit 3 clear holds the counter at 0. The divider runs and the counter
    /// does not go anywhere, so a LFO that was running and is switched off comes
    /// back to the top of the AM triangle rather than freezing where it was
    /// (YM2608 application manual `&22`).
    #[test]
    fn lfo_disabled_holds_zero() {
        let mut lfo = Lfo::new();
        write(&mut lfo, regs::REG_AMS_PMS, 0x30);
        write(&mut lfo, regs::REG_LFO, 0x07);
        for _ in 0..5000 {
            lfo.tick();
        }
        assert_eq!(
            lfo.counter(),
            0,
            "freq 7 with bit 3 clear: 5000 samples of nothing"
        );
        assert_eq!(
            lfo.amplitude(0, true),
            126,
            "and AM sits at the top of the triangle, which is counter 0"
        );

        // The same patch with the enable set runs, and switching it off again
        // pulls the counter back to 0 on the very next sample.
        let mut lfo = Lfo::new();
        write(&mut lfo, regs::REG_LFO, LFO_ON | 0x07);
        for _ in 0..20 {
            lfo.tick();
        }
        assert_eq!(lfo.counter(), 4, "four steps of the 5-sample frequency");
        write(&mut lfo, regs::REG_LFO, 0x07);
        lfo.tick();
        assert_eq!(lfo.counter(), 0, "one sample of `&22` 0 is enough");
    }

    /// `&22` 0 is a step every 108 samples, and the counter runs 0..127 of them
    /// and comes back to the start (the eight periods of [`STEP_PERIOD`] and the
    /// 7-bit counter of the YM2608 application manual `&22`).
    #[test]
    fn lfo_period_freq0() {
        let mut lfo = Lfo::new();
        write(&mut lfo, regs::REG_LFO, LFO_ON);

        for _ in 0..107 {
            lfo.tick();
        }
        assert_eq!(lfo.counter(), 0, "107 of the 108 samples of the first step");
        lfo.tick();
        assert_eq!(lfo.counter(), 1);

        for _ in 0..108 {
            lfo.tick();
        }
        assert_eq!(lfo.counter(), 2);

        // And a whole cycle of it: 128 steps, back to where it started.
        for _ in 0..108 * 126 {
            lfo.tick();
        }
        assert_eq!(lfo.counter(), 0, "128 steps of 108 samples is a cycle");
    }

    /// Every frequency of `&22` divides by its own [`STEP_PERIOD`], and nothing
    /// steps twice in the same sample or misses one either.
    #[test]
    fn lfo_period_every_frequency() {
        for freq in 0..8u8 {
            let mut lfo = Lfo::new();
            write(&mut lfo, regs::REG_LFO, LFO_ON | freq);
            let period = STEP_PERIOD[usize::from(freq)];

            for expected in 1..=4u8 {
                for _ in 0..period - 1 {
                    lfo.tick();
                }
                assert_eq!(
                    lfo.counter(),
                    expected - 1,
                    "freq {freq} is a {period}-sample step"
                );
                lfo.tick();
                assert_eq!(lfo.counter(), expected, "freq {freq}");
            }
        }
    }

    /// A `&22` write that *lowers* the LFO frequency part-way through a swing must
    /// not leave the divider counting towards a period it no longer has.
    ///
    /// `&22` bits 2-0 can be rewritten at any moment, and a driver moving from a
    /// slow tremolo to a fast one leaves [`Lfo::step`] past the new, smaller
    /// period. Counting on equality there is no way back: `step` can never reach
    /// the period again, so it climbs to 255 and `+= 1` overflows the `u8`. That
    /// is the #349 panic, which took out 403 ROMs of the M6-05 compatibility
    /// sweep.
    #[test]
    fn lfo_freq_lowered_mid_swing_keeps_dividing() {
        let mut lfo = Lfo::new();
        write(&mut lfo, regs::REG_LFO, LFO_ON);
        for _ in 0..120 {
            lfo.tick();
        }
        // 120 samples of the 108-sample frequency is one step plus 12 samples of
        // the next one.
        assert_eq!((lfo.counter(), lfo.step), (1, 12));

        // `&22` to the fastest frequency: a 5-sample period, well short of where
        // the divider already is.
        write(&mut lfo, regs::REG_LFO, LFO_ON | 0x07);
        let period = lfo.period();
        assert!(period < lfo.step, "the whole point: {period} < 12");

        // First of all, simply not panicking. There is deliberately no assertion in
        // this loop, so on the unfixed divider the overflow is what reports the bug.
        for _ in 0..5000 {
            lfo.tick();
        }

        // And recovered rather than merely surviving: the divider is never left
        // sitting at or above the period in force, and the counter is still
        // turning at that period - 1000 samples of a 5-sample divider is 200 steps.
        let mut steps = 0;
        for _ in 0..1000 {
            let before = lfo.counter();
            lfo.tick();
            assert!(
                lfo.step < period,
                "step {} parked above the {period}-sample period",
                lfo.step
            );
            if lfo.counter() != before {
                steps += 1;
            }
        }
        assert_eq!(steps, 200, "1000 samples of the new 5-sample frequency");
    }

    /// The phase a lowered `&22` lands on is pinned, so the fix cannot regress
    /// into a divider that recirculates every sample instead of every period.
    ///
    /// The divider is a counter, so the requirement is only that it recirculates
    /// once it has reached the period that is now programmed: the very next sample
    /// in this case, since it is already 12 past a 5-sample period, and every
    /// fifth sample after that. Nothing else about the counter moves - the 7-bit
    /// counter of [`COUNTER_MASK`] keeps its place from before the write.
    #[test]
    fn lfo_freq_lowered_mid_swing_restarts_the_divider_at_once() {
        let mut lfo = Lfo::new();
        write(&mut lfo, regs::REG_LFO, LFO_ON);
        for _ in 0..120 {
            lfo.tick();
        }
        write(&mut lfo, regs::REG_LFO, LFO_ON | 0x07);

        let mut advancing = Vec::new();
        for i in 0..40usize {
            let before = usize::from(lfo.counter());
            lfo.tick();
            let after = usize::from(lfo.counter());
            let delta = (after + 128 - before) % 128;
            assert!(delta <= 1, "one sample of a 5-sample divider moved {delta}");
            if delta == 1 {
                advancing.push(i);
            }
        }
        assert_eq!(
            advancing,
            vec![0, 5, 10, 15, 20, 25, 30, 35],
            "one step of the new 5-sample period, immediately, and then every 5"
        );
    }

    /// The AM triangle spans 0..126 and nothing outside it: 11.8 dB at the peak
    /// of the 1024 of a 96 dB attenuation scale, and silence at the bottom is not
    /// the LFO's to give (YM2608 application manual `&22`, AM output figure).
    #[test]
    fn am_triangle_spans_0_to_126() {
        let mut lfo = Lfo::new();
        write(&mut lfo, regs::REG_LFO, LFO_ON);
        write(&mut lfo, regs::REG_AMS_PMS, 0x30);

        let mut seen = vec![];
        for _ in 0..128 * usize::from(STEP_PERIOD[0]) {
            seen.push(lfo.amplitude(0, true));
            lfo.tick();
        }
        assert_eq!(seen.iter().min().copied(), Some(0));
        assert_eq!(seen.iter().max().copied(), Some(126));
        assert!(
            seen.iter().all(|am| *am <= 126),
            "AM is 0..126 at full depth"
        );
    }

    /// The four AMS values of `&B4` bits 5-4 peak at 126, 63, 15 and 0: 11.8,
    /// 5.9, 1.4 and 0 dB of the [`AM_SHIFT`] table, and an operator whose `&60`
    /// bit 7 is clear gets none of it whatever its channel is set to.
    #[test]
    fn am_depths_are_the_ams_table() {
        for (ams, peak) in [(3u8, 126u8), (2, 63), (1, 15), (0, 0)] {
            let mut lfo = Lfo::new();
            write(&mut lfo, regs::REG_LFO, LFO_ON);
            write(&mut lfo, regs::REG_AMS_PMS, ams << 4);

            let mut peak_seen = 0;
            for _ in 0..128 * usize::from(STEP_PERIOD[7]) {
                peak_seen = peak_seen.max(lfo.amplitude(0, true));
                lfo.tick();
            }
            assert_eq!(peak_seen, peak, "AMS {ams}");
            assert_eq!(
                lfo.amplitude(0, false),
                0,
                "AMS {ams} with `&60` bit 7 clear"
            );
        }
    }

    /// PM 0 has no offset at all, at every phase and every F-Num (YM2608
    /// application manual `&B4`: PMS 0 is the depth-0 row).
    #[test]
    fn pm_zero_is_still() {
        for phase in 0..32u8 {
            for fnum in [0x000u16, 0x001, 0x040, 0x400, 0x7FF] {
                assert_eq!(pm_offset(fnum, 0, phase), 0, "PMS 0 phase {phase}");
            }
        }
        // A depth-0 row of its own, too: nothing at all for a channel that has
        // never had a `&B4` written to it.
        assert_eq!(pm_offset(0x400, 0, 7), 0);
    }

    /// The PM offset is symmetric about the F-Num it was written: the two halves
    /// of the cycle are the same offsets with the other sign, which is what makes
    /// a vibrato a vibrato rather than a pitch slide.
    #[test]
    fn pm_offset_is_symmetric() {
        for pms in 0..8u8 {
            for phase in 0..16u8 {
                for fnum in [0x001u16, 0x123, 0x400, 0x7FF] {
                    assert_eq!(
                        pm_offset(fnum, pms, phase),
                        -pm_offset(fnum, pms, phase | PM_NEGATIVE),
                        "PMS {pms} phase {phase} against {:#04x}",
                        phase | PM_NEGATIVE,
                    );
                }
            }
        }
    }

    /// The peak of the PM swing, by PMS, against the YM2608 application manual's
    /// depth table.
    ///
    /// At an F-Num of 0x400 the top seven bits are a clean 64 and the shifts of
    /// [`PM_SHIFTS`] are exact, so the peaks are exact too: in halves of an
    /// F-Num of 0x400 one is 1/2048 of the frequency, which is 0.84 cents, and
    /// the eight rows peak at 0, 4, 8, 12, 16, 24, 48 and 96 of them - 0, 3.4,
    /// 6.7, 10, 13.4, 19.7, 39.4 and 77.9 cents against the manual's 0, 3.4,
    /// 6.7, 10, 14, 20, 40 and 80.
    #[test]
    fn pm_depth_matches_the_manual() {
        const FNUM: u16 = 0x400;
        let peaks: Vec<i32> = (0..8u8)
            .map(|pms| {
                (0..32u8)
                    .map(|phase| pm_offset(FNUM, pms, phase).abs())
                    .max()
                    .unwrap()
            })
            .collect();
        assert_eq!(peaks, vec![0, 4, 8, 12, 16, 24, 48, 96]);

        let cents: Vec<f64> = peaks
            .iter()
            .map(|peak| {
                let ratio = f64::from(*peak) / f64::from(FNUM) / 2.0;
                1200.0 * (1.0 + ratio).log2()
            })
            .collect();
        for (pms, expected) in cents
            .iter()
            .zip([0.0, 3.4, 6.7, 10.0, 14.0, 20.0, 40.0, 80.0])
        {
            assert!(
                (pms - expected).abs() <= 2.5,
                "PMS is {pms:.1} cents, the manual says {expected}"
            );
        }
    }

    /// The 16 steps of the PM phase are a triangle over the eight of
    /// [`PM_SHIFTS`]: up to the depth of the row and back down, once each way.
    #[test]
    fn pm_phase_is_a_triangle() {
        let steps: Vec<u8> = (0..16u8).map(fold_pm_step).collect();
        assert_eq!(steps, vec![0, 1, 2, 3, 4, 5, 6, 7, 7, 6, 5, 4, 3, 2, 1, 0]);
    }
}
