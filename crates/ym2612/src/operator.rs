//! The output of one operator: log sine, envelope, exponent, sign.
//!
//! An operator is asked a question in three parts - how far round its waveform
//! it is, how far the operator below it has pushed it, and how loud its envelope
//! says it is allowed to be - and answers with a signed 14-bit sample. The chip
//! answers by adding logarithms: the angle goes through [`crate::tables::LOGSIN`],
//! the envelope is added to it as an attenuation, and [`crate::tables::EXP`]
//! turns the sum back into a linear magnitude for the sign bit to make negative
//! or leave alone (Nemesis, "YM2612 FM operator" research on SpritesMind, and
//! the same pipeline as Nuked-OPN2's `OPN2_FMGenerate`).
//!
//! What is not here is the sum of the operators of a channel, the feedback of
//! `&2B`, and the DAC: an operator's modulation arrives as an argument, and
//! [`crate::Ym2612::clock`] still returns silence. That is M5-07a (#111), which
//! is what calls [`angle`] and [`output`] for a living.

use crate::tables::{EXP, LOGSIN};

/// The width of the [`angle`] this module works in: the top ten bits of the
/// 20-bit phase counter of [`crate::phase`], which is also the width of the
/// modulation an operator is handed (YM2608 application manual `&30`, and
/// Nuked-OPN2's `OPN2_FMGenerate`, which masks `pg_phase >> 10 + fm_mod` to
/// `0x3ff`).
const ANGLE_SHIFT: u32 = 10;

/// Ten bits of angle.
const ANGLE_MASK: u32 = 0x03FF;

/// Bit 9 of the angle: its sign. Clear is a positive output, set a negative one,
/// so the second half of the cycle is the first half upside down.
const SIGN_BIT: u16 = 0x0200;

/// Bit 8 of the angle: which way round the quarter wave is read. Clear walks
/// [`LOGSIN`] forwards from the zero crossing, set walks it backwards from the
/// peak, which is the mirror image the second quarter of a half wave needs.
const MIRROR_BIT: u16 = 0x0100;

/// The low eight angle bits: the index into the quarter wave.
const INDEX_MASK: u16 = 0x00FF;

/// The total attenuation is a log-domain number in 256ths of an octave - the
/// `4.8` of the ticket - so its low byte is the fraction the exponent reads and
/// the rest is how many octaves down the answer is.
const FRACTION_MASK: u32 = 0x00FF;

/// The octave of the total attenuation: the shift that turns 256ths into whole
/// octaves, and the width of the fraction the exponent table indexes.
const OCTAVE_BITS: u32 = 8;

/// The top of the 11-bit exponent mantissa, which [`EXP`] does not store.
const MANTISSA_TOP: u16 = 0x0400;

/// How far down the total attenuation has to be for the exponent to leave
/// nothing: the largest magnitude is `0x7ff << 2 = 8188` and that shifted down
/// by 13 octaves is 0, so at 13 and beyond the answer is silence either way.
///
/// The shift is not a saturating one, so the bound is what keeps this a shift
/// rather than a modulo-32 shift: a 13-bit total attenuation tops out at 0x1fff,
/// 31 octaves, and the check is what makes 24 of those octaves return 0 rather
/// than come back as a shift of 0.
const SILENT_OCTAVES: u32 = 13;

/// The ten-bit angle one operator reads its waveform at: the top ten bits of its
/// phase counter, plus the modulation another operator has pushed it by, in ten
/// bits.
///
/// The modulation is *added* to the phase, which is the whole of frequency
/// modulation here: an operator that is told to be a fifth of a wave further on
/// is a fifth of a wave further on whatever its own frequency is. It is added
/// modulo ten bits, so an operator driven hard enough to pass the end of the
/// cycle comes back to the start of it rather than saturating, which is what the
/// chip does and what makes a feedback level too loud for a slot sound like
/// metallic noise rather than like a volume reduction.
pub(crate) fn angle(counter: u32, modulation: i32) -> u16 {
    let counter = i32::try_from(counter >> ANGLE_SHIFT).expect("the phase counter is 20 bits");
    // The wrap is the hardware: ten bits, both ends, negatives included.
    (counter.wrapping_add(modulation) as u32 & ANGLE_MASK) as u16
}

/// What one operator outputs at angle `angle` with a total envelope attenuation
/// of `att`: the log-sin of the angle plus `att << 2`, back through the
/// exponent, and signed by bit 9 of the angle.
///
/// `att` is the 10-bit attenuation of [`crate::envelope`] - 0 at full level,
/// `0x3ff` silent - and it enters the log domain at four counts of the
/// 256th-octave per envelope step, so the whole envelope range covers the 96 dB
/// of a `0xff` octave (`0x3ff << 2 = 0xffc`). The result is 14 bits signed, and
/// the largest value it can reach is `8168` rather than the 8191 of 13 bits: the
/// exponent table tops out at a mantissa of 2042, not 2048, because a table of
/// `2^(-(i + 1) / 256)` has no entry for a whole octave of nothing removed.
/// See [`crate::tables::EXP`] and `operator_zero_att_peak_is_the_top_of_the_table`.
pub(crate) fn output(angle: u16, att: u16) -> i16 {
    // The mirror is what makes the second quarter of a half wave: an angle of
    // 0x100 + n reads the same table entry as n does, and 0x100 is the peak.
    let index = if angle & MIRROR_BIT != 0 {
        (angle ^ INDEX_MASK) & INDEX_MASK
    } else {
        angle & INDEX_MASK
    };

    let total = u32::from(LOGSIN[usize::from(index)]) + (u32::from(att) << 2);
    let octaves = total >> OCTAVE_BITS;
    if octaves >= SILENT_OCTAVES {
        return 0;
    }

    let mantissa = EXP[usize::from((total & FRACTION_MASK) as u8)] | MANTISSA_TOP;
    // 11-bit mantissa, two bits of shift up for the width of the output, and the
    // octaves back down: at most 8188, so the cast to i16 cannot change it.
    let magnitude = ((u32::from(mantissa) << 2) >> octaves) as i16;

    if angle & SIGN_BIT == 0 { magnitude } else { -magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The peak of the quarter wave at full level: an angle of `0x100` is a
    /// quarter of a cycle, which mirrors to `LOGSIN[255]` = 0, so the total
    /// attenuation is 0 and the answer is the top of the exponent table -
    /// `(0x3fa | 0x400) << 2 = 2042 << 2 = 8168`.
    ///
    /// SPEC DEVIATION from the ticket, which guessed "about 8188": 8188 is
    /// `0x7ff << 2`, the largest 13-bit magnitude, and this pipeline cannot
    /// reach it. The ticket's own formula for the exponent table -
    /// `round(2^(1 - (i + 1) / 256) * 1024) - 1024`, which is what Nuked-OPN2's
    /// `exprom` holds - puts its entry 0 at `0x3fa`, a mantissa of 2042, and
    /// 2042 * 4 is 8168. Nuked-OPN2 agrees against hardware: its
    /// `OPN2_FMGenerate` gives `((exprom[0xff] | 0x400) << 2) >> 0` at this
    /// exact angle and attenuation, which is the same 8168.
    #[test]
    fn op_output_zero_att_peak_is_the_top_of_the_table() {
        assert_eq!(output(0x100, 0), 8168);
    }

    /// Three quarters of a cycle is the same magnitude as a quarter and the
    /// other sign: bit 9 is the sign and bit 8 still mirrors, so the negation
    /// is the plain one.
    #[test]
    fn op_output_negative_half() {
        assert_eq!(output(0x300, 0), -8168);
    }

    /// The bottom of the envelope is the bottom of the chip: `att` `0x3ff` is
    /// `0xffc` of log-domain attenuation, 15 octaves, and every angle is
    /// silence whatever its log sine says.
    #[test]
    fn op_output_silent_at_max_att() {
        for angle in 0..=0x3ff {
            assert_eq!(output(angle, 0x3ff), 0, "angle {angle:#04x}");
        }
    }

    /// Half a cycle flips the sign and nothing else, for every angle: `p` and
    /// `p + 0x200` land on the same table entry - bit 8, the mirror, is not
    /// touched by a carry into bit 9 - so their magnitudes have to match
    /// exactly, at full level and at any attenuation.
    #[test]
    fn op_output_symmetry() {
        for angle in 0..=0x3ff {
            let opposite = (angle + 0x200) & 0x3ff;
            for att in [0, 0x40, 0x1ff, 0x3fe] {
                assert_eq!(
                    output(angle, att),
                    -output(opposite, att),
                    "angle {angle:#04x} against {opposite:#04x} at att {att:#x}"
                );
            }
        }
    }

    /// An angle never leaves its 14 bits: the largest magnitude is the peak of
    /// the table, and the smallest non-zero one is a single step, so nothing
    /// here can overflow the i16 it returns.
    #[test]
    fn op_output_stays_in_range() {
        for angle in 0..=0x3ff {
            for att in 0..=0x3ff {
                let out = output(angle, att);
                assert!((-8168..=8168).contains(&out), "angle {angle:#04x} att {att:#x}");
            }
        }
    }

    /// The angle is the top ten bits of the counter with the modulation added in
    /// ten bits: the low ten bits of the counter are not read at all, and a
    /// modulation that runs off either end of the cycle comes back round it.
    #[test]
    fn angle_is_the_top_ten_bits_plus_the_modulation() {
        assert_eq!(angle(0x0_0000, 0), 0);
        assert_eq!(angle(0x0_00400, 0), 1);
        assert_eq!(angle(0x0_03FF, 0), 0, "the low ten bits of the counter are dropped");
        assert_eq!(angle(0xF_FFFF, 0), 0x3FF, "the top of a 20-bit counter");
        assert_eq!(angle(0x1_2345, 0), 0x048);
        assert_eq!(angle(0x0_00400, 0xFF), 0x100, "the modulation is added, not ored");

        // Modulo ten bits, in both directions: the same wrap Nuked does with
        // `(fm_mod + (pg_phase >> 10)) & 0x3ff`.
        assert_eq!(angle(0, 0x400), 0, "a whole cycle of modulation is nothing");
        assert_eq!(angle(0, 0x401), 1);
        assert_eq!(angle(0, -1), 0x3FF, "and one below nothing is the top of the cycle");
        assert_eq!(angle(0x0_00400, -0x401), 0);
        assert_eq!(angle(0xF_FFFF, 0x200), 0x1FF, "still modulo, at the top end");
    }
}
