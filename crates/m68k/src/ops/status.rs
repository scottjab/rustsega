//! The instructions that read or write the status register, and the two that
//! move the user stack pointer (M1-26, #34).
//!
//! Opcodes (M68000UM section 8, "Instruction Formats"):
//!
//! | Instruction | Encoding | Operation |
//! |---|---|---|
//! | `MOVE SR,<ea>` | `0100 0000 11 eeeeee` | SR → data-alterable destination |
//! | `MOVE <ea>,CCR` | `0100 0100 11 eeeeee` | low 5 bits of source word → CCR |
//! | `MOVE <ea>,SR` | `0100 0110 11 eeeeee` | source word → SR (privileged) |
//! | `MOVE An,USP` | `0100 1110 0110 0 rrr` | An → USP (privileged) |
//! | `MOVE USP,An` | `0100 1110 0110 1 rrr` | USP → An (privileged) |
//! | `ANDI/ORI/EORI #imm,CCR` | `0010`/`0000`/`1010 000 0 1111 00` | byte `<#imm>` op CCR → CCR |
//! | `ANDI/ORI/EORI #imm,SR` | same, `ss` = `01` | word `<#imm>` op SR → SR (privileged) |
//!
//! `MOVE from CCR` (`0100 0010 11 eeeeee`) is a 68010 instruction and is
//! ILLEGAL here; [`crate::decode`] gives it `Op::Illegal`, so nothing below has
//! to know about it.
//!
//! Two rules the handlers share:
//!
//! * Every SR write goes through [`M68k::set_sr`] and every CCR write through
//!   [`M68k::set_ccr`], because a write that clears `S` switches the active stack
//!   pointer *immediately* — including the writes that make `S` and `T` come from
//!   the operand, like `ORI #imm,SR`. Nothing here may read the interrupt mask
//!   early and reuse it: whether an `SR` write unmasks a pending interrupt is
//!   M1-28 (#36)'s question, and it is answered by looking at `self.sr` after the
//!   instruction, so a cached copy would be a latent bug.
//! * `S` and `T` are *results* of the five SR-writing instructions, not the
//!   operand's `N`/`Z`-style flags: [`M68k::set_logic_flags`] is not used, since
//!   it would clear `V` and `C` that the operand sets.
//!
//! Illegal operands are rejected by [`crate::decode`]: `MOVE SR,<ea>` with an
//! immediate or address-register destination, and `MOVE <ea>,SR`/`CCR` with an
//! address-register source. The `resolve_ea` guards below are the same
//! belt-and-braces every other handler in this module tree carries.

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::ea::{Ea, Size};
use crate::ops::field;
use crate::ops::logic::LogicOp;

/// Shift of an EA mode, bits 5..3; the EA register is bits 2..0.
const EA_MODE_SHIFT: u32 = 3;
/// EA mode `111`, the mode whose register picks absolute/PC-relative/immediate.
const EA_MODE_111: u8 = 0b111;
/// EA register `100` inside mode `111`: immediate.
const EA_REG_IMMEDIATE: u8 = 0b100;
/// Bit 3 of `MOVE USP`: set for `MOVE USP,An` (`$4E68`+`r`), clear for
/// `MOVE An,USP` (`$4E60`+`r`) (M68000UM "MOVE to/from USP").
const USP_TO_AN: u16 = 0x0008;

impl M68k {
    /// `MOVE SR,<ea>`: `$40C0` + `<ea>` — the only read of SR that is not
    /// privileged on the 68000, which is why user-mode code can inspect its own
    /// flags at all (M68000UM "MOVE from SR — Move Status Register"; #34).
    ///
    /// The operand is a *word* and it is the whole SR; the condition codes are
    /// unaffected, which is what makes this the read side of the `SR` and the
    /// reason `move_from_sr_leaves_the_flags_alone` tests a full CCR rather than
    /// a zero one. On a memory destination the write is preceded by a read of
    /// the same location — the 68000's dummy read, as in
    /// [`crate::ops::arith::op_clr`] — so `move_from_sr_to_memory_reads_then_writes`
    /// can see it.
    pub(crate) fn op_move_from_sr(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let reg = field(opcode, 0);
        let Some(dest) = self.resolve_ea(bus, mode, reg, Size::Word) else {
            return self.op_illegal(bus);
        };

        self.read_ea(bus, dest, Size::Word);
        self.write_ea(bus, dest, Size::Word, u32::from(self.sr));
        self.advance_opcode(bus);
        0
    }

    /// `MOVE <ea>,CCR`: `$44C0` + `<ea>`. A word operand whose low five bits
    /// become X N Z V C (M68000UM "MOVE to CCR — Move to Condition Code
    /// Register"; #34).
    ///
    /// Not privileged, and it cannot become privileged: the destination is the
    /// CCR, so `S`, `T` and the interrupt mask are unreachable. [`M68k::set_ccr`]
    /// is the whole mask, and the flags of the instruction *are* the value it
    /// writes — there is no separate flag calculation.
    pub(crate) fn op_move_to_ccr(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let reg = field(opcode, 0);
        let Some(src) = self.resolve_ea(bus, mode, reg, Size::Word) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, Size::Word);

        self.set_ccr(value as u16);
        self.advance_opcode(bus);
        0
    }

    /// `MOVE <ea>,SR`: `$46C0` + `<ea>` — word operand to SR, privileged
    /// (M68000UM "MOVE to SR — Move to Status Register"; #34).
    ///
    /// [`M68k::set_sr`] masks with `SR_MASK` and swaps the stack pointers when
    /// `S` changes, so a write that clears `S` leaves `A7` holding the USP
    /// before the next instruction is fetched; nothing else has to be done here
    /// for `andi_to_sr_clears_s_switches_stack` to hold.
    ///
    /// The privilege check comes first, before the operand is resolved, so an
    /// illegal *and* unprivileged `MOVE $A0,SR` raises vector 8 with the stack
    /// the faulting instruction would have used — the `RTE` rule in
    /// [`crate::exceptions::check_supervisor`].
    pub(crate) fn op_move_to_sr(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        if !self.check_supervisor(bus) {
            return 0;
        }

        let mode = field(opcode, EA_MODE_SHIFT);
        let reg = field(opcode, 0);
        let Some(src) = self.resolve_ea(bus, mode, reg, Size::Word) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, Size::Word);

        self.set_sr(value as u16);
        self.advance_opcode(bus);
        0
    }

    /// `MOVE An,USP` (`$4E60`+`n`) and `MOVE USP,An` (`$4E68`+`n`), privileged in
    /// both directions (M68000UM "MOVE to/from USP — Move User Stack Pointer";
    /// #34).
    ///
    /// `An` is read and written as a *long* — there is no width field in these
    /// encodings — and `MOVE USP,An` writes `A7` itself through
    /// [`crate::ea::M68k::set_an`]'s path (`write_ea` on `Ea::AddrReg`), because
    /// `MOVE USP,A7` is legal and `A7` is where the active pointer lives. The
    /// instruction is privileged, so the write happens in supervisor mode and
    /// `A7` is the SSP there: the SSP moves and the USP is left alone, which is
    /// the asymmetry `move_usp_roundtrip` checks.
    pub(crate) fn op_move_usp(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        if !self.check_supervisor(bus) {
            return 0;
        }

        let reg = field(opcode, 0);
        if opcode & USP_TO_AN == 0 {
            self.usp = self.a[usize::from(reg)];
        } else {
            self.write_ea(bus, Ea::AddrReg(reg), Size::Long, self.usp);
        }

        self.advance_opcode(bus);
        0
    }

    /// `ANDI`/`ORI`/`EORI #imm,CCR`: `$023C`, `$003C`, `$0A3C` (M68000UM
    /// "ANDI to CCR", "ORI to CCR", "EORI to CCR"; #34).
    ///
    /// The CCR and a byte immediate are combined and the result is the CCR. The
    /// immediate occupies a whole extension word but only its low byte is used
    /// ([`crate::ea`]'s byte-immediate rule), and the top three bits of that byte
    /// fall off `CCR_MASK` — so `ORI #$FF,CCR` sets all five flags and nothing
    /// above them. Unprivileged, in user mode like [`M68k::op_move_to_ccr`].
    pub(crate) fn op_logic_ccr(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let op = LogicOp::from_imm(opcode);
        let Some(imm) = self.status_imm(bus, Size::Byte) else {
            return self.op_illegal(bus);
        };

        self.set_ccr(op.apply(u32::from(self.ccr()), imm) as u16);
        self.advance_opcode(bus);
        0
    }

    /// `ANDI`/`ORI`/`EORI #imm,SR`: `$027C`, `$007C`, `$0A7C` (M68000UM
    /// "ANDI to SR", "ORI to SR", "EORI to SR"; #34).
    ///
    /// [`M68k::op_logic_ccr`] with a word immediate and the whole SR as the
    /// destination, which is what makes it privileged and what makes
    /// [`M68k::set_sr`] (not [`M68k::set_ccr`]) the writer: `S` and `T` are bits
    /// of the result, so `EORI #SR_S,SR` leaves supervisor mode and moves `A7`.
    pub(crate) fn op_logic_sr(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        if !self.check_supervisor(bus) {
            return 0;
        }

        let op = LogicOp::from_imm(opcode);
        let Some(imm) = self.status_imm(bus, Size::Word) else {
            return self.op_illegal(bus);
        };

        self.set_sr(op.apply(u32::from(self.sr), imm) as u16);
        self.advance_opcode(bus);
        0
    }

    /// Fetch the instruction's own immediate operand: the mode `111` register
    /// `100` effective address, read at `size`. A byte immediate consumes a whole
    /// extension word (M68000UM "Immediate Addressing Mode"), which
    /// [`M68k::resolve_ea`] already handles, so the two callers only name the
    /// width. `None` is unreachable for this mode — it is the guard every
    /// `resolve_ea` caller writes.
    fn status_imm(&mut self, bus: &mut impl Bus, size: Size) -> Option<u32> {
        let ea = self.resolve_ea(bus, EA_MODE_111, EA_REG_IMMEDIATE, size)?;
        Some(self.read_ea(bus, ea, size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exceptions::{EXC_ILLEGAL, EXC_PRIVILEGE};
    use crate::sr::{CCR_MASK, SR_C, SR_INT_MASK, SR_N, SR_S, SR_T, SR_V, SR_X, SR_Z};
    use crate::testbus::{TestBus, cpu_with_program};

    /// Where [`cpu_with_program`] loads the program, so a test can name the
    /// address `pc` should reach.
    const PROGRAM: u32 = 0x0000_1000;
    /// A data address every memory test can use, clear of the program.
    const MEM: u32 = 0x0000_2000;
    /// A second data address, for `MOVE USP,An`-style tests that need `A7` to
    /// point somewhere that is not where the exception frame will go.
    const USER_SP: u32 = 0x0000_9000;
    /// SR as [`cpu_with_program`] leaves it: supervisor, mask 7, no trace, no
    /// condition codes.
    const RESET_SR: u16 = 0x2700;
    /// Every condition code as one CCR value, for "this instruction leaves the
    /// flags alone" tests.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as the
    /// following words.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, TestBus) {
        let mut words = vec![opcode];
        words.extend_from_slice(rest);
        cpu_with_program(&words)
    }

    /// The stack [`cpu_with_program`] installs `A7`/SSP with, which the privilege
    /// and `MOVE USP,A7` tests measure against.
    const STACK_TOP: u32 = 0x0001_0000;

    /// Run one `step` with `sr` already set to `sr`, the mode entered through
    /// [`M68k::set_sr`] so the stack pointers are where a real CPU would have
    /// left them.
    fn run_with_sr(opcode: u16, rest: &[u16], sr: u16) -> (M68k, TestBus) {
        let (mut cpu, bus) = run(opcode, rest);
        cpu.set_sr(sr);
        (cpu, bus)
    }

    // --- MOVE SR,<ea> -------------------------------------------------------

    /// `MOVE SR,D0` (`$40C0`) puts the whole SR in the low word of `D0` and
    /// leaves the flags alone; `MOVE SR,(A0)` writes the word to memory after
    /// reading it (M68000UM "MOVE from SR").
    #[test]
    fn move_from_sr_writes_the_status_register() {
        let (mut cpu, mut bus) = run(0x40C0, &[0x4E71]); // MOVE SR,D0
        cpu.sr = RESET_SR | SR_V | SR_C;
        cpu.step(&mut bus);
        assert_eq!(cpu.d[0], 0x2703, "low word only, upper half untouched");
        assert_eq!(cpu.sr, RESET_SR | SR_V | SR_C, "reading SR sets no flags");

        let (mut cpu, mut bus) = run(0x40D0, &[0x4E71]); // MOVE SR,(A0)
        cpu.a[0] = MEM;
        cpu.sr = 0x271F;
        bus.write_word(MEM, 0xA55A);
        cpu.step(&mut bus);
        assert_eq!(bus.read_word(MEM), 0x271F);
        assert_eq!(cpu.pc, PROGRAM + 2, "the instruction is one word");
    }

    /// A full CCR proves `MOVE SR,<ea>` writes the *value* rather than routing
    /// through a flag-setting helper: every flag is set in the source and every
    /// flag must still be set afterwards.
    #[test]
    fn move_from_sr_leaves_the_flags_alone() {
        let (mut cpu, mut bus) = run(0x40C1, &[0x4E71]); // MOVE SR,D1
        cpu.sr = RESET_SR | ALL_FLAGS;
        cpu.step(&mut bus);
        assert_eq!(cpu.sr, RESET_SR | ALL_FLAGS);
        assert_eq!(cpu.d[1] & u32::from(CCR_MASK), u32::from(ALL_FLAGS));
    }

    /// The ticket's `move_from_sr_allowed_in_user_mode`: `MOVE from SR` is not a
    /// privileged instruction on the 68000, so a user-mode CPU reads its own SR
    /// and takes no exception.
    #[test]
    fn move_from_sr_allowed_in_user_mode() {
        let (mut cpu, mut bus) = run_with_sr(0x40C2, &[0x4E71], RESET_SR & !SR_S); // MOVE SR,D2
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None, "no privilege violation");
        assert!(!cpu.is_supervisor(), "still in user mode");
        assert_eq!(cpu.d[2], u32::from(RESET_SR & !SR_S));
        assert_eq!(cpu.pc, PROGRAM + 2);
    }

    /// `MOVE SR,<ea>` on a memory destination does the 68000's dummy read, like
    /// `CLR`: the value is thrown away, the bus access is not.
    #[test]
    fn move_from_sr_to_memory_reads_then_writes() {
        /// Records the accesses the CPU makes, in order; the values do not matter,
        /// only that the destination is read before it is written.
        #[derive(Default)]
        struct Recording {
            log: Vec<String>,
        }
        impl Bus for Recording {
            fn read_byte(&mut self, a: u32) -> u8 {
                self.log.push(format!("read byte {a:08X}"));
                0
            }
            fn read_word(&mut self, a: u32) -> u16 {
                self.log.push(format!("read word {a:08X}"));
                0
            }
            fn write_byte(&mut self, a: u32, v: u8) {
                self.log.push(format!("write byte {a:08X} <= {v:02X}"));
            }
            fn write_word(&mut self, a: u32, v: u16) {
                self.log.push(format!("write word {a:08X} <= {v:04X}"));
            }
        }
        let (mut cpu, _) = run(0x40D0, &[0x4E71]); // MOVE SR,(A0)
        cpu.a[0] = MEM;
        // Prime the queue from a bus that cannot record, so the recording bus
        // below only serves the operand and the following opcode.
        cpu.prefetch = [0x40D0, 0x4E71];
        let mut bus = Recording::default();
        cpu.step(&mut bus);
        let log = bus.log.join(" ");
        assert!(
            log.contains("read word 00002000") && log.contains("write word 00002000"),
            "expected a read then a write at the destination, got {log}"
        );
        assert!(
            log.find("read word 00002000").unwrap() < log.find("write word 00002000").unwrap(),
            "the read comes first: {log}"
        );
    }

    // --- MOVE <ea>,CCR ------------------------------------------------------

    /// The ticket's `move_to_ccr_keeps_upper_byte`: `MOVE.W #$1234,CCR` takes the
    /// low five bits and nothing else, so T, S and the interrupt mask survive an
    /// operand whose upper byte is nonzero (M68000UM "MOVE to CCR").
    #[test]
    fn move_to_ccr_keeps_upper_byte() {
        let (mut cpu, mut bus) = run(0x44FC, &[0x1234, 0x4E71]); // MOVE.W #$1234,CCR
        cpu.sr = RESET_SR | ALL_FLAGS;
        cpu.step(&mut bus);
        assert_eq!(
            cpu.ccr(),
            SR_X | SR_Z,
            "the low five bits of $1234 are X and Z ($34 & $1F = $14)"
        );
        assert_eq!(
            cpu.sr & !u16::from(CCR_MASK),
            RESET_SR & !CCR_MASK,
            "T, S and the mask are untouched by a CCR write"
        );
        assert_eq!(cpu.pc, PROGRAM + 4, "opcode plus one extension word");
    }

    /// `MOVE <ea>,CCR` is not privileged, so user mode can change its own flags;
    /// and the value is read as a word, so `(A0)`'s upper byte is fetched and
    /// dropped.
    #[test]
    fn move_to_ccr_from_memory_in_user_mode() {
        let (mut cpu, mut bus) = run_with_sr(0x44D0, &[0x4E71], RESET_SR & !SR_S); // MOVE (A0),CCR
        cpu.a[0] = MEM;
        bus.write_word(MEM, 0xFF1F);
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None);
        assert_eq!(cpu.ccr(), 0x1F, "all five flags, from the low byte");
        assert_eq!(cpu.sr & (SR_T | SR_INT_MASK), RESET_SR & (SR_T | SR_INT_MASK));
    }

    // --- MOVE <ea>,SR ------------------------------------------------------

    /// The ticket's `move_to_sr_user_mode_privilege_violation`: vector 8, raised
    /// before the operand is read, so the stack moved only by the exception frame
    /// and `A7` never became the USP.
    #[test]
    fn move_to_sr_user_mode_privilege_violation() {
        let (mut cpu, mut bus) = run_with_sr(0x46FC, &[0x0000, 0x4E71], RESET_SR & !SR_S);
        // MOVE #0,SR in user mode: allowed, it would clear S *and* the mask, so a
        // written SR is distinguishable from the SR the exception entry leaves.
        bus.write_long(u32::from(EXC_PRIVILEGE) * 4, MEM);
        cpu.set_active_sp(USER_SP);

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_PRIVILEGE));
        assert_eq!(cpu.pc, MEM, "vector 8");
        assert!(cpu.is_supervisor(), "the exception entered supervisor mode");
        assert_eq!(cpu.ssp, STACK_TOP - 6, "only the exception frame moved");
        assert_eq!(cpu.usp, USER_SP, "the operand was never resolved on the USP");
        // The entry sets S (see `crate::exceptions::M68k::exception`), so the most
        // SR can be after a denied write is `RESET_SR`: mask 7, T clear, and none
        // of the operand's bits.
        assert_eq!(cpu.sr, RESET_SR, "the denied write did not happen");
    }

    /// `MOVE #imm,SR` is the one way to leave supervisor mode: `S` clears and
    /// `A7` becomes the USP before the next instruction (M68000UM "MOVE to SR"
    /// and section 1 "User Stack Pointer").
    #[test]
    fn move_to_sr_clears_s_and_switches_stack() {
        let (mut cpu, mut bus) = run(0x46FC, &[0x001F, 0x4E71]); // MOVE #$001F,SR
        let usp_before = 0x0000_8000;
        cpu.usp = usp_before;

        cpu.step(&mut bus);

        assert_eq!(cpu.sr, 0x001F);
        assert!(!cpu.is_supervisor());
        assert_eq!(cpu.a[7], usp_before, "A7 is the USP now");
        assert_eq!(cpu.ssp, STACK_TOP, "and the SSP was stored, not lost");
    }

    /// The `#imm` form of `MOVE to SR` reads one extension word and the
    /// PC-relative and memory forms work too, which is what `MOVEtoSR.json.gz`
    /// walks. `$46FC` is the `#imm` encoding.
    #[test]
    fn move_to_sr_from_immediate_sets_every_masked_bit() {
        let (mut cpu, mut bus) = run(0x46FC, &[0xFFFF, 0x4E71]); // MOVE.L #$FFFF,SR
        cpu.step(&mut bus);
        assert_eq!(cpu.sr, 0xA71F, "SR_MASK applied to the operand");
        assert!(cpu.is_supervisor());
    }

    // --- ANDI/ORI/EORI to CCR and SR --------------------------------------

    /// `ORI.B #$1F,CCR` sets all five flags; `ANDI.B` and `EORI.B` are the same
    /// shape with `&` and `^` (M68000UM "ORI to CCR"). The immediate is a byte
    /// and still costs a whole extension word.
    #[test]
    fn logic_to_ccr_combines_with_the_ccr() {
        let (mut cpu, mut bus) = run(0x003C, &[0x001F, 0x4E71]); // ORI.B #$1F,CCR
        cpu.step(&mut bus);
        assert_eq!(cpu.ccr(), 0x1F);
        assert_eq!(cpu.sr & !u16::from(CCR_MASK), RESET_SR & !CCR_MASK);

        let (mut cpu, mut bus) = run(0x023C, &[0x003C, 0x4E71]); // ANDI.B #$3C,CCR
        cpu.sr = RESET_SR | SR_N | SR_Z;
        cpu.step(&mut bus);
        assert_eq!(cpu.ccr(), SR_N | SR_Z, "0x0C & 0x3C");

        let (mut cpu, mut bus) = run(0x0A3C, &[0x0010, 0x4E71]); // EORI.B #$10,CCR
        cpu.sr = RESET_SR | SR_X;
        cpu.step(&mut bus);
        assert_eq!(cpu.ccr(), 0, "X toggled off and nothing else changed");
    }

    /// `ORI #imm,CCR` is unprivileged in both senses: it works in user mode and
    /// it cannot change `S` even with a `#$FF` operand.
    #[test]
    fn logic_to_ccr_is_not_privileged() {
        let (mut cpu, mut bus) = run_with_sr(0x003C, &[0x00FF, 0x4E71], RESET_SR & !SR_S);
        cpu.set_active_sp(USER_SP);
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None);
        assert_eq!(cpu.ccr(), 0x1F);
        assert!(!cpu.is_supervisor(), "bits above the CCR were dropped");
        assert_eq!(cpu.a[7], USER_SP, "so the stack never moved");
    }

    /// The ticket's `andi_to_sr_clears_s_switches_stack`: `ANDI.W #0,SR` clears
    /// everything, including `S`, so `A7` becomes the USP; the write goes through
    /// `set_sr`, which is what makes the swap happen (M68000UM "ANDI to SR").
    #[test]
    fn andi_to_sr_clears_s_switches_stack() {
        let usp_before = 0x0000_8000;
        let (mut cpu, mut bus) = run(0x027C, &[0x0000, 0x4E71]); // ANDI.W #$0000,SR
        cpu.usp = usp_before;

        cpu.step(&mut bus);

        assert_eq!(cpu.sr, 0);
        assert!(!cpu.is_supervisor());
        assert_eq!(cpu.a[7], usp_before, "A7 switched to the USP");
        assert_eq!(cpu.ssp, STACK_TOP);
    }

    /// `ORI.W #imm,SR` sets `T` and the mask, `EORI.W #SR_S,SR` leaves supervisor
    /// mode, and all three are privileged (M68000UM "ORI to SR", "EORI to SR").
    #[test]
    fn logic_to_sr_writes_the_upper_bits() {
        let (mut cpu, mut bus) = run(0x007C, &[0x8700, 0x4E71]); // ORI.W #$8700,SR
        cpu.step(&mut bus);
        assert_eq!(cpu.sr, RESET_SR | SR_T | SR_INT_MASK);

        let (mut cpu, mut bus) = run(0x0A7C, &[SR_S, 0x4E71]); // EORI.W #SR_S,SR
        cpu.usp = USER_SP;
        cpu.step(&mut bus);
        assert!(!cpu.is_supervisor(), "EORI toggled S off");
        assert_eq!(cpu.a[7], USER_SP);
    }

    /// The two to-SR privilege checks: `ORI/ANDI/EORI #imm,SR` in user mode is
    /// vector 8 with nothing written and no stack movement beyond the frame.
    #[test]
    fn logic_to_sr_in_user_mode_is_privilege_violation() {
        for opcode in [0x007Cu16, 0x027C, 0x0A7C] {
            let (mut cpu, mut bus) = run_with_sr(opcode, &[0x0000, 0x4E71], RESET_SR & !SR_S);
            bus.write_long(u32::from(EXC_PRIVILEGE) * 4, MEM);
            cpu.set_active_sp(USER_SP);
            cpu.step(&mut bus);
            assert_eq!(cpu.pending_exception, Some(EXC_PRIVILEGE), "${opcode:04X}");
            // The S bit the exception entry sets is the only difference from the
            // user-mode SR, so the operand never reached SR.
            assert_eq!(cpu.sr, RESET_SR, "${opcode:04X} wrote anyway");
            assert_eq!(cpu.usp, USER_SP, "${opcode:04X} read the operand");
            assert_eq!(cpu.ssp, STACK_TOP - 6, "${opcode:04X} frame");
        }
    }

    // --- MOVE USP ----------------------------------------------------------

    /// The ticket's `move_usp_roundtrip`: `MOVE A1,USP` then `MOVE USP,A2`
    /// returns the value, `MOVE USP,A7` moves the SSP (the instruction is
    /// privileged, so `A7` *is* the SSP), and neither form touches the flags or
    /// the memory bus (M68000UM "MOVE to/from USP").
    #[test]
    fn move_usp_roundtrip() {
        let (mut cpu, mut bus) = run(0x4E61, &[0x4E71]); // MOVE A1,USP
        cpu.a[1] = 0x0000_7F00;
        cpu.step(&mut bus);
        assert_eq!(cpu.usp, 0x0000_7F00);
        assert_eq!(cpu.sr, RESET_SR, "USP moves set no flags");

        let (mut cpu, mut bus) = run(0x4E6A, &[0x4E71]); // MOVE USP,A2
        cpu.usp = 0x0000_7F00;
        cpu.step(&mut bus);
        assert_eq!(cpu.a[2], 0x0000_7F00);

        // `MOVE USP,A7` in supervisor mode moves the SSP and leaves the USP.
        let (mut cpu, mut bus) = run(0x4E6F, &[0x4E71]); // MOVE USP,A7
        cpu.usp = 0x0000_7F00;
        cpu.step(&mut bus);
        assert_eq!(cpu.ssp, 0x0000_7F00);
        assert_eq!(cpu.a[7], cpu.ssp, "A7 follows the pointer it was given");
        assert_eq!(cpu.usp, 0x0000_7F00, "the USP was read, not written");

        // And the pair round-trips through a register.
        let (mut cpu, mut bus) = run(0x4E60, &[0x4E68, 0x4E71]); // MOVE A0,USP; MOVE USP,A0
        cpu.a[0] = 0x0000_1234;
        cpu.step(&mut bus);
        assert_eq!(cpu.usp, 0x0000_1234);
        cpu.a[0] = 0;
        cpu.step(&mut bus);
        assert_eq!(cpu.a[0], 0x0000_1234, "USP -> A0 gave it back");
    }

    /// Both USP directions are privileged: in user mode each takes vector 8 and
    /// neither moves the pointer it was given (M68000UM "MOVE to/from USP").
    #[test]
    fn move_usp_in_user_mode_is_privilege_violation() {
        for opcode in [0x4E61u16, 0x4E69] {
            let (mut cpu, mut bus) = run_with_sr(opcode, &[0x4E71], RESET_SR & !SR_S);
            bus.write_long(u32::from(EXC_PRIVILEGE) * 4, MEM);
            cpu.set_active_sp(USER_SP);
            cpu.a[1] = 0x0000_7F00;
            cpu.step(&mut bus);
            assert_eq!(cpu.pending_exception, Some(EXC_PRIVILEGE), "${opcode:04X}");
            assert_eq!(cpu.usp, USER_SP, "${opcode:04X} moved the USP");
            assert_eq!(cpu.pc, MEM);
        }
    }

    // --- decode-level rejections -------------------------------------------

    /// `MOVE from CCR` is 68010-only: `$42C0` raises the illegal-instruction
    /// exception rather than doing something plausible (M68000UM has no such
    /// instruction; the ticket's `move_from_ccr_is_illegal`).
    #[test]
    fn move_from_ccr_is_illegal() {
        let (mut cpu, mut bus) = run(0x42C0, &[0x4E71]);
        bus.write_long(u32::from(EXC_ILLEGAL) * 4, MEM);
        cpu.d[0] = 0xDEAD_FFFF;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        assert_eq!(cpu.pc, MEM);
        assert_eq!(cpu.d[0], 0xDEAD_FFFF, "the instruction did not run");
    }

    /// `$40FC` — `MOVE SR,#imm` — is an illegal operand, and `$46C8` —
    /// `MOVE A0,SR` — is not an encoded source; both are vector 4 with the
    /// registers left alone (M68000UM "MOVE to SR", "MOVE from SR", addressing
    /// modes).
    #[test]
    fn status_moves_reject_illegal_operands() {
        for opcode in [0x40FCu16, 0x46C8, 0x44CC] {
            let (mut cpu, mut bus) = run(opcode, &[0x0000, 0x4E71]);
            bus.write_long(u32::from(EXC_ILLEGAL) * 4, MEM);
            cpu.d[3] = 0xFFFF_FFFF;
            cpu.step(&mut bus);
            assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL), "${opcode:04X}");
            assert_eq!(cpu.d[3], 0xFFFF_FFFF, "${opcode:04X} ran anyway");
        }
    }
}
