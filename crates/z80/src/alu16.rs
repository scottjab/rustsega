//! The 16-bit arithmetic group (M2-07, #50): `ADD HL,rr`, `ADC HL,rr`,
//! `SBC HL,rr`, `INC rr` and `DEC rr`, with the `DD`/`FD` forms of the ones that
//! mention `HL`.
//!
//! `ADC`/`SBC HL,rr` exist only in the `ED` space, so they have no indexed
//! forms: `DD ED 4A` rots the prefix and executes the `HL` version. `ADD HL,rr`
//! and `INC`/`DEC rr` live in the main space where a prefix does apply, and it
//! redirects the pair in bits 5-4 when that pair is `HL` - which is why
//! `DD 29` is `ADD IX,IX`, with `IX` as both destination and source
//! (Z80 CPU Family User's Manual UM0080, section 2.2, "Instruction Set").
//!
//! Two things separate this group from the 8-bit ALU in [`crate::alu8`]:
//!
//! - `ADD HL,rr` establishes only `H`, `N`, `C` and `X`/`Y`; `S`, `Z` and `P/V`
//!   survive from whatever ran last (`09`: 1000 cases with `final.f` keeping the
//!   initial `S`, `Z` and `P/V`). So it cannot share the 8-bit ALU's
//!   all-bits-established flag byte.
//! - `X`/`Y` are bits 11 and 13 of the 16-bit result, i.e. bits 3 and 5 of its
//!   high byte, and `H` is the carry out of bit 11 - one bit position higher
//!   than the 8-bit half carry, and nothing to do with the low byte.
//!
//! `wz` (MEMPTR) takes `HL + 1` with `HL` from *before* the add for the three
//! arithmetic ops (`09`, `ed 42`, `ed 4a` and friends: 1000 cases each), and
//! does not move at all for `INC`/`DEC rr` (`03`, `2b`, `dd 23`: unchanged in
//! all 1000 cases of each). Nothing in this group touches memory.

use crate::{FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_X, FLAG_Y, FLAG_Z, IndexMode, Z80};

/// `ADD HL,rr` (`09 19 29 39`): 11 T-states, or 15 once a `DD`/`FD` makes it
/// `ADD IX,rr`.
const T_ADD_HL: u32 = 11;
const T_ADD_INDEXED: u32 = 15;
/// `ADC HL,rr` and `SBC HL,rr`: 15 T-states, the `ED` fetch included.
const T_ADC_SBC_HL: u32 = 15;
/// `INC rr` / `DEC rr`: 6 T-states, or 10 for `DD 23` / `FD 2B`.
const T_INC_DEC_RR: u32 = 6;
const T_INC_DEC_INDEXED: u32 = 10;

/// The flag bits `ADD HL,rr` establishes. `S`, `Z` and `P/V` are deliberately
/// absent: `09` keeps those three bits of `F` at their initial values in all
/// 1000 cases, so the instruction says nothing about them.
const FLAGS_ADD_HL: u8 = FLAG_H | FLAG_N | FLAG_C | FLAG_X | FLAG_Y;

/// `ADC`/`SBC HL,rr` establish all eight bits, `Z` included even though their
/// result is a pair and not a byte.
const ALL_FLAGS: u8 = 0xFF;

/// Half-carry for a 16-bit add or subtract: the carry out of bit 11 is what
/// `H` reports, and it shows up as bit 12 of the operand/result XOR - one
/// position above where [`crate::alu8`] looks.
const HALF_CARRY_16: u16 = 0x1000;
/// The sign bit of a pair, for the same XOR trick on `P/V`.
const SIGN_16: u16 = 0x8000;

/// `S` and `Z` from a 16-bit result: bit 15 for the sign, all sixteen bits for
/// zero. `P/V` is not here because it depends on the operands, not the result
/// alone, and `ADD HL,rr` does not derive `S`/`Z` at all.
///
/// Widening is by `as` rather than the `u16::from` the rest of the crate uses:
/// `From` is not yet stable as a const trait, so `const fn` cannot call it.
const fn sz16(result: u16) -> u8 {
    let mut flags = 0;
    if result & SIGN_16 != 0 {
        flags |= FLAG_S;
    }
    if result == 0 {
        flags |= FLAG_Z;
    }
    flags
}

/// The undocumented `X`/`Y` for a 16-bit result: bits 11 and 13 of the pair,
/// which are bits 3 and 5 of its high byte and so land in place with no shift.
const fn xy16(result: u16) -> u8 {
    ((result >> 8) as u8) & (FLAG_X | FLAG_Y)
}

/// `ADD HL,rr`: the sum and the five flags it establishes.
///
/// `H` is the carry out of bit 11 and `C` the carry out of bit 15, with no
/// carry in - that is what separates it from `ADC HL,rr` (UM0080, section 2.2,
/// "Instruction Set"). `S`, `Z` and `P/V` are not in the returned byte because
/// the instruction leaves them alone; `N` is in it as a zero, so the merge in
/// [`Z80::update_flags`] clears it.
const fn add_hl_flags(a: u16, b: u16) -> (u16, u8) {
    let sum = a as u32 + b as u32;
    let result = sum as u16;
    let mut flags = xy16(result);
    if (a ^ b ^ result) & HALF_CARRY_16 != 0 {
        flags |= FLAG_H;
    }
    if sum > u16::MAX as u32 {
        flags |= FLAG_C;
    }
    (result, flags)
}

/// `ADC HL,rr`: the sum including the carry in, and all eight flags.
///
/// The carry in joins the half-carry XOR because it can be what tips bit 11:
/// with `C` set, `0x0FFF + 0x0000` does carry out of bit 11 even though the two
/// operands alone do not.
const fn adc_hl_flags(a: u16, b: u16, carry_in: u16) -> (u16, u8) {
    let sum = a as u32 + b as u32 + carry_in as u32;
    let result = sum as u16;
    let mut flags = sz16(result) | xy16(result);
    if (a ^ b ^ carry_in ^ result) & HALF_CARRY_16 != 0 {
        flags |= FLAG_H;
    }
    // Signed overflow: two operands of one sign summing to the other.
    if (a ^ result) & (b ^ result) & SIGN_16 != 0 {
        flags |= FLAG_PV;
    }
    if sum > u16::MAX as u32 {
        flags |= FLAG_C;
    }
    (result, flags)
}

/// `SBC HL,rr`: the difference including the borrow in, and all eight flags.
///
/// The signed-overflow XOR flips to `a ^ b` and `a ^ result` because the
/// operands that must agree in sign are now the ones being subtracted. `H` is
/// the borrow out of bit 11 including the borrow in, which is why `HL - HL - 1`
/// reports a half borrow. `C` is the borrow out of bit 15, so it is set
/// whenever the arithmetic difference goes negative.
const fn sbc_hl_flags(a: u16, b: u16, borrow_in: u16) -> (u16, u8) {
    let difference = a as i32 - b as i32 - borrow_in as i32;
    let result = difference as u16;
    let mut flags = sz16(result) | xy16(result) | FLAG_N;
    if (a ^ b ^ borrow_in ^ result) & HALF_CARRY_16 != 0 {
        flags |= FLAG_H;
    }
    if (a ^ b) & (a ^ result) & SIGN_16 != 0 {
        flags |= FLAG_PV;
    }
    if difference < 0 {
        flags |= FLAG_C;
    }
    (result, flags)
}

impl Z80 {
    /// `ADD HL,rr` (`09 19 29 39`) and the `DD`/`FD` forms: 11 T-states, 15
    /// with a prefix.
    ///
    /// Bits 5-4 pick the operand pair, and in the prefixed space the pair field
    /// `2` is the index register itself, so `DD 29` is `ADD IX,IX` and `HL`
    /// never moves. `wz` takes the destination `+ 1` with the destination from
    /// *before* the add (`09`, `dd 29`: 1000 cases each).
    pub(crate) fn add_hl_rr(&mut self, op: u8, mode: IndexMode) -> u32 {
        let dest = self.index(mode);
        let src = self.pair(op, mode);
        let (result, flags) = add_hl_flags(dest, src);
        self.set_index(mode, result);
        self.update_flags(FLAGS_ADD_HL, flags);
        self.set_wz(dest.wrapping_add(1));
        self.latch_flags();
        match mode {
            IndexMode::Hl => T_ADD_HL,
            IndexMode::Ix | IndexMode::Iy => T_ADD_INDEXED,
        }
    }

    /// `ADC HL,rr` (`ED 4A/5A/6A/7A`) and `SBC HL,rr` (`ED 42/52/62/72`): 15
    /// T-states with the prefix, all eight flags established, `wz = HL + 1`.
    ///
    /// Bit 3 splits the pair, with `0` for `SBC` - the opposite way round to
    /// the 8-bit ALU, where `y = 1` is `ADC` and `y = 3` is `SBC`. There are no
    /// indexed forms: `ED` does not take a `DD`/`FD`, which rots into a `NOP`
    /// in front of it.
    pub(crate) fn adc_sbc_hl_rr(&mut self, op: u8) -> u32 {
        let dest = self.hl();
        let src = self.pair(op, IndexMode::Hl);
        let carry = u16::from(self.get_flag(FLAG_C));
        let (result, flags) = if (op >> 3) & 1 == 0 {
            sbc_hl_flags(dest, src, carry)
        } else {
            adc_hl_flags(dest, src, carry)
        };
        self.set_hl(result);
        self.update_flags(ALL_FLAGS, flags);
        self.set_wz(dest.wrapping_add(1));
        self.latch_flags();
        T_ADC_SBC_HL
    }

    /// `INC rr` (`03 13 23 33`) and `DD/FD 23`: 6 T-states, 10 for the index
    /// pair.
    ///
    /// These touch no flags at all - not even `H`, unlike 8-bit `INC` - so the
    /// flag byte survives untouched and `q` goes to 0 rather than to `F`,
    /// because nothing was computed (`03`, `dd 23`: `final.q == 0` in all 1000
    /// cases).
    pub(crate) fn inc_rr(&mut self, op: u8, mode: IndexMode) -> u32 {
        let value = self.pair(op, mode).wrapping_add(1);
        self.set_pair(op, mode, value);
        self.clear_latches();
        Self::pair_bump_cycles(mode)
    }

    /// `DEC rr` (`0B 1B 2B 3B`) and `DD/FD 2B`: as [`Z80::inc_rr`].
    pub(crate) fn dec_rr(&mut self, op: u8, mode: IndexMode) -> u32 {
        let value = self.pair(op, mode).wrapping_sub(1);
        self.set_pair(op, mode, value);
        self.clear_latches();
        Self::pair_bump_cycles(mode)
    }

    /// `INC`/`DEC` cost 6 T-states for a main-space pair and 10 once a prefix
    /// is paying for itself by naming `IX`/`IY`.
    const fn pair_bump_cycles(mode: IndexMode) -> u32 {
        match mode {
            IndexMode::Hl => T_INC_DEC_RR,
            IndexMode::Ix | IndexMode::Iy => T_INC_DEC_INDEXED,
        }
    }

    /// The pair that bits 5-4 of `rr` name, with `DD`/`FD` replacing `HL` by
    /// `IX`/`IY` in slot `2`. That is the only slot a prefix can redirect, so
    /// the same helper serves `09` (`BC`) and `29` (the destination itself).
    fn pair(&self, op: u8, mode: IndexMode) -> u16 {
        match (op >> 4) & 3 {
            0 => self.bc(),
            1 => self.de(),
            2 => self.index(mode),
            _ => self.sp(),
        }
    }

    /// Load the pair [`Z80::pair`] reads back.
    fn set_pair(&mut self, op: u8, mode: IndexMode, value: u16) {
        match (op >> 4) & 3 {
            0 => self.set_bc(value),
            1 => self.set_de(value),
            2 => self.set_index(mode, value),
            _ => self.set_sp(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::RamBus;

    /// A CPU with `program` sitting at its `PC`, so the step under test fetches
    /// the bytes the test names. Flags start clear: this group's flags are what
    /// the tests are about.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        (cpu, RamBus::at(pc, program))
    }

    /// `ADD HL,rr`: `H` is the carry out of bit 11 and `C` the carry out of bit
    /// 15, and neither is the 8-bit version shifted up in the obvious way.
    #[test]
    fn add_hl_half_carry_bit11() {
        const PC: u16 = 0x0100;
        // 0x0800 + 0x2800: bit 11 carries into bit 12, bit 15 does not carry
        // out, and the result's high byte 0x30 is what feeds Y.
        let (mut cpu, mut bus) = cpu_at(PC, &[0x09]);
        cpu.set_hl(0x0800);
        cpu.set_bc(0x2800);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.hl(), 0x3000);
        assert!(cpu.get_flag(FLAG_H), "bit 11 carried into bit 12");
        assert!(
            !cpu.get_flag(FLAG_C),
            "0x0800 + 0x2800 does not carry out of bit 15"
        );
        assert!(!cpu.get_flag(FLAG_N), "ADD clears N");
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            FLAG_Y,
            "high byte 0x30 has bit 5, not bit 3"
        );

        // The same instruction where only the low nibble carries: the half carry
        // is out of bit 11, so H must stay clear.
        let (mut cpu, mut bus) = cpu_at(PC, &[0x09]);
        cpu.set_hl(0xFFF0);
        cpu.set_bc(0x000F);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.hl(), 0xFFFF);
        assert!(
            !cpu.get_flag(FLAG_H),
            "the low nibble carried, bit 11 did not"
        );

        // And X/Y follow bits 11 and 13 of the result: bit 3 of the *low* byte
        // must not set X.
        let (mut cpu, mut bus) = cpu_at(PC, &[0x09]);
        cpu.set_hl(0x0008);
        cpu.set_bc(0x0000);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.hl(), 0x0008);
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            0,
            "X/Y come from the high byte, not from 0x08"
        );
    }

    /// `ADD HL,rr` establishes `H`, `N`, `C` and `X`/`Y` and leaves `S`, `Z` and
    /// `P/V` exactly as they were.
    #[test]
    fn add_hl_keeps_szpv() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x19]);
        cpu.set_hl(0x1000);
        cpu.set_de(0x0828);
        cpu.set_flags(FLAG_S | FLAG_Z | FLAG_PV | FLAG_N | FLAG_C);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.hl(), 0x1828);
        assert!(cpu.get_flag(FLAG_S), "S survives ADD HL");
        assert!(
            cpu.get_flag(FLAG_Z),
            "Z survives even though the result is not zero"
        );
        assert!(cpu.get_flag(FLAG_PV), "P/V survives");
        assert!(!cpu.get_flag(FLAG_N), "N is established, and ADD clears it");
        assert!(
            !cpu.get_flag(FLAG_C),
            "C is established, and 0x1000 + 0x0828 does not carry"
        );
        assert!(!cpu.get_flag(FLAG_H), "no carry out of bit 11");
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            FLAG_X,
            "high byte 0x18 has bit 3 set"
        );
        assert_eq!(
            cpu.q(),
            cpu.f(),
            "ADD HL recomputes flags, so q is the flag byte"
        );
    }

    /// `ADD HL,rr` does not add `C` - that is what separates it from
    /// `ADC HL,rr` - and then overwrites `C` with the carry out of bit 15.
    #[test]
    fn add_hl_ignores_carry_in() {
        const PC: u16 = 0x0100;
        let (mut cpu, mut bus) = cpu_at(PC, &[0x09, 0x09]);
        cpu.set_hl(0x1000);
        cpu.set_bc(0x2000);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.hl(), 0x3000, "no carry in is added");
        assert!(
            !cpu.get_flag(FLAG_C),
            "C is the carry out of bit 15, which did not happen"
        );

        cpu.set_hl(0xF000);
        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.hl(), 0x1000);
        assert!(cpu.get_flag(FLAG_C), "and a real carry does set it back");
    }

    /// `ADD HL,rr` leaves `wz` at `HL + 1` with `HL` from before the add, and
    /// the `+ 1` is a wrapping add.
    #[test]
    fn add_hl_sets_wz() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x09, 0x09]);
        cpu.set_hl(0x2345);
        cpu.set_bc(0x0100);
        cpu.set_wz(0xAAAA);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.wz(), 0x2346, "wz = HL + 1, HL from before");

        cpu.set_hl(0xFFFF);
        cpu.set_bc(0x0001);
        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(cpu.hl(), 0x0000, "the sum wrapped");
        assert_eq!(cpu.wz(), 0x0000, "and so does the +1");
    }

    /// Bits 5-4 pick the operand pair: `09` BC, `19` DE, `29` HL (a doubling,
    /// since `HL` is also the destination) and `39` SP.
    #[test]
    fn add_hl_pair_field_picks_bc_de_hl_sp() {
        const HL: u16 = 0x1000;
        let cases: [(u8, &str, u16); 4] = [
            (0x09, "BC", 0xB00C),
            (0x19, "DE", 0xD00E),
            (0x29, "HL", HL),
            (0x39, "SP", 0xF011),
        ];
        for (op, name, src) in cases {
            let (mut cpu, mut bus) = cpu_at(0x0100, &[op]);
            cpu.set_hl(HL);
            cpu.set_bc(0xB00C);
            cpu.set_de(0xD00E);
            cpu.set_sp(0xF011);

            assert_eq!(cpu.step(&mut bus), 11, "{op:02X}");

            assert_eq!(
                cpu.hl(),
                HL.wrapping_add(src),
                "{op:02X} is not ADD HL,{name}"
            );
        }
    }

    /// `ADC HL,rr`: `P/V` is the 16-bit signed overflow, the carry in is part of
    /// the sum, and every flag bit is established.
    #[test]
    fn adc_hl_overflow() {
        const PC: u16 = 0x0100;
        // 0x7FFF + 0x0001: two positives producing a negative, so overflow, with
        // a carry out of bit 11 as well but not out of bit 15.
        let (mut cpu, mut bus) = cpu_at(PC, &[0xED, 0x4A]);
        cpu.set_hl(0x7FFF);
        cpu.set_bc(0x0001);

        assert_eq!(cpu.step(&mut bus), 15);

        assert_eq!(cpu.hl(), 0x8000);
        assert!(cpu.get_flag(FLAG_PV), "0x7FFF + 1 overflows a word");
        assert!(cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_H), "0xFFF + 1 carries out of bit 11");
        assert!(!cpu.get_flag(FLAG_C), "no carry out of bit 15");
        assert!(!cpu.get_flag(FLAG_N), "ADC clears N");
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            0,
            "high byte 0x80 has neither bit 3 nor bit 5"
        );
        assert_eq!(cpu.wz(), 0x8000, "wz = HL + 1 from before the add");
        assert_eq!(cpu.q(), cpu.f());

        // The carry in alone is enough to tip 0x7FFF over, with nothing added.
        let (mut cpu, mut bus) = cpu_at(PC, &[0xED, 0x4A]);
        cpu.set_hl(0x7FFF);
        cpu.set_bc(0);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 15);

        assert_eq!(cpu.hl(), 0x8000, "0x7FFF + 0 + 1");
        assert!(cpu.get_flag(FLAG_PV));
        assert!(!cpu.get_flag(FLAG_C));
    }

    /// `SBC HL,rr` establishes all eight flag bits, `Z` included on a zero
    /// result, and takes its `H` from the borrow out of bit 11.
    #[test]
    fn sbc_hl_zero_sets_z() {
        const PC: u16 = 0x0100;
        // Every flag bar `C`: with the carry in set, `SBC HL,HL` is
        // `HL - HL - 1` and cannot be zero, so a zero result needs a clear `C`.
        let (mut cpu, mut bus) = cpu_at(PC, &[0xED, 0x62]);
        cpu.set_hl(0xB85B);
        cpu.set_flags(!FLAG_C);

        assert_eq!(cpu.step(&mut bus), 15);

        assert_eq!(cpu.hl(), 0x0000, "SBC HL,HL");
        assert_eq!(
            cpu.f(),
            FLAG_Z | FLAG_N,
            "every other bit is cleared, including the S, H and P/V that started \
             set, and the C that SBC HL,HL establishes clear"
        );
        assert_eq!(cpu.wz(), 0xB85C, "wz = HL + 1 from before the subtract");
        assert_eq!(cpu.q(), cpu.f());

        // 0x0000 - 0x0001: borrow out of bit 15 and out of bit 11, with the
        // result's high byte feeding X and Y.
        let (mut cpu, mut bus) = cpu_at(PC, &[0xED, 0x42]);
        cpu.set_hl(0x0000);
        cpu.set_bc(0x0001);

        assert_eq!(cpu.step(&mut bus), 15);

        assert_eq!(cpu.hl(), 0xFFFF);
        assert!(cpu.get_flag(FLAG_C), "0 - 1 borrows out of bit 15");
        assert!(cpu.get_flag(FLAG_H), "and borrows out of bit 11");
        assert!(cpu.get_flag(FLAG_N));
        assert!(cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(
            !cpu.get_flag(FLAG_PV),
            "0 - 1 does not overflow a signed word"
        );
        assert_eq!(cpu.f() & (FLAG_X | FLAG_Y), FLAG_X | FLAG_Y);
    }

    /// The `rr` field of the `ED` forms: bits 5-4 select `BC`, `DE`, `HL`, `SP`
    /// and bit 3 splits `SBC` (0) from `ADC` (1).
    #[test]
    fn adc_sbc_pair_field_picks_bc_de_hl_sp() {
        const HL: u16 = 0x1234;
        let sources: [(&str, u16); 4] =
            [("BC", 0xB00C), ("DE", 0xD00E), ("HL", HL), ("SP", 0xF011)];
        for (index, (name, src)) in sources.iter().enumerate() {
            for (offset, subtract) in [(0u8, true), (8u8, false)] {
                let op = 0x42 + 0x10 * index as u8 + offset;
                let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, op]);
                cpu.set_hl(HL);
                cpu.set_bc(0xB00C);
                cpu.set_de(0xD00E);
                cpu.set_sp(0xF011);

                assert_eq!(cpu.step(&mut bus), 15, "ED {op:02X}");

                let want = if subtract {
                    HL.wrapping_sub(*src)
                } else {
                    HL.wrapping_add(*src)
                };
                let mnemonic = if subtract { "SBC" } else { "ADC" };
                assert_eq!(cpu.hl(), want, "ED {op:02X} is not {mnemonic} HL,{name}");
                assert_eq!(cpu.get_flag(FLAG_N), subtract, "ED {op:02X}");
            }
        }
    }

    /// `INC rr` wraps the whole pair and touches nothing else: no flag moves,
    /// `wz` stays put, and `q` goes to zero because no flags were computed.
    #[test]
    fn inc_bc_wraps() {
        const AFTER: u8 = 0xDB;

        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x03, 0x0B]);
        cpu.set_bc(0xFFFF);
        cpu.set_flags(AFTER);
        cpu.set_wz(0x1234);
        cpu.set_q(0x5A);

        assert_eq!(cpu.step(&mut bus), 6);

        assert_eq!(cpu.bc(), 0x0000, "0xFFFF + 1 wraps the pair");
        assert_eq!(cpu.f(), AFTER, "INC rr touches no flags at all");
        assert_eq!(cpu.wz(), 0x1234, "wz is unchanged");
        assert_eq!(
            cpu.q(),
            0,
            "an instruction that computes no flags leaves q at 0"
        );

        assert_eq!(cpu.step(&mut bus), 6, "DEC BC");

        assert_eq!(cpu.bc(), 0xFFFF);
        assert_eq!(cpu.f(), AFTER, "DEC rr touches no flags either");
        assert_eq!(cpu.q(), 0);
    }

    /// `INC rr` / `DEC rr` for all four pairs, at 6 T-states each.
    #[test]
    fn inc_dec_rr_touches_every_pair() {
        /// `INC` opcode, `DEC` opcode, name for the messages, and how to reach
        /// the pair from the CPU.
        type PairCase = (u8, u8, &'static str, fn(&Z80) -> u16, fn(&mut Z80, u16));

        let cases: [PairCase; 4] = [
            (0x03, 0x0B, "BC", |c| c.bc(), |c, v| c.set_bc(v)),
            (0x13, 0x1B, "DE", |c| c.de(), |c, v| c.set_de(v)),
            (0x23, 0x2B, "HL", |c| c.hl(), |c, v| c.set_hl(v)),
            (0x33, 0x3B, "SP", |c| c.sp(), |c, v| c.set_sp(v)),
        ];
        for (index, (inc, dec, name, read, write)) in cases.iter().enumerate() {
            let base = 0x1000 + 0x1111 * index as u16;
            let (mut cpu, mut bus) = cpu_at(0x0100, &[*inc, *dec]);
            write(&mut cpu, base);

            assert_eq!(cpu.step(&mut bus), 6, "{inc:02X}");
            assert_eq!(read(&cpu), base.wrapping_add(1), "INC {name}");

            assert_eq!(cpu.step(&mut bus), 6, "{dec:02X}");
            assert_eq!(read(&cpu), base, "DEC {name}");
        }
    }

    /// `DD`/`FD` redirect both halves of `ADD HL,rr`: the pair field 2 is the
    /// index register itself, so `DD 29` is `ADD IX,IX`, and `HL` never moves.
    #[test]
    fn add_ix_includes_ix_itself() {
        const PC: u16 = 0x0100;
        for (prefix, name) in [(0xDDu8, "IX"), (0xFDu8, "IY")] {
            for (op, src) in [(0x09u8, 0x1020u16), (0x19, 0x2040), (0x39, 0x4080)] {
                let (mut cpu, mut bus) = cpu_at(PC, &[prefix, op]);
                cpu.set_hl(0xF00F);
                cpu.set_bc(0x1020);
                cpu.set_de(0x2040);
                cpu.set_sp(0x4080);
                cpu.set_ix(0x0100);
                cpu.set_iy(0x0200);
                let before = if prefix == 0xDD { cpu.ix() } else { cpu.iy() };

                assert_eq!(cpu.step(&mut bus), 15, "{prefix:02X} {op:02X}");

                let after = if prefix == 0xDD { cpu.ix() } else { cpu.iy() };
                assert_eq!(
                    after,
                    before.wrapping_add(src),
                    "{prefix:02X} {op:02X} is not ADD {name},the pair"
                );
                assert_eq!(cpu.hl(), 0xF00F, "HL must not move in this space");
                assert_eq!(cpu.wz(), before.wrapping_add(1), "wz = {name} + 1");
            }

            // Pair field 2 is the index register, not HL.
            let (mut cpu, mut bus) = cpu_at(PC, &[prefix, 0x29]);
            cpu.set_ix(0x3456);
            cpu.set_iy(0x789A);
            let before: u16 = if prefix == 0xDD { 0x3456 } else { 0x789A };

            assert_eq!(cpu.step(&mut bus), 15, "{prefix:02X} 29");

            let after = if prefix == 0xDD { cpu.ix() } else { cpu.iy() };
            assert_eq!(
                after,
                before.wrapping_mul(2),
                "{prefix:02X} 29 is not ADD {name},{name}"
            );
            assert_eq!(cpu.wz(), before.wrapping_add(1));
        }
    }

    /// `DD 23` / `FD 2B`: the index pair moves and `HL` does not, still no
    /// flags, no `q`, no `wz` change, 10 T-states including the prefix.
    #[test]
    fn inc_dec_index_pair_keeps_flags() {
        const PC: u16 = 0x0100;
        let (mut cpu, mut bus) = cpu_at(PC, &[0xDD, 0x23, 0xFD, 0x2B]);
        cpu.set_hl(0xF00F);
        cpu.set_ix(0x1234);
        cpu.set_iy(0x9FFF);
        cpu.set_flags(0x3C);
        cpu.set_wz(0x7777);

        assert_eq!(cpu.step(&mut bus), 10);

        assert_eq!(cpu.ix(), 0x1235);
        assert_eq!(cpu.hl(), 0xF00F, "DD 23 must not touch HL");
        assert_eq!(cpu.r(), 2, "the prefix and the opcode are both M1 fetches");

        assert_eq!(cpu.step(&mut bus), 10);

        assert_eq!(cpu.iy(), 0x9FFE);
        assert_eq!(cpu.ix(), 0x1235, "FD 2B must not touch IX");
        assert_eq!(cpu.f(), 0x3C, "INC/DEC rr still touches no flags");
        assert_eq!(cpu.q(), 0);
        assert_eq!(cpu.wz(), 0x7777, "and wz does not move");
        assert_eq!(bus.reads, vec![PC, PC + 1, PC + 2, PC + 3]);
    }

    /// A `DD`/`FD` in front of an `INC`/`DEC rr` that does not name `HL` has
    /// nothing to redirect: the prefix rots into a 4 T-state `NOP` and the
    /// unprefixed instruction runs, for 10 T-states overall (`dd 03`: 1000
    /// cases, BC incremented, `PC + 2`, `R + 2`).
    #[test]
    fn rotted_prefix_costs_nop_plus_instruction() {
        /// Prefix, opcode, how far the pair should move, and how to read it.
        type RottedCase = (u8, u8, i32, fn(&Z80) -> u16);

        let cases: [RottedCase; 3] = [
            (0xDD, 0x03, 1, |c| c.bc()),
            (0xFD, 0x1B, -1, |c| c.de()),
            (0xDD, 0x33, 1, |c| c.sp()),
        ];
        for (prefix, op, delta, read) in cases {
            let (mut cpu, mut bus) = cpu_at(0x0100, &[prefix, op]);
            cpu.set_bc(0x1234);
            cpu.set_de(0x1234);
            cpu.set_sp(0x1234);
            cpu.set_ix(0xAAAA);
            cpu.set_iy(0xBBBB);
            cpu.set_wz(0xCCCC);

            assert_eq!(cpu.step(&mut bus), 10, "{prefix:02X} {op:02X}");

            assert_eq!(read(&cpu), (0x1234 + delta) as u16, "{prefix:02X} {op:02X}");
            assert_eq!(cpu.ix(), 0xAAAA, "the wasted prefix must not move IX");
            assert_eq!(cpu.iy(), 0xBBBB);
            assert_eq!(cpu.pc(), 0x0102, "both bytes were consumed");
            assert_eq!(cpu.r(), 2, "the prefix and the opcode both count for R");
            assert_eq!(cpu.q(), 0);
        }
    }
}
