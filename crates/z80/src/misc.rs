//! The misc group: decimal adjust, the flag fiddles, `NEG`, the `I`/`R`
//! transfers, `RRD`/`RLD`, `HALT`, `DI`/`EI` and `IM n` (M2-13, #56).
//!
//! ```text
//! 0x27  DAA    4      0xED 57/5F  LD A,I/R   9
//! 0x2F  CPL    4      0xED 67/6F  RRD/RLD   18
//! 0x37  SCF    4      0x76        HALT       4
//! 0x3F  CCF    4      0xF3/FB     DI/EI      4
//! 0xED 44+    NEG    8      0xED 46/56/5E  IM 0/1/2  8
//! 0xED 47/4F  LD I/R,A          9
//! ```
//!
//! `DAA`, `CPL`, `SCF`, `CCF`, `NEG` and the register transfers read no memory,
//! so each is a flag write plus [`Z80::latch_flags`] - or, for the four that
//! move no flag at all, [`Z80::clear_latches`]. `RRD`/`RLD` are the only members
//! that touch the bus, and `HALT`/`DI`/`EI`/`IM n` are the only members with
//! state beyond the register set.

use crate::alu8::sub_flags;
use crate::rot::ALL_FLAGS;
use crate::{
    FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_X, FLAG_Y, FLAG_Z, SZXY_PARITY, Z80, Z80Bus,
};

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

    /// `LD I,A` (`ED 47`): 9 T-states.
    ///
    /// A plain register-to-interrupt transfer: no flag moves in any of the 1000
    /// cases of `ed 47.json`, and the latches are cleared like every other `ED`
    /// register instruction (Z80UM 2.2 for the timing).
    pub(crate) fn ld_i_a(&mut self) -> u32 {
        let value = self.a();
        self.set_i(value);
        self.clear_latches();
        9
    }

    /// `LD R,A` (`ED 4F`): 9 T-states.
    ///
    /// Like [`Z80::ld_i_a`], except that it writes the whole of `R` including
    /// bit 7 - the bit an opcode fetch never touches but a load happily sets
    /// ("The Undocumented Z80 Documented", Sean Young).
    pub(crate) fn ld_r_a(&mut self) -> u32 {
        let value = self.a();
        self.set_r(value);
        self.clear_latches();
        9
    }

    /// `LD A,I` (`ED 57`) and `LD A,R` (`ED 5F`): 9 T-states.
    ///
    /// The flag half of these two is the interesting part (Z80UM 2.2, `LD A,I`):
    /// `S`/`Z`/`X`/`Y` come from the loaded value, `H` and `N` are cleared, `C`
    /// survives, and `P/V` is not about the byte at all - it is `IFF2`. The
    /// undocumented `p` latch follows `P/V` in the general case, and the suite
    /// is stronger than that: `p` ends set in all 1000 cases of both files,
    /// including the 483 (`ED 57`) and 518 (`ED 5F`) that start with `IFF2`
    /// clear, so it is set here unconditionally rather than mirrored off `P/V`.
    fn ld_a_ir(&mut self, value: u8) -> u32 {
        self.set_a(value);
        let mut flags = SZXY_PARITY[usize::from(value)] & (FLAG_S | FLAG_Z | FLAG_X | FLAG_Y);
        if self.iff2() {
            flags |= FLAG_PV;
        }
        self.update_flags(
            FLAG_S | FLAG_Z | FLAG_X | FLAG_Y | FLAG_PV | FLAG_H | FLAG_N,
            flags,
        );
        self.latch_flags();
        self.set_p(true);
        9
    }

    /// `LD A,I` (`ED 57`).
    pub(crate) fn ld_a_i(&mut self) -> u32 {
        let value = self.i();
        self.ld_a_ir(value)
    }

    /// `LD A,R` (`ED 5F`).
    ///
    /// `R` is read after both opcode fetches have counted, which is what makes
    /// this instruction's value depend on how the fetch loop is written: the
    /// suite's `ed 5f` cases all end with `A` equal to the final `R`.
    pub(crate) fn ld_a_r(&mut self) -> u32 {
        let value = self.r();
        self.ld_a_ir(value)
    }

    /// `RRD` (`ED 67`) and `RLD` (`ED 6F`): 18 T-states.
    ///
    /// A four-nibble rotate through the accumulator: the byte at `HL` and the
    /// low nibble of `A` are the two ends of a 12-bit shift register, and
    /// `left` says which way it turns (Z80UM 2.2, `RRD`/`RLD`). Only the low
    /// nibble of `A` moves, but the flags - and so `X`/`Y` and `P/V` - are all
    /// taken from the whole of the new `A`, `H` and `N` are cleared, `C`
    /// survives, and `wz` ends up at `HL + 1`: the address the write left the
    /// CPU pointing at.
    fn rotate_digit(&mut self, bus: &mut impl Z80Bus, left: bool) -> u32 {
        let address = self.hl();
        let memory = bus.read(address);
        let a = self.a();
        let (value, stored) = if left {
            ((a & 0xF0) | (memory >> 4), (memory << 4) | (a & 0x0F))
        } else {
            ((a & 0xF0) | (memory & 0x0F), (memory >> 4) | (a << 4))
        };
        bus.write(address, stored);
        self.set_a(value);
        let flags = SZXY_PARITY[usize::from(value)];
        self.update_flags(
            FLAG_S | FLAG_Z | FLAG_X | FLAG_Y | FLAG_PV | FLAG_H | FLAG_N,
            flags,
        );
        self.set_wz(address.wrapping_add(1));
        self.latch_flags();
        18
    }

    /// `RRD` (`ED 67`): right rotate - the byte at `HL` moves up into `A`.
    pub(crate) fn rrd(&mut self, bus: &mut impl Z80Bus) -> u32 {
        self.rotate_digit(bus, false)
    }

    /// `RLD` (`ED 6F`): left rotate - `A`'s low nibble moves down into `HL`.
    pub(crate) fn rld(&mut self, bus: &mut impl Z80Bus) -> u32 {
        self.rotate_digit(bus, true)
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

    /// `LD A,I` and `LD A,R`: `P/V` is `IFF2`, not parity of the byte, and the
    /// transfers out of `A` move no flag at all.
    #[test]
    fn ld_a_i_pv_from_iff2() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0x57]);
        cpu.set_i(0x80);
        cpu.set_flags(FLAG_Z | FLAG_C);
        cpu.set_iff2(false);

        assert_eq!(cpu.step(&mut bus), 9);

        assert_eq!(cpu.a(), 0x80);
        assert_eq!(
            cpu.f(),
            FLAG_S | FLAG_C,
            "S and X/Y from the value, Z gone, H and N cleared, C untouched, P/V = IFF2 = 0"
        );

        cpu.set_iff2(true);
        assert_eq!(cpu.ld_a_i(), 9);
        assert_ne!(cpu.f() & FLAG_PV, 0, "P/V now mirrors IFF2");
        assert!(cpu.p(), "and the p latch is set whatever P/V came to");

        // LD A,R reads R as the two opcode fetches left it.
        cpu.set_r(0x12);
        assert_eq!(cpu.ld_a_r(), 9);
        assert_eq!(cpu.a(), cpu.r(), "A is the post-fetch R");

        // LD I,A and LD R,A do not touch the flags.
        cpu.set_a(0x34);
        cpu.set_flags(0xFF);
        let before = cpu.f();
        assert_eq!(cpu.ld_i_a(), 9);
        assert_eq!(cpu.i(), 0x34);
        assert_eq!(cpu.f(), before, "LD I,A moves no flag");
        cpu.set_a(0xA5);
        assert_eq!(cpu.ld_r_a(), 9);
        assert_eq!(cpu.r(), 0xA5, "LD R,A writes bit 7 too");
        assert_eq!(cpu.f(), before, "LD R,A moves no flag either");
    }

    /// `RLD` turns the 12-bit register left: `A`'s low nibble goes down into the
    /// low nibble of `(HL)`, `(HL)`'s high nibble comes up into `A`, and the
    /// other nibble of `A` is untouched. `RRD` turns it back.
    #[test]
    fn rld_rotates_nibbles() {
        let mut bus = RamBus::at(0x0100, &[0xED, 0x6F]).with_byte(0x8000, 0x82);
        let mut cpu = Z80::new();
        cpu.set_pc(0x0100);
        cpu.set_flags(0);
        cpu.set_a(0xF3);
        cpu.set_hl(0x8000);

        assert_eq!(cpu.step(&mut bus), 18);

        assert_eq!(
            cpu.a(),
            0xF8,
            "0x3 from A's low nibble went down, 0x8 came up"
        );
        assert_eq!(bus.writes, vec![(0x8000, 0x23)]);
        assert_eq!(cpu.hl(), 0x8000, "HL itself never moves");
        assert_eq!(cpu.wz(), 0x8001, "wz is the address the write left behind");
        assert_eq!(
            cpu.f(),
            FLAG_S | FLAG_X | FLAG_Y,
            "S/X/Y from the new A, P/V = parity(0xF8) = odd = clear, H and N cleared"
        );

        let mut bus = RamBus::at(0x0100, &[0xED, 0x67]).with_byte(0x8000, 0x39);
        let mut cpu = Z80::new();
        cpu.set_pc(0x0100);
        cpu.set_flags(0);
        cpu.set_a(0xF3);
        cpu.set_hl(0x8000);

        assert_eq!(cpu.step(&mut bus), 18);

        assert_eq!(
            cpu.a(),
            0xF9,
            "the low nibble of (HL) comes up on a right rotate"
        );
        assert_eq!(bus.writes, vec![(0x8000, 0x33)]);
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
