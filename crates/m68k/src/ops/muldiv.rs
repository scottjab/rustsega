//! `MULU` and `MULS`: the 16×16 → 32 multiplies (M68000UM MULU, MULS; M1-21, #29).
//!
//! ```text
//! MULU  1100 ddd 011 mmm rrr    Dn.w × <ea>.w → Dn.l, unsigned
//! MULS  1100 ddd 111 mmm rrr    Dn.w × <ea>.w → Dn.l, signed
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

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::ea::{Ea, Size, sign_extend_word};
use crate::ops::field;

/// Clocks a multiply costs whatever its operands are: the fetch, the operand
/// read, the 16-iteration array and the long write-back (M68000UM section 8,
/// "MULU"/"MULS", "Execution Clocks [..]: 38 + 2n"). The `[..]` in the manual
/// is the execution time alone; `+ 2n` covers the data-dependent part and the
/// addressing time is added separately by [`mul_ea_clocks`].
const MUL_BASE_CLOCKS: u32 = 38;
/// Clocks each iteration of the multiply array costs: `2n` in the formula above.
const MUL_ITERATION_CLOCKS: u32 = 2;

/// One 16-bit bus access: 4 clocks (M68000UM Appendix C, "Bus Timings").
const BUS_ACCESS: u32 = 4;
/// One extension word read from the instruction stream: one access.
const EXTENSION_WORD: u32 = 4;
/// The internal calculation an index byte needs on top of the access: the 68000
/// spends 5 clocks on an index where `An` indirect spends 4 (M68000UM
/// Appendix D, indexed addressing). `ea::INDEX_CALC_CLOCKS` is the same 2 for the
/// instructions whose EA [`M68k::resolve_ea`] prices; a multiply has no size
/// field for that path to work off, so it prices its own EA here.
const INDEX_CALC: u32 = 2;
/// What `-(An)` costs over a plain access: the decrement is an internal cycle,
/// as it is in `ea::PREDEC_CALC_CLOCKS`.
const PREDEC_CALC: u32 = 2;

/// Clocks the source effective address costs on top of `38 + 2n`.
///
/// Measured mode by mode against every non-exception case of `MULU.json.gz` and
/// `MULS.json.gz`: `length - (38 + 2n)` is exactly this constant for each mode,
/// in both files and with no scatter (see `tests/muldiv.rs`). It is the same
/// 4-clocks-per-access / 4-per-extension-word / 2-per-index-calculation model
/// [`crate::ea`] charges ADD and MOVE through; `MULU`/`MULS` have no size field,
/// so the group cannot share that calculation and this table is the result.
///
/// Mode `001` cannot reach a multiply: [`crate::decode`] decodes it to
/// [`crate::decode::Op::Illegal`] (#29).
const fn mul_ea_clocks(mode: u8, reg: u8) -> u32 {
    match mode {
        // `Dn`: the operand is already in the register file.
        0b000 => 0,
        // `(An)+`'s register update is free; `-(An)`'s is not (ea.rs charges the
        // same difference for the sized instructions).
        0b010 => BUS_ACCESS,
        0b011 => BUS_ACCESS,
        0b100 => BUS_ACCESS + PREDEC_CALC,
        0b101 => EXTENSION_WORD + BUS_ACCESS,
        0b110 => EXTENSION_WORD + BUS_ACCESS + INDEX_CALC,
        0b111 => match reg {
            0b000 => EXTENSION_WORD + BUS_ACCESS,
            0b001 => 2 * EXTENSION_WORD + BUS_ACCESS,
            0b010 => EXTENSION_WORD + BUS_ACCESS,
            0b011 => EXTENSION_WORD + BUS_ACCESS + INDEX_CALC,
            // `#imm`: the extension word *is* the operand, so one access, not two.
            _ => EXTENSION_WORD,
        },
        // Mode `001` is all that is left, and it cannot get here: see the note
        // above. `0` rather than a `todo!()` on a path the decoder closes.
        _ => 0,
    }
}

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
        MUL_BASE_CLOCKS + MUL_ITERATION_CLOCKS * iterations + mul_ea_clocks(mode, ea_reg)
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
    /// Returns the CPU and `step`'s clock count, like [`multiply`].
    fn divide(op_mode: u16, dividend: u32, divisor: u32) -> (M68k, u32) {
        let (mut cpu, mut bus) = cpu_with_program(&[div(op_mode, 0, 0b000, 1)]);
        cpu.d[0] = dividend;
        cpu.d[1] = divisor;
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
        assert_eq!(cpu.d[0], 0xFFFF_0001, "-7 / -4 = 1 remainder -3");
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
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0x0001_0001, "D0 is not written on overflow");
        assert!(cpu.flag_v());
        assert!(!cpu.flag_c(), "overflow sets V and clears C");
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
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0x8000_0000, "D0 untouched");
        assert!(cpu.flag_v());
        assert!(!cpu.flag_c());
        assert!(cpu.flag_n() && cpu.flag_z(), "N and Z left alone");

        // One below the cliff, `(-2^31 + 1) / -1` fits and is a normal divide.
        let (cpu, _) = divide(DIVS, 0x8000_0001, 0xFFFF);
        assert_eq!(cpu.d[0], 0x0000_7FFF, "quotient 0x7FFF, remainder 0");
        assert!(!cpu.flag_v());
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
            cpu.step(&mut bus);

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
                ALL_FLAGS,
                "the frame's SR is the SR as it was before the trap"
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
            (100u32, 7u32, 0x0002_000E),    // normal
            (0x0001_0001, 1, 0x0001_0001),  // overflow: Dn untouched
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
        assert_eq!(cpu.a[0], DATA + 2, "a trapped divide still consumed its operand");
    }

    /// The `DIVU` cost model: `108 + 2n` with `n` the set bits of the quotient,
    /// so the manual's 140-clock worst case is `108 + 2*16` and the cheapest
    /// divide is 108. Approximation — see [`div_clocks`].
    #[test]
    fn divu_timing_examples() {
        for (dividend, want) in [
            (0x0000_0000u32, 108), // quotient 0: no set bits
            (0x0000_0001, 110),    // one set bit
            (0x0000_00FF, 124),    // eight
            (0x0000_5555, 124),    // 0x5555 has eight set bits too
            (0x0000_FFFF, 140),    // the manual's worst case
        ] {
            let (_, cycles) = divide(DIVU, dividend, 1);
            assert_eq!(cycles, want, "DIVU ${dividend:08X}/1");
        }
    }

    /// The `DIVS` cost model: `126 + 2n` with `n` the bit-transition count of
    /// the quotient (the signed divider skips runs of equal bits, the same shape
    /// [`muls_iterations`] charges a multiply), anchored on the manual's
    /// 158-clock worst case: `126 + 2*16`.
    #[test]
    fn divs_timing_examples() {
        for (dividend, want) in [
            (0x0000_0000i32, 126), // quotient 0: no transitions at all
            (0x0000_0001, 130),    // one set bit, two transitions
            (0x0000_5555, 158),    // alternating bits: the worst case
            (-1, 128),             // quotient -1 = 0xFFFF: a single transition
        ] {
            let (_, cycles) = divide(DIVS, dividend as u32, 1);
            assert_eq!(cycles, want, "DIVS ${dividend:#X}/1");
        }
    }
}
