//! Decimal adjust, complement and the flag fiddles (M2-13, #56).
//!
//! ```text
//! 0x27  DAA   4      0x37  SCF   4
//! 0x2F  CPL   4      0x3F  CCF   4
//! ```
//!
//! None of them reads memory, so each is a flag write plus
//! [`Z80::latch_flags`]; `S`, `Z` and `P/V` are never in play for the three
//! single-byte flag instructions, which is why they mask only the bits they
//! actually compute.

use crate::alu8::sub_flags;
use crate::rot::ALL_FLAGS;
use crate::{FLAG_C, FLAG_H, FLAG_N, FLAG_X, FLAG_Y, SZXY_PARITY, Z80};

impl Z80 {
    /// `DAA` (`27`): 4 T-states.
    ///
    /// The correction is worked out from `A` and the flags *before* any adjusting:
    /// `0x06` when the old half-carry is set or the old low nibble is past 9,
    /// `0x60` when the old carry is set or the old `A` is past `0x99`, added or
    /// subtracted according to `N` (Z80 CPU Family User's Manual UM0080, section
    /// 2.2, `DAA`).
    ///
    /// Two flag results are in no manual and are what all 1000 cases of `27.json`
    /// say:
    ///
    /// * `P/V` is the parity of the adjusted result, not the decimal overflow.
    /// * `H` cannot be recomputed from the result - the adjuster already destroyed
    ///   the half-carry it would have come from - so on a subtraction it survives
    ///   only when the old low nibble needed no `0x06` borrow
    ///   (`(A_old & 0x0F) < 6`), and on an addition it is set exactly when the old
    ///   low nibble overflowed (`(A_old & 0x0F) > 9`).
    pub(crate) fn daa(&mut self) -> u32 {
        let a = self.a();
        let f = self.f();
        let mut adjust = 0u8;
        if f & FLAG_H != 0 || a & 0x0F > 9 {
            adjust += 0x06;
        }
        let mut carry = f & FLAG_C;
        if carry != 0 || a > 0x99 {
            adjust += 0x60;
            carry = FLAG_C;
        }
        let result = if f & FLAG_N != 0 {
            a.wrapping_sub(adjust)
        } else {
            a.wrapping_add(adjust)
        };
        let half = if f & FLAG_N != 0 {
            f & FLAG_H != 0 && a & 0x0F < 6
        } else {
            a & 0x0F > 9
        };
        let mut flags = SZXY_PARITY[usize::from(result)] | (f & FLAG_N) | carry;
        if half {
            flags |= FLAG_H;
        }
        self.set_a(result);
        self.update_flags(ALL_FLAGS, flags);
        self.latch_flags();
        4
    }

    /// `CPL` (`2F`): 4 T-states.
    ///
    /// Complements `A` and forces `H` and `N` (Z80UM 2.2). `S`, `Z` and `P/V` are
    /// left alone - `CPL` is not an operation on the flags - and `X`/`Y` take the
    /// undocumented bits of the new `A`.
    pub(crate) fn cpl(&mut self) -> u32 {
        let a = !self.a();
        self.set_a(a);
        let flags = (a & (FLAG_X | FLAG_Y)) | FLAG_H | FLAG_N;
        self.update_flags(FLAG_H | FLAG_N | FLAG_X | FLAG_Y, flags);
        self.latch_flags();
        4
    }

    /// `SCF` (`37`) and `CCF` (`3F`): 4 T-states each.
    ///
    /// `SCF` sets `C` and clears `H` and `N`; `CCF` toggles `C` and moves the old
    /// `C` into `H` (Z80UM 2.2). The undocumented part is `X`/`Y`, which come from
    /// the old flags, the previous instruction's `q` and `A` together:
    /// `((q ^ F) | A) & 0x28` ("The Undocumented Z80 Documented", Sean Young).
    /// Either operand alone leaves hundreds of the 1000 cases of `37.json` and
    /// `3f.json` wrong, and `q` is why these two are not flag-independent: `SCF`
    /// run twice with `A = 0` and `F = 0` gives `X/Y = 0x28` the second time
    /// because the first one latched `F` into `q`.
    fn flag_fiddle(&mut self, toggle: bool) -> u32 {
        let f = self.f();
        // S, Z and P/V survive untouched; H, N, C and X/Y are rebuilt below.
        let mut flags = f & !(FLAG_H | FLAG_N | FLAG_C | FLAG_X | FLAG_Y);
        flags |= ((self.q ^ f) | self.a()) & (FLAG_X | FLAG_Y);
        if toggle {
            if f & FLAG_C != 0 {
                flags |= FLAG_H;
            } else {
                flags |= FLAG_C;
            }
        } else {
            flags |= FLAG_C;
        }
        self.update_flags(FLAG_H | FLAG_N | FLAG_C | FLAG_X | FLAG_Y, flags);
        self.latch_flags();
        4
    }

    /// `SCF` (`37`): set carry.
    pub(crate) fn scf(&mut self) -> u32 {
        self.flag_fiddle(false)
    }

    /// `CCF` (`3F`): complement carry.
    pub(crate) fn ccf(&mut self) -> u32 {
        self.flag_fiddle(true)
    }

    /// `NEG` (`ED 44`) and its seven mirrors: 8 T-states.
    ///
    /// `NEG` is `A <- 0 - A` with every flag a subtract would take (Z80UM 2.2,
    /// `NEG`): `N` set, `C` set for every `A` but `0`, `V` set only for `0x80`.
    /// The mirrors `ED 4C`, `54`, `5C`, `64`, `6C`, `74` and `7C` are the same
    /// instruction on the real CPU, which leaves bits 4 and 3 of the operand
    /// field open in the `ED` group; all 1000 cases of each of the eight files
    /// agree, and `wz` stays where it was.
    pub(crate) fn neg(&mut self) -> u32 {
        let (result, flags) = sub_flags(0, self.a(), 0);
        self.set_a(result);
        self.update_flags(ALL_FLAGS, flags);
        self.latch_flags();
        8
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::RamBus;
    use crate::{FLAG_PV, FLAG_S, FLAG_Z};

    /// A CPU with `program` at its `PC` and every flag clear, so a test's
    /// `set_flags` call is the whole starting flag state.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        (cpu, RamBus::at(pc, program))
    }

    /// `DAA` after `ADD A,0x27` with `A = 0x15`: the binary sum `0x3C` is not a
    /// valid BCD byte, and the adjust turns it into the decimal answer `0x42`.
    #[test]
    fn daa_after_add_0x15_0x27() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xC6, 0x27, 0x27]);
        cpu.set_a(0x15);

        assert_eq!(cpu.step(&mut bus), 7, "ADD A,n is 7 T-states");
        assert_eq!(
            cpu.a(),
            0x3C,
            "binary sum, half-carry-free and no BCD sense yet"
        );
        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.a(), 0x42, "decimal 15 + 27");
        assert_eq!(
            cpu.f(),
            FLAG_H | FLAG_PV,
            "the low-nibble correction shows up in H, P/V is the even parity of 0x42"
        );
        assert_eq!(cpu.q(), cpu.f(), "DAA recomputed the flags, so q follows");
    }

    /// `DAA` with both nibbles over: the `0x60` correction wraps `A` and sets
    /// `C`, and `P/V` is the parity of the adjusted result.
    #[test]
    fn daa_adjusts_up_and_sets_carry() {
        let (mut cpu, _) = cpu_at(0x0100, &[0x27]);
        cpu.set_a(0x9B);
        cpu.set_flags(FLAG_H | FLAG_X | FLAG_Y);

        assert_eq!(cpu.daa(), 4);

        assert_eq!(cpu.a(), 0x01, "0x9B + 0x66");
        assert_eq!(
            cpu.f(),
            FLAG_H | FLAG_C,
            "S and Z from the result, P/V = parity(0x01) = clear, N clear"
        );
    }

    /// `DAA` on a subtraction: `H` only survives when the old low nibble did
    /// not need the `0x06` borrow.
    #[test]
    fn daa_subtract_half_carry() {
        let (mut cpu, _) = cpu_at(0x0100, &[0x27]);
        cpu.set_a(0x2B);
        cpu.set_flags(FLAG_H | FLAG_N);
        cpu.daa();
        assert_eq!(cpu.a(), 0x25);
        assert!(
            !cpu.get_flag(FLAG_H),
            "the low nibble borrowed, so H is gone"
        );

        cpu.set_a(0x21);
        cpu.set_flags(FLAG_H | FLAG_N);
        cpu.daa();
        assert_eq!(cpu.a(), 0x1B);
        assert!(
            cpu.get_flag(FLAG_H),
            "0x1 - 0x6 needed no borrow, so H stays"
        );
    }

    /// `NEG` of `0x80`: the result is `0x80` again, and that one value is what
    /// sets overflow. `C` is set because `0 - 0x80` borrowed; `0` would not be.
    #[test]
    fn neg_0x80_sets_v() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0x44]);
        cpu.set_a(0x80);

        assert_eq!(cpu.step(&mut bus), 8);

        assert_eq!(cpu.a(), 0x80);
        assert_eq!(
            cpu.f(),
            FLAG_S | FLAG_N | FLAG_PV | FLAG_C,
            "S and P/V from the negation, N set, C set, H and Z clear"
        );

        cpu.set_a(0x00);
        assert_eq!(cpu.neg(), 8);
        assert_eq!(cpu.f(), FLAG_Z | FLAG_N, "NEG 0 clears C and sets Z");
        assert_eq!(cpu.a(), 0);
    }

    /// `CPL` complements `A`, forces `H` and `N`, takes `X`/`Y` from the new
    /// `A`, and leaves `S`, `Z` and `P/V` alone.
    #[test]
    fn cpl_complements_a_and_forces_h_n() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x2F]);
        cpu.set_a(0b0100_0101);
        cpu.set_flags(FLAG_Z | FLAG_PV | FLAG_C);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.a(), 0b1011_1010);
        assert_eq!(
            cpu.f(),
            FLAG_Z | FLAG_PV | FLAG_C | FLAG_H | FLAG_N | FLAG_X | FLAG_Y,
            "S/Z/P-V/C survive, H and N are forced, X and Y take A bits 3 and 5"
        );
    }

    /// `SCF`'s `X`/`Y` come from `q ^ F`, not from `F` alone: with `q = 0x80`
    /// cancelling the sign bit of a flags byte that has neither undocumented
    /// bit, and `A = 0`, both stay clear.
    #[test]
    fn scf_xy_uses_q() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x37]);
        cpu.set_a(0x00);
        cpu.set_flags(FLAG_S | FLAG_Z | FLAG_PV | FLAG_H);
        cpu.set_q(0x80);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(
            cpu.f(),
            FLAG_S | FLAG_Z | FLAG_PV | FLAG_C,
            "q ^ F = 0x54 has neither X nor Y, and neither does A"
        );

        cpu.set_q(0x00);
        cpu.set_a(0x28);
        cpu.scf();
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            FLAG_X | FLAG_Y,
            "with q clear the old F still has no undocumented bits, so A supplies both"
        );
    }

    /// `CCF` toggles `C` and moves the old `C` into `H`.
    #[test]
    fn ccf_toggles_c_and_moves_it_into_h() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x3F, 0x3F]);
        cpu.set_a(0x00);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(cpu.f(), FLAG_H, "the old C landed in H, C is clear");

        assert_eq!(cpu.step(&mut bus), 4);
        assert!(cpu.get_flag(FLAG_C), "and back again");
        assert!(!cpu.get_flag(FLAG_H));
    }
}