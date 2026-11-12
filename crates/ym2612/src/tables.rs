//! The two ROMs of an operator: the quarter-wave log sine and the exponent.
//!
//! The chip never holds a sample of anything. It holds the logarithm of the sine
//! of an angle and the mantissa of a power of two, and turns their sum back into
//! a linear number at the end of every operator: the log domain is what lets one
//! operator multiply another by adding to it, and why the envelope of
//! [`crate::envelope`] is an attenuation rather than a volume (Nemesis, "YM2612
//! FM operator" research on SpritesMind; the same two tables Nuked-OPN2 holds as
//! `logsinrom` and `exprom`).
//!
//! Both arrays are hardcoded rather than computed at start-up: they are
//! constants of the silicon, the runtime is a browser, and no float is needed
//! anywhere near the sample path. `tables_match_formula` recomputes them from
//! the closed form in a test, so a paste error cannot pass.

/// The quarter-wave log sine: `-log2(sin((2i + 1) * PI / 1024)) * 256`, rounded,
/// for the 256 half-eighths of a quarter wave.
///
/// Twelve bits, and it is read through the top ten bits of a phase counter: bit
/// 9 of the angle is its sign, bit 8 which way round this quarter is read, and
/// the low eight index here (see [`crate::operator`]). The sample points are the
/// odd eighths, `2i + 1` of 512 to a half wave, so index 0 is the smallest sine
/// the table can name - the largest attenuation, `0x859` - and index 255 the
/// largest sine, which attenuates by nothing at all.
///
/// The values are the closed form above, and they are byte for byte the
/// `logsinrom` of Nuked-OPN2's core (`nukeykt/Nuked-OPN2`, `ym3438.c`, read 11
/// Nov 2026, LGPL and consulted for its values alone). Nuked indexes its copy at
/// the same ten angles and so stores this table forwards, exactly as here: from
/// `0x859` at 0 down to `0` at 255.
#[allow(dead_code)] // first caller is the operator sum of M5-07a (#111)
pub(crate) const LOGSIN: [u16; 256] = [0; 256];

/// The exponent: the 10-bit mantissa of `2^(-(i + 1) / 256)` scaled to a full
/// 2048, as an 11-bit value minus its own top bit.
///
/// An operator's total attenuation is a log-domain number in 256ths of an octave
/// - the `4.8` of a 13-bit sum of [`LOGSIN`] and the envelope times four - and
/// turning it back into a linear amplitude takes the top eight bits as a shift
/// and the low eight as an index here. The full 11-bit mantissa is
/// `EXP[i] | 0x400`, which is why the entries stop at `0x3fa` rather than
/// reaching `0x3ff`: the top bit of the mantissa is not stored, and an entry of
/// `0x3ff` - a mantissa of 2047 - would be an amplitude of 2048/2048 of full
/// scale, which the chip only ever rounds down from.
///
/// The values are that closed form, and they are Nuked-OPN2's `exprom` read the
/// other way round: it indexes its copy at `(level & 0xff) ^ 0xff` and so stores
/// index 0 of this table as its own index `0xff` (the same file, consulted for
/// its values alone). `tables_match_formula` pins this table against the closed
/// form, and `exp_table_endpoints` pins the two ends against Nuked.
#[allow(dead_code)] // first caller is the operator sum of M5-07a (#111)
pub(crate) const EXP: [u16; 256] = [0; 256];

#[cfg(test)]
mod tests {
    use super::*;

    /// The two ends of the log sine: index 0 is the smallest sine there is, so
    /// the largest attenuation in 12 bits, and index 255 is a quarter wave
    /// peak, which attenuates by nothing (`0x859` and `0` are also the ends of
    /// Nuked-OPN2's `logsinrom`, which this table matches value for value).
    #[test]
    fn logsin_endpoints() {
        assert_eq!(LOGSIN[0], 0x859);
        assert_eq!(LOGSIN[255], 0);
    }

    /// The log sine never rises: a bigger sine is a smaller attenuation, and the
    /// whole table runs from `0x859` down to `0`.
    ///
    /// Weakly decreasing rather than strictly: `-log2(sin(x)) * 256` rounds to
    /// `0` for the last eighteen entries as well as for the peak itself, so the
    /// table ends in a run of zeros and cannot be strictly ordered there.
    #[test]
    fn logsin_monotonic_decreasing() {
        for i in 1..LOGSIN.len() {
            assert!(
                LOGSIN[i] <= LOGSIN[i - 1],
                "logsin[{i}] = {:#x} above logsin[{}] = {:#x}",
                LOGSIN[i],
                i - 1,
                LOGSIN[i - 1]
            );
        }
        assert_eq!(LOGSIN[LOGSIN.len() - 1], 0);
        assert!(LOGSIN.iter().all(|v| *v <= 0x859), "the table is 12 bits");
    }

    /// The two ends of the exponent, read off Nuked-OPN2's `exprom`
    /// (`nukeykt/Nuked-OPN2`, `ym3438.c`, read 11 Nov 2026, LGPL and read only
    /// for its values): its `exprom[0xff] = 0x3fa` is this table's entry 0,
    /// because its exponent step indexes the ROM as `(level & 0xff) ^ 0xff`, and
    /// its `exprom[0x00] = 0x000` is this table's entry 255.
    ///
    /// Which is the whole shape of the table in two numbers: a whole octave of
    /// attenuation is a mantissa of `2048 * 2^(-1/256) = 2042` rather than
    /// 2048 - the table has no entry for a mantissa of exactly 1.0, and so no
    /// operator reaches the top of its range - and the last eighth-octave of the
    /// table is a mantissa of exactly 1024, half of it.
    #[test]
    fn exp_table_endpoints() {
        assert_eq!(EXP[0], 0x3fa);
        assert_eq!(EXP[255], 0x000);
    }

    /// Both tables against the closed forms their doc comments give, entry for
    /// entry: the arrays are hardcoded for the browser's sake, and this is what
    /// keeps the paste honest.
    #[test]
    fn tables_match_formula() {
        for (i, expected) in LOGSIN.iter().enumerate() {
            let sine = ((2 * i + 1) as f64 * std::f64::consts::PI / 1024.0).sin();
            let value = (-sine.log2() * 256.0).round();
            assert!((0.0 ..= f64::from(u16::MAX)).contains(&value), "logsin[{i}] is 12 bits");
            assert_eq!(value as u16, *expected, "logsin[{i}]");
        }

        for (i, expected) in EXP.iter().enumerate() {
            let mantissa = 2_f64.powf(1.0 - (i + 1) as f64 / 256.0) * 1024.0;
            let value = mantissa.round() - 1024.0;
            assert!(
                (0.0 ..= f64::from(u16::MAX)).contains(&value),
                "exp[{i}] is an 11-bit mantissa without its top bit"
            );
            assert_eq!(value as u16, *expected, "exp[{i}]");
        }
    }
}
