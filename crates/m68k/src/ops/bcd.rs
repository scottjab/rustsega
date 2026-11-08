//! Packed decimal arithmetic: `ABCD`, `SBCD` and `NBCD` (M1-23, #31).
//!
//! All three add or subtract in binary and then re-correct the answer to decimal
//! by adding or subtracting 6 in each nibble that overflowed (M68000UM section
//! 8, "ABCD"/"SBCD"/"NBCD", "Description"). They are the arithmetic of a chain
//! of multi-byte decimal numbers: `X` carries the decimal carry or borrow in and
//! back out, so `ABCD -(A1),-(A0)` repeated walks a BCD string one byte at a
//! time.
//!
//! ```text
//! ABCD   1100 xxx 100 0m yyy   m = 0: Dy + Dx + X -> Dx
//!                                m = 1: -(Ay) + -(Ax) + X -> (Ax)
//! SBCD   1000 xxx 100 0m yyy   m = 0: Dx - Dy - X -> Dx
//!                                m = 1: -(Ax) - -(Ay) - X -> (Ax)
//! NBCD   0100 1000 00 <ea>     0 - <ea> - X -> <ea>
//! ```
//!
//! # The correction, and why invalid BCD falls out of it
//!
//! Both directions are one shape, and it is the manual's:
//!
//! * add: correct the low nibble with `+6` when `lo(x) + lo(y) + X > 9`, then
//!   correct the byte with `+$60` when that sum is above `$9F`;
//! * subtract: correct the low nibble with `-6` when `lo(x) - lo(y) - X < 0`,
//!   then correct the byte with `-$60` when the **uncorrected** difference is
//!   negative.
//!
//! The last clause is the one that is easy to get wrong: a subtract whose low
//! nibble needed `-6` can still leave a byte that the *whole* did not borrow
//! from, and correcting the high nibble on the corrected value then subtracts
//! `$60` a second time. Keying it on the uncorrected difference is what
//! `SBCD.json.gz` measures, and it is why [`bcd_sub`] keeps `raw` and
//! `low_corrected` apart instead of testing one value twice.
//!
//! Nothing here asks whether the nibbles are decimal digits at all, and that is
//! what the hardware does: `$FF + $FF + X` is `$66` with `C` set, because the two
//! `+6` corrections run exactly as they would for valid digits. Both files are
//! full of invalid-nibble operands, so the correction rule is what pins them —
//! see the `bcd_*` tests.
//!
//! # The flags
//!
//! `X` and `C` are both set to the decimal carry or borrow, so `X` is never left
//! behind. `N` is the result's bit 7. `Z` is sticky exactly as it is for
//! [`super::arith`]'s `ADDX`/`SUBX`: a zero result leaves it alone and a non-zero
//! one clears it.
//!
//! `V` is the interesting one. It is not signed overflow of the binary sum: it
//! says the decimal correction pushed the answer across the sign, so it compares
//! the uncorrected value's bit 7 with the corrected byte's — set when an add's
//! raw bit 7 was clear and the corrected bit 7 is set, and when a subtract's raw
//! bit 7 was set and the corrected bit 7 is clear. Measured that way against all
//! 11 874 non-exception cases of the three suite files, and nothing disagrees.
//!
//! # Clocks
//!
//! M68000UM section 8 gives `ABCD` and `SBCD` 6 clocks register form and 18
//! memory form (plus 4 for each `An` movement), and `NBCD` 8 for `Dn` and
//! 14..22 by source mode. These handlers return `0`, like every other ALU module
//! in this core: the bus helpers and the EA layer already self-charge their own
//! clocks, and who owns which part of an instruction's total is M1-31 (#39/#40),
//! not this ticket. The three files therefore run under
//! `Check::StateSkipAddressErrors`, which does not compare `length`.

use crate::M68k;
use crate::bus::Bus;
use crate::ea::{Ea, Size};
use crate::ops::field;
use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};

/// Bit 3 of an `ABCD`/`SBCD` opcode: clear for the register pair, set for
/// `-(Ay),-(Ax)` (M68000UM section 8, "ABCD"/"SBCD", "Format").
const MEMORY_MODE_BIT: u16 = 0b1000;

/// `ABCD`'s and `SBCD`'s `xxx` field: `Dx` in the register form, `Ax` in the
/// memory form.
const DEST_REG_SHIFT: u32 = 9;

/// `-(An)`, the only mode either memory form addresses with.
const AN_PRE: u8 = 0b100;

/// A packed decimal answer: the corrected byte, and the flags that follow from
/// it directly.
#[derive(Clone, Copy, Debug)]
struct Bcd {
    /// The corrected result byte, which `N` reads.
    result: u8,
    /// The decimal carry (add) or borrow (subtract): both `C` and `X`.
    carry: bool,
    /// Whether the decimal correction crossed bit 7 — `V`, which for this family
    /// is the decimal overflow and not the binary one (M68000UM
    /// "ABCD"/"SBCD"/"NBCD", "Condition Code Flags").
    overflow: bool,
}

/// `ABCD`'s byte: `$dst + $src + X`, decimal-corrected (M68000UM section 8,
/// "ABCD").
///
/// `u16` throughout, so the carry out of the byte is a `> 0xFF` test and the
/// `$60` correction can be applied to a value that has already overflowed:
/// `$FF + $FF + 1` is `$166` before its high correction, not `$66 + $60`.
fn bcd_add(dst: u8, src: u8, extend: bool) -> Bcd {
    let e = u16::from(extend);
    let raw = u16::from(dst) + u16::from(src) + e;
    let nibbles = u16::from(dst & 0x0F) + u16::from(src & 0x0F) + e;
    let low_corrected = raw + if nibbles > 9 { 6 } else { 0 };
    let corrected = low_corrected + if low_corrected > 0x9F { 0x60 } else { 0 };

    let result = corrected as u8;
    Bcd {
        result,
        carry: corrected > 0xFF,
        // Raw bit 7 clear, corrected bit 7 set: the `+6`/`+$60` carry crossed
        // the sign.
        overflow: raw & 0x80 == 0 && result & 0x80 != 0,
    }
}

/// `SBCD`'s and `NBCD`'s byte: `$dst - $src - X`, decimal-corrected (M68000UM
/// section 8, "SBCD"; `NBCD` is the same with `dst` zero).
///
/// `i16` holds the signed difference, so its borrow out is simply `< 0`. The high
/// correction keys on `raw < 0` and not on `low_corrected < 0`; see the module's
/// section on the correction.
fn bcd_sub(dst: u8, src: u8, extend: bool) -> Bcd {
    let e = i16::from(extend);
    let raw = i16::from(dst) - i16::from(src) - e;
    let nibbles = i16::from(dst & 0x0F) - i16::from(src & 0x0F) - e;
    let low_corrected = raw - if nibbles < 0 { 6 } else { 0 };
    let corrected = low_corrected - if raw < 0 { 0x60 } else { 0 };

    let result = corrected as u8;
    Bcd {
        result,
        carry: corrected < 0,
        // Raw bit 7 set, corrected bit 7 clear: the `-$60` borrow crossed the
        // sign. `raw` is negative here in exactly the cases its low bit 7 was
        // set, since a byte difference never reaches below `-$100`.
        overflow: raw & 0x80 != 0 && result & 0x80 == 0,
    }
}

impl M68k {
    /// `ABCD`: `1100 xxx 100 0m yyy` (M68000UM section 8, "ABCD"; M1-23, #31).
    ///
    /// `Dy + Dx + X → Dx`, or the bytes at `-(Ay)` and `-(Ax)` added the same way
    /// and written back to `(Ax)`.
    pub(crate) fn op_abcd(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.op_bcd_pair(bus, opcode, false)
    }

    /// `SBCD`: `1000 xxx 100 0m yyy` (M68000UM section 8, "SBCD"; M1-23, #31).
    ///
    /// `Dx - Dy - X → Dx`, or `-(Ax) - -(Ay) - X → (Ax)`: the same two operand
    /// forms as [`M68k::op_abcd`] with the subtraction the other way round.
    pub(crate) fn op_sbcd(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.op_bcd_pair(bus, opcode, true)
    }

    /// `NBCD <ea>`: `0100 1000 00 <ea>` (M68000UM section 8, "NBCD"; M1-23, #31).
    ///
    /// `0 - <ea> - X → <ea>`, in place, one byte. `NBCD` is `NEG`'s operand set —
    /// a data-alterable EA, which [`crate::decode`] has already checked — and the
    /// zero it subtracts from is a *decimal* zero, so `NBCD` of `$00` with `X`
    /// clear is `$00` with `C` clear, and with `X` set it is `$99` with `C` set.
    pub(crate) fn op_nbcd(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, 3);
        let reg = field(opcode, 0);
        let Some(dest) = self.resolve_ea(bus, mode, reg, Size::Byte) else {
            return self.op_illegal(bus);
        };
        // Read before writing: the operand is also the destination, and a mode
        // that moves a register — `(An)+`, `-(An)` — must move it once.
        let value = self.read_ea(bus, dest, Size::Byte) as u8;
        let answer = bcd_sub(0, value, self.flag_x());
        self.set_bcd_flags(answer);
        self.write_ea(bus, dest, Size::Byte, u32::from(answer.result));
        self.advance_opcode(bus);
        0
    }

    /// `ABCD` and `SBCD` as one body: `subtract` picks `Dx - Dy - X` over
    /// `Dy + Dx + X`, and bit 3 of the opcode picks the `-(Ay),-(Ax)` pair over
    /// the register pair (M68000UM section 8, "ABCD"/"SBCD").
    ///
    /// Memory mode resolves and reads `-(Ay)` before `-(Ax)`, exactly as
    /// [`super::arith`]'s `ADDX` and `SUBX` do: `ABCD.B -(A0),-(A0)` reads
    /// `(A0 - 1)` and `(A0 - 2)`, writes `(A0 - 2)`, and moves `A0` twice. Every
    /// one of the suite's 64 `Ax == Ay` cases of each file measures that; the
    /// pointer never reads the address it has already decremented.
    fn op_bcd_pair(&mut self, bus: &mut impl Bus, opcode: u16, subtract: bool) -> u32 {
        let dest = field(opcode, DEST_REG_SHIFT);
        let src = field(opcode, 0);

        if opcode & MEMORY_MODE_BIT != 0 {
            let Some(src_ea) = self.resolve_ea(bus, AN_PRE, src, Size::Byte) else {
                return self.op_illegal(bus);
            };
            let right = self.read_ea(bus, src_ea, Size::Byte) as u8;
            let Some(dest_ea) = self.resolve_ea(bus, AN_PRE, dest, Size::Byte) else {
                return self.op_illegal(bus);
            };
            let left = self.read_ea(bus, dest_ea, Size::Byte) as u8;
            let answer = bcd_operate(left, right, self.flag_x(), subtract);
            self.set_bcd_flags(answer);
            self.write_ea(bus, dest_ea, Size::Byte, u32::from(answer.result));
        } else {
            let left = self.d[usize::from(dest)] as u8;
            let right = self.d[usize::from(src)] as u8;
            let answer = bcd_operate(left, right, self.flag_x(), subtract);
            self.set_bcd_flags(answer);
            self.write_ea(bus, Ea::DataReg(dest), Size::Byte, u32::from(answer.result));
        }

        self.advance_opcode(bus);
        0
    }

    /// The five condition codes of a packed decimal answer.
    ///
    /// `X` and `C` are the decimal carry or borrow and are written together, `N`
    /// and `V` come off [`Bcd`], and `Z` is sticky: read before it is written, so
    /// a zero result leaves it alone and a non-zero one clears it (M68000UM
    /// "ABCD"/"SBCD"/"NBCD", "Condition Code Flags", and all three suite files —
    /// the same rule [`super::arith`] uses for `ADDX`/`SUBX`).
    fn set_bcd_flags(&mut self, answer: Bcd) {
        let z_before = self.flag_z();
        self.set_flag(SR_X, answer.carry);
        self.set_flag(SR_C, answer.carry);
        self.set_flag(SR_N, answer.result & 0x80 != 0);
        self.set_flag(SR_V, answer.overflow);
        self.set_flag(SR_Z, answer.result == 0 && z_before);
    }
}

/// `ABCD`/`SBCD`'s byte: `left + right + X` or `left - right - X`, whichever the
/// opcode's mnemonic says.
fn bcd_operate(left: u8, right: u8, extend: bool, subtract: bool) -> Bcd {
    if subtract {
        bcd_sub(left, right, extend)
    } else {
        bcd_add(left, right, extend)
    }
}
