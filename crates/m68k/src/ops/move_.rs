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
//! For a mode `111` destination, bits 11..9 are the address-mode sub-mode
//! rather than a register — the same job the register field does in a normal EA
//! field. `MOVE.B D1,$2001.W` is therefore `$11C1` (bits 11..9 = `000`, absolute
//! short), not `$13C1`; `$13C1` is `MOVE.B D1,(d8,PC).W`.
//!
//! Two things make MOVE unlike every other instruction with an EA field:
//!
//! - The **destination fields are swapped**: destination *register* is bits
//!   11..9 and destination *mode* is bits 8..6, whereas a source EA field is
//!   the usual mode-in-bits-5..3, reg-in-bits-2..0. Reading them the other way
//!   round is the classic MOVE bug.
//! - The **size field is not [`Size::from_bits`]**: `01` is byte, `11` word and
//!   `10` long. `00` in bits 13..12 with bits 15..14 clear is not a move at all
//!   but the `$0000` group (`ORI`, `BTST`, `MOVEP`, `MOVE to CCR/SR`), so
//!   [`Size::from_move_bits`] is its own decoder.
//!
//! Destination mode `001` is not MOVE but **MOVEA** — its own instruction,
//! word and long only, no flags touched — so the byte group with that mode
//! (`MOVE.B D0,A0`-shaped opcodes) is an illegal instruction, as is a byte MOVE
//! whose *source* is an An: both would read or write an address register a byte
//! at a time, which no 68000 instruction does.
//!
//! [`crate::decode`] applies those rules, so the handlers below never re-check
//! them; `Op::Move`, `Op::Movea` and `Op::Moveq` mean "valid move operand
//! pair".
//!
//! # Order of operations
//!
//! The source EA is resolved **and read in full** before the destination EA is
//! resolved, so `MOVE.L (A0)+,(A0)+` reads from the old `A0` and increments,
//! and only then takes the incremented `A0` as its destination and increments
//! again. Resolving both EAs first would write one long later than the chip
//! does and leave `A0` incremented twice before the read.
//!
//! The real chip's prefetch quirk — for some destination modes and forms the
//! final prefetch word is fetched *before* the destination write instead of
//! after it — is deliberately not modelled here. What it changes is the
//! bus-transaction trace, plus the refilled queue word only for a destination
//! that overlaps the instruction stream, which no move case in the suite is; the
//! trace is M1-31's job (#39, part 1 of which is literally "cycle checks …
//! MOVE"), not this ticket's.
//!
//! # Flags
//!
//! MOVE and MOVEQ set N and Z from the result and clear V and C, leaving X
//! alone; MOVEA changes no flags at all (M68000PM section 8, "MOVE", "MOVEA",
//! "MOVEQ"). [`M68k::set_logic_flags`] is the shared implementation.

use crate::M68k;
use crate::bus::Bus;
use crate::decode::MOVEQ_BIT8;
use crate::ea::{Ea, Size, sign_extend_byte};

/// Shift of MOVE's size field, bits 13..12 (`01` byte, `11` word, `10` long).
const SIZE_SHIFT: u32 = 12;
/// Shift of MOVE's destination *register*, bits 11..9 — the swapped half of the
/// destination field (see the module docs).
const DEST_REG_SHIFT: u32 = 9;
/// Shift of MOVE's destination *mode*, bits 8..6.
const DEST_MODE_SHIFT: u32 = 6;
/// Shift of the source EA's mode, bits 5..3; the register is bits 2..0.
const SRC_MODE_SHIFT: u32 = 3;
/// Width of a mode or register field.
const FIELD_MASK: u16 = 0b111;

impl M68k {
    /// `MOVE <ea>,Dn | (An) | …`: copy one operand, at the width in bits 13..12
    /// (M68000PM section 8, "MOVE — Move").
    ///
    /// The order is the instruction: resolve and read the source completely,
    /// then resolve the destination, then write. That is what makes
    /// `MOVE.L (A0)+,(A0)+` copy from the old `A0` to the incremented one, and
    /// it is why the source is read before `resolve_ea` is called a second time
    /// — resolving both first would increment `A0` twice before the read.
    ///
    /// Flags: N and Z from the result, V and C cleared, X unchanged
    /// ([`M68k::set_logic_flags`]).
    ///
    /// Clocks: 0, because every access this performs charges itself — the
    /// operand bus cycles and the extension-word queue pops through
    /// [`M68k::read_ea`] / [`M68k::write_ea`] / [`M68k::resolve_ea`], at
    /// [`crate::cpu::BUS_CYCLE_CLOCKS`] each. The instruction's own per-mode
    /// execution time on top of that (the `MOVE.L (A0)+,(A0)+` pair of
    /// register-update cycles and friends) is M1-31 (#39/#40); the conformance
    /// checks here are state-based, not cycle-based.
    pub(crate) fn op_move(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let Some(size) = Size::from_move_bits(opcode >> SIZE_SHIFT) else {
            // `classify` only routes the three real move sizes here, so this is
            // unreachable; naming the illegal route keeps it defined.
            return self.op_illegal(bus);
        };

        let src_mode = field(opcode, SRC_MODE_SHIFT);
        let src_reg = field(opcode, 0);
        let Some(src) = self.resolve_move_ea(bus, src_mode, src_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, size);

        let dest_reg = field(opcode, DEST_REG_SHIFT);
        let dest_mode = field(opcode, DEST_MODE_SHIFT);
        let Some(dest) = self.resolve_move_ea(bus, dest_mode, dest_reg, size) else {
            return self.op_illegal(bus);
        };
        self.write_ea(bus, dest, size, value);

        self.set_logic_flags(value, size);
        self.advance_opcode(bus);
        0
    }

    /// `MOVEA <ea>,An`: the move-group opcode whose destination mode is `001`
    /// (M68000PM section 8, "MOVEA — Move Address to Address Register").
    ///
    /// Word and long only, so a byte never reaches here. A word source is sign
    /// extended into the full 32-bit register, which [`M68k::write_ea`] already
    /// does for an [`Ea::AddrReg`] destination for exactly this reason, and no
    /// flag is touched — `MOVEA` is the one move that leaves the whole CCR
    /// alone, which is why it calls neither [`M68k::set_nz`] nor
    /// [`M68k::set_logic_flags`].
    ///
    /// Clocks: as for [`M68k::op_move`], only the self-charging accesses.
    pub(crate) fn op_movea(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let Some(size) = Size::from_move_bits(opcode >> SIZE_SHIFT) else {
            return self.op_illegal(bus);
        };

        let src_mode = field(opcode, SRC_MODE_SHIFT);
        let src_reg = field(opcode, 0);
        let Some(src) = self.resolve_move_ea(bus, src_mode, src_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, size);

        let dest_reg = field(opcode, DEST_REG_SHIFT);
        self.write_ea(bus, Ea::AddrReg(dest_reg), size, value);

        self.advance_opcode(bus);
        0
    }

    /// `MOVEQ #imm8,Dn` (M68000PM section 8, "MOVEQ — Move Quick to Data
    /// Register"): the low byte of the opcode sign extended into a whole data
    /// register, which is why the flags are computed at [`Size::Long`] — the
    /// result is a full 32-bit value, not a byte sitting in the low bits.
    ///
    /// Clocks: the queue refill of [`M68k::advance_opcode`] charges the whole
    /// instruction, which is the 4 clocks a `MOVEQ` really costs.
    pub(crate) fn op_moveq(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        if opcode & MOVEQ_BIT8 != 0 {
            // Bit 8 set is not a `MOVEQ` on the 68000; `classify` routes those
            // to `Op::Illegal`, so this is the same unreachable guard.
            return self.op_illegal(bus);
        }
        let reg = usize::from(field(opcode, DEST_REG_SHIFT));
        let value = sign_extend_byte(opcode as u8);
        self.d[reg] = value;
        self.set_logic_flags(value, Size::Long);
        self.advance_opcode(bus);
        0
    }

    /// Resolve one operand field of a move instruction, which [`crate::decode`]
    /// has already validated.
    ///
    /// [`M68k::resolve_ea`] declines only a field that decoding rejects — an
    /// invalid mode, or a byte access to an An — so `None` here means decode and
    /// this handler disagree. The debug assert makes that loud in tests and the
    /// illegal-instruction route is what a release build takes, rather than
    /// executing an instruction with a half-resolved operand.
    fn resolve_move_ea(&mut self, bus: &mut impl Bus, mode: u8, reg: u8, size: Size) -> Option<Ea> {
        let ea = self.resolve_ea(bus, mode, reg, size);
        debug_assert!(
            ea.is_some(),
            "decode let an invalid move operand through: mode {mode:b} reg {reg:b} {size:?}"
        );
        ea
    }
}

/// One 3-bit field of `opcode` at `shift`.
#[inline]
const fn field(opcode: u16, shift: u32) -> u8 {
    // `as` rather than `u8::from` because this is a `const fn` and `From` is not
    // a const trait on this toolchain (see `ea::sign_extend_word`).
    ((opcode >> shift) & FIELD_MASK) as u8
}

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
        // `MOVE.B D1,$2001.W` with D1's low byte zero: the byte result is zero,
        // so Z sets and N does not — even though bit 31 of the source long is
        // set. Flags computed from the whole long get N wrong here.
        let (mut cpu, mut bus) = run(0x11C1, &[0x2001, 0x4E71]);
        cpu.d[1] = 0xDEAD_0000;
        bus.write_byte(0x2001, 0x42);
        bus.write_byte(0x2002, 0x5A);
        cpu.set_ccr(SR_N);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(0x2001), 0x00, "the byte was written");
        assert_eq!(bus.read_byte(0x2002), 0x5A, "a byte is one byte");
        assert_eq!(cpu.d[1], 0xDEAD_0000, "the source register is untouched");
        assert!(cpu.flag_z(), "the byte result is zero, so Z is set");
        assert!(!cpu.flag_n(), "bit 7 of the byte is clear, so N is clear");
    }

    /// `MOVE.B D0,$2000.W` (`$1038` + the absolute word): a byte goes to the
    /// absolute address and nothing else does (M68000PM section 8, "MOVE").
    #[test]
    fn move_b_to_memory_abs_w() {
        // `MOVE.B D0,$2000.W`: one byte of D0 into memory.
        let (mut cpu, mut bus) = run(0x11C0, &[0x2000, 0x4E71]);
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
        assert_eq!(
            bus.read_word(0x200E),
            0xBEEF,
            "copied to the decremented A0"
        );
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

    /// Moving the stack pointer means moving both places it lives: the active
    /// `a[7]` and the stored `ssp`/`usp` slot for the current mode (see
    /// [`M68k::set_active_sp`]), which a save state and the conformance harness
    /// both read back. MOVEA is the first instruction with a writable address
    /// register, so it is the first to exercise that in the EA layer; the case
    /// that caught it is `305f [MOVEA.w (A7)+, A0] 117` in `MOVEA.w.json.gz`.
    #[test]
    fn movea_to_a7_moves_the_stored_stack_pointer_too() {
        // MOVEA.L D0,A7. `SR_RESET` leaves S = 1, so the active pointer here is
        // the supervisor one.
        let (mut cpu, mut bus) = run(0x2E40, &[0x4E71]);
        cpu.d[0] = 0xFFFF_FF00;
        cpu.step(&mut bus);
        assert_eq!(cpu.a[7], 0xFFFF_FF00, "word-for-word a MOVEA to A7");
        assert_eq!(cpu.ssp, cpu.a[7], "the stored SSP follows a[7]");

        // MOVE.L (A7)+,D1: an increment of the stack pointer moves it the same
        // way, from the source side this time. Size `10` long, destination reg
        // bits 11..9 = `001` (D1) with destination mode `000` (Dn), source mode
        // bits 5..3 = `011` ((An)+) with source reg `111` (A7).
        let (mut cpu, mut bus) = run(0x221F, &[0x4E71]);
        let sp = cpu.a[7];
        bus.write_long(sp, 0xDEAD_BEEF);
        cpu.step(&mut bus);
        assert_eq!(cpu.d[1], 0xDEAD_BEEF, "the operand is at the old SP");
        assert_eq!(cpu.a[7], sp + 4, "post-increment steps by the size");
        assert_eq!(cpu.ssp, cpu.a[7], "the stored SSP follows a[7]");
    }
}
