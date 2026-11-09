//! Rotates and shifts: the accumulator rotates `07 0F 17 1F` and the `CB`-prefixed
//! rotate/shift group `CB 00`-`3F` (M2-08, #51).
//!
//! Both spaces pick their operation from the same three opcode bits, in the
//! order the hardware decodes them - RLC, RRC, RL, RR, SLA, SRA, SLL, SRL - so
//! [`y`] of the opcode indexes one [`shift_byte`] table and the accumulator
//! rotates are that table's first four entries (`07` = RLC, `0F` = RRC,
//! `17` = RL, `1F` = RR). What differs is the flag treatment: the accumulator
//! rotates leave `S`, `Z` and `P/V` alone, while the `CB` group takes all of
//! them from the result.
//!
//! `SLL` (`CB 30`-`37`) is not in any Zilog manual. It is `SLA` with bit 0
//! forced to 1, and it is real: the suite ships `cb 30` through `cb 37`, and the
//! hardware they were captured from sets `C` from bit 7 as `SLA` does.

use crate::decode::{y, z};
use crate::{FLAG_C, FLAG_H, FLAG_N, FLAG_X, FLAG_Y, SZXY_PARITY, Z80, Z80Bus};

/// Shift operation index: `y` of the opcode, the operation field of the `CB`
/// group (Z80 CPU Family User's Manual UM0080, section 2.2, the rotate and
/// shift instruction descriptions).
const SHIFT_RLC: u8 = 0;
const SHIFT_RRC: u8 = 1;
const SHIFT_RL: u8 = 2;
const SHIFT_RR: u8 = 3;
const SHIFT_SLA: u8 = 4;
const SHIFT_SRA: u8 = 5;
/// Undocumented `SLL`: `SLA` with bit 0 set instead of cleared.
const SHIFT_SLL: u8 = 6;
const SHIFT_SRL: u8 = 7;

/// The byte a rotate or shift leaves in its target, and the bit it shifts out
/// into `C`.
///
/// The rotate pairs differ only in what fills the end the operand just vacated:
/// a circular rotate (`RLC`/`RRC`) refills it with the bit that left the other
/// end, a carry rotate (`RL`/`RR`) with the old carry. The shifts fill with 0,
/// except `SRA`, which duplicates bit 7 so a negative value stays negative, and
/// the undocumented `SLL`, which duplicates a 1 ("The Undocumented Z80
/// Documented", Sean Young).
fn shift_byte(kind: u8, value: u8, carry_in: u8) -> (u8, u8) {
    // The bit leaving at the top is what `C` gets on every leftward operation;
    // the one leaving at the bottom is what `C` gets on every rightward one.
    let out_left = (value >> 7) & 1;
    let out_right = value & 1;
    match kind {
        SHIFT_RLC => ((value << 1) | out_left, out_left),
        SHIFT_RRC => ((value >> 1) | (out_right << 7), out_right),
        SHIFT_RL => ((value << 1) | carry_in, out_left),
        SHIFT_RR => ((value >> 1) | (carry_in << 7), out_right),
        SHIFT_SLA => (value << 1, out_left),
        SHIFT_SRA => ((value >> 1) | (value & 0x80), out_right),
        SHIFT_SLL => ((value << 1) | 1, out_left),
        SHIFT_SRL => (value >> 1, out_right),
        _ => unreachable!("shift kinds are the three bits of y, got {kind}"),
    }
}

/// The flags the four accumulator rotates establish: `C` takes the bit that
/// wrapped round, `H` and `N` clear, and `X`/`Y` copy bits 5 and 3 of the new
/// `A`.
///
/// `S`, `Z` and `P/V` are missing from this byte on purpose: UM0080's flag notes
/// for `RLCA`, `RRCA`, `RLA` and `RRA` mark them unchanged rather than computed,
/// and the suite agrees - in all 1000 cases of `07`, `0F`, `17` and `1F` the
/// final S/Z/P/V are the initial ones whatever the result was.
fn accum_flags(result: u8, carry_out: u8) -> u8 {
    (result & (FLAG_X | FLAG_Y)) | carry_out
}

/// The bits an accumulator rotate establishes: everything [`accum_flags`] speaks
/// for, and nothing else.
const ACCUM_VALID: u8 = FLAG_C | FLAG_H | FLAG_N | FLAG_X | FLAG_Y;

/// A `CB` shift claims every flag bit, so nothing of the old `F` survives.
const ALL_FLAGS: u8 = 0xFF;

/// The flag byte a `CB` rotate or shift leaves: `S`, `Z`, `P/V` (parity) and
/// `X`/`Y` from the result - which is what [`SZXY_PARITY`] is for - `C` the bit
/// that shifted out, and `H` and `N` clear (UM0080 flag notes for `RLC` and
/// friends: S, Z, P/V set from the result, H and N reset, C set according to the
/// bit shifted out).
fn shift_flags(result: u8, carry_out: u8) -> u8 {
    SZXY_PARITY[usize::from(result)] | carry_out
}

impl Z80 {
    /// `RLCA`, `RRCA`, `RLA`, `RRA` (`07 0F 17 1F`): 4 T-states.
    ///
    /// `y(op)` is the operation in the shift table's own order, so `07`/`0F`
    /// are the circular pair and `17`/`1F` the pair that rotates through `C`.
    pub(crate) fn rotate_accumulator(&mut self, op: u8) -> u32 {
        let carry_in = u8::from(self.get_flag(FLAG_C));
        let (result, carry_out) = shift_byte(y(op), self.a(), carry_in);
        self.set_a(result);
        self.update_flags(ACCUM_VALID, accum_flags(result, carry_out));
        self.latch_flags();
        4
    }

    /// `CB 00`-`3F`: rotate or shift a register (8 T-states), or the byte at
    /// `HL` (15 T-states, read and written back in place).
    ///
    /// `z(op)` is the operand slot and `y(op)` the operation, both in the same
    /// order as the accumulator rotates. Unlike those, this group takes every
    /// flag from the result, and `wz` does not move (`cb 00`-`3f`: 1000 cases
    /// each leave `wz` where they found it).
    pub(crate) fn rot_shift(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let slot = z(op);
        let carry_in = u8::from(self.get_flag(FLAG_C));
        let value = if slot == 6 {
            bus.read(self.hl())
        } else {
            self.reg_slot(slot)
        };
        let (result, carry_out) = shift_byte(y(op), value, carry_in);
        if slot == 6 {
            bus.write(self.hl(), result);
        } else {
            self.set_reg_slot(slot, result);
        }
        self.update_flags(ALL_FLAGS, shift_flags(result, carry_out));
        self.latch_flags();
        // The memory form is a read-modify-write, which is the whole difference
        // in timing between it and the register form.
        if slot == 6 { 15 } else { 8 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::RamBus;
    use crate::{FLAG_PV, FLAG_S, FLAG_Z};

    /// A CPU with `program` sitting at its `PC`, so the step under test fetches
    /// the bytes the test names. Flags start clear: `Z80::new()` brings `F` up
    /// at `0xFF`, and this group's flags are what the test is about.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        (cpu, RamBus::at(pc, program))
    }

    /// `RLCA` (`07`): bit 7 goes to both `C` and bit 0, and only `C`, `H`, `N`
    /// and `X`/`Y` move - `S`, `Z` and `P/V` are left exactly as they were.
    #[test]
    fn rlca_sets_c_from_bit7() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x07]);
        cpu.set_a(0x85);
        // Every flag set, so "unchanged" cannot be mistaken for "cleared".
        cpu.set_flags(FLAG_S | FLAG_Z | FLAG_PV | FLAG_H | FLAG_N);

        assert_eq!(cpu.step(&mut bus), 4, "RLCA is 4 T-states");

        assert_eq!(cpu.a(), 0x0B, "0x85 rotated left");
        assert!(cpu.get_flag(FLAG_C), "bit 7 of 0x85 goes to C");
        assert!(!cpu.get_flag(FLAG_H), "rotates always clear H");
        assert!(!cpu.get_flag(FLAG_N), "rotates always clear N");
        assert!(cpu.get_flag(FLAG_S), "S is not an output of RLCA");
        assert!(cpu.get_flag(FLAG_Z), "Z is not an output of RLCA");
        assert!(cpu.get_flag(FLAG_PV), "P/V is not an output of RLCA");
        assert!(cpu.get_flag(FLAG_X), "X is bit 3 of the new A");
        assert!(!cpu.get_flag(FLAG_Y), "Y is bit 5 of the new A");
        assert_eq!(cpu.q(), cpu.f(), "the flags were recomputed, so q follows");

        // And a value whose bit 7 is clear takes C down with it: the wrap fills
        // bit 0 with 0, and the old carry is gone.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x07]);
        cpu.set_a(0x02);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.a(), 0x04);
        assert!(!cpu.get_flag(FLAG_C));
    }

    /// `RRA` (`1F`): the carry in is the old `C`, not bit 0 of `A`, and the new
    /// `C` is bit 0 of `A` - the two never mix.
    #[test]
    fn rra_uses_old_carry() {
        // C set, A = 0x00: bit 7 of the result can only have come from C. `Z`
        // starts set and `S`/`P/V` start clear while the result 0x80 is negative
        // and odd-parity, so all three prove the op did not touch them.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x1F]);
        cpu.set_a(0x00);
        cpu.set_flags(FLAG_C | FLAG_Z);

        assert_eq!(cpu.step(&mut bus), 4, "RRA is 4 T-states");

        assert_eq!(cpu.a(), 0x80, "C enters at bit 7");
        assert!(!cpu.get_flag(FLAG_C), "bit 0 of 0x00 was clear");
        assert!(!cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_S), "S is untouched, not taken from 0x80");
        assert!(cpu.get_flag(FLAG_Z), "Z is untouched, not cleared by 0x80");
        assert!(
            !cpu.get_flag(FLAG_PV),
            "P/V is untouched, not parity of 0x80"
        );
        assert!(!cpu.get_flag(FLAG_X), "X is bit 3 of the new A, 0x80");
        assert!(!cpu.get_flag(FLAG_Y), "Y is bit 5 of the new A, 0x80");

        // C clear, A = 0x01: bit 0 leaves for C and a zero enters at bit 7.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x1F]);
        cpu.set_a(0x01);
        cpu.set_flags(0);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.a(), 0x00);
        assert!(cpu.get_flag(FLAG_C), "bit 0 of 0x01 goes to C");
    }

    /// `SLL r` (`CB 30` for `B`, `CB 37` for `A`; undocumented): `SLA` with bit 0 set.
    ///
    /// `SLL` is the one shift with no Zilog documentation, so this pins it
    /// against the three things an implementation could plausibly get wrong:
    /// the 1 in bit 0, that `C` still comes from bit 7 (not from bit 0, which is
    /// what `SLA` would have left behind), and that the result flags are the
    /// usual `CB` set rather than the accumulator rotates' "leave S/Z/P/V alone".
    #[test]
    fn sll_sets_bit0() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x30]);
        cpu.set_b(0x81);
        cpu.set_flags(FLAG_S | FLAG_Z | FLAG_PV | FLAG_H | FLAG_N | FLAG_X | FLAG_Y);

        assert_eq!(cpu.step(&mut bus), 8, "SLL r is 8 T-states");

        assert_eq!(cpu.b(), 0x03, "0x81 shifted left, bit 0 forced to 1");
        assert!(cpu.get_flag(FLAG_C), "C still comes from bit 7, as for SLA");
        assert!(!cpu.get_flag(FLAG_S), "S is from the result 0x03");
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_PV), "0x03 has an even number of bits set");
        assert!(!cpu.get_flag(FLAG_H), "CB shifts clear H");
        assert!(!cpu.get_flag(FLAG_N), "CB shifts clear N");
        assert!(!cpu.get_flag(FLAG_X), "X is bit 3 of the result");
        assert!(!cpu.get_flag(FLAG_Y), "Y is bit 5 of the result");
        assert_eq!(cpu.q(), cpu.f());

        // Bit 7 clear: C follows it down, and the 1 in bit 0 is still there.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x37]);
        cpu.set_a(0x00);

        assert_eq!(cpu.step(&mut bus), 8);

        assert_eq!(cpu.a(), 0x01, "not 0x00: SLL is not SLA");
        assert!(!cpu.get_flag(FLAG_C));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_PV), "0x01 has an odd number of bits set");
    }

    /// `SRA r` (`CB 28`): bit 7 is duplicated, so the sign survives the shift.
    #[test]
    fn sra_keeps_sign() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x28]);
        cpu.set_b(0x81);

        assert_eq!(cpu.step(&mut bus), 8);

        assert_eq!(cpu.b(), 0xC0, "bit 7 is copied down as well as shifted out");
        assert!(cpu.get_flag(FLAG_S), "a negative value stays negative");
        assert!(cpu.get_flag(FLAG_C), "bit 0 of 0x81 was set");
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_PV), "0xC0 has an even number of bits set");

        // Shrinking a positive keeps it positive, and 0x01 -> 0x00 is where the
        // result flags say so: Z set, S clear, even parity.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x28]);
        cpu.set_b(0x01);

        assert_eq!(cpu.step(&mut bus), 8);

        assert_eq!(cpu.b(), 0x00);
        assert!(cpu.get_flag(FLAG_Z));
        assert!(
            !cpu.get_flag(FLAG_S),
            "S follows the result, not the old bit 7"
        );
        assert!(cpu.get_flag(FLAG_PV), "zero has even parity");
        assert!(cpu.get_flag(FLAG_C));
    }

    /// `RLC (HL)` (`CB 06`): 15 T-states, a read-modify-write at `HL`, and the
    /// same flags as the register form.
    #[test]
    fn rlc_hl_writes_memory() {
        const PC: u16 = 0x0200;
        let (mut cpu, mut bus) = cpu_at(PC, &[0xCB, 0x06]);
        cpu.set_hl(0x9100);
        cpu.set_wz(0x1234);
        bus = bus.with_byte(0x9100, 0x85);

        assert_eq!(cpu.step(&mut bus), 15, "CB on (HL) is 15 T-states");

        assert_eq!(bus.peek(0x9100), 0x0B);
        assert_eq!(bus.writes, vec![(0x9100, 0x0B)]);
        assert!(cpu.get_flag(FLAG_C), "bit 7 of the byte shifted out to C");
        assert!(cpu.get_flag(FLAG_X), "X is bit 3 of the stored result");
        assert!(!cpu.get_flag(FLAG_Y));
        assert!(!cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_N));
        assert_eq!(cpu.hl(), 0x9100, "HL itself is not the destination");
        assert_eq!(cpu.wz(), 0x1234, "CB shifts must not disturb wz");
        assert_eq!(cpu.b(), 0, "z = 6 must not touch B");
        assert_eq!(cpu.q(), cpu.f());
        assert_eq!(bus.reads, vec![PC, PC + 1, 0x9100]);
    }
}
