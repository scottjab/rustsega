//! Status register (SR) bit constants and masks.
//!
//! The SR is 16 bits. Bit layout, from the M68000UM programming-model chapter,
//! section "Status Register (SR)" and "Condition Code Register (CCR)":
//!
//! ```text
//!  15  14  13  12  11  10  9   8   7   6   5   4   3   2   1   0
//! +---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+
//! | T | 0 | S | 0 | 0 | I2| I1| I0| 0 | 0 | 0 | X | N | Z | V | C |
//! +---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+
//! ```
//!
//! Bits 14, 12, 11, 7, 6 and 5 do not exist on the 68000. Every write to SR
//! must be masked with [`SR_MASK`]; the CCR is the low byte and is masked with
//! [`CCR_MASK`].
//!
//! The module also owns the shared condition-code rules — [`M68k::set_nz`] and
//! [`M68k::set_logic_flags`] — so instruction tickets apply the same rule
//! instead of each re-deriving which of N, Z, V, C and X it may touch.

use crate::M68k;
use crate::ea::Size;

/// Trace bit (bit 15). Taking a trace exception is not implemented yet.
pub const SR_T: u16 = 0x8000;
/// Supervisor bit (bit 13). 1 = supervisor mode, 0 = user mode. Changing it
/// swaps the active stack pointer, see `M68k::set_sr`.
pub const SR_S: u16 = 0x2000;
/// Interrupt mask, bits 10..8. 0 accepts any interrupt, 7 blocks all
/// (non-NMI) interrupts.
pub const SR_INT_MASK: u16 = 0x0700;
/// Extend flag (bit 4), used by add/subtract with carry and by shifts.
pub const SR_X: u16 = 0x0010;
/// Negative flag (bit 3): result bit 15 (or 31) of the last operation.
pub const SR_N: u16 = 0x0008;
/// Zero flag (bit 2): the last result was zero.
pub const SR_Z: u16 = 0x0004;
/// Overflow flag (bit 1): signed overflow of the last operation.
pub const SR_V: u16 = 0x0002;
/// Carry flag (bit 0): carry out of the most significant bit.
pub const SR_C: u16 = 0x0001;

/// Every bit that actually exists on the 68000 SR: T, S, I2–I0, X, N, Z, V, C.
/// All SR writes go through `M68k::set_sr`, which ANDs with this.
pub const SR_MASK: u16 = 0xA71F;

/// The CCR is the low byte of the SR: X, N, Z, V, C.
pub const CCR_MASK: u16 = 0x1F;

/// SR right after a reset: S = 1 (supervisor), T = 0, interrupt mask = 7, all
/// condition codes clear. See M68000UM chapter 5 "Reset".
pub const SR_RESET: u16 = 0x2700;

impl M68k {
    /// Set N and Z from `value` at operand width `size`, leaving V, C and X
    /// untouched.
    ///
    /// N is the most significant bit *of the operand* and Z means "the operand
    /// is zero", so both are computed on `value` masked to `size`: a byte MOVE
    /// of `$00` sets Z even when the destination register's upper bits are
    /// nonzero (M68000UM, section 1 "Condition Codes": N and Z reflect the
    /// result of the operation at its own size). [`crate::ea::Size::msb`] and
    /// [`crate::ea::Size::mask`] are the width's bit and mask.
    pub(crate) fn set_nz(&mut self, value: u32, size: Size) {
        let result = value & size.mask();
        self.set_flag(SR_N, result & size.msb() != 0);
        self.set_flag(SR_Z, result == 0);
    }

    /// The flag rule of the move and logic class: N and Z from the result, V
    /// and C cleared, X unchanged.
    ///
    /// MOVE and MOVEQ use this (M68000PM section 8, "MOVE — Move" and "MOVEQ —
    /// Move Quick to Data Register": "Condition codes ... N and Z are set
    /// according to the result; V and C are cleared; X is unaffected"). `ANDI`/
    /// `ORI`/`EORI`/`NOT` and the shift group take the same rule when their
    /// tickets land, which is why it lives here rather than in one handler.
    ///
    /// MOVEA is *not* one of them: it leaves every flag alone, so it calls
    /// neither helper.
    pub(crate) fn set_logic_flags(&mut self, value: u32, size: Size) {
        self.set_nz(value, size);
        self.set_flag(SR_V, false);
        self.set_flag(SR_C, false);
    }
}

/// Tests for `test_cc`, the condition-code table this module owns (M1-16, #24).
#[cfg(test)]
mod tests {
    use super::*;

    /// N, Z, V and C of `bits`, the four low bits read one per flag, so a test
    /// can walk all sixteen flag combinations with `bits in 0..=0xF`.
    fn ccr_from(bits: u16) -> u16 {
        let n = (bits & 0b1000) >> 3;
        let z = (bits & 0b0100) >> 2;
        let v = (bits & 0b0010) >> 1;
        let c = bits & 0b0001;
        n * SR_N | z * SR_Z | v * SR_V | c * SR_C
    }

    /// All sixteen condition codes, each checked against every combination of
    /// N, V, Z and C — 256 assertions from the ticket's table (M68000PM section
    /// 8, "CONDITION CODES"). The expectations are written per mnemonic, which
    /// is what makes the GE/LT and GT/LE pairs — the ones a hand-rolled table
    /// gets backwards — visible.
    #[test]
    fn condition_table() {
        let codes: [(u8, &str, fn(bool, bool, bool, bool) -> bool); 16] = [
            (0b0000, "T", |_n, _z, _v, _c| true),
            (0b0001, "F", |_n, _z, _v, _c| false),
            (0b0010, "HI", |_n, z, _v, c| !c && !z),
            (0b0011, "LS", |_n, z, _v, c| c || z),
            (0b0100, "CC", |_n, _z, _v, c| !c),
            (0b0101, "CS", |_n, _z, _v, c| c),
            (0b0110, "NE", |_n, z, _v, _c| !z),
            (0b0111, "EQ", |_n, z, _v, _c| z),
            (0b1000, "VC", |_n, _z, v, _c| !v),
            (0b1001, "VS", |_n, _z, v, _c| v),
            (0b1010, "PL", |n, _z, _v, _c| !n),
            (0b1011, "MI", |n, _z, _v, _c| n),
            (0b1100, "GE", |n, _z, v, _c| n == v),
            (0b1101, "LT", |n, _z, v, _c| n != v),
            (0b1110, "GT", |n, z, v, _c| !z && n == v),
            (0b1111, "LE", |n, z, v, _c| z || n != v),
        ];

        for bits in 0..=0xFu16 {
            let sr = ccr_from(bits);
            let n = bits & 8 != 0;
            let z = bits & 4 != 0;
            let v = bits & 2 != 0;
            let c = bits & 1 != 0;
            for (cc, name, expect) in codes {
                assert_eq!(
                    test_cc(cc, sr),
                    expect(n, z, v, c),
                    "{name} (cc {cc:04b}) at N{n} V{v} Z{z} C{c}"
                );
                // A code is four bits, so a stray upper nibble must not reach a
                // different arm: `cc | 0xF0` is the same condition.
                assert_eq!(
                    test_cc(cc | 0xF0, sr),
                    expect(n, z, v, c),
                    "{name} ignores the bits above the condition field"
                );
            }
        }
    }

    /// A condition test reads N, Z, V and C and nothing else: setting X, T, S
    /// and the whole interrupt mask changes no answer (`SR_MASK` keeps those
    /// bits real, so this is a question about `test_cc`, not about the SR).
    #[test]
    fn condition_test_ignores_the_other_sr_bits() {
        let others = SR_X | SR_T | SR_S | SR_INT_MASK;
        for bits in 0..=0xFu16 {
            for cc in 0..=0xFu8 {
                let plain = ccr_from(bits);
                assert_eq!(
                    test_cc(cc, plain),
                    test_cc(cc, plain | others),
                    "cc {cc:04b} at SR {plain:04X}"
                );
            }
        }
    }
}
