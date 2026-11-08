//! Tests for `LEA`, `PEA`, `LINK` and `UNLINK` (M1-15, #23).

#[cfg(test)]
mod tests {
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::{TestBus, cpu_with_program};
    use crate::M68k;

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
        assert_eq!(bus.read_long(0x0000_2300), 0xDEAD_BEEF, "LEA never reads");
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
            cpu.a[0],
            0xFFFF_B3EB,
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
    /// condition code moves, so `Z` stays clear-by-luck-free and `X` survives
    /// (M68000UM "LEA — Condition Codes: not affected").
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
        assert_eq!(bus.read_long(STACK_TOP - 4), MEM, "the address, not $DEADBEEF");
        assert_eq!(cpu.ssp, STACK_TOP - 4, "the stored stack pointer moved");
        assert_eq!(bus.read_long(MEM), 0xDEAD_BEEF, "PEA never reads memory");
    }

    /// `PEA d16(PC)` (`$487A`) with a negative displacement: the pushed long is
    /// the whole 32-bit address, so the value on the stack is `$FFFF8002` and
    /// not the bus-masked `$00FF8002` (7153 of `PEA.json.gz`'s 8065 cases sit
    /// above the mask).
    #[test]
    fn pea_pushes_the_unmasked_address() {
        let (mut cpu, mut bus) = run(0x487A, &[0x8000, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(STACK_TOP - 4), 0xFFFF_8002);
    }

    /// `PEA (A0),A1`-style flag silence: `PEA` neither reads the operand nor
    /// computes a result, so no flag moves (M68000UM "PEA — Condition Codes:
    /// not affected").
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

        assert_eq!(bus.read_long(STACK_TOP - 4), 0xBEEF, "old A6 went on the stack");
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
}
