//! The branch/condition group: `Bcc` and `BRA` (M68000UM Format VI), `BSR`,
//! and the two halves of Format V, `Scc` and `DBcc` (M1-16, #24).
//!
//! All four decide with [`crate::sr::test_cc`] on the condition code in bits
//! 11..8 of the opcode, which is why they are one module: `BEQ`, `SBEQ` and
//! `DBEQ` cannot disagree about what Z means.
//!
//! # The base a displacement is measured from
//!
//! Every branch displacement in this group is added to `pc + 2` — the address of
//! the displacement itself, which is what [`crate::ea::M68k::ext_addr`] names —
//! and not to the address of the opcode. For `DBcc` that is not obvious from the
//! manual's phrasing, so it is measured: all 2005 of `DBcc.json.gz`'s
//! branch-taking, non-skipped cases want `address of the displacement word +
//! d16`, and `ops::flow::tests::dbcc_branch_base_is_the_displacement_word` pins
//! it. `Bcc` and `BSR` use the same base, which the same measurement confirms.
//!
//! `pc + 2` is *not* masked to 24 bits, and neither is the target: `pc` is a
//! 32-bit value that only the bus fetches through a mask, and 907 of
//! `DBcc.json.gz`'s, 7 of `BSR.json.gz`'s and 4 of `Bcc.json.gz`'s cases end
//! with a `pc` above `$FFFFFF`. See [`crate::ea`] and #193, which is the same
//! finding for `LEA`/`PEA`.
//!
//! # Timing
//!
//! The handlers return 0, like every other handler in this module: the clocks a
//! branch costs come from the self-charging bus helpers, which is enough to keep
//! the property the manual cares about — a taken branch refills the queue and so
//! costs [`crate::M68k::jump_to`]'s 8 clocks, a not-taken one slides the window
//! for 4. The reference totals (Bcc taken 10, `Bcc.B` not taken 8, `Bcc.W` not
//! taken 12, BSR 18, DBcc 12/10/14, Scc 4/6 — M68000UM appendix "Instruction
//! Timing") are M1-31's (#39/#40) to settle, and adding them here on top of the
//! cycles the queue already charged would only double-count them.

use crate::M68k;
use crate::bus::Bus;
use crate::ea::{Ea, Size, sign_extend_byte, sign_extend_word};
use crate::ops::field;
use crate::sr::test_cc;

/// Bit positions of the fields the four handlers re-read (M68000UM Formats V and
/// VI): the condition code is bits 11..8, the EA mode is 5..3 and the EA — or
/// `DBcc`'s counter — register is 2..0.
const CC_SHIFT: u32 = 8;
/// The EA mode field, [`crate::ea::M68k::resolve_ea`]'s first argument.
const EA_MODE_SHIFT: u32 = 3;
/// The EA register field, [`crate::ea::M68k::resolve_ea`]'s second argument.
const EA_REG_SHIFT: u32 = 0;

/// A `Bcc`/`BSR` byte displacement of zero is not a branch to the next
/// instruction: it is the escape hatch that means a 16-bit displacement word
/// follows (M68000UM "Bcc — Branch Conditional").
const WORD_DISPLACEMENT: u8 = 0;
/// How many bytes wider the instruction is when that word is there, which is how
/// much further `BSR`'s return address is.
const WORD_DISPLACEMENT_WIDTH: u32 = 2;

/// The value `Scc` writes when the condition is true: `$FF`, not `$01`
/// (M68000UM "Scc — Set Conditional": "if condition FALSE, 0; else, all ones").
const SCC_TRUE: u8 = 0xFF;

/// The value `DBcc`'s counter holds after the decrement that ends the loop, and
/// the reason the loop does not branch a final time: the counter is a *word*, so
/// `0 - 1` is `$FFFF` rather than "ran out" (M68000UM "DBcc — Decrement and
/// Branch": "if result of decrement = $FFFF, do not branch").
const DBCC_EXPIRED: u16 = 0xFFFF;

impl M68k {
    /// `Bcc d8`, `Bcc d16`, and `BRA` under them as condition code `0000`
    /// (M68000UM "Bcc — Branch Conditional", "BRA — Branch Program").
    ///
    /// Take the branch and the queue is refilled at `pc + 2 + disp`; leave it and
    /// the instruction just finishes, having spent the displacement word if there
    /// was one. Neither moves a flag: `Bcc` reads N, Z, V and C and writes none
    /// (M68000UM "68000 CCR Affected by Instruction Set").
    pub(crate) fn op_bcc(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let base = self.branch_base();
        let (disp, _wide) = self.branch_displacement(bus, opcode);

        if test_cc(condition(opcode), self.sr) {
            self.jump_to(bus, base.wrapping_add(disp));
        } else {
            self.advance_opcode(bus);
        }
        0
    }

    /// `BSR d8`/`BSR d16`, `0110 0001 dddddddd`: `Bcc` with a call attached
    /// (M68000UM "BSR — Branch to Subroutine").
    ///
    /// The pushed long is the address of the *following instruction*, so it is
    /// `pc + 2` for a byte displacement and `pc + 4` for a word one — the branch
    /// base plus however wide the displacement was. `RTS` is #33's.
    pub(crate) fn op_bsr(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let base = self.branch_base();
        let (disp, wide) = self.branch_displacement(bus, opcode);

        self.push_long(bus, base.wrapping_add(wide));
        self.jump_to(bus, base.wrapping_add(disp));
        0
    }

    /// `Scc <ea>`, `0101 cccc 11 eeeeee` at every mode but `001`
    /// (M68000UM "Scc — Set Conditional"): `$FF` where the condition holds and
    /// `$00` where it does not, into a byte-sized operand.
    ///
    /// Decode already rejected the modes that cannot be a byte destination
    /// (#24's split of `0x5000..=0x5FFF`), so `resolve_ea` cannot fail here; the
    /// `else` is the same belt-and-braces `op_clr` uses.
    pub(crate) fn op_scc(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let reg = field(opcode, EA_REG_SHIFT);
        let holds = test_cc(condition(opcode), self.sr);

        let Some(dest) = self.resolve_ea(bus, mode, reg, Size::Byte) else {
            return self.op_illegal(bus);
        };
        // The 68000's dummy read, as in `op_clr`: a register read costs nothing,
        // a memory one is a real bus cycle whose value is thrown away.
        self.read_ea(bus, dest, Size::Byte);
        self.write_ea(
            bus,
            dest,
            Size::Byte,
            u32::from(if holds { SCC_TRUE } else { 0 }),
        );

        self.advance_opcode(bus);
        0
    }

    /// `DBcc Dn,d16`, `0101 cccc 11 001 rrr` (M68000UM "DBcc — Decrement and
    /// Branch"): with the condition true, fall through and leave the counter
    /// alone; otherwise decrement the low word of `Dn` and branch unless that
    /// decrement ran the loop out.
    ///
    /// The counter is `Dn.w` only — bits 31..16 keep their value, which is why
    /// this writes through `write_ea` at [`Size::Word`] instead of assigning to
    /// `self.d[]` — and the base is the address of the displacement word.
    pub(crate) fn op_dbcc(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let base = self.branch_base();
        // The displacement word is popped before the condition is looked at: it is
        // part of the instruction either way, and the base has to be read while
        // `pc` is still the opcode's address.
        let disp = sign_extend_word(self.fetch_ext_word(bus));

        if test_cc(condition(opcode), self.sr) {
            self.advance_opcode(bus);
            return 0;
        }
        let counter = field(opcode, EA_REG_SHIFT);
        let next = (self.d[usize::from(counter)] as u16).wrapping_sub(1);
        self.write_ea(bus, Ea::DataReg(counter), Size::Word, u32::from(next));

        if next == DBCC_EXPIRED {
            self.advance_opcode(bus);
        } else {
            self.jump_to(bus, base.wrapping_add(disp));
        }
        0
    }

    /// The address a branch displacement is measured from: `pc + 2`, the address
    /// of the displacement word, captured before anything is popped exactly like
    /// [`crate::ea::M68k::ext_addr`] — except that this one does not mask, because
    /// `pc` does not (see the module docs).
    #[inline]
    fn branch_base(&self) -> u32 {
        self.pc.wrapping_add(2)
    }

    /// The displacement of a `Bcc`/`BRA`/`BSR`, and how much wider that made the
    /// instruction: bits 7..0 sign extended, unless they are zero, in which case a
    /// 16-bit displacement word follows and is popped here (M68000UM "Bcc":
    /// "If displacement = $00 then a word displacement follows").
    ///
    /// The other side of that rule is what the 68020 added and the 68000 does not
    /// have: `$FF` is a displacement of -1 here, never a long-branch marker, so
    /// `$60FF` is `BRA.B #-1`.
    fn branch_displacement(&mut self, bus: &mut impl Bus, opcode: u16) -> (u32, u32) {
        let low = opcode as u8;
        if low == WORD_DISPLACEMENT {
            (
                sign_extend_word(self.fetch_ext_word(bus)),
                WORD_DISPLACEMENT_WIDTH,
            )
        } else {
            (sign_extend_byte(low), 0)
        }
    }
}

/// The condition code of a `Bcc`/`BSR`/`Scc`/`DBcc` opcode: bits 11..8, which is
/// four bits wide by construction because bit 12 is the group's own bit
/// (M68000UM Formats V and VI).
const fn condition(opcode: u16) -> u8 {
    (opcode >> CC_SHIFT) as u8
}

#[cfg(test)]
mod tests {
    use crate::M68k;
    use crate::bus::Bus;
    use crate::sr::test_cc;
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

    /// `Scc` to `D0` (`0101 cccc 11 000 000`): OR in the condition code.
    const SCC_D0: u16 = 0x50C0;
    /// `DBcc D0,d16` (`0101 cccc 11 001 000`): the same condition field, and the
    /// mode field that separates it from `Scc`.
    const DBCC_D0: u16 = 0x50C8;
    /// `Bcc`/`BRA`/`BSR` with a `+2` byte displacement (`0110 cccc 00000010`).
    const BCC_PLUS_2: u16 = 0x6002;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as the
    /// following words.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    /// `BRA.B #-2` (`$60FE`): the displacement is added to the address of the
    /// *opcode word plus two*, so `-2` from `$1000` lands back on `$1000` — the
    /// one-byte self loop (M68000UM "BRA — Branch Program", "Program
    /// Counter-Relative Addressing Mode").
    #[test]
    fn bra_byte_backwards() {
        let (mut cpu, mut bus) = run(0x60FE, &[0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, PROGRAM, "$1002 - 2");
        assert_eq!(
            cpu.prefetch,
            [0x60FE, 0x4E71],
            "the queue is refetched from the target, not slid along"
        );
    }

    /// `BEQ.W $F0` (`$6700`, `$00F0`): a zero byte displacement means a 16-bit
    /// displacement word follows, and `$FF` is *not* a long-branch marker on the
    /// 68000 — it is just -1 (ticket #24, "0xFF is NOT a long branch").
    #[test]
    fn beq_word_displacement() {
        let (mut cpu, mut bus) = run(0x6700, &[0x00F0, 0x4E71]);
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, PROGRAM + 0xF2, "$1002 + $F0");
        assert_eq!(cpu.sr & SR_Z, SR_Z, "a branch takes no flag");
    }

    /// `BEQ.W` with the condition false still consumes the displacement word:
    /// it falls through to `$1004`, not to `$1002`.
    #[test]
    fn beq_word_displacement_not_taken_skips_the_word() {
        // Reset leaves Z clear, so BEQ is false here.
        let (mut cpu, mut bus) = run(0x6700, &[0x00F0, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, PROGRAM + 4);
        assert_eq!(
            cpu.prefetch,
            [0x4E71, 0],
            "the queue is two words from there"
        );
    }

    /// `BSR.B #-4` (`$61FC`): pushes the address of the instruction *after* the
    /// BSR — `pc + 2` for a byte displacement — then branches to `pc + 2 - 4`.
    /// The push is a long, big endian, and both stack pointers move (M68000UM
    /// "BSR — Branch to Subroutine", section 2 "Program Stack").
    #[test]
    fn bsr_pushes_return() {
        let (mut cpu, mut bus) = run(0x61FC, &[0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[7], STACK_TOP - 4);
        assert_eq!(
            bus.read_long(STACK_TOP - 4),
            PROGRAM + 2,
            "the return address is the next instruction, not the branch target"
        );
        assert_eq!(cpu.ssp, STACK_TOP - 4, "the stored mirror moves too");
        assert_eq!(cpu.pc, PROGRAM - 2, "$1002 - 4, not the return address");
    }

    /// `BSR.W $F0` (`$6100`, `$00F0`): the return address is past the
    /// displacement word, so `$1004` and not `$1002` (8065 of 8065 cases of
    /// `BSR.json.gz` push `opcode + 2` for a byte displacement and `opcode + 4`
    /// for a word one).
    #[test]
    fn bsr_word_displacement_pushes_the_address_after_the_word() {
        let (mut cpu, mut bus) = run(0x6100, &[0x00F0, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(STACK_TOP - 4), PROGRAM + 4);
        assert_eq!(cpu.pc, PROGRAM + 0xF2);
    }

    /// A taken branch discards the queue: the instruction that was already
    /// fetched behind the branch must not run. `$1002 MOVEQ #1,D1` is skipped
    /// and `$1004 MOVEQ #2,D2` is what executes next.
    #[test]
    fn taken_branch_refills_the_queue_and_skips_ahead() {
        let (mut cpu, mut bus) = run(0x6002, &[0x7001, 0x7202, 0x4E71]);

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, PROGRAM + 4);
        assert_eq!(
            cpu.prefetch,
            [0x7202, 0x4E71],
            "the queue holds the words at the target, not the ones already fetched"
        );

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0, "the skipped MOVEQ #1,D0 never ran");
        assert_eq!(cpu.d[1], 2, "the target ran");
    }

    /// The branch target keeps its bits above the 24-bit bus mask: `BRA.W
    /// $8000` from `$1000` is `$1002 - $8000 = $FFFF9002`, and `pc` ends there
    /// even though the fetch that follows is masked to `$00FF9002`. Measured:
    /// 907 of `DBcc.json.gz`'s non-skipped cases, 7 of `BSR.json.gz`'s and 4 of
    /// `Bcc.json.gz`'s end with `pc` above the mask (see #193, same rule for
    /// `LEA`/`PEA`).
    #[test]
    fn branch_target_is_not_masked_but_its_fetch_is() {
        let (mut cpu, mut bus) = run(0x6000, &[0x8000, 0x4E71]);
        bus.load_words(0x00FF_9002, &[0x1234, 0x5678]);

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, 0xFFFF_9002, "the whole 32-bit sum");
        assert_eq!(cpu.prefetch, [0x1234, 0x5678], "fetched through the mask");
    }

    /// `DBF D0,loop` with `D0 = 3` runs the body four times: the counter is
    /// decremented first and the branch is taken while the result is not -1, so
    /// the body runs `n + 1` times. `D0.w` ends at `$FFFF` and the upper word is
    /// untouched — the decrement is a word operation (M68000UM "DBcc — Decrement
    /// and Branch"; `DBcc.json.gz` never changes bits 31..16).
    #[test]
    fn dbf_loops_n_plus_1_times() {
        let (mut cpu, mut bus) = run(0x5201, &[0x51C8, 0xFFFC, 0x4E71]); // ADDQ.B #1,D1 / DBRA $FFFC
        cpu.d[0] = 0xDEAD_0003;

        let mut steps = 0;
        while cpu.pc != PROGRAM + 6 && steps < 10 {
            cpu.step(&mut bus);
            steps += 1;
        }

        assert_eq!(steps, 8, "body + DBRA four times, then the fall-through");
        assert_eq!(cpu.d[1], 4, "the body ran four times for a count of 3");
        assert_eq!(cpu.d[0], 0xDEAD_FFFF, "only the low word is decremented");
    }

    /// `DBcc` with the condition TRUE does not branch and does not decrement.
    #[test]
    fn dbcc_true_falls_through() {
        let (mut cpu, mut bus) = run(DBCC_D0, &[0x0FF0, 0x4E71]); // DBcc.T D0 — always true
        cpu.d[0] = 0xDEAD_BEEF;

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, PROGRAM + 4, "no branch");
        assert_eq!(cpu.d[0], 0xDEAD_BEEF, "and no decrement");
    }

    /// `DBcc` with the condition false and a counter of zero decrements to
    /// `$FFFF`, does not branch, and leaves `$FFFF` in the counter: the
    /// "iteration count exhausted" exit (M68000UM "DBcc").
    #[test]
    fn dbcc_expired_falls_through_with_ffff() {
        let (mut cpu, mut bus) = run(DBCC_D0 | (1 << 8), &[0x0FF0, 0x4E71]); // DBF D0
        cpu.d[0] = 0xDEAD_0000;

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, PROGRAM + 4, "the loop exits by falling through");
        assert_eq!(cpu.d[0], 0xDEAD_FFFF);
    }

    /// The base of a `DBcc` displacement is the address of the displacement word
    /// itself: from `$1000` with `$0FF0`, `DBF` lands on `$1FF2`, which is
    /// `$1002 + $0FF0` and neither `$1000 + $0FF0` nor `$1004 + $0FF0`. All 2005
    /// branch-taking, non-skipped cases of `DBcc.json.gz` agree on this base.
    #[test]
    fn dbcc_branch_base_is_the_displacement_word() {
        let (mut cpu, mut bus) = run(DBCC_D0 | (1 << 8), &[0x0FF0, 0x4E71]);
        cpu.d[0] = 1;

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, 0x0000_1FF2);
        assert_eq!(cpu.d[0], 0, "decremented once");
    }

    /// `SEQ D0` (`$57C0`) with Z set writes `$FF` to the low byte and leaves the
    /// other three alone (M68000UM "Scc — Set Conditional"; 1280 of `Scc.json.gz`'s
    /// cases are the `Dn` form and all of them keep bits 31..8).
    #[test]
    fn seq_writes_ff() {
        let (mut cpu, mut bus) = run(0x57C0, &[0x4E71]);
        cpu.d[0] = 0xDEAD_BEEF;
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_BEFF);
        assert_eq!(cpu.pc, PROGRAM + 2);
    }

    /// `SNE D0` (`$56C0`) with Z set writes `$00`: false is zero, not "leave it
    /// alone".
    #[test]
    fn sne_writes_00() {
        let (mut cpu, mut bus) = run(0x56C0, &[0x4E71]);
        cpu.d[0] = 0xDEAD_BEEF;
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_BE00);
    }

    /// `Scc` to memory: like `CLR`, the 68000 reads the destination byte before
    /// it writes it (M68000PM "Scc" — the read-free form is a later machine).
    /// Final state cannot show a read, so this is the one test here that
    /// inspects the bus, through [`RecordingBus`].
    #[test]
    fn scc_to_memory_reads_then_writes() {
        let mut bus = RecordingBus::new();
        bus.load(PROGRAM, &[0x57D0u16, 0x4E71] /* SEQ.B (A0), NOP */);
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        cpu.a[0] = MEM;
        bus.poke_byte(MEM, 0xBE);
        cpu.set_ccr(SR_Z);
        bus.accesses.clear();

        cpu.step(&mut bus);

        assert_eq!(
            &bus.accesses[..2],
            [Access::ReadByte(MEM), Access::WriteByte(MEM)],
            "SEQ.B (A0) reads the resolved byte and writes it once, in that order"
        );
        assert_eq!(bus.peek_byte(MEM), 0xFF, "Z was set, so SEQ is true");
    }

    /// Nothing in this group moves a flag: no branch, no `DBcc` and no `Scc`
    /// touches N, Z, V, C or X (M68000UM, "68000 CCR Affected by Instruction
    /// Set": all four are listed with "not affected").
    #[test]
    fn none_of_the_group_moves_a_flag() {
        let (mut cpu, mut bus) = run(0x6002, &[0x4E71]); // BRA taken
        cpu.set_ccr(ALL_FLAGS);
        cpu.step(&mut bus);
        assert_eq!(cpu.sr & ALL_FLAGS, ALL_FLAGS, "BRA taken");

        let (mut cpu, mut bus) = run(0x6700, &[0x0FF0, 0x4E71]); // BEQ not taken
        cpu.set_ccr(ALL_FLAGS);
        cpu.set_flag(SR_Z, false);
        let before = cpu.sr & ALL_FLAGS;
        cpu.step(&mut bus);
        assert_eq!(cpu.sr & ALL_FLAGS, before, "BEQ not taken");

        let (mut cpu, mut bus) = run(DBCC_D0 | (1 << 8), &[0xFFFC, 0x4E71]); // DBF taken
        cpu.d[0] = 1;
        cpu.set_ccr(ALL_FLAGS);
        cpu.step(&mut bus);
        assert_eq!(cpu.sr & ALL_FLAGS, ALL_FLAGS, "DBF taken");

        let (mut cpu, mut bus) = run(0x57C0, &[0x4E71]); // SEQ false
        cpu.set_ccr(ALL_FLAGS);
        cpu.set_flag(SR_Z, false);
        let before = cpu.sr & ALL_FLAGS;
        cpu.step(&mut bus);
        assert_eq!(cpu.sr & ALL_FLAGS, before, "SEQ false");
    }

    /// `Bcc` branches exactly when [`test_cc`] says so, for all sixteen codes —
    /// the check that the handler shares the table rather than re-deriving it.
    #[test]
    fn bcc_uses_the_shared_condition_table() {
        for sr in [0u16, SR_N | SR_Z | SR_V | SR_C] {
            for cc in 0..0x10u16 {
                if cc == 0b0001 {
                    continue; // that one is BSR, pinned by `bsr_pushes_return`
                }
                let (mut cpu, mut bus) = run(BCC_PLUS_2 | (cc << 8), &[0x4E71]);
                cpu.set_ccr(sr);

                cpu.step(&mut bus);

                let taken = test_cc(cc as u8, sr);
                assert_eq!(
                    cpu.pc,
                    if taken { PROGRAM + 4 } else { PROGRAM + 2 },
                    "cc {cc:04b} at SR {sr:04X}"
                );
            }
        }
    }

    /// The same for `Scc`: every code writes `$FF` exactly when [`test_cc`] says
    /// the condition holds.
    #[test]
    fn scc_uses_the_shared_condition_table() {
        for sr in [0u16, SR_N | SR_Z | SR_V | SR_C] {
            for cc in 0..0x10u16 {
                let (mut cpu, mut bus) = run(SCC_D0 | (cc << 8), &[0x4E71]);
                cpu.d[0] = 0x100;
                cpu.set_ccr(sr);

                cpu.step(&mut bus);

                let want: u32 = if test_cc(cc as u8, sr) { 0xFF } else { 0 };
                assert_eq!(cpu.d[0] & 0xFF, want, "cc {cc:04b} at SR {sr:04X}");
            }
        }
    }

    /// What [`RecordingBus`] records: one bus access, in the order the CPU made
    /// it, with the access width in the variant name (`Scc` is a byte
    /// instruction, so a stray word access is the bug worth naming).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Access {
        ReadByte(u32),
        WriteByte(u32),
        ReadWord(u32),
        WriteWord(u32),
    }

    /// A bus that keeps the addresses it was asked about, which
    /// [`crate::testbus::TestBus`] deliberately does not: the only way to see a
    /// read that throws its value away is to count it (as in
    /// `arith::tests::clr_to_memory_reads_then_writes`, #22).
    struct RecordingBus {
        mem: Vec<u8>,
        accesses: Vec<Access>,
    }

    impl RecordingBus {
        fn new() -> Self {
            Self {
                mem: vec![0; 1 << 20],
                accesses: Vec::new(),
            }
        }

        /// The reset vector the CPU needs: SSP at $000000 and the program at
        /// `PROGRAM`.
        fn load(&mut self, addr: u32, words: &[u16]) {
            self.poke_long(0, STACK_TOP);
            self.poke_long(4, PROGRAM);
            for (n, word) in words.iter().enumerate() {
                let where_ = addr + 2 * n as u32;
                self.poke_word(where_, *word);
            }
        }

        fn poke_byte(&mut self, addr: u32, value: u8) {
            self.mem[addr as usize] = value;
        }

        fn poke_word(&mut self, addr: u32, value: u16) {
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }

        fn poke_long(&mut self, addr: u32, value: u32) {
            let i = addr as usize;
            self.mem[i..i + 4].copy_from_slice(&value.to_be_bytes());
        }

        fn peek_byte(&self, addr: u32) -> u8 {
            self.mem[addr as usize]
        }
    }

    impl Bus for RecordingBus {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.accesses.push(Access::ReadByte(addr));
            self.mem[addr as usize]
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.accesses.push(Access::ReadWord(addr));
            let i = addr as usize;
            u16::from_be_bytes([self.mem[i], self.mem[i + 1]])
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.accesses.push(Access::WriteByte(addr));
            self.mem[addr as usize] = value;
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.accesses.push(Access::WriteWord(addr));
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }
    }
}
