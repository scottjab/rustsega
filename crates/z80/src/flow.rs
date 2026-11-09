//! Jumps, calls, returns and restarts (M2-10, #53).
//!
//! ```text
//! 0xC3  JP   nn        10      0xCD  CALL nn        17
//! 0xC2/CA/D2/DA/E2/EA/F2/FA    0xC4/CC/D4/DC/E4/EC/F4/FC
//!         JP   cc,nn   10      CALL cc,nn  17 / 10
//! 0xE9  JP   (HL)      4       0xC9  RET          10
//! 0xDD/FD E9  JP (IX)  8       0xC0/..  RET cc   11 / 5
//! 0x18  JR   e         12      0xED 45/4D  RETN / RETI  14
//! 0x20/28/30/38  JR cc,e 12/7  0xC7/CF/..  RST p  11
//! 0x10  DJNZ e         13 / 8
//! ```
//!
//! None of them moves a flag, and every one leaves `q`, `p` and the EI latch
//! clear - all 1000 cases of every file in `crates/z80/tests/flow.rs` agree, so
//! it is done in one place ([`Z80::clear_latches`]).
//!
//! Two pieces of undocumented behaviour, both checked against all 1000 cases of
//! each file and both following the rule "the load that never happened still
//! happened" (see `w` in the "Z80 Cycles" page of *Z80 Undocumented
//! Features*, z80-docs.z80info.org):
//!
//! * `wz`: `JP`/`CALL` take `nn` whether or not they branch, `RET` takes the
//!   return address it popped, `JR`/`DJNZ` take the target but only when the
//!   branch happens, and `JP (HL)`/`JP (IX)` leave `wz` alone entirely.
//! * The not-taken timings: `JP cc` still fetches both operand bytes (10 T),
//!   `CALL cc` fetches them without pushing (10 T), `JR cc` and `DJNZ` fetch the
//!   displacement and do not jump (7 T and 8 T), and `RET cc` does not read the
//!   stack at all (5 T).
//!
//! [`Z80::clear_latches`]: crate::Z80::clear_latches

use crate::decode::{IndexMode, y};
use crate::{FLAG_C, FLAG_PV, FLAG_S, FLAG_Z, Z80, Z80Bus};

/// `JP nn`, and `JP cc,nn` in both directions: the two operand bytes are part
/// of the opcode fetch either way.
const T_JP: u32 = 10;
/// `JP (HL)` - a register hop and nothing else.
const T_JP_HL: u32 = 4;
/// `JP (IX)` / `JP (IY)`: `JP (HL)` plus the two prefix M1 cycles.
const T_JP_INDEX: u32 = T_JP_HL + 4;
/// `JR e`, and `JR cc,e` taken.
const T_JR: u32 = 12;
/// `JR cc,e` not taken: `T_JR` minus the branch cycle, the operand read still paid for.
const T_JR_NOT_TAKEN: u32 = 7;
/// The `B` decrement `DJNZ` adds to both sides of a `JR`.
const T_DJNZ: u32 = 1;
/// `CALL nn`, and `CALL cc,nn` taken.
const T_CALL: u32 = 17;
/// `CALL cc,nn` not taken: the operand read of a `JP cc` with no push.
const T_CALL_NOT_TAKEN: u32 = 10;
/// `RET`.
const T_RET: u32 = 10;
/// `RET cc` taken: `RET` plus the condition test.
const T_RET_CC: u32 = 11;
/// `RET cc` not taken: no stack read at all, the cheapest return there is.
const T_RET_CC_NOT_TAKEN: u32 = 5;
/// `RETN` and `RETI`.
const T_RETI: u32 = 14;
/// `RST p`.
const T_RST: u32 = 11;

impl Z80 {
    /// Evaluate condition code `cc` - the `y` field of a conditional opcode -
    /// against `F`.
    ///
    /// The Z80 decodes the eight conditions in this order (`y = 0` is NZ, then
    /// Z, NC, C, PO, PE, P, M) for `JP`, `CALL` and `RET` alike; `JR` uses the
    /// same field with only its low two bits, which is why it has four
    /// conditional forms rather than eight.
    fn condition(&self, cc: u8) -> bool {
        match cc {
            0 => !self.get_flag(FLAG_Z),
            1 => self.get_flag(FLAG_Z),
            2 => !self.get_flag(FLAG_C),
            3 => self.get_flag(FLAG_C),
            4 => !self.get_flag(FLAG_PV),
            5 => self.get_flag(FLAG_PV),
            6 => !self.get_flag(FLAG_S),
            _ => self.get_flag(FLAG_S),
        }
    }

    /// `JP nn` (`C3`): 10 T-states, `wz = nn`.
    pub(crate) fn jp_nn(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let target = self.fetch_word(bus);
        self.set_pc(target);
        self.set_wz(target);
        self.clear_latches();
        T_JP
    }

    /// `JP cc,nn` (`C2`/`CA`/`D2`/`DA`/`E2`/`EA`/`F2`/`FA`): 10 T-states and
    /// `wz = nn` whether or not the jump happens, because both operand bytes are
    /// fetched before the condition is looked at.
    pub(crate) fn jp_cc_nn(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let target = self.fetch_word(bus);
        self.set_wz(target);
        if self.condition(y(op)) {
            self.set_pc(target);
        }
        self.clear_latches();
        T_JP
    }

    /// `JP (HL)` (`E9`) and `JP (IX)`/`JP (IY)` (`DD`/`FD E9`).
    ///
    /// The prefixed form costs the two prefix M1 cycles as well (`e9` is 4
    /// T-states, `dd e9` 8), and neither moves `wz`: there is no memory access
    /// for it to come near.
    pub(crate) fn jp_index(&mut self, mode: IndexMode) -> u32 {
        let target = self.index(mode);
        self.set_pc(target);
        self.clear_latches();
        if mode == IndexMode::Hl {
            T_JP_HL
        } else {
            T_JP_INDEX
        }
    }

    /// `JR e` (`18`): 12 T-states, `wz = target`.
    pub(crate) fn jr_e(&mut self, bus: &mut impl Z80Bus) -> u32 {
        self.branch_relative(bus, true)
    }

    /// `JR cc,e` (`20`/`28`/`30`/`38`): 12 T-states taken, 7 not taken.
    ///
    /// `JR` numbers its conditions with only two of the three `y` bits - `20`
    /// is NZ, `28` Z, `30` NC, `38` C, and the `y` values 4-7 do not exist here
    /// - hence the mask, which `JP`/`CALL`/`RET` do not need.
    pub(crate) fn jr_cc(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let taken = self.condition((op >> 3) & 3);
        self.branch_relative(bus, taken)
    }

    /// `DJNZ e` (`10`): `B` is decremented first, then the branch - which costs
    /// one T-state more than the same `JR` at either end (13 taken, 8 not).
    pub(crate) fn djnz(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let b = self.b().wrapping_sub(1);
        self.set_b(b);
        T_DJNZ + self.branch_relative(bus, b != 0)
    }

    /// The relative branch under `JR`, `JR cc` and `DJNZ`: fetch the signed
    /// displacement and jump to `PC + e`.
    ///
    /// `fetch_byte` has already moved `PC` past the operand, which is what `e`
    /// is relative to (UM0080, "Relative Conditional Jump": "the content of
    /// PC incremented by 2 plus the displacement byte").
    fn branch_relative(&mut self, bus: &mut impl Z80Bus, taken: bool) -> u32 {
        let disp = self.fetch_byte(bus);
        self.clear_latches();
        if !taken {
            return T_JR_NOT_TAKEN;
        }
        let target = self.pc().wrapping_add(i16::from(disp as i8) as u16);
        self.set_pc(target);
        self.set_wz(target);
        T_JR
    }

    /// `CALL nn` (`CD`): 17 T-states, `wz = nn`.
    pub(crate) fn call_nn(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let target = self.fetch_word(bus);
        self.set_wz(target);
        let return_addr = self.pc();
        self.push16(bus, return_addr);
        self.set_pc(target);
        self.clear_latches();
        T_CALL
    }

    /// `CALL cc,nn` (`C4`/`CC`/`D4`/`DC`/`E4`/`EC`/`F4`/`FC`): 17 T-states and
    /// `wz = nn` when it happens, 10 T-states when it does not - the operands
    /// are fetched either way, only the push is skipped.
    pub(crate) fn call_cc_nn(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let target = self.fetch_word(bus);
        self.set_wz(target);
        if !self.condition(y(op)) {
            self.clear_latches();
            return T_CALL_NOT_TAKEN;
        }
        let return_addr = self.pc();
        self.push16(bus, return_addr);
        self.set_pc(target);
        self.clear_latches();
        T_CALL
    }

    /// `RET` (`C9`): 10 T-states, `wz` ending as the return address it read.
    pub(crate) fn ret(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let return_addr = self.pop16(bus);
        self.set_pc(return_addr);
        self.set_wz(return_addr);
        self.clear_latches();
        T_RET
    }

    /// `RET cc` (`C0`/`C8`/`D0`/`D8`/`E0`/`E8`/`F0`/`F8`): 11 T-states taken,
    /// 5 not taken - a not-taken `RET cc` does not read the stack at all, and
    /// so leaves `wz` alone.
    pub(crate) fn ret_cc(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        if !self.condition(y(op)) {
            self.clear_latches();
            return T_RET_CC_NOT_TAKEN;
        }
        let return_addr = self.pop16(bus);
        self.set_pc(return_addr);
        self.set_wz(return_addr);
        self.clear_latches();
        T_RET_CC
    }

    /// `RETN` (`ED 45`, and the `ED 55`/`5D`/`65`/`6D`/`75`/`7D` mirrors): a
    /// `RET` that also reverts `IFF1` to `IFF2`.
    pub(crate) fn retn(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let iff2 = self.iff2();
        self.set_iff1(iff2);
        let return_addr = self.pop16(bus);
        self.set_pc(return_addr);
        self.set_wz(return_addr);
        self.clear_latches();
        T_RETI
    }

    /// `RETI` (`ED 4D`).
    ///
    /// The documented difference from `RETN` is that it ends an interrupt
    /// service routine; the hardware gives the two the same visible state here,
    /// so `reti` is `retn` and the interrupt acknowledgement that would tell
    /// them apart is M2-14, not this ticket.
    pub(crate) fn reti(&mut self, bus: &mut impl Z80Bus) -> u32 {
        self.retn(bus)
    }

    /// `RST p` (`C7`/`CF`/`D7`/`DF`/`E7`/`EF`/`F7`/`FF`): 11 T-states, a
    /// one-byte call to the vector `y * 8` with the return address below it and
    /// `wz` holding the vector.
    pub(crate) fn rst(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let target = u16::from(y(op) << 3);
        let return_addr = self.pc();
        self.push16(bus, return_addr);
        self.set_pc(target);
        self.set_wz(target);
        self.clear_latches();
        T_RST
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::RamBus;
    use crate::{FLAG_C, FLAG_PV, FLAG_S, FLAG_Z, Z80};

    /// Sentinel `wz`: no target, return address or operand in these tests is
    /// ever `0x2345`, so a test can tell "this instruction loaded `wz`" from
    /// "this instruction left `wz` alone".
    const WZ0: u16 = 0x2345;

    /// A CPU with `program` at `pc`, every flag clear and `wz` preloaded.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        cpu.set_wz(WZ0);
        (cpu, RamBus::at(pc, program))
    }

    /// The same, with `SP` parked where a stack write is visible.
    fn cpu_with_stack(pc: u16, program: &[u8], stack: u16) -> (Z80, RamBus) {
        let (mut cpu, bus) = cpu_at(pc, program);
        cpu.set_sp(stack);
        (cpu, bus)
    }

    /// The condition field of a conditional opcode, as the flag bit it tests
    /// and whether the branch fires when that bit is *set*.
    ///
    /// `y(op)` selects the conditions in the order the Z80 decodes them - NZ, Z,
    /// NC, C, PO, PE, P, M (Z80 CPU Family User's Manual UM0080, section 2.2,
    /// "Conditional jump / call / return") - so one table covers `JP cc`,
    /// `CALL cc`, `RET cc` and, in its first four entries, `JR cc`.
    fn condition(y: u8) -> (u8, bool) {
        match y {
            0 => (FLAG_Z, false),
            1 => (FLAG_Z, true),
            2 => (FLAG_C, false),
            3 => (FLAG_C, true),
            4 => (FLAG_PV, false),
            5 => (FLAG_PV, true),
            6 => (FLAG_S, false),
            _ => (FLAG_S, true),
        }
    }

    /// Drive one conditional opcode with both states of its condition bit and
    /// report whether the branch was taken.
    ///
    /// `taken` and `not_taken` are the expected T-state counts, which is how the
    /// condition table above and the timings of the whole group are pinned at
    /// once; every caller also checks that the other condition value takes the
    /// other branch.
    /// `cc` is the condition number to drive, passed in rather than read off
    /// `op` because `JR` reaches it with two bits where the other three
    /// instructions use three.
    fn run_conditional(
        op: u8,
        cc: u8,
        program: &[u8],
        taken: u32,
        not_taken: u32,
        mut case: impl FnMut(bool, &mut Z80, &mut RamBus),
    ) {
        let (flag, taken_when_set) = condition(cc);
        for set in [true, false] {
            let (mut cpu, mut bus) = cpu_with_stack(0x0100, program, 0x8000);
            cpu.set_flags(if set { flag } else { 0 });
            bus = bus.with_byte(0x8000, 0x34).with_byte(0x8001, 0x12);
            let is_taken = set == taken_when_set;
            let cost = if is_taken { taken } else { not_taken };
            assert_eq!(
                cpu.step(&mut bus),
                cost,
                "{op:02X} with {} {}",
                if set { "set" } else { "clear" },
                if is_taken { "taken" } else { "not taken" }
            );
            case(is_taken, &mut cpu, &mut bus);
        }
    }

    /// The eight condition codes, checked through `JP cc,nn`, `CALL cc,nn`,
    /// `RET cc` and `JR cc,e`: the same `y` picks the condition in all four.
    #[test]
    fn condition_codes_are_y() {
        for y in 0..8u8 {
            // `JP cc,nn`: 10 T-states either way, so only PC tells the two
            // paths apart.
            run_conditional(
                0xC2 + 8 * y,
                y,
                &[0xC2 + 8 * y, 0x00, 0x40],
                10,
                10,
                |taken, cpu, _| {
                    assert_eq!(cpu.pc(), if taken { 0x4000 } else { 0x0103 });
                },
            );
            // `CALL cc,nn`: 17 taken, 10 not.
            run_conditional(
                0xC4 + 8 * y,
                y,
                &[0xC4 + 8 * y, 0x00, 0x40],
                17,
                10,
                |taken, cpu, bus| {
                    assert_eq!(cpu.pc(), if taken { 0x4000 } else { 0x0103 });
                    assert_eq!(cpu.sp(), if taken { 0x7FFE } else { 0x8000 });
                    assert_eq!(bus.writes.len(), if taken { 2 } else { 0 });
                },
            );
            // `RET cc`: 11 taken, 5 not.
            run_conditional(0xC0 + 8 * y, y, &[0xC0 + 8 * y], 11, 5, |taken, cpu, _| {
                assert_eq!(cpu.pc(), if taken { 0x1234 } else { 0x0101 });
                assert_eq!(cpu.sp(), if taken { 0x8002 } else { 0x8000 });
            });
            // `JR cc,e` only reaches the first four conditions: bit 3 of `y`
            // would be a fifth and sixth condition the hardware does not have.
            if y < 4 {
                run_conditional(
                    0x20 + 8 * y,
                    y,
                    &[0x20 + 8 * y, 0x10],
                    12,
                    7,
                    |taken, cpu, _| {
                        assert_eq!(cpu.pc(), if taken { 0x0112 } else { 0x0102 });
                    },
                );
            }
        }
    }

    /// `JR e` (`18`) with a negative displacement: `e` is signed, and it is
    /// added to the address *after* the instruction, not to the opcode.
    #[test]
    fn jr_negative() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x18, 0xFA]);

        assert_eq!(cpu.step(&mut bus), 12, "JR e is 12 T-states");

        assert_eq!(cpu.pc(), 0x00FC, "PC + 2 - 6, not PC - 6");
        assert_eq!(cpu.wz(), 0x00FC, "wz follows the jump");
        assert_eq!(cpu.r(), 1);
        assert!(bus.writes.is_empty(), "JR writes nothing");
        assert_eq!(cpu.f(), 0, "JR leaves the flags alone");

        // Forward is the same instruction: `0x7F` is +127.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x18, 0x7F]);
        assert_eq!(cpu.step(&mut bus), 12);
        assert_eq!(cpu.pc(), 0x0181, "0x0102 + 127");

        // And a target that wraps the address space still lands where the
        // arithmetic says.
        let (mut cpu, mut bus) = cpu_at(0xFFFE, &[0x18, 0x01]);
        assert_eq!(cpu.step(&mut bus), 12);
        assert_eq!(cpu.pc(), 0x0001, "0x10000 + 1 is 0x0001");
        assert_eq!(cpu.wz(), 0x0001);
    }

    /// `DJNZ e` (`10`) is a `JR` with a decrement in front of it: `B` counts
    /// down every pass, one pass per non-zero result, and no flag moves.
    #[test]
    fn djnz_loop_counts() {
        // `10 FE` jumps back onto itself: the body of a counted loop.
        let (mut cpu, mut bus) = cpu_with_stack(0x0100, &[0x10, 0xFE], 0x8000);
        cpu.set_b(3);
        cpu.set_flags(FLAG_Z | FLAG_C);

        for expect_b in [2, 1] {
            assert_eq!(cpu.step(&mut bus), 13, "DJNZ taken is 13 T-states");
            assert_eq!(cpu.b(), expect_b);
            assert_eq!(cpu.pc(), 0x0100, "still in the loop");
            assert_eq!(cpu.wz(), 0x0100, "wz follows the taken jump");
        }

        assert_eq!(
            cpu.step(&mut bus),
            8,
            "the last pass pays for the decrement but not for the jump"
        );
        assert_eq!(cpu.b(), 0);
        assert_eq!(cpu.pc(), 0x0102, "loop exited");
        assert_eq!(
            cpu.wz(),
            0x0100,
            "a not-taken DJNZ leaves wz where the last taken one left it"
        );
        assert_eq!(cpu.f(), FLAG_Z | FLAG_C, "DJNZ moves no flag");
    }

    /// `CALL nn` then `RET`: the return address goes on the stack the way
    /// `PUSH` does (high byte first) and comes back to the byte after the call.
    #[test]
    fn call_ret_roundtrip() {
        let (mut cpu, mut bus) = cpu_with_stack(0x0100, &[0xCD, 0x00, 0x40], 0x8002);
        bus = bus.with_byte(0x4000, 0xC9);

        assert_eq!(cpu.step(&mut bus), 17, "CALL nn is 17 T-states");
        assert_eq!(cpu.pc(), 0x4000);
        assert_eq!(cpu.wz(), 0x4000, "wz is the call target");
        assert_eq!(cpu.sp(), 0x8000);
        assert_eq!(
            bus.writes,
            vec![(0x8001, 0x01), (0x8000, 0x03)],
            "return address PC + 3, high byte first like PUSH"
        );

        // The subroutine body is one `RET`, cheaper than the call that reached
        // it, and `wz` ends holding the address it read back off the stack.
        assert_eq!(cpu.step(&mut bus), 10, "RET is 10 T-states");
        assert_eq!(cpu.pc(), 0x0103, "back from the subroutine");
        assert_eq!(cpu.sp(), 0x8002, "and the stack is empty again");
        assert_eq!(cpu.wz(), 0x0103, "wz is the return address");
        assert_eq!(cpu.f(), 0, "neither instruction touches the flags");
    }

    /// `RET cc` that does not fire must not read the stack at all, and 5
    /// T-states is all it costs.
    #[test]
    fn ret_cc_not_taken_5t() {
        let (mut cpu, mut bus) = cpu_with_stack(0x0100, &[0xC0, 0x00], 0x8000);
        cpu.set_flags(FLAG_Z);
        bus = bus.with_byte(0x8000, 0x34).with_byte(0x8001, 0x12);

        assert_eq!(cpu.step(&mut bus), 5, "RET cc not taken is 5 T-states");

        assert_eq!(cpu.pc(), 0x0101, "execution carries straight on");
        assert_eq!(cpu.sp(), 0x8000, "the stack was not read");
        assert_eq!(
            cpu.wz(),
            WZ0,
            "neither does wz, unlike a not-taken JP cc / CALL cc"
        );
        assert_eq!(bus.reads, vec![0x0100], "the opcode fetch and nothing else");
        assert_eq!(cpu.f(), FLAG_Z);
    }

    /// `RST` (`FF` is `RST 0x38`): a one-byte call into the restart vectors,
    /// one vector per `y`, at `y * 8`.
    #[test]
    fn rst_38() {
        let (mut cpu, mut bus) = cpu_with_stack(0x0200, &[0xFF], 0x8000);

        assert_eq!(cpu.step(&mut bus), 11, "RST is 11 T-states");

        assert_eq!(cpu.pc(), 0x0038, "FF is y = 7, so 7 * 8");
        assert_eq!(cpu.wz(), 0x0038, "wz is the vector, not the return address");
        assert_eq!(cpu.sp(), 0x7FFE);
        assert_eq!(
            bus.writes,
            vec![(0x7FFF, 0x02), (0x7FFE, 0x01)],
            "PC + 1 pushed high byte first"
        );
        assert_eq!(cpu.f(), 0, "RST leaves the flags alone");

        for y in 0..8u8 {
            let op = 0xC7 + 8 * y;
            let (mut cpu, mut bus) = cpu_with_stack(0x0200, &[op], 0x8000);
            assert_eq!(cpu.step(&mut bus), 11, "{op:02X} is RST");
            assert_eq!(cpu.pc(), u16::from(y * 8), "{op:02X} vector");
            assert_eq!(cpu.wz(), u16::from(y * 8));
        }
    }

    /// `RETN` (`ED 45`) and `RETI` (`ED 4D`) both copy `IFF2` into `IFF1`;
    /// neither touches `IFF2`, `IM` or the flags.
    #[test]
    fn retn_copies_iff2() {
        for op in [0x45u8, 0x4D] {
            for iff2 in [false, true] {
                let (mut cpu, mut bus) = cpu_with_stack(0x0100, &[0xED, op], 0x8000);
                cpu.set_iff1(false);
                cpu.set_iff2(iff2);
                bus = bus.with_byte(0x8000, 0x34).with_byte(0x8001, 0x12);

                assert_eq!(cpu.step(&mut bus), 14, "ED {op:02X} is 14 T-states");

                assert_eq!(cpu.iff1(), iff2, "IFF1 is restored from IFF2");
                assert_eq!(cpu.iff2(), iff2, "IFF2 is not consumed");
                assert_eq!(cpu.pc(), 0x1234);
                assert_eq!(cpu.sp(), 0x8002);
                assert_eq!(cpu.wz(), 0x1234, "wz is the return address");
                assert_eq!(cpu.f(), 0);
            }
        }

        // The undocumented RETN mirrors behave the same way again.
        for op in [0x55u8, 0x5D, 0x65, 0x6D, 0x75, 0x7D] {
            let (mut cpu, mut bus) = cpu_with_stack(0x0100, &[0xED, op], 0x8000);
            cpu.set_iff1(false);
            cpu.set_iff2(true);
            bus = bus.with_byte(0x8000, 0x34).with_byte(0x8001, 0x12);

            assert_eq!(cpu.step(&mut bus), 14, "ED {op:02X} mirrors ED 45");
            assert!(cpu.iff1(), "ED {op:02X} copies IFF2 like RETN");
            assert_eq!(cpu.pc(), 0x1234);
        }
    }

    /// `JP cc,nn` costs 10 T-states and loads `nn` into `wz` even when the
    /// condition is false: both operand bytes are fetched before the condition
    /// is looked at (`c2`/`ca`/.../`fa`, 1000 cases each).
    #[test]
    fn jp_cc_not_taken_is_10t_and_loads_wz() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xC3, 0x00, 0x40]);

        assert_eq!(cpu.step(&mut bus), 10, "JP nn is 10 T-states");
        assert_eq!(cpu.pc(), 0x4000);
        assert_eq!(cpu.wz(), 0x4000);

        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCA, 0x00, 0x40]);
        cpu.set_flags(0);

        assert_eq!(cpu.step(&mut bus), 10, "JP Z with Z clear is still 10");
        assert_eq!(cpu.pc(), 0x0103, "but it does not jump");
        assert_eq!(cpu.wz(), 0x4000, "and it still leaves nn in wz");
    }

    /// `JP (HL)` (`E9`) and `JP (IX)` / `JP (IY)` (`DD`/`FD E9`): no operands,
    /// and `wz` is *not* the jump target.
    #[test]
    fn jp_indirect_leaves_wz() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xE9]);
        cpu.set_hl(0x9ABC);

        assert_eq!(cpu.step(&mut bus), 4, "JP (HL) is 4 T-states");
        assert_eq!(cpu.pc(), 0x9ABC);
        assert_eq!(cpu.wz(), WZ0, "JP (HL) leaves wz where it found it");

        for prefix in [0xDDu8, 0xFD] {
            let (mut cpu, mut bus) = cpu_at(0x0100, &[prefix, 0xE9]);
            cpu.set_ix(0x1234);
            cpu.set_iy(0x5678);

            assert_eq!(cpu.step(&mut bus), 8, "{prefix:02X} E9 is 8 T-states");
            assert_eq!(cpu.pc(), if prefix == 0xDD { 0x1234 } else { 0x5678 });
            assert_eq!(cpu.wz(), WZ0, "JP (IX) leaves wz alone");
            assert_eq!(cpu.r(), 2, "two M1 fetches");
        }
    }

    /// `JR cc` not taken is 7 T-states - the displacement byte was already
    /// fetched, so `PC` still moves on by two - and `wz` stays put.
    #[test]
    fn jr_cc_not_taken_is_7t() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x20, 0x10]);
        // `20` is `JR NZ`, so it is the *set* Z flag that stops it.
        cpu.set_flags(FLAG_Z);

        assert_eq!(cpu.step(&mut bus), 7, "JR cc not taken");
        assert_eq!(cpu.pc(), 0x0102, "the displacement byte was consumed");
        assert_eq!(cpu.wz(), WZ0, "wz only moves when the jump happens");

        let (mut cpu, mut bus) = cpu_at(0x0100, &[0x20, 0x10]);
        assert_eq!(cpu.step(&mut bus), 12, "JR NZ with Z clear");
        assert_eq!(cpu.pc(), 0x0112);
        assert_eq!(cpu.wz(), 0x0112);
    }

    /// `CALL cc,nn` not taken: 10 T-states, no push, but `nn` still ends up in
    /// `wz` because the operands are fetched either way.
    #[test]
    fn call_cc_not_taken_is_10t() {
        let (mut cpu, mut bus) = cpu_with_stack(0x0100, &[0xC4, 0x00, 0x40], 0x8000);
        cpu.set_flags(FLAG_Z);

        assert_eq!(cpu.step(&mut bus), 10, "CALL cc not taken");
        assert_eq!(cpu.pc(), 0x0103);
        assert_eq!(cpu.sp(), 0x8000, "nothing was pushed");
        assert!(bus.writes.is_empty());
        assert_eq!(cpu.wz(), 0x4000);
    }

    /// None of this group computes flags, so every one of them ends in
    /// [`Z80::clear_latches`]: `q` 0, `p` false, the EI latch clear.
    #[test]
    fn flow_instructions_clear_the_latches() {
        let programs: [&[u8]; 10] = [
            &[0xC3, 0x00, 0x40],
            &[0xCA, 0x00, 0x40],
            &[0xE9],
            &[0x18, 0x01],
            &[0x10, 0x01],
            &[0xCD, 0x00, 0x40],
            &[0xCC, 0x00, 0x40],
            &[0xC9],
            &[0xF8],
            &[0xFF],
        ];
        for program in programs {
            let (mut cpu, mut bus) = cpu_with_stack(0x0100, program, 0x8000);
            cpu.set_p(true);
            cpu.set_ei_pending(true);
            cpu.set_q(0xC5);
            bus = bus.with_byte(0x8000, 0x00).with_byte(0x8001, 0x00);
            cpu.step(&mut bus);
            assert_eq!(cpu.q(), 0, "{program:?} cleared q");
            assert!(!cpu.p(), "{program:?} cleared p");
            assert!(!cpu.ei_pending(), "{program:?} cleared the EI latch");
        }
    }
}
