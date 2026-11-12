//! The channel mix: the eight algorithms, operator 1's feedback, and the sum of
//! the carriers.
//!
//! An [`FmChannel`] is one of the six channels of the chip seen from the far
//! side of the operators: it knows which way the four of them are wired
//! together, what `&B0` says about feedback, and what the channel sounds like
//! once its four operators have answered. [`FmChannel::step`] is the whole of
//! it - one call per output sample, after the phase counters and the envelopes
//! have moved - and [`FmChannel::output`] is the 14-bit sum it leaves behind.
//! The DAC of M5-07b (#112) is what reads that sum, which is why the sum is
//! `#[allow(dead_code)]` here.
//!
//! ## The connection table
//!
//! `&B0` bits 2-0 choose one of the eight connection modes and bits 5-3 the
//! feedback of operator 1 (YM2608 application manual "3. Register descriptions",
//! `&B0`). Written with S1..S4 for the four operators, "->" for "modulates the
//! phase of" and "+" for "is summed into the output with", the eight are:
//!
//! ```text
//! 0: S1 -> S2 -> S3 -> S4 -> out
//! 1: (S1 + S2) -> S3 -> S4 -> out
//! 2: (S1 + (S2 -> S3)) -> S4 -> out
//! 3: ((S1 -> S2) + S3) -> S4 -> out
//! 4: (S1 -> S2) + (S3 -> S4) -> out
//! 5: S1 -> each of S2, S3, S4; out = S2 + S3 + S4
//! 6: (S1 -> S2) + S3 + S4
//! 7: S1 + S2 + S3 + S4
//! ```
//!
//! ## The chip order, and the four paths that are a step behind
//!
//! The chip has one operator's worth of log-sin and exponent hardware and walks
//! the 24 slots of the six channels through it in the order the registers are
//! laid out - [`SLOT_S1`], [`SLOT_S3`], [`SLOT_S2`], [`SLOT_S4`], which is why
//! the register offsets of a channel run `+00`, `+04`, `+08`, `+0C` and not
//! S1..S4 (see [`crate::phase`]). A carrier can be wired to two things only: the
//! slot the hardware evaluated immediately before it, whose output is in hand and
//! therefore fresh, and one of two delay registers holding the last output of S1
//! and of S2 - the S1 one being the same pair of taps the feedback reads. So a
//! modulation is fresh exactly when the modulator is the slot in front of its
//! carrier in the S1, S3, S2, S4 walk, which is S1 to S2 and S3 to S4, and every
//! other connection in the table is a step old:
//!
//! - S2 to S3, in algorithms 0, 1 and 2, where S3 is evaluated before S2 at all.
//! - S2 to S4, in algorithm 3, where S2 is evaluated before S4 but is not the
//!   slot immediately in front of it.
//! - S1 to S3, in algorithms 1 and 5, and S1 to S4, in algorithms 2 and 5, both
//!   of which come from the delay register the feedback shares.
//!
//! The set was checked against the connection table of
//! Nuked-OPN2's `ym3438.c` (`nukeykt/Nuked-OPN2`, read 11 Nov 2026), whose
//! `fm_algorithm` routes S3 and S4 through `fm_op1`/`fm_op2` delay registers in
//! exactly those algorithms and through the just-generated `fm_out` of the
//! previous slot in the rest. Nothing of that file is reproduced here: this is
//! the manual's own table, read through the S1, S3, S2, S4 order the `+00`
//! `+04` `+08` `+0C` offsets force on the chip.
//!
//! ## Feedback, and the width of a modulation
//!
//! Operator 1 modulates itself with its own two previous outputs,
//! `(out(n-1) + out(n-2)) >> (10 - FB)` and nothing at all for FB 0 (YM2608
//! application manual `&B0`; the same shift the YM3812/OPL2 programming manuals
//! give for the feedback of their first operator, which the OPN core shares).
//! A modulating operator's 14-bit output, on the other hand, enters the
//! carrier's ten-bit angle half as wide: it is shifted right by one first, so the
//! loudest modulation one operator can lay on another is 8168 >> 1 = 4084 of the
//! 1024 of a cycle - just under four cycles of the carrier's own wave, which is
//! what lets a two-operator algorithm fill out a spectrum so hard. Both are
//! added, modulo ten bits, by [`crate::operator::angle`].

use crate::operator;
use crate::phase::{SLOT_S1, SLOT_S2, SLOT_S3, SLOT_S4, SLOTS};

/// Connection modes of `&B0` bits 2-0.
const ALGORITHMS: usize = 8;

/// The order the chip walks the four operators of a channel in, which is the
/// order the slot offsets of `+00`, `+04`, `+08` and `+0C` name them in (see
/// [`crate::phase`] and the module docs).
const EVAL_ORDER: [usize; SLOTS] = [SLOT_S1, SLOT_S3, SLOT_S2, SLOT_S4];

/// How far the two taps of the feedback are shifted down before they are added
/// to operator 1's angle: `FEEDBACK_SHIFT - FB`, so FB 7 loses three bits and FB
/// 1 loses nine (YM2608 application manual `&B0`).
const FEEDBACK_SHIFT: u32 = 10;

/// How far a modulating operator's output is shifted down before it is added to
/// the ten-bit angle of its carrier (module docs, and #111).
const MODULATION_SHIFT: u32 = 1;

/// The bottom of the channel sum, the 14-bit signed range #111 gives the mix
/// (the 9-bit DAC domain and the ladder of M5-07b (#112) are what turn this into
/// a sample on the pin).
const OUTPUT_MIN: i16 = -8192;

/// The top of the channel sum, the complement of [`OUTPUT_MIN`].
const OUTPUT_MAX: i16 = 8191;

/// One operator's output as a modulation source: which slot, and whether the
/// carrier reads it from this step or from the step before. See the module docs
/// for which of the two the hardware can reach.
#[derive(Clone, Copy, Debug)]
struct Tap {
    /// The slot the value comes from, one of [`SLOT_S1`] and friends.
    slot: usize,
    /// Whether the value is the one the slot produced last step rather than the
    /// one it is producing now.
    before: bool,
}

/// A tap read from the current step: the tapped operator is evaluated before its
/// carrier, so its output is in hand.
const fn now(slot: usize) -> Tap {
    Tap {
        slot,
        before: false,
    }
}

/// A tap read from the previous step: the tapped operator is evaluated after its
/// carrier, so the chip has nothing for it but the value it left behind.
const fn before(slot: usize) -> Tap {
    Tap { slot, before: true }
}

/// The modulation inputs of one carrier, of which a carrier has at most two.
type Taps = [Option<Tap>; 2];

/// The empty list of modulation inputs.
const NONE: Taps = [None, None];

/// The one-modulator form of [`Taps`].
const fn one(tap: Tap) -> Taps {
    [Some(tap), None]
}

/// The two-modulator form of [`Taps`].
const fn two(first: Tap, second: Tap) -> Taps {
    [Some(first), Some(second)]
}

/// What modulates each operator of each algorithm, `[algorithm][carrier]` with
/// the carriers in [`SLOT_S1`], [`SLOT_S3`], [`SLOT_S2`], [`SLOT_S4`] order -
/// the order of the slot offsets, not of the operators.
///
/// The table is the connection list of the module docs read through the chip
/// order, delays and all; operator 1 is never a carrier of another operator, so
/// its column is empty in every row and its only modulation is the feedback.
const MODULATORS: [[Taps; SLOTS]; ALGORITHMS] = [
    //           S1                                    S3                                        S2                              S4
    /* 0 */
    [
        NONE,
        one(before(SLOT_S2)),
        one(now(SLOT_S1)),
        one(now(SLOT_S3)),
    ],
    /* 1 */
    [
        NONE,
        two(before(SLOT_S1), before(SLOT_S2)),
        NONE,
        one(now(SLOT_S3)),
    ],
    /* 2 */
    [
        NONE,
        one(before(SLOT_S2)),
        NONE,
        two(before(SLOT_S1), now(SLOT_S3)),
    ],
    /* 3 */
    [
        NONE,
        NONE,
        one(now(SLOT_S1)),
        two(before(SLOT_S2), now(SLOT_S3)),
    ],
    /* 4 */ [NONE, NONE, one(now(SLOT_S1)), one(now(SLOT_S3))],
    /* 5 */
    [
        NONE,
        one(before(SLOT_S1)),
        one(now(SLOT_S1)),
        one(before(SLOT_S1)),
    ],
    /* 6 */ [NONE, NONE, one(now(SLOT_S1)), NONE],
    /* 7 */ [NONE, NONE, NONE, NONE],
];

/// Which operators the channel output is the sum of, as a bitmask of
/// `1 << slot` in [`SLOT_S1`] order: S4 in every algorithm, S2 from algorithm 4
/// on and S3 from algorithm 5 on, S1 only in algorithm 7. The `Out` column of
/// the connection table in the module docs, which is what the manual's own
/// per-algorithm diagrams say.
const CARRIERS: [u8; ALGORITHMS] = [
    1 << SLOT_S4,
    1 << SLOT_S4,
    1 << SLOT_S4,
    1 << SLOT_S4,
    (1 << SLOT_S2) | (1 << SLOT_S4),
    (1 << SLOT_S2) | (1 << SLOT_S3) | (1 << SLOT_S4),
    (1 << SLOT_S2) | (1 << SLOT_S3) | (1 << SLOT_S4),
    (1 << SLOT_S1) | (1 << SLOT_S2) | (1 << SLOT_S3) | (1 << SLOT_S4),
];

/// One channel's operator sum: `&B0`, the outputs of the four operators of the
/// step it is in, the values the delayed taps and the feedback read from the
/// steps before, and the mix.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FmChannel {
    /// `&B0` bits 2-0. Zero is algorithm 0, which is also what a chip that has
    /// never been written to `&B0` does.
    algorithm: u8,
    /// `&B0` bits 5-3, 0 for no feedback.
    feedback: u8,
    /// The operators' outputs, as of the step in hand: [`EVAL_ORDER`] overwrites
    /// one entry per slot, so an entry still holds the previous step's value
    /// until its own turn comes.
    out: [i16; SLOTS],
    /// The outputs of the step before, which is what the [`before`] taps read.
    was: [i16; SLOTS],
    /// The two taps of operator 1's feedback: `feed[0]` is S1's output from the
    /// step before this one and `feed[1]` the one before that, both raw - the
    /// shift of `10 - FB` is applied when they are read (YM2608 application
    /// manual `&B0`).
    feed: [i16; 2],
    /// The channel sum the last [`FmChannel::step`] left behind.
    mix: i16,
}

impl FmChannel {
    /// A channel wired as algorithm 0 with no feedback, every operator silent,
    /// which is what `&B0` reads as before anything has written it.
    pub(crate) fn new() -> Self {
        Self {
            algorithm: 0,
            feedback: 0,
            out: [0; SLOTS],
            was: [0; SLOTS],
            feed: [0; 2],
            mix: 0,
        }
    }

    /// A write of this channel's `&B0`: bits 5-3 the feedback and bits 2-0 the
    /// algorithm (YM2608 application manual `&B0`). Both take effect on the next
    /// [`FmChannel::step`], which is what a chip with write-only registers does.
    pub(crate) fn register_written(&mut self, value: u8) {
        self.feedback = (value >> 3) & 0x07;
        self.algorithm = value & 0x07;
    }

    /// One output sample of the channel: evaluate the four operators in the
    /// chip's own order with the modulation each of them is entitled to, then
    /// sum the carriers.
    ///
    /// `phase` is each operator's 20-bit phase counter and `att` its 10-bit
    /// envelope attenuation, both in [`SLOT_S1`] order.
    pub(crate) fn step(&mut self, phase: [u32; SLOTS], att: [u16; SLOTS]) {
        for &slot in &EVAL_ORDER {
            let modulation = self.modulation(slot);
            let angle = operator::angle(phase[slot], modulation);
            self.out[slot] = operator::output(angle, att[slot]);
        }

        self.feed[1] = self.feed[0];
        self.feed[0] = self.out[SLOT_S1];
        self.was = self.out;
        self.mix = self.sum();
    }

    /// The channel sum of the last step, 14-bit signed.
    ///
    /// `#[allow(dead_code)]` because the chip has nowhere to put it yet: the DAC
    /// output stage of M5-07b (#112) is the first reader, and until then only
    /// this module's tests ask for it.
    #[allow(dead_code)]
    pub(crate) fn output(&self) -> i16 {
        self.mix
    }

    /// What operator `slot` is modulated by right now: its taps, added in the
    /// ten-bit domain they all end up in, plus operator 1's feedback.
    fn modulation(&self, slot: usize) -> i32 {
        let taps = MODULATORS[usize::from(self.algorithm)][slot];
        let mut modulation = 0;
        for tap in taps.into_iter().flatten() {
            let value = if tap.before {
                self.was[tap.slot]
            } else {
                self.out[tap.slot]
            };
            modulation += i32::from(value) >> MODULATION_SHIFT;
        }
        if slot == SLOT_S1 {
            modulation += self.feedback_modulation();
        }
        modulation
    }

    /// The feedback of operator 1: its two previous outputs, added and shifted
    /// by what `&B0` says, and nothing at all for FB 0 (YM2608 application
    /// manual `&B0`, and the module docs for why the shift is not the
    /// [`MODULATION_SHIFT`] the modulating operators go through).
    fn feedback_modulation(&self) -> i32 {
        if self.feedback == 0 {
            return 0;
        }
        (i32::from(self.feed[0]) + i32::from(self.feed[1]))
            >> (FEEDBACK_SHIFT - u32::from(self.feedback))
    }

    /// The sum of the carriers of this channel's algorithm, clamped to the
    /// 14-bit signed range rather than to the 9-bit DAC domain of #112.
    fn sum(&self) -> i16 {
        let carriers = CARRIERS[usize::from(self.algorithm)];
        let mut sum = 0i32;
        for slot in 0..SLOTS {
            if carriers & (1 << slot) != 0 {
                sum += i32::from(self.out[slot]);
            }
        }
        sum.clamp(i32::from(OUTPUT_MIN), i32::from(OUTPUT_MAX)) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ym2612;
    use crate::phase::SLOTS;

    /// Port 0 and port 1: the address latch and the data port of part I, which
    /// channel 1 - the one every test here plays - lives on.
    const ADDR_1: u8 = 0;
    const DATA_1: u8 = 1;

    /// A total level that silences an operator: `&40` 127 is 1016 of the 10-bit
    /// attenuation, and 1016 is past the 13 octaves [`crate::operator::output`]
    /// turns into nothing (`&40`).
    const MUTE: u8 = 0x7F;

    /// The `&30` byte of every operator here: detune 0 and multiple 1, so the
    /// four operators of a channel all count at the channel's own increment.
    const MUL_1: u8 = 0x01;

    /// The `&50` byte of every operator here: rate scaling 0 and attack rate 31,
    /// which is the 62 the envelope calls instant, so the key-on itself is at
    /// full level and a test does not have to clock an envelope up first.
    const ATTACK_NOW: u8 = 0x1F;

    /// The register offset of a slot within a channel's per-operator groups:
    /// the `+00`, `+04`, `+08`, `+0C` of [`SLOT_S1`] order.
    fn offset(slot: usize) -> u8 {
        u8::try_from(slot).expect("four slots") << 2
    }

    /// `&30` + `offset`, `&40` + `offset` and `&50` + `offset` of one operator
    /// of channel 1: multiple 1, total level `total_level`, instant attack.
    fn operator_patch(chip: &mut Ym2612, slot: usize, total_level: u8) {
        chip.write(ADDR_1, 0x30 + offset(slot));
        chip.write(DATA_1, MUL_1);
        chip.write(ADDR_1, 0x40 + offset(slot));
        chip.write(DATA_1, total_level);
        chip.write(ADDR_1, 0x50 + offset(slot));
        chip.write(DATA_1, ATTACK_NOW);
    }

    /// Channel 1 at block 4, F-Num 0x210, all four operators at the `levels`
    /// given in [`SLOT_S1`] order, and a key-on of all four: the fixed patch of
    /// every test here, less the `&B0` write [`patch`] adds.
    ///
    /// The increment of every operator is then `(0x210 << 4) >> 1` = 4224, a
    /// cycle every 248 samples: 1024 samples are a little over four cycles of it,
    /// so a run both crosses the peak of the wave and is short enough to hash
    /// (`&30`, `&A0`, `&A4`).
    fn patch_operators(chip: &mut Ym2612, levels: [u8; SLOTS]) {
        for (slot, level) in levels.iter().enumerate() {
            operator_patch(chip, slot, *level);
        }
        chip.write(ADDR_1, 0xA4);
        chip.write(DATA_1, 0x22);
        chip.write(ADDR_1, 0xA0);
        chip.write(DATA_1, 0x10);
        chip.write(ADDR_1, 0x28);
        chip.write(DATA_1, 0xF0);
    }

    /// [`patch_operators`] plus the `&B0` of `algorithm` and `feedback`.
    fn patch(chip: &mut Ym2612, algorithm: u8, feedback: u8, levels: [u8; SLOTS]) {
        patch_operators(chip, levels);
        chip.write(ADDR_1, 0xB0);
        chip.write(DATA_1, (feedback << 3) | algorithm);
    }

    /// The channel sum of `channel` of `chip`: what the mix of the last
    /// `Ym2612::clock` left behind.
    fn channel_out(chip: &Ym2612, channel: usize) -> i16 {
        chip.channels[channel].output()
    }

    /// Clock `chip` `count` times and collect channel 0's answer each time.
    fn samples(chip: &mut Ym2612, count: usize) -> Vec<i16> {
        let mut taken = Vec::with_capacity(count);
        for _ in 0..count {
            chip.clock();
            taken.push(channel_out(chip, 0));
        }
        taken
    }

    /// FNV-1a 64 over the low byte and the high byte of every sample, the same
    /// recipe `testkit::frame_hash` uses for pixels and for the same reason: a
    /// hash of a whole run of samples belongs in a golden, and a byte at a time
    /// is what keeps this crate free of a testkit dependency.
    fn hash(wave: &[i16]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for sample in wave {
            for byte in [(*sample & 0xff) as u8, (*sample >> 8) as u8] {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
        hash
    }

    /// Every one of the four operators is silent, so the channel is: the
    /// `levels` of these tests are in [`SLOT_S1`] order, so the fourth entry is
    /// S4 and the third is S2.
    #[test]
    fn alg7_sums_four_carriers() {
        let mut four = Ym2612::new();
        patch(&mut four, 7, 0, [0, 0, 0, 0]);
        let mut one = Ym2612::new();
        patch(&mut one, 7, 0, [0, MUTE, MUTE, MUTE]);

        let four = samples(&mut four, 256);
        let one = samples(&mut one, 256);

        let mut clamped = 0;
        let mut plain = 0;
        for (all, single) in four.iter().zip(&one) {
            let expected = (i32::from(*single) * 4).clamp(-8192, 8191);
            assert_eq!(
                i32::from(*all),
                expected,
                "four identical carriers are four times one of them, clamped"
            );
            if expected == i32::from(OUTPUT_MAX) || expected == i32::from(OUTPUT_MIN) {
                clamped += 1;
            } else {
                plain += 1;
            }
        }

        // Without both halves the test would be a tautology about the clamp, and
        // with neither it would be about nothing at all.
        assert!(clamped > 0 && plain > 0, "{clamped} clamped, {plain} not");
    }

    /// Algorithm 0 is S1 -> S2 -> S3 -> S4 -> out: S4 is the only carrier, so
    /// muting it is muting the channel however loud the other three are.
    #[test]
    fn alg0_only_s4_is_carrier() {
        let mut muted = Ym2612::new();
        patch(&mut muted, 0, 0, [0, 0, 0, MUTE]);
        let mut loud = Ym2612::new();
        patch(&mut loud, 0, 0, [0, 0, 0, 0]);

        assert!(
            samples(&mut muted, 128).iter().all(|s| *s == 0),
            "S4 is the only carrier of algorithm 0"
        );
        assert!(
            samples(&mut loud, 128).iter().any(|s| *s != 0),
            "and the patch is not silent with S4 open"
        );
    }

    /// Algorithm 4 is (S1 -> S2) + (S3 -> S4): its carriers are S2 and S4, so
    /// muting those two mutes the channel even with S1 and S3 both running as
    /// modulators.
    #[test]
    fn alg4_two_carriers() {
        let mut muted = Ym2612::new();
        patch(&mut muted, 4, 0, [0, 0, MUTE, MUTE]);
        let mut loud = Ym2612::new();
        patch(&mut loud, 4, 0, [0, 0, 0, MUTE]);

        assert!(
            samples(&mut muted, 128).iter().all(|s| *s == 0),
            "S2 and S4 are the whole of algorithm 4's output"
        );
        assert!(
            samples(&mut loud, 128).iter().any(|s| *s != 0),
            "and S2 on its own is enough to hear it"
        );
    }

    /// FB 0 is no feedback at all: not a delay line that is multiplied by
    /// nothing, a delay line that is not read. A channel whose taps are full of
    /// garbage from somewhere else therefore sounds exactly like one whose taps
    /// are empty.
    #[test]
    fn feedback_zero_matches_no_feedback() {
        let phase = [0x0_0400; SLOTS];
        let att = [0; SLOTS];

        // Only `feed` is dirtied, because that is all FB 0 claims: `was` is the
        // previous-step delay of the connection table, which algorithm 0 reads
        // from S2 to S3 whatever the feedback says, and dirtying it here would be
        // a test of the tap of `alg0_modulates_s3_with_the_previous_s2` rather
        // than of `feedback_modulation`.
        let mut empty = FmChannel::new();
        let mut dirty = FmChannel::new();
        dirty.feed = [4096, -3000];

        let mut quiet = Vec::new();
        let mut noisy = Vec::new();
        for step in 0u32..64 {
            let phase = phase.map(|p| p + step * 600);
            empty.step(phase, att);
            dirty.step(phase, att);
            quiet.push(empty.output());
            noisy.push(dirty.output());
        }
        assert_eq!(
            quiet, noisy,
            "algorithm 0 with FB 0 reads nothing but S4, whose taps are its own"
        );

        // And at the chip end: a `&B0` write of 0 is the same as never writing
        // the register at all.
        let mut written = Ym2612::new();
        patch(&mut written, 0, 0, [0, 0, 0, 0]);
        let mut untouched = Ym2612::new();
        patch_operators(&mut untouched, [0, 0, 0, 0]);
        assert_eq!(
            samples(&mut written, 64),
            samples(&mut untouched, 64),
            "the default `&B0` is algorithm 0 with no feedback"
        );
    }

    /// The other side of that: the previous-step outputs of the connection table
    /// are a real delay line, and they are read even with FB 0. An algorithm 0
    /// channel whose S2 comes into the first step holding garbage is therefore
    /// wrong for the first sample only - S3 taps it, S4 taps S3, and by the next
    /// step every slot has produced a value of its own (`FmChannel::modulation`,
    /// and the module docs for which taps are a step behind).
    #[test]
    fn previous_step_outputs_are_read_for_one_step() {
        let phase = [0x0_0400; SLOTS];
        let att = [0; SLOTS];

        let mut empty = FmChannel::new();
        let mut dirty = FmChannel::new();
        dirty.feed = [4096, -3000];
        dirty.was = [5000, -5000, 4000, -4000];

        let mut quiet = Vec::new();
        let mut noisy = Vec::new();
        for step in 0u32..64 {
            let phase = phase.map(|p| p + step * 600);
            empty.step(phase, att);
            dirty.step(phase, att);
            quiet.push(empty.output());
            noisy.push(dirty.output());
        }

        assert_ne!(
            quiet[0], noisy[0],
            "the first step has to feel the stale S2"
        );
        assert_eq!(
            &quiet[1..],
            &noisy[1..],
            "and one step is all of it: the delay line is refilled from here on"
        );
    }

    /// FB 7 against the same patch with FB 0: the first 64 samples move. S1 is
    /// only a carrier in algorithm 7, which is the algorithm this chooses so the
    /// feedback cannot hide behind another operator.
    #[test]
    fn feedback_changes_waveform() {
        let mut loud = Ym2612::new();
        patch(&mut loud, 7, 7, [0, MUTE, MUTE, MUTE]);
        let mut plain = Ym2612::new();
        patch(&mut plain, 7, 0, [0, MUTE, MUTE, MUTE]);

        let loud = samples(&mut loud, 64);
        let plain = samples(&mut plain, 64);
        assert!(
            loud.iter().zip(&plain).any(|(a, b)| a != b),
            "feedback 7 has to change what S1 is read at"
        );
    }

    /// The S2 to S3 tap of algorithm 0 is one step behind: S3 is evaluated
    /// before S2, so what it reads is S2's output of the step before.
    ///
    /// The test tells the two readings apart by moving the phase between the
    /// steps: were S3 to read S2's current output it would have to be evaluated
    /// after it, which is the thing the chip cannot do.
    #[test]
    fn alg0_modulates_s3_with_the_previous_s2() {
        let mut channel = FmChannel::new();
        channel.algorithm = 0;

        channel.step([0; SLOTS], [0; SLOTS]);
        let first = channel.out[SLOT_S2];

        let phase = [0x0_8000; SLOTS];
        channel.step(phase, [0; SLOTS]);
        let second = channel.out[SLOT_S2];
        assert_ne!(first, second, "the two steps have to differ");

        let expected = operator::output(operator::angle(0x0_8000, i32::from(first) >> 1), 0);
        assert_eq!(
            channel.out[SLOT_S3], expected,
            "S3 is read at the angle the previous step's S2 gives it"
        );
        assert_ne!(
            channel.out[SLOT_S3],
            operator::output(operator::angle(0x0_8000, i32::from(second) >> 1), 0),
            "which is not this step's S2"
        );
    }

    /// The other half of the delay finding: in algorithm 5 S2 reads S1 fresh,
    /// one evaluation behind it in the same step, while S3 and S4 - which the
    /// chip reaches before S2 - read S1 from the step before.
    #[test]
    fn alg5_reads_s1_fresh_for_s2_and_old_for_s3_and_s4() {
        let mut channel = FmChannel::new();
        channel.algorithm = 5;

        channel.step([0; SLOTS], [0; SLOTS]);
        let first = channel.out[SLOT_S1];

        let phase = [0x0_8000; SLOTS];
        channel.step(phase, [0; SLOTS]);
        let second = channel.out[SLOT_S1];
        assert_ne!(first, second, "S1 moves with the phase");

        let old = operator::output(operator::angle(0x0_8000, i32::from(first) >> 1), 0);
        let fresh = operator::output(operator::angle(0x0_8000, i32::from(second) >> 1), 0);
        assert_ne!(old, fresh, "and the two readings have to differ");

        assert_eq!(channel.out[SLOT_S3], old, "S3 is a step behind S1");
        assert_eq!(channel.out[SLOT_S4], old, "S4 is a step behind S1");
        assert_ne!(
            channel.out[SLOT_S2], old,
            "S2 is not: it is evaluated after S1, in this step"
        );
    }

    /// The eight algorithms of one fixed patch, hashed over the first 1024
    /// samples.
    ///
    /// The constants are this code's own output, recorded once by running it and
    /// printed below for the sanity check #111 asks for: every one of them is
    /// non-zero, the patches do all sound different from each other, and the
    /// shapes are those the table asks for - algorithm 7, where every operator
    /// is a carrier at full level, spends most of its run pinned at the top and
    /// bottom of the 14-bit clamp and so has the fewest distinct samples of the
    /// eight, and algorithm 0, where three modulators stack onto one carrier,
    /// is the busiest. They are a regression guard on the mix, not a hardware
    /// measurement: a change to the tables above, to the operator of #110 or to
    /// the envelope of #109 is what moves them.
    #[test]
    fn golden_alg_hash() {
        const GOLDEN: [u64; ALGORITHMS] = [
            0x6239_f805_2323_0c7e,
            0xcf18_2442_1539_79ac,
            0x5724_efc7_b40f_d4d2,
            0xadfd_e7b8_660b_a181,
            0xcc89_7651_7443_a364,
            0xb567_89a3_8bc6_6cea,
            0xe744_3358_b2e3_8089,
            0x38f3_d43a_dcf5_eb6f,
        ];

        for (algorithm, expected) in GOLDEN.iter().enumerate() {
            let mut chip = Ym2612::new();
            patch(&mut chip, algorithm as u8, 0, [0, 0, 0, 0]);
            let wave = samples(&mut chip, 1024);
            let actual = hash(&wave);
            assert_ne!(actual, 0, "algorithm {algorithm} is silent");
            assert_eq!(actual, *expected, "algorithm {algorithm}");
        }
    }
}
