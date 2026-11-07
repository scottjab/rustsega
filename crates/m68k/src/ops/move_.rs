//! MOVE, MOVEA and MOVEQ — the move class (M1-09, #17).
//!
//! The module is named `move_` because `move` is a Rust keyword; the
//! instructions themselves are the ordinary ones, not `MOVEP`/`MOVEM` (which
//! are #18/#19 and out of #17's scope).
//!
//! # MOVE format
//!
//! `00ss RRR MMM mmm rrr` (M68000UM, "Instruction Formats", Format III):
//!
//! ```text
//!  15 14 13 12 11 10  9  8  7  6  5  4  3  2  1  0
//! +--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+
//! | 0  0 | size  | dest reg  | dest mode | src EA  |
//! +--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+--+
//! ```
//!
//! Two things make MOVE unlike every other instruction with an EA field:
//!
//! - The **destination fields are swapped**: destination *register* is bits
//!   11..9 and destination *mode* is bits 8..6, whereas a source EA field is
//!   the usual mode-in-bits-5..3, reg-in-bits-2..0. Reading them the other way
//!   round is the classic MOVE bug.
//! - The **size field is not [`Size::from_bits`]**: `01` is byte, `11` word and
//!   `10` long; `00` is not a MOVE at all (it is the `MOVE to CCR/SR` group,
//!   which is group `$0000` and not this instruction).
//!
//! Destination mode `001` is not MOVE but **MOVEA** — its own instruction,
//! word and long only, no flags touched — so the byte group with that mode
//! (`MOVE.B D0,A0`-shaped opcodes) is an illegal instruction, as is a byte MOVE
//! whose *source* is an An: both would be a byte access to an address register,
//! which [`crate::ea`] rejects for every instruction.
//!
//! # Order of operations
//!
//! The source EA is resolved **and read in full** before the destination EA is
//! resolved, so `MOVE.L (A0)+,(A0)+` reads from the old `A0` and increments,
//! and only then takes the incremented `A0` as its destination and increments
//! again. Resolving both EAs first would write one long later than the chip
//! does and leave `A0` incremented twice before the read.
//!
//! The real chip's prefetch quirk — for a `-(An)` long destination and some
//! memory-to-memory forms the final prefetch happens *before* the destination
//! write — is deliberately not modelled here: it changes the bus-transaction
//! trace, not the architectural state that `Check::State*` compares, and
//! #37/#39 are the tickets that care about the trace.
//!
//! # Flags
//!
//! MOVE and MOVEQ set N and Z from the result and clear V and C, leaving X
//! alone; MOVEA changes no flags at all (M68000PM section 8, "MOVE", "MOVEA",
//! "MOVEQ"). [`M68k::set_logic_flags`] is the shared implementation.

#[cfg(test)]
mod tests {
    use crate::M68k;
    use crate::bus::Bus;
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::cpu_with_program;

    /// Where [`cpu_with_program`] loads the program, so a test can name the
    /// address `pc` should reach.
    const PROGRAM: u32 = 0x0000_1000;

    /// Every condition code as one CCR value, for "this instruction must leave
    /// the flags alone" tests.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as
    /// the following words, and hand back the CPU and bus.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, crate::testbus::TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    /// `MOVEQ #-1,D0` (`$70FF`): the 8-bit data is sign extended to the full
    /// 32 bits, N comes from the result and Z is clear (M68000PM section 8,
    /// "MOVEQ").
    #[test]
    fn moveq_sign_extends() {
        let (mut cpu, mut bus) = run(0x70FF, &[0x4E71]);
        cpu.d[0] = 0xAAAA_AAAA;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xFFFF_FFFF, "bit 7 of the data sign extends");
        assert!(cpu.flag_n(), "N from bit 31 of the result");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v(), "MOVEQ clears V");
        assert!(!cpu.flag_c(), "MOVEQ clears C");
        assert_eq!(cpu.pc, PROGRAM + 2, "MOVEQ is one word");
        assert_eq!(cpu.d[1..], [0; 7][..], "only the addressed Dn moves");
    }

    /// `MOVEQ #0,D0` (`$7000`): Z, and N clear even though the sign-extended
    /// result is all zeros.
    #[test]
    fn moveq_zero_sets_z() {
        let (mut cpu, mut bus) = run(0x7000, &[0x4E71]);
        cpu.d[0] = 0xFFFF_FFFF;
        cpu.set_ccr(SR_N);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0);
        assert!(cpu.flag_z());
        assert!(!cpu.flag_n());
    }

    /// `MOVEQ` selects its destination from bits 11..9, not from the low bits:
    /// `$7EFF` is `MOVEQ #-1,D7`.
    #[test]
    fn moveq_targets_the_register_in_bits_11_9() {
        let (mut cpu, mut bus) = run(0x7EFF, &[0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[7], 0xFFFF_FFFF);
        assert_eq!(cpu.d[0], 0, "D0 is not the destination");
    }

    /// `MOVE.W D1,D0` (`$3001`) writes only the low word of D0 and keeps the
    /// upper word (M68000UM, section 3 "Data Registers").
    #[test]
    fn move_w_d1_d0_keeps_upper() {
        let (mut cpu, mut bus) = run(0x3001, &[0x4E71]);
        cpu.d[0] = 0xAAAA_0000;
        cpu.d[1] = 0x1234;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xAAAA_1234);
        assert_eq!(cpu.d[1], 0x1234, "the source is not disturbed");
    }

    /// MOVE clears V and C and leaves X alone, while N and Z come from the
    /// result (M68000PM section 8, "MOVE").
    #[test]
    fn move_clears_v_c_keeps_x() {
        let (mut cpu, mut bus) = run(0x3001, &[0x4E71]);
        cpu.d[1] = 0;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert!(cpu.flag_x(), "X is unchanged by MOVE");
        assert!(!cpu.flag_v(), "V is cleared");
        assert!(!cpu.flag_c(), "C is cleared");
        assert!(cpu.flag_z(), "Z from a zero result");
        assert!(!cpu.flag_n(), "N from a non-negative result");
    }

    /// A byte result is zero for Z/N purposes even when the register's upper
    /// bits are not: Z and N are computed at the operand width.
    #[test]
    fn move_b_flags_come_from_the_byte_not_the_long() {
        // `MOVE.B $2000.W,D0` with `$2000` = $00 and the byte above it $FF.
        let (mut cpu, mut bus) = run(0x1238, &[0x2001, 0x4E71]);
        cpu.d[1] = 0xDEAD_BEEF;
        bus.write_word(0x2000, 0xFF00);
        cpu.set_ccr(SR_N);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[1], 0xDEAD_0000, "byte write keeps the upper bits");
        assert!(cpu.flag_z(), "the byte result is zero, so Z is set");
        assert!(!cpu.flag_n(), "bit 7 of the byte is clear, so N is clear");
    }

    /// `MOVE.B D0,$2000.W` (`$1038` + the absolute word): a byte goes to the
    /// absolute address and nothing else does (M68000PM section 8, "MOVE").
    #[test]
    fn move_b_to_memory_abs_w() {
        let (mut cpu, mut bus) = run(0x1038, &[0x2000, 0x4E71]);
        cpu.d[0] = 0xDEAD_BEEF;

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(0x2000), 0xEF, "the low byte of D0");
        assert_eq!(bus.read_byte(0x1FFF), 0, "nothing below the address");
        assert_eq!(bus.read_byte(0x2001), 0, "and nothing above it");
        assert_eq!(cpu.d[0], 0xDEAD_BEEF, "the source register is untouched");
    }

    /// `MOVEA.W D0,A0` (`$3040`): the low word is sign extended into the full
    /// 32-bit register, and no flag changes (M68000PM section 8, "MOVEA";
    /// M68000UM section 1 "Address Registers").
    #[test]
    fn movea_w_sign_extends_and_keeps_flags() {
        let (mut cpu, mut bus) = run(0x3040, &[0x4E71]);
        cpu.d[0] = 0xDEAD_8000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0xFFFF_8000, "sign extended, not zero extended");
        assert_eq!(cpu.ccr(), ALL_FLAGS, "MOVEA changes no flags");
        assert_eq!(cpu.d[0], 0xDEAD_8000, "and reads the source, not writes it");
    }

    /// `MOVE.L D1,A0` (`$2041`) is a long: no sign extension, no flags.
    #[test]
    fn movea_l_takes_the_whole_long() {
        let (mut cpu, mut bus) = run(0x2041, &[0x4E71]);
        cpu.d[1] = 0xDEAD_BEEF;
        cpu.set_ccr(SR_Z | SR_V | SR_C);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0xDEAD_BEEF);
        assert_eq!(cpu.ccr(), SR_Z | SR_V | SR_C);
    }

    /// `MOVE.L (A0)+,(A0)+` (`$20D8`): the source is read and `A0` incremented
    /// *before* the destination is resolved, so the value lands at `A0 + 4` and
    /// `A0` ends 8 above where it started. Getting the order wrong writes to
    /// the source address instead.
    #[test]
    fn move_l_postinc_both() {
        let (mut cpu, mut bus) = run(0x20D8, &[0x4E71]);
        cpu.a[0] = 0x0000_2000;
        bus.write_long(0x2000, 0xDEAD_BEEF);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_long(0x2004),
            0xDEAD_BEEF,
            "the destination is the incremented address"
        );
        assert_eq!(
            bus.read_long(0x2000),
            0xDEAD_BEEF,
            "the source was read before the destination was resolved"
        );
        assert_eq!(cpu.a[0], 0x0000_2008, "both increments happen");
        assert!(cpu.flag_n(), "N from the long result");
        assert!(!cpu.flag_z());
    }

    /// `MOVE.W -(A1),-(A0)` (`$3121`): each register decrements exactly once,
    /// the source read happens at the decremented source address, and the write
    /// lands at the decremented destination address.
    #[test]
    fn move_w_predec_source_and_dest() {
        // Assembled from the format in the module docs: size `11` = word
        // (0x3000), dest reg `000` = A0 in bits 11..9, dest mode `100` = -(An)
        // in bits 8..6 (0x0100), source field `100 001` = -(A1) (0x0021).
        let (mut cpu, mut bus) = run(0x3121, &[0x4E71]);
        cpu.a[0] = 0x0000_2010;
        cpu.a[1] = 0x0000_2020;
        bus.write_word(0x201E, 0xBEEF);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0x0000_200E, "A0 decremented once");
        assert_eq!(cpu.a[1], 0x0000_201E, "A1 decremented once");
        assert_eq!(bus.read_word(0x200E), 0xBEEF, "copied to the decremented A0");
        assert_eq!(bus.read_word(0x201E), 0xBEEF, "source still there");
    }

    /// An opcode this ticket rejects in decode takes the illegal-instruction
    /// exception rather than half-executing: `MOVEQ` with bit 8 set is the one
    /// the manual calls out by name for the 68000 (M68000UM Format IV).
    #[test]
    fn moveq_with_bit_8_set_is_an_illegal_instruction() {
        use crate::exceptions::EXC_ILLEGAL;
        let (mut cpu, mut bus) = run(0x71FF, &[0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        assert_eq!(cpu.d[0], 0, "nothing was moved");
    }
}
