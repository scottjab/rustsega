//! The output stage: the 9-bit DAC of one channel, the ladder effect of the
//! discrete chip, and the sum of the six.
//!
//! A channel's 14-bit sum ([`crate::channel`]) is not a sample yet. What leaves
//! the chip is the output of six digital-to-analogue converters, one per channel,
//! and each of those reads only the top nine bits of what it is given - the low
//! five are not wired to anything (plutiedev, "YM2612 DAC"; the `>> 5` the
//! community cores apply, and the `rshift` of 5 ymfm passes for the OPN2 parts
//! in `ym2612::generate`). [`dac`] is that step for a channel routed to one side,
//! [`muted`] is what is left of a channel on the side it is not routed to, [`mix`]
//! is the sum of the six per side as `&B4`-`&B6` route them (M5-11, #116), and
//! [`ChipModel`] says which chip is doing it.
//!
//! ## The ladder
//!
//! On a discrete YM2612 the DAC is a resistor ladder of matched transistors, and
//! the biggest of them - the one carrying the most weight, at the crossover of
//! the sign - does not match the rest. The step between the code just below zero
//! and the code at zero is therefore several times the step anywhere else in the
//! range, which is what makes a quiet tone on a Model 1 gritty in a way no
//! amount of dithering fixes: around the crossover the wave is read across a gap
//! of eight codes instead of one. The integrated YM3438 of a Model 2 has no
//! ladder to mismatch and answers with the plain quantised value.
//!
//! The measurements the model below rests on are `+4` on the non-negative side
//! and `-3` on the negative side of the 9-bit code, applied per channel and
//! before the six are summed:
//!
//! - Aaron Giles's ymfm (`aaronsgiles/ymfm`, `src/ymfm_opn.h`,
//!   `dac_discontinuity`, read 11 Nov 2026): `value < 0 ? value - 3 : value + 4`,
//!   applied per channel to that channel's `>> 5` sum in `ym2612::generate` of
//!   `src/ymfm_opn.cpp` and not at all in `ym3438::generate`.
//! - Genesis Plus GX's `core/sound/ym2612.c` (Eke-Eke; changelog 03-12-2017,
//!   "added discrete YM2612 DAC distortion emulation (ladder effect)", read
//!   11 Nov 2026): `+ 4 << 5` for a channel whose output is non-negative and
//!   `- (4 - pan bit) << 5` for a negative one, so `-3` for a channel that is
//!   routed out and `-4` for one that is panned off. The `<< 5` is the same 9-bit
//!   LSB seen from the 14-bit domain this crate keeps its channel sums in.
//!
//! Both are per channel rather than once on the mixed output, which is what the
//! hardware is: a channel that is panned off still drives its converter, so it
//! still contributes its offset - `+4` at zero like everybody else. That the two
//! sources agree on `-3` for a routed channel is why the four the ticket guessed
//! for every code below zero is not used here (see [`LADDER_DOWN`]); everything
//! else about them is the ticket's own model. Neither source is reproduced
//! beyond those two constants.
//!
//! Level matching, the multiplexing of the six converters and the analogue
//! filtering of the board are the mixer's, of M5-14 (#119).

use crate::phase::CHANNELS;
use crate::regs::{PAN_LEFT, PAN_RIGHT};

// Serialising this chip state for save states (M6-07a, #126). Gated on the
// crate's `serde` feature so `cargo build --no-default-features` keeps the
// core with no dependencies at all, as docs/ARCHITECTURE.md asks of it.
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Which output stage [`crate::Ym2612`] is modelling.
///
/// The two are wired to the bus the same way and differ only in the DAC: see the
/// module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ChipModel {
    /// The discrete YM2612 of a Model 1: a 9-bit DAC with the ladder effect, so
    /// a channel carries [`LADDER_UP`] at and above zero and [`LADDER_DOWN`]
    /// below it.
    Ym2612,
    /// The integrated YM3438 of a Model 2 or Model 3: the same 9-bit
    /// quantisation with no ladder, so a silent channel is at exactly 0.
    Ym3438,
}

/// How far down a 14-bit channel sum is shifted to reach the 9-bit DAC: the low
/// five bits of it are not wired to the ladder (module docs).
const DAC_SHIFT: u32 = 5;

/// What the ladder of a [`ChipModel::Ym2612`] adds to a code at or above zero,
/// in 9-bit LSBs. See the module docs for where this and [`LADDER_DOWN`] come
/// from.
const LADDER_UP: i32 = 4;

/// What the ladder of a [`ChipModel::Ym2612`] takes off a code below zero, in
/// 9-bit LSBs.
///
/// SPEC DEVIATION from the ticket, which gives 4 for this. Three is what both
/// references measure for a channel that is actually routed to the output: ymfm
/// subtracts 3 outright, and Genesis Plus GX subtracts `4 - pan_bit`, which is
/// 3 for a routed channel and 4 only for a panned-off one. The ticket's 4 is
/// that muted case read as the general one - and the muted case is real, so it
/// has a constant of its own in [`LADDER_MUTED_DOWN`].
const LADDER_DOWN: i32 = 3;

/// What the ladder of a [`ChipModel::Ym2612`] takes off a channel that is not
/// routed to the side being summed at all, in 9-bit LSBs: the 4 of Genesis Plus
/// GX's `4 - pan_bit` with the routing bit at zero, one step further out than the
/// [`LADDER_DOWN`] of a live channel.
///
/// Nuked-OPN2's `OPN2_ChOutput` has the same shape at its own scale - the side a
/// channel is not routed to gets the sign of that channel's own output where the
/// routed one gets the code - so two of the three references put a muted channel's
/// step on the sign of what it was playing rather than on zero. ymfm puts it at
/// `dac_discontinuity(0)`, which is [`LADDER_UP`] whatever the channel was doing;
/// the ladder is analogue, so the sign is the reading kept here (read 12 Nov 2026).
const LADDER_MUTED_DOWN: i32 = LADDER_DOWN + 1;

/// The largest magnitude one channel can leave at [`dac`]: the 9-bit code runs
/// to 255 - [`crate::channel`] clamps its sum to -8192..8191, so `>> 5` cannot
/// reach the -256 an unclamped -8192 would give - and the ladder adds
/// [`LADDER_UP`] to it. The bottom of the range is one less in magnitude again,
/// at `-256 - LADDER_DOWN` = -259.
const DAC_MAX: i32 = 255 + LADDER_UP;

/// Six channels' worth of [`DAC_MAX`] has to stay inside the i16 [`crate`]
/// returns, because the sum below is deliberately not clamped: it is the sum of
/// six converters, not of six samples, and the hardware has nothing to clamp it
/// with. 6 * 259 is 1554.
const _: () = assert!(DAC_MAX * CHANNELS as i32 <= i16::MAX as i32);

/// One channel's reading at its converter: the 14-bit sum `value` through the
/// 9-bit DAC of `model`, ladder and all, in 9-bit LSBs.
///
/// The shift is arithmetic, which is all "keeping the sign" means here: the sum
/// -1 is the code below the sum 0, not a wrap to the top of the range.
///
/// The domain is the 14-bit channel sum, which [`crate::channel`] clamps to
/// -8192..8191, and `dac` being total over that domain is what [`mix`] and the
/// assertion by [`DAC_MAX`] are built on.
pub(crate) fn dac(value: i16, model: ChipModel) -> i32 {
    let code = i32::from(value) >> DAC_SHIFT;
    match model {
        ChipModel::Ym2612 => {
            if code < 0 {
                code - LADDER_DOWN
            } else {
                code + LADDER_UP
            }
        }
        ChipModel::Ym3438 => code,
    }
}

/// What a channel that is not routed to this side leaves on it: nothing of the
/// code, and the step of ladder its own sign buys it - [`LADDER_UP`] at and above
/// zero and [`LADDER_MUTED_DOWN`] below it on a discrete chip, and nothing at all
/// on a 3438.
///
/// A converter that is wired to nothing still drives its ladder, which is why a
/// panned-off channel is not silence on a Model 1 (module docs, and M5-11 #116).
pub(crate) fn muted(value: i16, model: ChipModel) -> i32 {
    match model {
        ChipModel::Ym2612 => {
            if i32::from(value) >> DAC_SHIFT < 0 {
                -LADDER_MUTED_DOWN
            } else {
                LADDER_UP
            }
        }
        ChipModel::Ym3438 => 0,
    }
}

/// The channel 6 PCM sample `data` of `&2A` in the 14-bit domain the channel sums
/// live in: unsigned, centred on 0x80, and spread over the six bits the 9-bit DAC
/// of [`dac`] cannot see.
///
/// The arithmetic is `((data - 128) << 6)`, which leaves the 9-bit code
/// `(data - 128) << 1` after [`DAC_SHIFT`]: `&2A` drives the top eight bits of the
/// converter and the ninth is only reachable from the `&2C` test register, so a
/// PCM sample only ever lands on the even codes (YM2608 application manual "3.
/// Register descriptions", `&2A`; plutiedev, "DAC"; and the `(write_data ^ 0x80)
/// << 1` of Nuked-OPN2's `OPN2_WriteReg`, read 12 Nov 2026, which is this same
/// code).
///
/// So 0x80 is the centre and a sum of 0, 0xFF is 8128 - the code 254 - and 0x00 is
/// -8192, which is the very bottom of the domain [`crate::channel`] clamps to.
pub(crate) fn pcm_sample(data: u8) -> i16 {
    (i16::from(data) - 128) << 6
}

/// The six channel sums of one step through their converters and added, per
/// output side: what the chip puts on its two pins, in 9-bit LSBs.
///
/// `values` are the six channel sums in channel order and `pan` the two routing
/// bits of each, [`PAN_LEFT`] and [`PAN_RIGHT`] of `&B4`-`&B6` (M5-11, #116).
///
/// Every channel is in, including a silent one and one routed to nothing - a
/// converter with nothing to convert still sits at its offset, which on a discrete
/// YM2612 is [`LADDER_UP`] (module docs, and the reason a silent Model 1 chip has
/// a DC level rather than silence). Panning decides where a channel's own code
/// goes, not whether its converter is offset, so a channel routed to neither side
/// contributes [`muted`] to both.
pub(crate) fn mix(values: [i16; CHANNELS], pan: [u8; CHANNELS], model: ChipModel) -> (i16, i16) {
    (
        side(values, pan, PAN_LEFT, model),
        side(values, pan, PAN_RIGHT, model),
    )
}

/// One output `side`, `enable` the routing bit of it: the six channels of
/// `values`, each through its converter if `pan` routes it here and through
/// [`muted`] if it does not, added.
fn side(values: [i16; CHANNELS], pan: [u8; CHANNELS], enable: u8, model: ChipModel) -> i16 {
    let sum: i32 = values
        .iter()
        .zip(pan)
        .map(|(value, route)| {
            if route & enable != 0 {
                dac(*value, model)
            } else {
                muted(*value, model)
            }
        })
        .fold(0, i32::wrapping_add);
    // Safe because of the assertion by [`DAC_MAX`]: six channels of at most 259 -
    // [`muted`] is inside that bound too - cannot leave the i16, so no clamping
    // and no wrapping is possible.
    sum as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two chip models, for the tests that are about the quantisation rather
    /// than about the ladder.
    const BOTH: [ChipModel; 2] = [ChipModel::Ym2612, ChipModel::Ym3438];

    /// The 32 channel sums that share one 9-bit code: `code << 5` and the 31
    /// values above it, which the DAC cannot see.
    fn code_neighbours(code: i32) -> impl Iterator<Item = i16> {
        (0..32).map(move |low| ((code << DAC_SHIFT) + low) as i16)
    }

    /// Everything a channel sums to that shares its top nine bits arrives at the
    /// pin as the same sample, for either chip model: the low five bits are not
    /// wired up. And the next code along does arrive differently, so this is a
    /// test about quantisation rather than about a stub that ignores its
    /// argument.
    #[test]
    fn dac_quantizes_to_9_bits() {
        for model in BOTH {
            for code in -256..256 {
                let first = dac((code << DAC_SHIFT) as i16, model);
                for value in code_neighbours(code).skip(1) {
                    assert_eq!(
                        dac(value, model),
                        first,
                        "{value} shares code {code} with {} on {model:?}",
                        code << DAC_SHIFT,
                    );
                }
                if code + 1 < 256 {
                    assert_ne!(
                        dac(((code + 1) << DAC_SHIFT) as i16, model),
                        first,
                        "codes {code} and {} have to differ on {model:?}",
                        code + 1
                    );
                }
            }
        }
    }

    /// The ladder itself, and the reason a silent chip is not silent on a
    /// Model 1: the crossover of the 9-bit DAC is eight codes wide instead of
    /// one.
    ///
    /// The values are the pair the module docs cite - `+4` at and above zero,
    /// `-3` below it - in 9-bit LSBs, pinned outright because they are the whole
    /// of the model.
    #[test]
    fn ladder_offset_on_ym2612() {
        // A silent channel, a sum of 0 and so a code of 0, contributes a DC
        // offset on the discrete chip and nothing at all on the ASIC.
        assert_eq!(dac(0, ChipModel::Ym2612), 4);
        assert_eq!(dac(0, ChipModel::Ym3438), 0);

        // The step across the crossover, against the single code between any
        // other pair of neighbours.
        assert_eq!(dac(-1, ChipModel::Ym2612), -4);
        assert_eq!(
            dac(0, ChipModel::Ym2612) - dac(-1, ChipModel::Ym2612),
            8,
            "the ladder: the +4 and the -3 of it, added"
        );
        assert_eq!(
            dac(2 * 32, ChipModel::Ym2612) - dac(32, ChipModel::Ym2612),
            1,
            "one code anywhere else on the ladder"
        );

        // Away from the crossover the ladder is a plain offset: every code of a
        // half carries exactly its own +4 or -3, and the 3438 carries nothing.
        for code in -256..256i32 {
            let value = (code << DAC_SHIFT) as i16;
            let offset = if code < 0 { -LADDER_DOWN } else { LADDER_UP };
            assert_eq!(dac(value, ChipModel::Ym2612), code + offset, "{code}");
            assert_eq!(dac(value, ChipModel::Ym3438), code, "{code}");
        }
    }

    /// The bound the six-channel sum trusts: no channel sum, ladder or no
    /// ladder, gets outside `+-DAC_MAX`, so six of them are a twentieth of an
    /// i16 and [`mix`] can add them up without clamping.
    ///
    /// The domain swept is the one `dac` is fed - the 14-bit range
    /// [`crate::channel`] clamps every channel sum to - and its two ends are
    /// where a shift or a clamp is most likely to be wrong. Outside it the
    /// function would have something to clamp and this bound would not hold,
    /// which is what the compile-time assertion by [`DAC_MAX`] and the pin in
    /// `crate::tests::sum_of_six_fits_i16` are together standing guard over.
    #[test]
    fn one_channel_stays_inside_its_bound() {
        for model in BOTH {
            for value in (-8192..=8191).step_by(7) {
                let sample = dac(value as i16, model);
                assert!(
                    (-DAC_MAX..=DAC_MAX).contains(&sample),
                    "{value} on {model:?} is {sample}"
                );
            }
            let ladder = if model == ChipModel::Ym2612 { 1 } else { 0 };
            assert_eq!(dac(-8192, model), -256 - LADDER_DOWN * ladder);
            assert_eq!(dac(8191, model), 255 + LADDER_UP * ladder);
        }
        assert_eq!(DAC_MAX, 259);
        assert!(DAC_MAX * CHANNELS as i32 <= i16::MAX as i32);
    }

    /// Six silent channels are six converters at their offset: a DC level of
    /// `6 * 4` on a discrete chip, and exactly nothing on a 3438 - on both sides,
    /// which is what a chip that has never been told to pan answers with.
    #[test]
    fn mix_adds_the_offset_of_every_channel() {
        assert_eq!(
            mix(
                [0; CHANNELS],
                [PAN_LEFT | PAN_RIGHT; CHANNELS],
                ChipModel::Ym2612
            ),
            (6 * 4, 6 * 4)
        );
        assert_eq!(
            mix(
                [0; CHANNELS],
                [PAN_LEFT | PAN_RIGHT; CHANNELS],
                ChipModel::Ym3438
            ),
            (0, 0)
        );
    }

    /// A channel that is not routed to a side loses its code and keeps its step
    /// of ladder: `+4` at and above zero and `-4` below it, the `-3` of a routed
    /// channel with the routing bit of Genesis Plus GX's `4 - pan_bit` at zero.
    /// A 3438 has no ladder to keep and gives nothing at all.
    #[test]
    fn a_panned_off_channel_keeps_only_its_ladder_step() {
        // 32 and -64 are the codes +1 and -2, so a routed channel is one code and
        // a step while a panned-off one is the step alone.
        assert_eq!(dac(32, ChipModel::Ym2612), 5);
        assert_eq!(muted(32, ChipModel::Ym2612), 4);
        assert_eq!(dac(-64, ChipModel::Ym2612), -5);
        assert_eq!(muted(-64, ChipModel::Ym2612), -4);

        // The step of a panned-off channel follows the sign of the channel, not
        // the code it is no longer allowed to send: every negative sum is at -4
        // and every other at +4.
        for value in (-8192..=8191).step_by(37) {
            let want = if value < 0 { -4 } else { 4 };
            assert_eq!(muted(value, ChipModel::Ym2612), want, "{value}");
            assert_eq!(muted(value, ChipModel::Ym3438), 0, "{value}");
        }
    }

    /// `mix` per side: each channel goes to the sides its two bits say, and to
    /// nowhere else - with the ladder step of the channels that do not go there
    /// still sitting on that side.
    #[test]
    fn mix_routes_each_channel_by_its_pan_bits() {
        let values = [32, -64, 0, 0, 0, 0];
        let pan = [
            PAN_LEFT,
            PAN_RIGHT,
            PAN_LEFT | PAN_RIGHT,
            PAN_LEFT | PAN_RIGHT,
            0,
            0,
        ];

        // Left: +1 and its +4, the -2's channel panned off so -4, and four more
        // silent converters at +4 each.
        assert_eq!(mix(values, pan, ChipModel::Ym2612), (17, 15));
        // Right: the +1's channel panned off, so nothing of the code, and the -2
        // routed whole. No ladder anywhere on a 3438.
        assert_eq!(mix(values, pan, ChipModel::Ym3438), (1, -2));
    }

    /// `&2A` to a channel sum: 0x80 is the centre and silence, and the code the
    /// 9-bit DAC of [`dac`] sees is `(data - 128) << 1` - every other code of the
    /// converter, since the bottom one is `&2C`'s and not `&2A`'s (M5-11, #116).
    #[test]
    fn pcm_sample_spreads_2a_across_the_dac() {
        assert_eq!(pcm_sample(0x80), 0);
        assert_eq!(pcm_sample(0xFF), 8128);
        assert_eq!(pcm_sample(0x00), -8192);

        for data in 0..=255u8 {
            assert_eq!(
                dac(pcm_sample(data), ChipModel::Ym3438),
                (i32::from(data) - 128) * 2,
                "{data:#04X}"
            );
        }
    }
}
