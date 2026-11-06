//! The 8-bit ALU and the 8-bit `INC`/`DEC` group (M2-06, #49).
//!
//! Eight operations against the accumulator - `ADD`, `ADC`, `SUB`, `SBC`,
//! `AND`, `XOR`, `OR`, `CP` - in three operand shapes (register or `(HL)`,
//! immediate, `(IX+d)`), plus `INC`/`DEC` on the same three shapes.
//!
//! The ALU operation is `y` of the opcode in every shape, which is why one
//! [`Z80::alu8`] serves all twenty-four encodings: `80`-`BF`,
//! `C6 CE D6 DE E6 EE F6 FE` and `DD/FD` with `z = 6` all put the operation in
//! the same three bits, in the order the hardware wires the ALU - ADD, ADC,
//! SUB, SBC, AND, XOR, OR, CP (Z80 CPU Family User's Manual UM0080, section
//! 2.2, "Instruction Set").
//!
//! Two undocumented behaviours live here, both from "The Undocumented Z80
//! Documented" (Sean Young):
//!
//! - `X`/`Y` (flag bits 3 and 5) are copies of result bits 3 and 5, except for
//!   `CP`, whose result is thrown away so the bits come from the operand
//!   instead. `CP` is the only ALU op with that exception.
//! - The hidden `q` latch, which holds the flag byte whenever an instruction
//!   recomputed the flags. See [`Z80::latch_flags`].
//!
//! `wz` (MEMPTR) is untouched by the unprefixed forms - `86`, `34`, `fe` and
//! friends are 0-of-1000 `wz` changes across their suite files - and is set to
//! `IX+d` by the indexed ones, which [`Z80::index_address`] already does.

use crate::decode::{y, z};
use crate::{FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_X, FLAG_Y, IndexMode, SZXY_PARITY, Z80, Z80Bus};

/// ALU operation index, which is `y` of the opcode (see the module header for
/// why the order is the hardware's).
const ALU_ADD: u8 = 0;
const ALU_ADC: u8 = 1;
const ALU_SUB: u8 = 2;
const ALU_SBC: u8 = 3;
const ALU_AND: u8 = 4;
const ALU_XOR: u8 = 5;
const ALU_OR: u8 = 6;
const ALU_CP: u8 = 7;

/// Every flag bit is established by an ALU op, so none of the old flags
/// survive into the new `F`.
const ALL_FLAGS: u8 = 0xFF;

/// The bits `INC`/`DEC` establish: everything except `C`, which they leave
/// exactly as it was (UM0080, the `INC`/`DEC` flag notes).
const FLAGS_KEEP_C: u8 = !FLAG_C;

/// `ADD`/`ADC`: the sum, and the flag byte with all eight bits established.
///
/// `H` is the carry out of bit 3, which shows up as bit 4 of the result
/// disagreeing with bit 4 of both operands - the same XOR trick as the full
/// carry, one bit lower. `P/V` is signed overflow: both operands pushing the
/// result to the other side of the sign bit.
///
/// These helpers are `const fn` so the compiler can fold the flag maths, which
/// also means widening with `as` rather than the `u16::from` used everywhere
/// else: `From` is not yet stable as a const trait.
const fn add_flags(a: u8, b: u8, carry_in: u8) -> (u8, u8) {
    let sum = a as u32 + b as u32 + carry_in as u32;
    let result = sum as u8;
    let mut flags = SZXY_PARITY[result as usize] & !FLAG_PV;
    if (a ^ b ^ result) & 0x10 != 0 {
        flags |= FLAG_H;
    }
    if (a ^ result) & (b ^ result) & 0x80 != 0 {
        flags |= FLAG_PV;
    }
    if sum > 0xFF {
        flags |= FLAG_C;
    }
    // `N` stays clear, which is the only bit `SZXY_PARITY` does not cover.
    (result, flags)
}

/// `SUB`/`SBC`/`CP`: the difference, and the flag byte with all eight bits
/// established.
///
/// `H` is the borrow out of bit 4 and `P/V` the signed overflow, both from the
/// same XOR tricks as [`add_flags`] with the operand pair that can disagree
/// about the sign. `C` is the borrow out of bit 7, so it is set whenever the
/// arithmetic difference goes negative.
const fn sub_flags(a: u8, b: u8, borrow_in: u8) -> (u8, u8) {
    let difference = a as i32 - b as i32 - borrow_in as i32;
    let result = difference as u8;
    let mut flags = (SZXY_PARITY[result as usize] & !FLAG_PV) | FLAG_N;
    if (a ^ b ^ result) & 0x10 != 0 {
        flags |= FLAG_H;
    }
    if (a ^ b) & (a ^ result) & 0x80 != 0 {
        flags |= FLAG_PV;
    }
    if difference < 0 {
        flags |= FLAG_C;
    }
    (result, flags)
}

/// `AND`: S/Z/X/Y and `P/V` from the result, `H` set, `N` and `C` clear.
///
/// `H` set unconditionally is what tells `DAA` that the last operation was an
/// `AND` (UM0080 flag table); every other logic op clears it.
const fn and_flags(result: u8) -> (u8, u8) {
    (result, SZXY_PARITY[result as usize] | FLAG_H)
}

/// `XOR`/`OR`: S/Z/X/Y and `P/V` from the result, `H`, `N` and `C` clear.
const fn orxor_flags(result: u8) -> (u8, u8) {
    (result, SZXY_PARITY[result as usize])
}

/// `INC`: the incremented value and the flags it establishes.
///
/// `H` is the carry out of bit 3 of a `+1`, so it is set exactly when the low
/// nibble was `0xF`; `P/V` is the signed overflow, which for `+1` can only be
/// `0x7F` -> `0x80`. `C` is not in the returned byte because `INC` does not
/// establish it.
const fn inc_flags(value: u8) -> (u8, u8) {
    let result = value.wrapping_add(1);
    let mut flags = SZXY_PARITY[result as usize] & !FLAG_PV;
    if value & 0x0F == 0x0F {
        flags |= FLAG_H;
    }
    if value == 0x7F {
        flags |= FLAG_PV;
    }
    (result, flags)
}

/// `DEC`: the decremented value and the flags it establishes.
///
/// `H` is the borrow out of bit 4, set exactly when the low nibble was `0`,
/// and `P/V` the signed overflow, which for `-1` can only be `0x80` -> `0x7F`.
const fn dec_flags(value: u8) -> (u8, u8) {
    let result = value.wrapping_sub(1);
    let mut flags = (SZXY_PARITY[result as usize] & !FLAG_PV) | FLAG_N;
    if value & 0x0F == 0 {
        flags |= FLAG_H;
    }
    if value == 0x80 {
        flags |= FLAG_PV;
    }
    (result, flags)
}

impl Z80 {
    /// The 8-bit ALU proper: `op` is the operation index (`y` of the opcode)
    /// and `rhs` the operand, already fetched by whichever encoding got here.
    ///
    /// `CP` is the odd one out: it computes the flags of `SUB`, leaves `A`
    /// alone, and takes `X`/`Y` from the operand - the result it discards is
    /// what the other ops copy those two bits from. Returning `A` unchanged
    /// from the `CP` arm lets every op share the one `set_a` below.
    fn alu8(&mut self, op: u8, rhs: u8) {
        let a = self.a();
        let carry = u8::from(self.get_flag(FLAG_C));
        let (result, flags) = match op {
            ALU_ADD => add_flags(a, rhs, 0),
            ALU_ADC => add_flags(a, rhs, carry),
            ALU_SUB => sub_flags(a, rhs, 0),
            ALU_SBC => sub_flags(a, rhs, carry),
            ALU_AND => and_flags(a & rhs),
            ALU_XOR => orxor_flags(a ^ rhs),
            ALU_OR => orxor_flags(a | rhs),
            ALU_CP => {
                let (_, mut flags) = sub_flags(a, rhs, 0);
                flags = (flags & !(FLAG_X | FLAG_Y)) | (rhs & (FLAG_X | FLAG_Y));
                (a, flags)
            }
            _ => unreachable!("the ALU operation field is always 0..=7, got {op}"),
        };
        self.set_a(result);
        self.update_flags(ALL_FLAGS, flags);
        self.latch_flags();
    }

    /// `ALU A,r` / `ALU A,(HL)` (`80`-`BF`): 4 T-states, 7 once the operand is
    /// the memory at `HL`. `wz` does not move.
    pub(crate) fn alu_a_r(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let src = z(op);
        let value = if src == 6 {
            bus.read(self.hl())
        } else {
            self.reg_slot(src)
        };
        self.alu8(y(op), value);
        if src == 6 { 7 } else { 4 }
    }

    /// `ALU A,n` (`C6 CE D6 DE E6 EE F6 FE`): 7 T-states.
    pub(crate) fn alu_a_n(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let value = self.fetch_byte(bus);
        self.alu8(y(op), value);
        7
    }

    /// `ALU A,(IX+d)` / `ALU A,(IY+d)`: 19 T-states, `wz = IX + d`.
    ///
    /// Only `z = 6` encodings exist here (`86`, `8E`, ... `BE`): the other `z`
    /// values in this block are the undocumented `IXH`/`IXL` forms, which are
    /// M2-15 (#58).
    pub(crate) fn alu_a_index(&mut self, bus: &mut impl Z80Bus, mode: IndexMode, op: u8) -> u32 {
        let addr = self.index_address(bus, mode);
        let value = bus.read(addr);
        self.alu8(y(op), value);
        19
    }

    /// `INC r` (`04 0C 14 1C 24 2C 3C`): 4 T-states.
    ///
    /// Named `_reg`, not `inc_r`: `Z80::inc_r` is the `R` refresh counter bump
    /// that every opcode fetch does, and it is part of the public API.
    pub(crate) fn inc_reg(&mut self, op: u8) -> u32 {
        let slot = y(op);
        let (result, flags) = inc_flags(self.reg_slot(slot));
        self.set_reg_slot(slot, result);
        self.update_flags(FLAGS_KEEP_C, flags);
        self.latch_flags();
        4
    }

    /// `DEC r` (`05 0D 15 1D 25 2D 3D`): 4 T-states. See [`Z80::inc_reg`] for
    /// why these two are not called `inc_r`/`dec_r`.
    pub(crate) fn dec_reg(&mut self, op: u8) -> u32 {
        let slot = y(op);
        let (result, flags) = dec_flags(self.reg_slot(slot));
        self.set_reg_slot(slot, result);
        self.update_flags(FLAGS_KEEP_C, flags);
        self.latch_flags();
        4
    }

    /// `INC (HL)` (`34`): 11 T-states, and `wz` stays where it was.
    pub(crate) fn inc_hl_ind(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let addr = self.hl();
        let (result, flags) = inc_flags(bus.read(addr));
        bus.write(addr, result);
        self.update_flags(FLAGS_KEEP_C, flags);
        self.latch_flags();
        11
    }

    /// `DEC (HL)` (`35`): 11 T-states.
    pub(crate) fn dec_hl_ind(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let addr = self.hl();
        let (result, flags) = dec_flags(bus.read(addr));
        bus.write(addr, result);
        self.update_flags(FLAGS_KEEP_C, flags);
        self.latch_flags();
        11
    }

    /// `INC (IX+d)` (`DD 34 d`): 23 T-states, `wz = IX + d`.
    pub(crate) fn inc_index_ind(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let addr = self.index_address(bus, mode);
        let (result, flags) = inc_flags(bus.read(addr));
        bus.write(addr, result);
        self.update_flags(FLAGS_KEEP_C, flags);
        self.latch_flags();
        23
    }

    /// `DEC (IX+d)` (`DD 35 d`): 23 T-states, `wz = IX + d`.
    pub(crate) fn dec_index_ind(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let addr = self.index_address(bus, mode);
        let (result, flags) = dec_flags(bus.read(addr));
        bus.write(addr, result);
        self.update_flags(FLAGS_KEEP_C, flags);
        self.latch_flags();
        23
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FLAG_S;
    use crate::FLAG_Z;
    use crate::testutil::RamBus;

    /// A CPU with `program` sitting at its `PC`, so the step under test fetches
    /// the bytes the test names. Flags start clear: `Z80::new()` brings `F` up
    /// at `0xFF`, and this group's flags are what the test is about.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        (cpu, RamBus::at(pc, program))
    }

    /// `ADD A,1` on `0x7F`: the textbook signed overflow, where `H` and `V`
    /// both set while `C` stays clear.
    #[test]
    fn add_overflow_0x7f_plus_1() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xC6, 0x01]);
        cpu.set_a(0x7F);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x80);
        assert!(cpu.get_flag(FLAG_S), "bit 7 of the result is set");
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_H), "0x0F + 0x01 carries out of bit 3");
        assert!(cpu.get_flag(FLAG_PV), "0x7F + 1 overflows a signed byte");
        assert!(!cpu.get_flag(FLAG_C), "no carry out of bit 7");
        assert!(!cpu.get_flag(FLAG_N), "ADD clears N");
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            0,
            "0x80 has bits 5 and 3 clear"
        );
        assert_eq!(
            cpu.q(),
            cpu.f(),
            "q follows the flag byte an ALU op computes"
        );
    }

    /// `ADD A,2` on `0xFF`: carry out, result wrapped, and no signed overflow
    /// because `-1 + 2` is a perfectly good signed byte.
    #[test]
    fn add_carry_out_sets_c_only() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xC6, 0x02]);
        cpu.set_a(0xFF);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x01);
        assert!(cpu.get_flag(FLAG_C));
        assert!(
            cpu.get_flag(FLAG_H),
            "0x0F + 0x02 still carries out of bit 3"
        );
        assert!(!cpu.get_flag(FLAG_PV));
        assert!(!cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_Z));
    }

    /// `ADC A,0` with `C` set: the carry in is part of the sum, and it is what
    /// tips `0x7F` into overflow.
    #[test]
    fn adc_includes_carry_in() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCE, 0x00]);
        cpu.set_a(0x7F);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x80, "0x7F + 0 + 1");
        assert!(cpu.get_flag(FLAG_PV));
        assert!(cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_C), "0x80 does not carry out of bit 7");
    }

    /// `SUB 1` from `0`: borrow out of bit 7 and a borrow from bit 4 as well,
    /// with `N` set to mark it as a subtraction.
    #[test]
    fn sub_borrow() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xD6, 0x01]);
        cpu.set_a(0x00);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0xFF);
        assert!(cpu.get_flag(FLAG_C), "0 - 1 borrows out of bit 7");
        assert!(cpu.get_flag(FLAG_H), "0 - 1 borrows from bit 4 too");
        assert!(cpu.get_flag(FLAG_N));
        assert!(cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(
            !cpu.get_flag(FLAG_PV),
            "0 - 1 does not overflow a signed byte"
        );
        assert_eq!(cpu.f() & (FLAG_X | FLAG_Y), FLAG_X | FLAG_Y, "from 0xFF");
    }

    /// `SBC 0xFF` from `0` with `C` set: `0 - 255 - 1` wraps exactly to `0`, so
    /// `Z` and `C` are both set while `V` is not.
    #[test]
    fn sbc_wraps_to_zero_with_borrow() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xDE, 0xFF]);
        cpu.set_a(0x00);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x00);
        assert!(cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_PV), "0 - (-1) - 1 = 0 in signed terms");
    }

    /// `CP` leaves `A` alone and copies `X`/`Y` from the operand, because the
    /// result it computed is discarded.
    ///
    /// Both halves are needed: with `CP 0x28` the operand and the discarded
    /// result (`0xD8`) happen to agree on bits 5 and 3, so only the second case
    /// proves the bits are not coming from the result.
    #[test]
    fn cp_xy_from_operand() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xFE, 0x28]);
        cpu.set_a(0x00);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x00, "CP must not touch A");
        assert_eq!(cpu.f() & (FLAG_X | FLAG_Y), FLAG_X | FLAG_Y, "from 0x28");
        assert!(
            cpu.get_flag(FLAG_S),
            "the discarded 0x00-0x28 = 0xD8 is negative"
        );
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_N));

        // 0x00 - 0x10 = 0xF0, whose bits 5 and 3 are both set while the
        // operand 0x10 has neither: only the operand may set X and Y.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xFE, 0x10]);
        cpu.set_a(0x00);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x00);
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            0,
            "X/Y come from 0x10, not 0xF0"
        );
        assert!(cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_H));
        assert!(cpu.get_flag(FLAG_C));
    }

    /// `AND` is the only logic op that sets `H`, and `P/V` is parity of the
    /// result rather than overflow.
    #[test]
    fn and_sets_h() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xE6, 0x0F]);
        cpu.set_a(0xF0);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x00);
        assert!(cpu.get_flag(FLAG_H), "AND always leaves H set");
        assert!(cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_PV), "zero has even parity");
        assert!(!cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_C));
        assert!(!cpu.get_flag(FLAG_S));

        // A non-zero result still sets H, and X/Y follow the result.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xE6, 0x0F]);
        cpu.set_a(0xFF);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x0F);
        assert!(cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_X));
        assert!(!cpu.get_flag(FLAG_Y));
        assert!(cpu.get_flag(FLAG_PV), "0x0F has an even number of bits set");
    }

    /// `XOR` and `OR` clear `H` (which is how `DAA` can tell them from `AND`)
    /// and take `P/V` from parity.
    #[test]
    fn xor_and_or_clear_h_and_c() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xEE, 0x0F]);
        cpu.set_a(0x0F);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x00);
        assert!(cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_PV));

        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xF6, 0xF0]);
        cpu.set_a(0x0F);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0xFF);
        assert!(cpu.get_flag(FLAG_S));
        assert_eq!(cpu.f() & (FLAG_X | FLAG_Y), FLAG_X | FLAG_Y);
        assert!(!cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_PV), "0xFF has an even number of bits set");
    }

    /// `INC` on `0x7F`: `H` and `V` together, `C` untouched, and `N` clear.
    #[test]
    fn inc_7f_sets_v() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x04]);
        cpu.set_b(0x7F);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.b(), 0x80);
        assert!(cpu.get_flag(FLAG_PV), "0x7F + 1 is a signed overflow");
        assert!(cpu.get_flag(FLAG_H), "the low nibble carried");
        assert!(cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_N), "INC always clears N");
        assert!(cpu.get_flag(FLAG_C), "INC must not touch C");
        assert_eq!(cpu.f() & (FLAG_X | FLAG_Y), 0, "X/Y follow the result 0x80");
    }

    /// `DEC` on `0x10`: the low nibble was zero, so a borrow into it sets `H`.
    #[test]
    fn dec_10_sets_h() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x0D]);
        cpu.set_c(0x10);
        cpu.set_flags(FLAG_C);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.c(), 0x0F);
        assert!(cpu.get_flag(FLAG_H), "0x10 - 1 borrows from bit 4");
        assert!(cpu.get_flag(FLAG_N), "DEC always sets N");
        assert!(!cpu.get_flag(FLAG_PV));
        assert!(!cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_C), "DEC must not touch C");
        assert!(cpu.get_flag(FLAG_X), "X follows the result 0x0F");
        assert!(!cpu.get_flag(FLAG_Y));
    }

    /// The two `INC`/`DEC` boundaries that are easy to get off by one: `0x00`
    /// wrapping up, and `0x80` wrapping down into positive territory.
    #[test]
    fn inc_dec_wrap_boundaries() {
        // 0x00 -> 0x01: no half carry, no overflow, Z clear.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x0C]);
        cpu.set_c(0x00);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.c(), 0x01);
        assert!(!cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_PV));
        assert!(!cpu.get_flag(FLAG_Z));

        // 0x80 -> 0x7F: overflow, and H because the low nibble was zero.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x15]);
        cpu.set_d(0x80);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!(cpu.d(), 0x7F);
        assert!(cpu.get_flag(FLAG_PV), "0x80 - 1 is a signed overflow");
        assert!(cpu.get_flag(FLAG_H));
        assert!(cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_S));
    }

    /// `INC`/`DEC (HL)`: 11 T-states each, a read-modify-write in place, and
    /// `wz` does not move.
    #[test]
    fn inc_dec_hl_ind_read_modify_write() {
        const PC: u16 = 0x0200;
        let (mut cpu, mut bus) = cpu_at(PC, &[0x34, 0x35]);
        cpu.set_hl(0x8123);
        cpu.set_wz(0x1234);
        bus = bus.with_byte(0x8123, 0x7F);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(bus.peek(0x8123), 0x80);
        assert_eq!(bus.writes, vec![(0x8123, 0x80)]);
        assert!(cpu.get_flag(FLAG_PV));
        assert_eq!(cpu.wz(), 0x1234, "INC (HL) must not disturb wz");

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(bus.peek(0x8123), 0x7F);
        assert!(cpu.get_flag(FLAG_PV));
        assert!(cpu.get_flag(FLAG_H));
        assert_eq!(bus.reads, vec![PC, 0x8123, PC + 1, 0x8123]);
    }

    /// `ADD A,(HL)` (`86`) reads the operand from memory and costs 7 T-states
    /// instead of 4.
    #[test]
    fn alu_hl_operand_reads_and_costs_7() {
        const PC: u16 = 0x0300;
        let (mut cpu, mut bus) = cpu_at(PC, &[0x86]);
        cpu.set_a(0x7F);
        cpu.set_hl(0x9000);
        bus = bus.with_byte(0x9000, 0x01);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x80);
        assert!(cpu.get_flag(FLAG_PV));
        assert_eq!(bus.reads, vec![PC, 0x9000]);
        assert!(bus.writes.is_empty());
    }

    /// The indexed operand: `DD 86 d` is `ADD A,(IX+d)` at 19 T-states, with
    /// `wz` left holding the address that was used.
    #[test]
    fn alu_index_operand_costs_19_and_sets_wz() {
        const PC: u16 = 0x0400;
        let (mut cpu, mut bus) = cpu_at(PC, &[0xDD, 0x86, 0x02]);
        cpu.set_a(0x10);
        cpu.set_ix(0x9100);
        cpu.set_wz(0xAAAA);
        bus = bus.with_byte(0x9102, 0x05);

        assert_eq!(cpu.step(&mut bus), 19);

        assert_eq!(cpu.a(), 0x15);
        assert_eq!(cpu.wz(), 0x9102, "wz = IX + d");
        assert_eq!(cpu.r(), 2, "the prefix and the opcode are M1 fetches");
        assert_eq!(bus.reads, vec![PC, PC + 1, PC + 2, 0x9102]);
    }

    /// `DD 34 d` / `FD 35 d`: `INC`/`DEC (IX+d)` at 23 T-states, reading and
    /// writing the indexed address, `wz = IX + d`, and `C` preserved.
    #[test]
    fn inc_dec_index_ind_costs_23_and_sets_wz() {
        const PC: u16 = 0x0500;
        let (mut cpu, mut bus) = cpu_at(PC, &[0xDD, 0x34, 0xFE, 0xFD, 0x35, 0x01]);
        cpu.set_ix(0x9100);
        cpu.set_iy(0x9100);
        cpu.set_flags(FLAG_C);
        bus = bus.with_byte(0x90FE, 0x7F);

        assert_eq!(cpu.step(&mut bus), 23);

        // d = 0xFE is -2, so IX + d = 0x90FE.
        assert_eq!(bus.peek(0x90FE), 0x80);
        assert_eq!(cpu.wz(), 0x90FE);
        assert!(cpu.get_flag(FLAG_C), "INC (IX+d) must not touch C");

        let mut bus = bus.with_byte(0x9101, 0x80);
        assert_eq!(cpu.step(&mut bus), 23);

        assert_eq!(bus.peek(0x9101), 0x7F);
        assert_eq!(cpu.wz(), 0x9101);
        assert!(cpu.get_flag(FLAG_PV), "0x80 - 1 overflows");
        assert!(cpu.get_flag(FLAG_C), "DEC (IX+d) must not touch C either");
    }

    /// `q` records the flag byte after anything that recomputes the flags, and
    /// goes back to zero for an instruction that leaves them alone.
    #[test]
    fn q_set_after_alu_cleared_after_ld() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xB0, 0x41, 0x04]);
        cpu.set_a(0x28);
        cpu.set_b(0x34);

        // OR B: flags computed, so q is the flag byte.
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(cpu.a(), 0x3C);
        let flags_after_or = cpu.f();
        assert_eq!(cpu.q(), flags_after_or);
        assert_ne!(cpu.q(), 0);

        // LD B,C: flags untouched, so q is zero even though F did not change.
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(cpu.q(), 0);
        assert_eq!(
            cpu.f(),
            flags_after_or,
            "the flags themselves are unchanged"
        );

        // INC B: flags again, and q carries the unchanged C bit too.
        cpu.set_flags(FLAG_C);
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(cpu.q(), cpu.f());
        assert!(cpu.q() & FLAG_C != 0);
    }

    /// The operation order is the ALU's wiring, so all three operand shapes can
    /// share `y(op)` as the operation index.
    #[test]
    fn alu_operation_order_is_add_adc_sub_sbc_and_xor_or_cp() {
        // A = 0x3C, B = 0x5A, C clear: every operation on the same pair.
        let expected = [0x96u8, 0x96, 0xE2, 0xE2, 0x18, 0x66, 0x7E, 0x3C];
        for (index, op) in [0x80u8, 0x88, 0x90, 0x98, 0xA0, 0xA8, 0xB0, 0xB8]
            .into_iter()
            .enumerate()
        {
            let (mut cpu, mut bus) = cpu_at(0x0100, &[op]);
            cpu.set_a(0x3C);
            cpu.set_b(0x5A);

            assert_eq!(cpu.step(&mut bus), 4, "{op:02X}");

            assert_eq!(
                cpu.a(),
                expected[index],
                "opcode {op:02X} is not the {}th ALU operation",
                index + 1
            );
            assert_eq!(cpu.get_flag(FLAG_N), index == 2 || index == 3 || index == 7);
        }
    }
}
