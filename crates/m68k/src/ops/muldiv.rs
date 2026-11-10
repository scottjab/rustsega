//! `MULU`, `MULS`, `DIVU` and `DIVS`: the 16-bit multiplies and divides
//! (M68000UM MULU, MULS, DIVU, DIVS; M1-21 #29, M1-22 #30).
//!
//! ```text
//! MULU  1100 ddd 011 mmm rrr    Dn.w × <ea>.w → Dn.l, unsigned
//! MULS  1100 ddd 111 mmm rrr    Dn.w × <ea>.w → Dn.l, signed
//! DIVU  1000 ddd 011 mmm rrr    Dn.l / <ea>.w → remainder:quotient, unsigned
//! DIVS  1000 ddd 111 mmm rrr    Dn.l / <ea>.w → remainder:quotient, signed
//! ```
//!
//! Both multiply the *low word* of the destination register — the destination is
//! read as a word and written as a long — by the word the source EA names, and
//! the source is read before the destination is written, so `MULU (A0)+,D0`
//! multiplies what was at `A0` and steps `A0` exactly once.
//!
//! Flags: `N` and `Z` from the low **32** bits of the result — a negative product
//! is one with bit 31 set, not bit 15 — `V` and `C` cleared, `X` unaffected.
//!
//! Timing is `38 + 2n` for the multiply itself plus the source's addressing
//! time, where `n` counts what the hardware's 16-iteration array has to do
//! (M68000UM section 8, "MULU — Multiply Unsigned" / "MULS — Multiply Signed"):
//!
//! * `MULU`: `n` = number of set bits in the source word;
//! * `MULS`: `n` = number of `0`↔`1` transitions in the source shifted left by
//!   one, taken as 17 bits (see [`muls_iterations`]).
//!
//! The cost is therefore a property of the operand: `MULU #0,D0` is 42 clocks and
//! `MULU #$FFFF,D0` is 74, and a program cannot make a multiply cheap.
//!
//! # The divides
//!
//! `DIVU`/`DIVS` divide the destination **long** by the source **word** and pack
//! the answer remainder-high / quotient-low. Three outcomes, in this order: a
//! zero source raises divide by zero ([`crate::EXC_DIV_ZERO`], vector 5, slot
//! `$14`, the frame holding the divide's own address because the instruction is
//! restarted); a quotient that does not fit a word sets `V`, clears `C` and
//! leaves `Dn` alone; otherwise the low word is the quotient, the high word the
//! remainder, `N`/`Z` come from the quotient and `V`/`C` are cleared. `DIVS`
//! truncates toward zero and its remainder takes the sign of the **dividend**.
//!
//! Divide timing is `4 + 2n` over the source's own addressing time, in `n` the
//! loops the divider walks for *this* pair of operands (see [`div_clocks`]): 10
//! clocks flat for a divide that overflows, and 88 to 130 for one that finishes.

use crate::bus::Bus;
use crate::cpu::{M68k, OPCODE_CLOCKS};
use crate::ea::{Ea, Size, sign_extend_word};
use crate::exceptions::{EXC_DIV_ZERO, FAULTED_FETCH_CLOCKS};
use crate::ops::field;
use crate::sr::{SR_C, SR_N, SR_V, SR_Z};

/// Clocks a multiply costs whatever its operands are: the fetch, the operand
/// read, the 16-iteration array and the long write-back (M68000UM section 8,
/// "MULU"/"MULS", "Execution Clocks [..]: 38 + 2n"). The `[..]` in the manual
/// is the execution time alone and `+ 2n` the data-dependent part. The
/// addressing time is not added here: [`M68k::read_ea`] charged it when it did
/// the access, and [`M68k::op_mul`] takes the opcode fetch out of the total for
/// the same reason (M1-31, #39).
const MUL_BASE_CLOCKS: u32 = 38;
/// Clocks each iteration of the multiply array costs: `2n` in the formula above.
const MUL_ITERATION_CLOCKS: u32 = 2;

/// Clocks each loop of the divide costs: the `2n` of `4 + 2n` (M68000UM
/// instruction-set timing, "DIVU — Divide Unsigned": `14 + n • (2) + (ea)`,
/// whose `n` this is and whose `14` the measured constant is 10 below).
const DIV_LOOP_CLOCKS: u32 = 2;
/// Loops a divide spends before its walk starts — the `4` of `n = 4 + Σ`steps`.
const DIV_BASE_LOOPS: u32 = 4;
/// Steps of the walk: the 31 dividend bits above bit 0 (see [`div_loops`]).
const DIV_WALK_STEPS: u32 = 31;
/// The loops `DIVS` spends over `DIVU` before the sign corrections of
/// [`div_loops`]: the manual puts `DIVS` 18 clocks above `DIVU` at the worst case
/// and 6 at the typical one, and the walk that finishes lands between them, at 14
/// clocks with no operand needing its sign fixed and up to 20 with both of them
/// negative and the quotient coming out negative (measured, [#238]).
const DIVS_BASE_LOOPS: u32 = 7;
/// A `DIVU` that overflows: 10 clocks over the source's own addressing time,
/// measured over all 2469 overflowing cases of `DIVU.json.gz`. See
/// [`div_overflow_clocks`].
const DIVU_OVERFLOW_CLOCKS: u32 = 10;
/// A `DIVS` that overflows with a **positive** dividend: 16 clocks, same
/// measurement. See [`div_overflow_clocks`].
const DIVS_OVERFLOW_CLOCKS: u32 = 16;
/// A `DIVS` that overflows with a **negative** dividend: 18 clocks. The sign of
/// the dividend, and not of the divisor, is what splits the suite's 3753
/// overflowing `DIVS` cases — 18 for every one of the 1850 whose dividend is
/// negative and 16 for every one of the 1903 whose dividend is positive,
/// whichever way round the divisor's sign goes (M1-31b, #40).
const DIVS_OVERFLOW_NEGATIVE_CLOCKS: u32 = 18;

/// `n` for MULU: the number of set bits in the source word (M68000UM section 8,
/// "MULU — Multiply Unsigned": `38 + 2n`, `n` counted over the source operand).
/// Which way round `n` goes — source, destination, or something else — is what
/// `mulu_timing` pins, since 38 clocks of the answer look the same either way.
const fn mulu_iterations(source: u16) -> u32 {
    source.count_ones()
}

/// `n` for MULS: the number of `0`↔`1` transitions in the source shifted left by
/// one, taken as 17 bits (M68000UM section 8, "MULS — Multiply Signed": the
/// signed multiplier's `38 + 2n` counts transitions, not set bits, because its
/// array skips runs of equal bits).
///
/// `source << 1` is the operand with a `0` appended below bit 0, and the 17 bits
/// have 16 adjacencies; counting the ones of `v ^ (v >> 1)` below bit 16 counts
/// exactly those. The appended `0` is not decoration — it is why `$FFFF` costs
/// one iteration (bit 0 differs from bit 1) where a plain "adjacencies of the
/// source" count would give zero, and so 38 clocks where the pinned suite says
/// 40. `muls_timing` and `iteration_counts` are what hold it in place.
const fn muls_iterations(source: u16) -> u32 {
    let shifted = (source as u32) << 1;
    let transitions = shifted ^ (shifted >> 1);
    (transitions & 0xFFFF).count_ones()
}

/// `32 / 16` as the two divides need it, or `None` for **division overflow**
/// (M68000UM DIVU/DIVS: "if the magnitude of the quotient is too large for a
/// word the operation is terminated, `V` is set and the destination is
/// unaltered"). The signed form truncates toward zero and leaves the remainder
/// with the sign of the dividend — "the sign of the remainder is the sign of the
/// dividend" — which is what Rust's `/` and `%` already do.
///
/// Both halves go through a wider type on purpose: `-2^31 / -1` is the one case
/// whose true quotient does not fit the operands' own width, and a 32-bit `abs()`
/// or negation wraps on it (`divs_overflow_min_int_by_minus_one`).
fn divide_32_by_16(dividend: u32, divisor: u16, signed: bool) -> Option<(u16, u16)> {
    if signed {
        let dividend = i64::from(dividend as i32);
        let divisor = i64::from(divisor as i16);
        let quotient = dividend / divisor;
        if quotient > i64::from(i16::MAX) || quotient < i64::from(i16::MIN) {
            return None;
        }
        Some((quotient as u16, (dividend % divisor) as u16))
    } else {
        let divisor = u32::from(divisor);
        // The quotient needs 17 bits exactly when the dividend's top word is
        // already at least the divisor: one compare, and the `/` below is never
        // reached with a zero divisor (the caller checks first).
        if (dividend >> 16) >= divisor {
            return None;
        }
        Some(((dividend / divisor) as u16, (dividend % divisor) as u16))
    }
}

/// A divide that finishes costs `4 + 2n` over its operands, in the `n` of
/// [`div_loops`] — **the loops this pair of operands walks**, not a count of the
/// answer the way [`mulu_iterations`] counts the multiplier's set bits.
///
/// The shape is M68000UM instruction-set timing's `14 + n • (2) + (ea)` for
/// DIVU/DIVS with two differences the pinned suite forces (M1-31b #40 measured
/// it, #238 fitted it): the fixed part is the 4-clock opcode fetch alone, ten
/// below the manual's 14, and the `n` runs to 63 for `DIVU`, which is exactly
/// the 63 the manual's 140-clock worst case divides out to.
///
/// So a `DIVU D1,D0` is 88 to 130 clocks and a `DIVS` up to 152, against the
/// 108-to-140 and 126-to-158 the approximation #30 shipped used to claim; an
/// overflowing divide is 10, 16 or 18 however the operands look
/// ([`div_overflow_clocks']).
const fn div_clocks(signed: bool, dividend: u32, divisor: u16) -> u32 {
    OPCODE_CLOCKS + DIV_LOOP_CLOCKS * div_loops(signed, dividend, divisor)
}

/// The walk behind [`div_clocks`]: `DIV_BASE_LOOPS` loops of preamble, then one
/// loop for each of the 31 dividend bits above bit 0 that the divider shifts
/// into the remainder, one more for each trial subtract that borrows and has to
/// be undone, and one **fewer** for each shift that carries out of the remainder
/// — that carry already is the quotient bit `1`, so its subtract rides along on
/// the shift rather than costing a compare of its own.
///
/// The 32nd dividend bit is not a step: the last quotient bit is the last
/// compare's own answer. And the 16 steps of the overflow test are not separate
/// from the walk — the remainder is still empty for all of them and they always
/// fail, which is what puts the floor of `n` at 35 rather than at 4 and matches
/// the 63-loop worst case of the manual's DIVU footnote.
///
/// `DIVS` walks the same path over the **absolute** values of its operands — the
/// quotient is positive when the signs agree and the remainder follows the
/// dividend (M68000UM DIVS) — and pays [`DIVS_BASE_LOOPS`] over `DIVU`, two more
/// loops for a negative dividend and one more when the signs disagree and the
/// quotient has to be put back negative.
///
/// Measured, not fitted to a guess: exact on all 613 `DIVU` and all 333 `DIVS`
/// `Dn`-source cases of the pinned files that finish, and — with the source's
/// own access taken back out — on all 4964 / 4992 runnable cases of
/// `DIVU.json.gz` and `DIVS.json.gz` over every addressing mode, which
/// `divu_file_passes` and `divs_file_passes` check (#238).
const fn div_loops(signed: bool, dividend: u32, divisor: u16) -> u32 {
    let negative_dividend = (dividend as i32) < 0;
    let negative_divisor = (divisor as i16) < 0;
    let sign_loops = if signed {
        // A negative dividend is a long to negate and costs two loops; a
        // disagreeing sign puts the quotient back negative on one loop of its
        // own, which is all a negative divisor costs — taken as an absolute
        // value alongside the dividend's, it buys nothing else. The sign restore
        // runs on the sign alone, so it is paid for even when the quotient is
        // zero; the suite has no divide that finishes that way and cannot say
        // either way (#238).
        DIVS_BASE_LOOPS
            + 2 * negative_dividend as u32
            + (negative_dividend != negative_divisor) as u32
    } else {
        0
    };
    let dividend = if signed && negative_dividend {
        dividend.wrapping_neg()
    } else {
        dividend
    };
    let divisor = if signed && negative_divisor {
        divisor.wrapping_neg()
    } else {
        divisor
    };

    let divisor = divisor as u32;
    let mut remainder = 0u32;
    let mut failed = 0;
    let mut carried = 0;
    let mut bit = DIV_WALK_STEPS;
    while bit > 0 {
        remainder = (remainder << 1) | ((dividend >> bit) & 1);
        if remainder >> 16 != 0 {
            carried += 1;
        }
        if remainder < divisor {
            failed += 1;
        } else {
            remainder -= divisor;
        }
        bit -= 1;
    }
    DIV_BASE_LOOPS + DIV_WALK_STEPS + failed - carried + sign_loops
}

/// A divide that overflows never walks the quotient at all, and the suite says
/// so in clocks: every one of its 2469 overflowing `DIVU` cases costs 10 clocks
/// on top of the source's own access, and every one of its 3753
/// overflowing `DIVS` cases 16 or 18 depending on the sign of the dividend
/// ([`DIVS_OVERFLOW_NEGATIVE_CLOCKS`]). It is the only number in this file
/// measured from the chip's timing rather than from the manual, and it says an
/// aborted divide is roughly a *tenth* of the cost of the divide it gave up on.
const fn div_overflow_clocks(signed: bool, dividend: u32) -> u32 {
    if !signed {
        DIVU_OVERFLOW_CLOCKS
    } else if (dividend as i32) < 0 {
        DIVS_OVERFLOW_NEGATIVE_CLOCKS
    } else {
        DIVS_OVERFLOW_CLOCKS
    }
}

impl M68k {
    /// `MULU <ea>,Dn` — unsigned multiply (M68000UM MULU; M1-21, #29).
    pub(crate) fn op_mulu(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.op_mul(bus, opcode, false)
    }

    /// `MULS <ea>,Dn` — signed multiply (M68000UM MULS; M1-21, #29).
    pub(crate) fn op_muls(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.op_mul(bus, opcode, true)
    }

    /// The two multiplies as one body: they differ only in how the two words are
    /// interpreted, and in what they count for `n`.
    fn op_mul(&mut self, bus: &mut impl Bus, opcode: u16, signed: bool) -> u32 {
        let reg = field(opcode, 9);
        let mode = field(opcode, 3);
        let ea_reg = field(opcode, 0);
        debug_assert_ne!(
            mode,
            crate::decode::AN_DIRECT_MODE,
            "MULU/MULS with an An source is illegal and decode rejects it (${opcode:04X})"
        );
        let source = match self.resolve_ea(bus, mode, ea_reg, Size::Word) {
            Some(ea) => ea,
            None => return self.op_illegal(bus),
        };

        // Read both operands before writing either: the destination is also an
        // operand (`MULU D0,D1`), and a mode that moves a register — `(A0)+`,
        // `-(A0)` — must move it once, when the source is read.
        let multiplicand = self.read_ea(bus, Ea::DataReg(reg), Size::Word) as u16;
        let multiplier = self.read_ea(bus, source, Size::Word) as u16;
        let iterations = if signed {
            muls_iterations(multiplier)
        } else {
            mulu_iterations(multiplier)
        };
        let result = if signed {
            let product = sign_extend_word(multiplicand) as i32;
            product.wrapping_mul(sign_extend_word(multiplier) as i32) as u32
        } else {
            u32::from(multiplicand) * u32::from(multiplier)
        };

        self.write_ea(bus, Ea::DataReg(reg), Size::Long, result);
        self.set_logic_flags(result, Size::Long);
        self.advance_opcode(bus);
        // The manual's `38 + 2n` counts the opcode fetch, which `advance_opcode`
        // has just charged and which no EA of this instruction paid for, and the
        // source EA charged itself in `read_ea` — so the handler hands back the
        // documented total less the fetch (M1-31, #39).
        MUL_BASE_CLOCKS - OPCODE_CLOCKS + MUL_ITERATION_CLOCKS * iterations
    }

    /// `DIVU <ea>,Dn` — unsigned 32/16 divide (M68000UM DIVU; M1-22, #30).
    pub(crate) fn op_divu(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.op_div(bus, opcode, false)
    }

    /// `DIVS <ea>,Dn` — signed 32/16 divide (M68000UM DIVS; M1-22, #30).
    pub(crate) fn op_divs(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.op_div(bus, opcode, true)
    }

    /// The two divides as one body, in the shape M68000UM DIVU/DIVS give them:
    /// `Dn.l / <ea>.w`, quotient to the **low** word of `Dn` and remainder to its
    /// **high** word, `N`/`Z` taken from the quotient and `V` from whether it
    /// fitted.
    ///
    /// Three outcomes, and their order is what matters:
    ///
    /// * **divide by zero** — group 1 exception, vector [`crate::EXC_DIV_ZERO`]
    ///   (slot `$14`), raised before the destination is touched and before the
    ///   instruction advances, so the frame carries the divide's own address.
    /// * **division overflow** — `V = 1`, `C = 0`, `Dn` untouched.
    /// * **a normal divide** — the packed remainder-high / quotient-low result.
    ///
    /// The source is a word read, so `resolve_ea` is called with `Size::Word`
    /// exactly as [`Self::op_mul`] does; a source that resolves to an odd access
    /// raises address error from inside `resolve_ea` and returns `None` here
    /// (M1-29 #37 is why 3101 of the 8065 cases of `DIVU.json.gz` end in the
    /// suite's exception handler instead of on a result).
    fn op_div(&mut self, bus: &mut impl Bus, opcode: u16, signed: bool) -> u32 {
        let reg = field(opcode, 9);
        let mode = field(opcode, 3);
        let ea_reg = field(opcode, 0);
        debug_assert_ne!(
            mode,
            crate::decode::AN_DIRECT_MODE,
            "DIVU/DIVS with an An source is illegal and decode rejects it (${opcode:04X})"
        );
        // The divide's own address, captured *before* the EA is resolved: an
        // extension-word mode advances `pc` while it is fetched (see
        // [`Self::ext_addr`]), and a faulted divide is restarted, so the frame
        // has to carry the address of the instruction rather than of whatever
        // `pc` ended up pointing at. `div_by_zero_traps_vector_5` and the
        // suite's one divide-by-zero case, `80ef [DIVU (d16, A7), D0] 5745`,
        // hold it: the push is `$00000C00`, the divide itself.
        let divide_pc = self.pc;
        let source = match self.resolve_ea(bus, mode, ea_reg, Size::Word) {
            Some(ea) => ea,
            None => return self.op_illegal(bus),
        };
        // Both operands are read before `Dn` is written, and the source is read
        // on every path: an overflowing or trapping divide still consumes it, so
        // `DIVU (A0)+,D0` moves A0 exactly once however the divide ends.
        let dividend = self.read_ea(bus, Ea::DataReg(reg), Size::Long);
        let divisor = self.read_ea(bus, source, Size::Word) as u16;

        if divisor == 0 {
            // Divide by zero (M68000UM "Division by Zero"). The instruction is
            // *restarted*, so the frame carries the address of the divide
            // itself, not of the instruction after it. The ticket guessed "the PC
            // of the instruction following the divide"; the suite is explicit:
            // case `80ef [DIVU (d16, A7), D0] 5745` starts at `$000C00` and
            // pushes the frame `$000C00 / $2710`, i.e. its own address.
            //
            // The condition codes are cleared except `X`, and `Dn` is left
            // alone — same case: SR `$271D` (X,N,Z,C) in, `$2710` (X) out.
            self.set_flag(SR_N, false);
            self.set_flag(SR_Z, false);
            self.set_flag(SR_V, false);
            self.set_flag(SR_C, false);
            // The exception's 34 clocks are charged by [`M68k::exception`];
            // what it does not cover is the opcode fetch this divide had already
            // taken before it found the zero, and that is what
            // [`FAULTED_FETCH_CLOCKS`] hands back — see it for the arithmetic
            // this case pins. Unlike [`Self::op_illegal`], which faults with
            // nothing of its own spent and so returns 0.
            self.exception(bus, EXC_DIV_ZERO, divide_pc);
            return FAULTED_FETCH_CLOCKS;
        }

        // The two outcomes cost different amounts, so which one this is decides
        // the clock count as well as the destination: see [`div_clocks`] and
        // [`div_overflow_clocks`].
        match divide_32_by_16(dividend, divisor, signed) {
            Some((quotient, remainder)) => {
                self.write_ea(
                    bus,
                    Ea::DataReg(reg),
                    Size::Long,
                    u32::from(quotient) | (u32::from(remainder) << 16),
                );
                // `N` is bit 15 of the quotient and `Z` its zero test — of the
                // quotient alone, never of the packed long, whose top bit is the
                // remainder's sign (M68000UM DIVU: "N,Z set from the result",
                // the result being the quotient).
                self.set_logic_flags(u32::from(quotient), Size::Word);
                self.advance_opcode(bus);
                // Less the opcode fetch, as in [`M68k::op_mul`]; the source EA
                // charged itself.
                div_clocks(signed, dividend, divisor) - OPCODE_CLOCKS
            }
            None => {
                // Overflow. `V` set and `C` cleared; `N`, `Z` and `X` are left
                // exactly as they were. The manual calls `N` and `Z` undefined
                // here, but the chip is specific, and so is every overflowing
                // case of the pinned files (2469 in `DIVU`, 3753 in `DIVS`).
                self.set_flag(SR_V, true);
                self.set_flag(SR_C, false);
                self.advance_opcode(bus);
                div_overflow_clocks(signed, dividend) - OPCODE_CLOCKS
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::cpu_with_program;

    /// Data for the EA tests: well clear of the program at `$1000`.
    const DATA: u32 = 0x0000_4000;
    /// The five flags a multiply is specified in terms of.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    const MULU: u16 = 0b011;
    const MULS: u16 = 0b111;

    /// `1100 ddd ooo mmm rrr` (M68000UM MULU/MULS).
    const fn mul(op_mode: u16, dst: u16, mode: u16, reg: u16) -> u16 {
        0xC000 | (dst << 9) | (op_mode << 6) | (mode << 3) | reg
    }

    /// `MULx D1,D0` — the instruction with no EA at all — with `a` in D0 and `b`
    /// in D1, stepped once. Returns the CPU, so the product, the flags and the
    /// clock count can all be checked.
    fn multiply(op_mode: u16, a: u32, b: u32) -> (M68k, u32) {
        let (mut cpu, mut bus) = cpu_with_program(&[mul(op_mode, 0, 0b000, 1)]);
        cpu.d[0] = a;
        cpu.d[1] = b;
        let cycles = cpu.step(&mut bus);
        (cpu, cycles)
    }

    /// 0xFFFF × 0xFFFF unsigned is `$FFFE0001`: the maximum product, which only
    /// fits because the result is a long, and whose bit 31 sets `N`.
    #[test]
    fn mulu_max() {
        let (cpu, cycles) = multiply(MULU, 0xFFFF_FFFF, 0xFFFF_FFFF);
        assert_eq!(cpu.d[0], 0xFFFE_0001);
        assert!(cpu.flag_n(), "bit 31 of the result is set");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v(), "MULU clears V");
        assert!(
            !cpu.flag_c(),
            "MULU clears C rather than carrying out of it"
        );
        // 16 set bits in the source: 38 + 2*16, with no addressing time at all.
        assert_eq!(cycles, 70);
    }

    /// MULS is signed both ways. `-1 × -1 = 1` is also MULU's answer, so the
    /// three pairs here are the ones where the two differ or must not.
    #[test]
    fn muls_is_signed() {
        let (cpu, _) = multiply(MULS, 0xFFFF_FFFE, 0x0000_0003);
        assert_eq!(cpu.d[0], 0xFFFF_FFFA, "-2 × 3");
        assert!(cpu.flag_n());

        let (cpu, _) = multiply(MULS, 0xFFFF_FFFF, 0xFFFF_FFFF);
        assert_eq!(cpu.d[0], 0x0000_0001, "-1 × -1");
        assert!(!cpu.flag_n());

        let (cpu, _) = multiply(MULS, 0xFFFF_8000, 0xFFFF_8000);
        assert_eq!(cpu.d[0], 0x4000_0000, "-32768 × -32768");
        assert!(!cpu.flag_n());

        // The unsigned reading of the first pair, for contrast.
        let (cpu, _) = multiply(MULU, 0xFFFF_FFFE, 0x0000_0003);
        assert_eq!(cpu.d[0], 0x0002_FFFA, "65534 × 3 unsigned");
        assert!(!cpu.flag_n());
    }

    /// `0` sets `Z`, and the destination long is overwritten even when the
    /// product is zero — the `0xDEAD` half has to go.
    #[test]
    fn zero_sets_z() {
        for op_mode in [MULU, MULS] {
            let (cpu, cycles) = multiply(op_mode, 0xDEAD_0000, 0x0000_0000);
            assert_eq!(cpu.d[0], 0, "anything × 0 is 0 (op {op_mode:03b})");
            assert!(cpu.flag_z());
            assert!(!cpu.flag_n());
            assert_eq!(cycles, MUL_BASE_CLOCKS, "a zero source costs n = 0");
        }
    }

    /// `N` and `Z` come from the 32-bit product, not from its low word: a
    /// product of `$00010000` has a zero low word and must not set `Z`.
    #[test]
    fn flags_come_from_the_long_result() {
        let (cpu, _) = multiply(MULU, 0x0000_0100, 0x0000_0100);
        assert_eq!(cpu.d[0], 0x0001_0000);
        assert!(!cpu.flag_z(), "the low word is zero but the result is not");
        assert!(!cpu.flag_n());

        let (cpu, _) = multiply(MULU, 0x0000_8000, 0x0000_8000);
        assert_eq!(cpu.d[0], 0x4000_0000, "unsigned, so nowhere near negative");
        assert!(!cpu.flag_n());
    }

    /// `X` is unaffected by a multiply, in both directions, whatever `N`/`Z`/`V`/`C`
    /// end up as (M68000UM MULU/MULS: "X — unaffected").
    #[test]
    fn extend_flag_is_untouched() {
        for (op_mode, a, b) in [
            (MULU, 0xFFFF_FFFF, 0xFFFF_FFFF),
            (MULS, 0x0000_0000, 0x1234_5678),
        ] {
            let (mut cpu, mut bus) = cpu_with_program(&[mul(op_mode, 0, 0b000, 1)]);
            cpu.sr |= ALL_FLAGS;
            cpu.d[0] = a;
            cpu.d[1] = b;
            cpu.step(&mut bus);
            assert!(cpu.flag_x(), "{op_mode:03b}: X must survive");
        }
    }

    /// Only the low 16 bits of `Dn` take part in either multiply: MULS with a
    /// destination whose low word is `$FFFE` is `-2` no matter what the top half
    /// holds, and MULU's `n` likewise comes from the source word alone.
    #[test]
    fn operands_are_words() {
        let (cpu, _) = multiply(MULS, 0x1234_FFFE, 0x0000_0003);
        assert_eq!(
            cpu.d[0], 0xFFFF_FFFA,
            "the top half of D0 is not an operand"
        );

        let (cpu, cycles) = multiply(MULU, 0x0000_0001, 0xFF00_0000);
        assert_eq!(cpu.d[0], 0, "only D1's low word multiplies");
        assert_eq!(cycles, MUL_BASE_CLOCKS, "and only it counts for n");
    }

    /// The source EA is resolved and read exactly once, before the destination
    /// long is written: `MULU (A0)+,D0` steps A0 by one word, and `MULU
    /// -(A0),D0` by minus one.
    #[test]
    fn side_effecting_sources_step_once() {
        let (mut cpu, mut bus) = cpu_with_program(&[mul(MULU, 0, 0b011, 0)]);
        bus.load_words(DATA, &[0x0003, 0x0007]);
        cpu.a[0] = DATA;
        cpu.d[0] = 5;
        let cycles = cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 15, "5 × 3, the word at A0 before it moved");
        assert_eq!(cpu.a[0], DATA + 2, "A0 moved once");
        // 3 has two set bits: 38 + 4, plus the 4 clocks of the (An)+ read.
        assert_eq!(cycles, 46);

        let (mut cpu, mut bus) = cpu_with_program(&[mul(MULU, 0, 0b100, 0)]);
        bus.load_words(DATA, &[0x0006, 0x0000]);
        cpu.a[0] = DATA + 2;
        cpu.d[0] = 5;
        let cycles = cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 30, "5 × 6");
        assert_eq!(cpu.a[0], DATA, "A0 moved once, down");
        // 6 has two set bits: 38 + 4, plus 4 for the read and 2 for the decrement.
        assert_eq!(cycles, 48);
    }

    /// `n` in `38 + 2n`, in isolation from the bus: the corners of both formulas.
    #[test]
    fn iteration_counts() {
        assert_eq!(mulu_iterations(0x0000), 0);
        assert_eq!(mulu_iterations(0x0001), 1);
        assert_eq!(mulu_iterations(0x8000), 1);
        assert_eq!(mulu_iterations(0xAAAA), 8);
        assert_eq!(mulu_iterations(0xFFFF), 16);

        assert_eq!(muls_iterations(0x0000), 0);
        assert_eq!(muls_iterations(0x0001), 2);
        assert_eq!(muls_iterations(0xAAAA), 15);
        assert_eq!(muls_iterations(0x5555), 16);
        assert_eq!(muls_iterations(0xFFFF), 1);
    }

    /// MULU's clock count: `38 + 2n` with `n` the set bits of the source.
    #[test]
    fn mulu_timing() {
        for (source, want) in [(0x0000u16, 38), (0x0001, 40), (0xAAAA, 54), (0xFFFF, 70)] {
            let (_, cycles) = multiply(MULU, 1, u32::from(source));
            assert_eq!(cycles, want, "MULU ${source:04X},D0");
        }
    }

    /// MULS's clock count: `38 + 2n` with `n` the transition count of the source
    /// shifted left by one — `38`, `40`, `68`, `70` for the ticket's four cases.
    #[test]
    fn muls_timing() {
        for (source, want) in [(0x0000u16, 38), (0xFFFF, 40), (0xAAAA, 68), (0x5555, 70)] {
            let (_, cycles) = multiply(MULS, 1, u32::from(source));
            assert_eq!(cycles, want, "MULS ${source:04X},D0");
        }
    }

    /// What each source EA costs on top of `38 + 2n`, measured with a source
    /// value of `$0000` so that `n = 0` and the addressing is all that is left.
    /// The pinned suite checks the same table against every case of
    /// `MULU.json.gz` and `MULS.json.gz`; this is the readable version of it
    /// (M68000UM MULU/MULS "plus effective address calculation time", and
    /// Appendix C/D for what the modes cost).
    #[test]
    fn effective_address_clocks() {
        // (mode, reg, extension words, total clocks for a zero source word).
        // `TestBus` is zeroed, so every source here reads $0000: index reg 0 is
        // D0 = 0 and the PC-relative displacements point at zero words too.
        let cases: &[(u16, u16, &[u16], u32)] = &[
            (0b000, 1, &[], 38),                   // D1
            (0b010, 0, &[], 42),                   // (A0)
            (0b011, 0, &[], 42),                   // (A0)+
            (0b100, 0, &[], 44),                   // -(A0)
            (0b101, 0, &[0x0000], 46),             // (d16,A0)
            (0b110, 0, &[0x0000], 48),             // (d8,A0,Xn) with a zero D0 index
            (0b111, 0b000, &[0x4000], 46),         // ($4000).W
            (0b111, 0b001, &[0x0000, 0x4000], 50), // ($00004000).L
            (0b111, 0b010, &[0x0000], 46),         // (d16,PC)
            (0b111, 0b011, &[0x0000], 48),         // (d8,PC,Xn) with a zero D0 index
            (0b111, 0b100, &[0x0000], 42),         // #$0000
        ];
        for &(mode, reg, ext, want) in cases {
            let mut program = vec![mul(MULU, 0, mode, reg)];
            program.extend_from_slice(ext);
            let (mut cpu, mut bus) = cpu_with_program(&program);
            cpu.a[0] = DATA;
            cpu.d[0] = 0;
            cpu.d[1] = 0;
            let cycles = cpu.step(&mut bus);
            assert_eq!(cycles, want, "MULU <mode {mode:b} reg {reg:b}>,D0");
        }
    }

    // ---------------------------------------------------------------- DIV
    //
    // `DIVU`/`DIVS` share this file with the multiplies (they share its opcode
    // group's neighbour, its word-read source and its abort semantics).
    // Semantics measured against `DIVU.json.gz` / `DIVS.json.gz`; the cycle
    // model at the end is an approximation and says so (M1-22, #30).

    const DIVU: u16 = 0b011;
    const DIVS: u16 = 0b111;

    /// `1000 ddd ooo mmm rrr` (M68000UM DIVU / DIVS).
    const fn div(op_mode: u16, dst: u16, mode: u16, reg: u16) -> u16 {
        0x8000 | (dst << 9) | (op_mode << 6) | (mode << 3) | reg
    }

    /// `DIVx D1,D0` with `dividend` in D0 and `divisor` in D1, stepped once.
    /// Returns the CPU and `step`'s clock count, like [`multiply`]. Both are
    /// `i32` so the signed cases read as signed: an operand of `$FFFF` as a
    /// divisor is `-1`, which is the interesting one.
    fn divide(op_mode: u16, dividend: i32, divisor: i32) -> (M68k, u32) {
        let (mut cpu, mut bus) = cpu_with_program(&[div(op_mode, 0, 0b000, 1)]);
        cpu.d[0] = dividend as u32;
        cpu.d[1] = divisor as u32;
        let cycles = cpu.step(&mut bus);
        (cpu, cycles)
    }

    /// `DIVU` of the ticket's example: 100 / 7 is quotient 14 and remainder 2,
    /// and they are packed quotient-low, remainder-high (M68000UM DIVU:
    /// "the least significant word contains the quotient, the most significant
    /// word contains the remainder").
    #[test]
    fn divu_basic() {
        let (cpu, _) = divide(DIVU, 100, 7);
        assert_eq!(cpu.d[0], 0x0002_000E, "quotient 14 low, remainder 2 high");
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v(), "a divide that fits clears V");
        assert!(!cpu.flag_c());
    }

    /// A quotient of zero sets `Z`, and the remainder still occupies the top of
    /// the destination: the flags come from the quotient alone, never from the
    /// packed long.
    #[test]
    fn divu_zero_quotient_sets_z() {
        let (cpu, _) = divide(DIVU, 3, 7);
        assert_eq!(cpu.d[0], 0x0003_0000, "3 / 7 = 0 remainder 3");
        assert!(cpu.flag_z());
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_v());
    }

    /// `N` is bit 15 of the quotient — of the quotient, not of the packed long,
    /// whose bit 15 and bit 31 are something else entirely.
    #[test]
    fn div_flags_come_from_the_quotient() {
        // Remainder negative, quotient positive: bit 31 of the result is set.
        let (cpu, _) = divide(DIVS, -7, 2);
        assert!(cpu.flag_n(), "bit 15 of the quotient (-3) is set");
        // Quotient 1 with a negative remainder: the packed long is negative and
        // `N` is still clear.
        let (cpu, _) = divide(DIVS, -7, -4);
        assert_eq!(cpu.d[0], 0xFFFD_0001, "-7 / -4 = 1 remainder -3");
        assert!(!cpu.flag_n());
    }

    /// `DIVS` truncates toward zero and the **remainder takes the sign of the
    /// dividend** (M68000UM DIVS: "the sign of the remainder is the sign of the
    /// dividend"). The ticket's example is -7 / 2 = -3 remainder -1.
    #[test]
    fn divs_negative_remainder_sign() {
        let (cpu, _) = divide(DIVS, -7, 2);
        assert_eq!(cpu.d[0], 0xFFFF_FFFD, "-7 / 2 = -3 remainder -1");
        assert!(cpu.flag_n());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v());
        assert!(!cpu.flag_c());

        // The other three sign combinations: the quotient rounds toward zero
        // and the remainder follows the dividend, never the divisor.
        let (cpu, _) = divide(DIVS, 7, 2);
        assert_eq!(cpu.d[0], 0x0001_0003, "7 / 2 = 3 remainder 1");
        let (cpu, _) = divide(DIVS, 7, -2);
        assert_eq!(cpu.d[0], 0x0001_FFFD, "7 / -2 = -3 remainder 1");
        let (cpu, _) = divide(DIVS, -7, -2);
        assert_eq!(cpu.d[0], 0xFFFF_0003, "-7 / -2 = 3 remainder -1");
    }

    /// Overflow — the quotient does not fit a word — leaves `Dn` **untouched**,
    /// sets `V`, clears `C`, and leaves `N`, `Z` and `X` exactly as they were.
    /// The manual calls `N` and `Z` undefined here; the chip is specific, and
    /// this is what it does: over every `DIVU`/`DIVS` case of the pinned suite
    /// whose operands can be evaluated (1567 + 2479), a divide whose true
    /// quotient exceeds a word leaves all three unchanged.
    #[test]
    fn divu_overflow_sets_v_keeps_dn() {
        let (mut cpu, mut bus) = cpu_with_program(&[div(DIVU, 0, 0b000, 1)]);
        cpu.sr |= ALL_FLAGS;
        cpu.d[0] = 0x0001_0001;
        cpu.d[1] = 1;
        let clocks = cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0x0001_0001, "D0 is not written on overflow");
        assert!(cpu.flag_v());
        assert!(!cpu.flag_c(), "overflow sets V and clears C");
        assert_eq!(
            clocks, 10,
            "and it is cheap: the walk never ran, so 10 clocks, not the 88 that the cheapest divide which did run one costs"
        );
        assert!(cpu.flag_n(), "N is left alone");
        assert!(cpu.flag_z(), "Z is left alone");
        assert!(cpu.flag_x(), "X is unaffected");

        // The same divide one below the overflow point writes normally and
        // clears V: `N`/`Z` then come from the quotient.
        let (cpu, _) = divide(DIVU, 0xFFFF, 1);
        assert_eq!(cpu.d[0], 0x0000_FFFF, "65535 / 1 fits");
        assert!(!cpu.flag_v());
        assert!(cpu.flag_n(), "bit 15 of the quotient 0xFFFF");
    }

    /// `-2^31 / -1` is overflow (the true quotient is 2^31, which needs 18 bits
    /// for a signed word) and must not panic: no `abs()` and no negation of a
    /// 32-bit `MIN` anywhere in the divide (Rust would wrap or panic on it).
    #[test]
    fn divs_overflow_min_int_by_minus_one() {
        let (mut cpu, mut bus) = cpu_with_program(&[div(DIVS, 0, 0b000, 1)]);
        cpu.sr |= ALL_FLAGS;
        cpu.d[0] = 0x8000_0000;
        cpu.d[1] = 0xFFFF;
        let clocks = cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0x8000_0000, "D0 untouched");
        assert!(cpu.flag_v());
        assert!(!cpu.flag_c());
        assert_eq!(
            clocks, 18,
            "18 clocks for the aborted `DIVS`: the dividend is negative, and the
             sign of the dividend is what splits the suite's 3753 overflowing
             `DIVS` cases between 16 and 18 (`DIVS_OVERFLOW_NEGATIVE_CLOCKS`)"
        );
        assert!(cpu.flag_n() && cpu.flag_z(), "N and Z left alone");

        // `0x7FFF / -1` is the last divide that still fits: quotient -32767,
        // exactly `i16::MIN`, written as a normal result with `V` clear.
        let (cpu, _) = divide(DIVS, 0x7FFF, -1);
        assert_eq!(cpu.d[0], 0x0000_8001, "quotient 0x8001, remainder 0");
        assert!(!cpu.flag_v());
        assert!(cpu.flag_n());
    }

    /// Divide by zero is exception vector 5 — the long at `$14` — and the frame
    /// carries **the address of the divide itself**, not of the next
    /// instruction, because the 68000 restarts a faulted divide (M68000UM
    /// "Division by Zero"). The ticket guessed "next instruction"; the suite
    /// says otherwise:
    ///
    /// `80ef [DIVU (d16, A7), D0] 5745` starts at `$000C00`, its divisor word at
    /// `$0000F0` is zero, and the frame it pushes holds `$000C00`.
    ///
    /// The condition codes end up cleared except `X`: that case goes from SR
    /// `$271D` (X,N,Z,C set) to `$2710` (X only).
    #[test]
    fn div_by_zero_traps_vector_5() {
        const HANDLER: u32 = 0x0000_2000;
        for op_mode in [DIVU, DIVS] {
            let (mut cpu, mut bus) = cpu_with_program(&[div(op_mode, 0, 0b000, 1)]);
            bus.write_long(0x14, HANDLER);
            cpu.sr |= ALL_FLAGS;
            cpu.d[0] = 0xDEAD_BEEF;
            cpu.d[1] = 0;
            let sp = cpu.a[7];
            let cycles = cpu.step(&mut bus);

            // 4 clocks of opcode fetch, which the fault had already spent, and
            // the entry's 34: see [`FAULTED_FETCH_CLOCKS`] and the suite's one
            // cycle-checked case of it, `DIVU (d16, A7), D0` at 46 clocks with
            // 8 of those 46 in the `(d16,An)` read.
            assert_eq!(cycles, 38, "a faulted divide still paid for its fetch");
            assert_eq!(cpu.pending_exception, Some(crate::EXC_DIV_ZERO));
            assert_eq!(cpu.pc, HANDLER, "the vector-5 handler ran");
            assert_eq!(cpu.a[7], sp - 6, "a group 1 frame was pushed");
            assert_eq!(
                bus.read_long(sp - 4),
                0x0000_1000,
                "the frame holds the divide's own address, not pc + 2 (op {op_mode:03b})"
            );
            assert_eq!(
                Bus::read_word(&mut bus, sp - 6) & ALL_FLAGS,
                SR_X,
                "the frame's SR has N, Z, V and C already cleared: the suite's case goes from SR $271D to a pushed $2710"
            );
            assert_eq!(cpu.d[0], 0xDEAD_BEEF, "the divide wrote nothing");
            assert!(!cpu.flag_n() && !cpu.flag_z() && !cpu.flag_v() && !cpu.flag_c());
            assert!(cpu.flag_x(), "X survives the trap");
        }
    }

    /// The source EA is resolved and read before the destination is written, on
    /// every path: a side-effecting source moves once even when the divide then
    /// overflows or traps (the same rule [`super::M68k::op_mul`] follows, and
    /// the suite's divide-by-zero case reads `$0000F0` before pushing its frame).
    #[test]
    fn div_source_steps_once_on_every_path() {
        for (dividend, divisor, want_d0) in [
            (100u32, 7u32, 0x0002_000E),   // normal
            (0x0001_0001, 1, 0x0001_0001), // overflow: Dn untouched
        ] {
            let (mut cpu, mut bus) = cpu_with_program(&[div(DIVU, 0, 0b011, 0)]);
            bus.load_words(DATA, &[u16::try_from(divisor).unwrap(), 0]);
            cpu.a[0] = DATA;
            cpu.d[0] = dividend;
            cpu.step(&mut bus);
            assert_eq!(cpu.d[0], want_d0, "{dividend:#X} / {divisor:#X}");
            assert_eq!(cpu.a[0], DATA + 2, "A0 moved once (op {divisor:#X})");
        }

        // And on the trap path.
        let (mut cpu, mut bus) = cpu_with_program(&[div(DIVS, 0, 0b011, 0)]);
        bus.write_long(0x14, 0x0000_2000);
        bus.load_words(DATA, &[0, 0]);
        cpu.a[0] = DATA;
        cpu.d[0] = 1;
        cpu.step(&mut bus);
        assert_eq!(
            cpu.a[0],
            DATA + 2,
            "a trapped divide still consumed its operand"
        );
    }

    /// The `DIVU` clock count at three points taken straight out of
    /// `DIVU.json.gz` — the cheapest divide its 613 completed `Dn` cases
    /// contain, its worst case, and two between them — so the walk of
    /// [`div_loops`] is pinned against the chip rather than against itself. The
    /// worst case is 130 clocks for a `Dn` source, and the 63 loops behind it
    /// are the 63 the manual's 140-clock worst case divides out to
    /// (`14 + 2 • 63`): the two agree on `n` and differ by the ten clocks of
    /// fixed time this core does not charge ([#238], [`div_clocks`]).
    #[test]
    fn divu_timing_examples() {
        for (dividend, divisor, want) in [
            (0xAFCC_BF14i32, 0xEB2Bi32, 88), //  n = 42, the cheapest in the file
            (0x4B85_A7DA, 0xE436, 104),      //  n = 50
            (0x6679_284B, 0xC0CD, 130),      //  n = 63, the worst case
        ] {
            let (cpu, cycles) = divide(DIVU, dividend, divisor);
            assert_eq!(
                cycles, want,
                "DIVU ${dividend:08X}/${divisor:04X}, quotient ${:08X}",
                cpu.d[0] & 0xFFFF
            );
        }
    }

    /// The clock count is the walk's, not the answer's: `DIVU.json.gz` contains
    /// two divides with the same quotient `$9F67` that cost 106 and 116 clocks,
    /// and the whole reason #238 exists is that no count of the quotient — set
    /// bits, transitions, bit length — separates them.
    #[test]
    fn divu_clocks_are_not_a_function_of_the_quotient() {
        let (cpu, a) = divide(DIVU, 0x5AFA_921Cu32 as i32, 0x921C);
        let (cpu2, b) = divide(DIVU, 0x36C8_9B4Eu32 as i32, 0x57FB);
        assert_eq!(cpu.d[0] & 0xFFFF, 0x9F67);
        assert_eq!(cpu2.d[0] & 0xFFFF, 0x9F67, "the same quotient");
        assert_eq!((a, b), (106, 116), "two different costs");

        // And the last dividend bit is not a step of the walk at all: flipping
        // it changes the quotient and not the clocks.
        let (cpu, same) = divide(DIVU, 0x5AFA_921Cu32 as i32, 0x921C);
        let (cpu2, also) = divide(DIVU, 0x5AFA_921Du32 as i32, 0x921C);
        assert_ne!(cpu.d[0], cpu2.d[0], "bit 0 of the dividend is in the quotient");
        assert_eq!(same, also, "but not in the walk");
    }

    /// The `DIVS` clock count: the same walk over the absolute values, at the
    /// three measured points of `DIVS.json.gz` — 126, 136 and its worst case
    /// 152 — plus the two sign terms of [`div_loops`] in isolation, on four
    /// divides whose walk is identical because only their signs differ.
    #[test]
    fn divs_timing_examples() {
        for (dividend, divisor, want) in [
            (0x23CD_47A1i32, 0x47A1i32, 126), //  n = 61, the cheapest in the file
            (0x191D_87A8, 0xB4A8, 136),       //  n = 66
            (0xFF92_A491u32 as i32, 0x6B10, 152), //  n = 74: negative dividend, negative quotient
        ] {
            let (cpu, cycles) = divide(DIVS, dividend, divisor);
            assert_eq!(
                cycles, want,
                "DIVS ${dividend:08X}/${divisor:04X}, quotient ${:08X}",
                cpu.d[0] & 0xFFFF
            );
        }

        // 7 / 2 four ways: one walk, four sign treatments, and the differences
        // are exactly the two loops a negative dividend costs and the one loop
        // of putting a negative quotient back.
        let plus = divide(DIVS, 7, 2).1;
        assert_eq!(divide(DIVS, 7, -2).1, plus + 2, "signs differ: one loop");
        assert_eq!(divide(DIVS, -7, -2).1, plus + 4, "negative dividend, signs agree");
        assert_eq!(divide(DIVS, -7, 2).1, plus + 6, "both terms at once");
    }
}
