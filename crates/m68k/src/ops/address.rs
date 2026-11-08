//! Address-computation and stack-frame instructions: `LEA`, `PEA`, `LINK` and
//! `UNLK` (M1-15, #23).
//!
//! Opcodes (M68000UM section 8, "Instruction Formats"):
//!
//! | Instruction | Encoding             | Operation                                          |
//! |---|---|---|
//! | `LEA`  | `0100 ddd 111 mmm rrr` | `<ea>` → `An`                                  |
//! | `PEA`  | `0100 1000 01 mmm rrr` | `<ea>` → stack                                 |
//! | `LINK` | `0100 1110 0101 0nnn` | `An` → stack; `An` = `SP`; `SP` = `SP` + `d16`  |
//! | `UNLK` | `0100 1110 0101 1nnn` | `An` = (`An`); `SP` = `An` + 4                  |
//!
//! None of the four affects a condition code, which is why none of them calls
//! [`M68k::set_logic_flags`]: all 32 260 cases of the four suite files leave
//! `SR` alone.
//!
//! # The address is not the bus address
//!
//! `LEA` and `PEA` are the first instructions whose operand is the address an EA
//! field *computes* rather than what is stored at it, so they go through
//! [`M68k::ea_address`] instead of [`M68k::resolve_ea`]. The difference is the
//! mask: [`crate::ea::Ea::Mem`] is the address that reaches the bus and is
//! therefore truncated to 24 bits, but the value `LEA` stores in a register and
//! `PEA` pushes keeps all 32 — see [`M68k::ea_address`] and #193.
//!
//! # `A7` is an ordinary destination here
//!
//! `LINK` and `UNLK` name `An` and the stack pointer in the same instruction,
//! and for `n = 7` they are the same register. The order of the writes below is
//! what the suite measures, not a stylistic choice:
//!
//! * `LINK A7` reads `An` *after* the predecrement, so it pushes `$7FC` rather
//!   than the old `SSP` (`4e57 [LINK A7, #] 2`, 1005 cases);
//! * `UNLK A7` writes `SP = An + 4` before writing the popped long, so the
//!   popped long wins (`4e5f [UNLINK A7] 5`, 991 cases).

use crate::M68k;
use crate::bus::Bus;
use crate::ea::{Ea, SP_REG, Size, sign_extend_word};
use crate::ops::field;

impl M68k {
    /// `LEA <ea>,An` (`0100 ddd 111 mmm rrr`): the address the operand field
    /// computes, into an address register (M68000UM "LEA — Address Register
    /// Indirect and Program"). Nothing is read or written at that address, and
    /// no flag moves.
    ///
    /// `decode` has already rejected the modes `LEA` cannot encode (`Dn`, `An`,
    /// `(An)+`, `-(An)`, `#imm`), so `ea_address` declining is the same
    /// decoder/handler disagreement [`M68k::resolve_ea`] treats the same way.
    pub(crate) fn op_lea(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let dest = field(opcode, 9);
        let mode = field(opcode, 3);
        let reg = field(opcode, 0);
        let Some(addr) = self.ea_address(bus, mode, reg) else {
            return self.op_illegal(bus);
        };

        // Long into an address register, so the whole computed address lands
        // there: `47d4 [LEA (A4), A3] 1` copies `$ABAF41E5` including its top
        // byte, which the bus would have dropped.
        self.write_ea(bus, Ea::AddrReg(dest), Size::Long, addr);
        self.advance_opcode(bus);
        0
    }

    /// `PEA <ea>` (`0100 1000 01 mmm rrr`): `LEA`'s operand pushed instead of
    /// stored (M68000UM "PEA — Push Effective Address"). The operand is read
    /// before the stack pointer moves, which is only observable through the bus
    /// order — the value is an address, so there is nothing to alias with.
    pub(crate) fn op_pea(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, 3);
        let reg = field(opcode, 0);
        let Some(addr) = self.ea_address(bus, mode, reg) else {
            return self.op_illegal(bus);
        };

        self.push_long(bus, addr);
        self.advance_opcode(bus);
        0
    }

    /// `LINK An,d16` (`0100 1110 0101 0nnn`): build a frame — push `An`, let it
    /// point at what it pushed, then move `SP` over `d16` bytes of locals
    /// (M68000UM "LINK — Link Address Register").
    ///
    /// The three writes are in the manual's order, which for `An == A7` is the
    /// order the suite measures: `A7` is decremented, *then* read, so `LINK A7`
    /// pushes the decremented pointer rather than the pointer it started with.
    /// Doing it the other way round is wrong in 1005 of `LINK.json.gz`'s cases.
    pub(crate) fn op_link(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, 0);
        // The displacement is the instruction's extension word, fetched before
        // anything else happens.
        let disp = sign_extend_word(self.fetch_ext_word(bus));

        let frame = self.a[usize::from(SP_REG)].wrapping_sub(4);
        self.write_ea(bus, Ea::AddrReg(SP_REG), Size::Long, frame);
        // Read after `SP` has moved: for `LINK A7` this is the instruction's
        // whole personality, and it is what the hardware bus cycle shows.
        let value = self.a[usize::from(reg)];
        self.write_l(bus, frame, value);

        self.write_ea(bus, Ea::AddrReg(reg), Size::Long, frame);
        let sp = frame.wrapping_add(disp);
        self.write_ea(bus, Ea::AddrReg(SP_REG), Size::Long, sp);

        self.advance_opcode(bus);
        0
    }

    /// `UNLK An` — `UNLINK` in the manual — (`0100 1110 0101 1nnn`): release the
    /// frame `LINK` built, `An` coming back from the long it pushed (M68000UM
    /// "UNLINK — Unlink Address Register").
    ///
    /// `SP = An + 4` is written before `An = popped`, and for `An == A7` the
    /// second write therefore wins: `A7` ends up holding the long it popped
    /// rather than the address above it (991 cases of `UNLINK.json.gz`).
    pub(crate) fn op_unlk(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, 0);
        let frame = self.a[usize::from(reg)];
        let value = self.read_l(bus, frame);

        self.write_ea(bus, Ea::AddrReg(SP_REG), Size::Long, frame.wrapping_add(4));
        self.write_ea(bus, Ea::AddrReg(reg), Size::Long, value);

        self.advance_opcode(bus);
        0
    }

    /// Push one long: `SP` moves down four first, then the value goes where it
    /// points (M68000UM section 2 "Program Stack": the stack grows down and `SP`
    /// always points at the last item pushed, never at the next free slot).
    ///
    /// Always four — a long — so `ea_step`'s byte quirk for `A7` does not apply.
    /// The masked address reaches the bus but `SP` itself keeps its upper bits:
    /// `write_ea` masks the bus address, not the value stored in `a[7]`, and
    /// `UNLINK.json.gz` ends 8032 of its 8065 cases with an `SSP` above
    /// `$FFFFFF`.
    ///
    /// `pub(crate)` rather than private to this module because `BSR`
    /// ([`super::flow`]) pushes a return address the same way (#24).
    pub(crate) fn push_long(&mut self, bus: &mut impl Bus, value: u32) {
        let sp = self.a[usize::from(SP_REG)].wrapping_sub(4);
        self.write_ea(bus, Ea::AddrReg(SP_REG), Size::Long, sp);
        self.write_l(bus, sp, value);
    }
}

/// Tests for `LEA`, `PEA`, `LINK` and `UNLINK` (M1-15, #23).
#[cfg(test)]
mod tests {
    use crate::M68k;
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::{TestBus, cpu_with_program};

    /// Where [`cpu_with_program`] loads the program, so a test can name the
    /// address `pc` should reach.
    const PROGRAM: u32 = 0x0000_1000;
    /// Initial stack pointer from [`cpu_with_program`].
    const STACK_TOP: u32 = 0x0001_0000;
    /// A data address every memory test can use.
    const MEM: u32 = 0x0000_2000;

    /// Every condition code as one CCR value, for "this instruction leaves the
    /// flags alone" tests.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as the
    /// following words.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    /// `LEA d16(A0),A1` (`$43E8` = `0100 ddd 111 101 000`): the sum of `A0` and
    /// the sign extended displacement, with memory left alone and the
    /// displacement word consumed (M68000UM "LEA").
    #[test]
    fn lea_d16_an() {
        let (mut cpu, mut bus) = run(0x43E8, &[0xFF00, 0x4E71]);
        cpu.a[0] = 0x0000_2400;
        bus.write_long(0x0000_2300, 0xDEAD_BEEF);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[1], 0x0000_2300, "$2400 - $100");
        assert_eq!(
            bus.read_long(0x0000_2300),
            0xDEAD_BEEF,
            "LEA never reads memory"
        );
        assert_eq!(cpu.pc, PROGRAM + 4, "the displacement is part of the word");
    }

    /// The address `LEA` computes is not the address that reaches the bus, so
    /// the top 8 bits survive: `Ea::Mem` masks to 24 bits because the bus has no
    /// more than 24 lines, but `LEA` stores a register. `LEA.json.gz` has 7162
    /// of its 8065 cases ending above the mask (#193), e.g. case
    /// `41fa [LEA (d16, PC), A0] 59` from `pc = $000C00` with displacement
    /// `$FFFFA3E9` ends with `A0 = $FFFFAFEB`.
    #[test]
    fn lea_d16_an_keeps_the_whole_32_bit_sum() {
        let (mut cpu, mut bus) = run(0x43E8, &[0x0010, 0x4E71]);
        cpu.a[0] = 0xFF00_0000;

        cpu.step(&mut bus);

        assert_eq!(
            cpu.a[1], 0xFF00_0010,
            "a bus-masked $0000_0010 is the bug #193 reports"
        );
    }

    /// `LEA d16(PC),A0` (`$41FA`): the base is the address of the displacement
    /// word itself, `pc + 2`, not the instruction address, and the negative
    /// displacement is not masked on the way into `A0`
    /// (M68000UM "Program Counter Displacement Addressing Mode").
    #[test]
    fn lea_d16_pc() {
        let (mut cpu, mut bus) = run(0x41FA, &[0xA3E9, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(
            cpu.a[0], 0xFFFF_B3EB,
            "$1002 - $5C17, the word at $1002 and not the instruction at $1000"
        );
    }

    /// `LEA (xxx).L,A2` (`$45F9` = `0100 ddd 111 111 001`): two extension words
    /// used verbatim, again above the bus mask.
    #[test]
    fn lea_abs_l_keeps_the_upper_bits() {
        let (mut cpu, mut bus) = run(0x45F9, &[0xFFFF, 0x1234, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[2], 0xFFFF_1234);
        assert_eq!(cpu.pc, PROGRAM + 6, "opcode and both extension words");
    }

    /// `LEA (A0),A1` (`$43D0`) with every flag preset and a zero address: no
    /// condition code moves, so `Z` stays set even though the result is zero
    /// for once and `X` survives (M68000UM "LEA — Condition Codes: not
    /// affected").
    #[test]
    fn lea_leaves_the_flags_alone() {
        let (mut cpu, mut bus) = run(0x43D0, &[0x4E71]);
        cpu.a[0] = 0;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[1], 0);
        assert!(
            cpu.flag_x() && cpu.flag_n() && cpu.flag_z() && cpu.flag_v() && cpu.flag_c(),
            "LEA affects no flag"
        );
    }

    /// `LEA (A0),A7` (`$4FD0`): writing the stack pointer keeps its stored
    /// mirror in step, exactly like `MOVEA` to `A7` (see `ea::M68k::set_an`).
    #[test]
    fn lea_to_a7_moves_the_stored_stack_pointer() {
        let (mut cpu, mut bus) = run(0x4FD0, &[0x4E71]);
        cpu.a[0] = MEM;

        cpu.step(&mut bus);

        assert_eq!(cpu.a[7], MEM);
        assert_eq!(cpu.ssp, MEM, "the supervisor stack pointer moved with it");
    }

    /// `PEA (A0)` (`$4850` = `0100 1000 01 010 000`): the address itself is
    /// pushed, not its contents, and `A7` moves down by four (M68000UM "PEA").
    #[test]
    fn pea_pushes_the_address() {
        let (mut cpu, mut bus) = run(0x4850, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.write_long(MEM, 0xDEAD_BEEF);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[7], STACK_TOP - 4);
        assert_eq!(
            bus.read_long(STACK_TOP - 4),
            MEM,
            "the address, not $DEADBEEF"
        );
        assert_eq!(cpu.ssp, STACK_TOP - 4, "the stored stack pointer moved");
        assert_eq!(bus.read_long(MEM), 0xDEAD_BEEF, "PEA never reads memory");
    }

    /// `PEA d16(PC)` (`$487A`) with a negative displacement: the pushed long is
    /// the whole 32-bit address, so the value on the stack is `$FFFF9002` and
    /// not the bus-masked `$00FF9002` (7153 of `PEA.json.gz`'s 8065 cases sit
    /// above the mask).
    #[test]
    fn pea_pushes_the_unmasked_address() {
        let (mut cpu, mut bus) = run(0x487A, &[0x8000, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(STACK_TOP - 4), 0xFFFF_9002);
    }

    /// `PEA` with every flag preset: it neither reads the operand nor computes a
    /// result, so no flag moves (M68000UM "PEA — Condition Codes: not
    /// affected").
    #[test]
    fn pea_leaves_the_flags_alone() {
        let (mut cpu, mut bus) = run(0x4850, &[0x4E71]);
        cpu.a[0] = 0;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert!(
            cpu.flag_x() && cpu.flag_n() && cpu.flag_z() && cpu.flag_v() && cpu.flag_c(),
            "PEA affects no flag"
        );
    }

    /// `LINK A6,#-8` then `UNLK A6` (`$4E56`,`$FFF8` then `$4E5E`): the frame is
    /// `[old A6]` at the new `A6`, `A6` holds the address it was pushed at, and
    /// `UNLK` puts both back (M68000UM "LINK"/"UNLINK").
    #[test]
    fn link_then_unlk_round_trips() {
        let (mut cpu, mut bus) = run(0x4E56, &[0xFFF8, 0x4E5E, 0x4E71]);
        cpu.a[6] = 0xBEEF;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_long(STACK_TOP - 4),
            0xBEEF,
            "old A6 went on the stack"
        );
        assert_eq!(cpu.a[6], STACK_TOP - 4, "A6 is the frame address");
        assert_eq!(cpu.a[7], STACK_TOP - 4 - 8, "then SP moves over the frame");

        cpu.step(&mut bus);

        assert_eq!(cpu.a[6], 0xBEEF, "the saved pointer came back");
        assert_eq!(cpu.a[7], STACK_TOP, "and the stack was released");
        assert!(
            cpu.flag_x() && cpu.flag_n() && cpu.flag_z() && cpu.flag_v() && cpu.flag_c(),
            "neither instruction affects a flag"
        );
    }

    /// `LINK A7,#-8` (`$4E57`): `A7` is the destination *and* the stack pointer,
    /// and the long written to the stack is the value `A7` had *after* the
    /// predecrement — `$7FC`-style, not the old stack pointer.
    /// `LINK.json.gz` case `4e57 [LINK A7, #] 2` starts at `SSP = $800` and
    /// writes `$000007FC` to `$7FC` (1005 of 8065 cases are `LINK A7`).
    #[test]
    fn link_a7_pushes_the_moved_stack_pointer() {
        let (mut cpu, mut bus) = run(0x4E57, &[0xFFF8, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_long(STACK_TOP - 4),
            STACK_TOP - 4,
            "the pointer after the decrement, not $10000"
        );
        assert_eq!(cpu.a[7], STACK_TOP - 4 - 8);
        assert_eq!(cpu.ssp, STACK_TOP - 4 - 8);
    }

    /// `UNLK A7` (`$4E5F`): the destination is the stack pointer too, and the
    /// long that is popped wins over the `An + 4` that released the frame —
    /// `A7` ends holding the popped value itself. `UNLINK.json.gz`: 991 cases
    /// are `UNLINK A7`, and every one of them leaves `A7` at the popped long
    /// (`a7 = val`), not at `val + 4` and not at the old `SSP + 4`.
    #[test]
    fn unlk_a7_leaves_the_popped_long_in_the_stack_pointer() {
        let (mut cpu, mut bus) = run(0x4E5F, &[0x4E71]);
        cpu.a[7] = STACK_TOP - 4;
        bus.write_long(STACK_TOP - 4, 0x0000_ABCD);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[7], 0x0000_ABCD);
        assert_eq!(cpu.ssp, 0x0000_ABCD, "the stored mirror follows A7");
    }

    /// `UNLK A0` (`$4E58`) on a frame pointer with bits above the bus mask: the
    /// read is a bus access, so it goes to `$002000`, while `SP = An + 4` is
    /// register arithmetic and keeps them (see the `UNLINK.json.gz` case
    /// `4e58 [UNLINK A0] 1`, whose frame pointer is `$3BF09D06`, read at bus
    /// address `$F09D06`, and whose final `SSP` is `$3BF09D0A`).
    #[test]
    fn unlk_reads_through_the_mask_and_adds_without_it() {
        let (mut cpu, mut bus) = run(0x4E58, &[0x4E71]);
        cpu.a[0] = 0x0100_2000;
        bus.write_long(MEM, 0xDEAD_BEEF);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0xDEAD_BEEF);
        assert_eq!(cpu.a[7], 0x0100_2004, "SP keeps the bits the bus dropped");
    }
}
