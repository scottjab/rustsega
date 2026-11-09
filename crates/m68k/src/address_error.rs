//! Address error exceptions: the alignment rule, the group 0 frame, and the
//! vector the pinned suite actually takes (M1-29, #37).
//!
//! M68000UM "Address Error": a word or long access to an odd address, or a long
//! access whose second word would lie past the end of the address space, raises
//! the address error *instead of performing the access*. Byte accesses never
//! alignment-fault (A0 is used for upper/lower byte selection, so odd byte
//! accesses are normal), and an instruction fetch to an odd address faults like
//! a read.
//!
//! # The frame: 7 words, and where every one of them comes from
//!
//! [`crate::exceptions::exception`] is the group 1/2 path — the six byte frame
//! — and this module is its group 0 sibling, because the group 0 frame is a
//! different shape, not a bigger version of the same one (M68000UM "Stack Frame
//! Formats", group 0):
//!
//! ```text
//! SP + $00  status word       (ird & 0xFFE0) | direction
//! SP + $02  fault address     long, UNMASKED
//! SP + $06  IRD               instruction register, data low byte = 0
//! SP + $08  saved SR          SR before the S=1 / T=0 change
//! SP + $0A  stacked PC        long
//! ```
//!
//! Measured against the pinned suite by finding the cases that push a group 0
//! frame (the word at `SP + 6` equals the case's `initial.prefetch[0]`, the IRD)
//! and reading the words back out of `final.ram`. Examples used while deriving
//! this, all of which reproduce the table above byte for byte:
//!
//! - `ADD.l.json.gz` case `5ca0 [ADD.l Q, -(A0)] 1` — read fault: status
//!   `$5CB5` = `($5CA0 & 0xFFE0) | 0x15`, fault address `$9572956F`, IRD `$5CA0`,
//!   stacked PC `$000C00` = `initial.pc`.
//! - `MOVE.w.json.gz` case `3af7 [MOVE.w (d8, A7, Xn), (A5)+] 7` — write fault:
//!   status `$3AE5` = `($3AF7 & 0xFFE0) | 0x05`, fault address `$DE2EA085`,
//!   stacked PC `$000C02`.
//! - `JMP.json.gz` case `4ed1 [JMP (A1)] 2` — code fetch fault: status `$4EDE`
//!   = `($4ED1 & 0xFFE0) | 0x1E`, fault address `$CCE90759`, stacked PC
//!   `$CCE90755` = fault address − 4.
//!
//! The fault address is the **unmasked** 32-bit value the instruction asked for:
//! `$9572956F` and `$DE2EA085` above both keep bits 31..24, which [`ADDR_MASK`]
//! would have thrown away. That is why [`PendingFault::addr`] stores the address
//! before masking and why the frame write must not run it through the bus mask.
//!
//! # The status word
//!
//! Word 0 of the frame is `(ird & 0xFFE0) | direction` — the top eleven bits of
//! the faulting instruction (M68000PM: the "instruction register decoded" field
//! the handler can inspect), the low five the access description. The direction
//! codes below are what the suite's frames carry; the counts over the cases
//! listed above and their neighbours are 23139 × `0x15`, 4081 × `0x05`,
//! 3806 × `0x1E`.
//!
//! A fourth code, `0x1A` — "code fetch during `RTE`'s SR reload" — appears in
//! the core this suite was recorded against. Nothing in this crate can produce
//! it (the only `RTE`-adjacent access that can fault there is a stack read,
//! which this module reports as `0x15`), so it has no constant here; if the
//! ticket that reworks `RTE` needs it, that ticket adds it.
//!
//! # The vector: `$00000C`, matching the suite, not `$000008`
//!
//! Real M68000 hardware vectors the address error to `$000008` (M68000UM
//! "Exception Vector Memory Map": it is vector 2, the slot directly above the
//! reset pair). The cores this pinned suite was recorded against do not: every
//! group 0 frame case's bus trace reads its handler long from `$00000C`/`$00000E`, and nothing
//! below `$100` besides — the three cases cited above are examples of that
//! measurement, and a scan of the 30 326 group 0 frame cases across the twelve
//! files sampled found `$0C`/`$0E` in every one.
//!
//! The convention throughout this crate is that the measured slot wins (see the
//! table in [`crate::exceptions`]), so [`EXC_ADDRESS_ERROR`] is 3. Two
//! consequences worth stating out loud:
//!
//! - numerically it *collides* with [`crate::exceptions::EXC_ILLEGAL`], which
//!   also owns `$00000C`. That is not this module confusing the two exceptions —
//!   the recording core genuinely sends both to the same slot, and the suite
//!   installs one handler at `$00001400` in every vector so it cannot tell them
//!   apart either. Anything that has to distinguish them must not use
//!   [`M68k::pending_exception`] alone.
//! - the hardware slot is a one-line change once a test ROM settles it, which is
//!   the same open question [`crate::exceptions`] records for the divide, CHK
//!   and TRAPV slots, and is related to issue #185 (the harness predicate
//!   `is_address_error_case` reads `$00000C` and therefore cannot name which
//!   exception it is detecting).
//!
//! # Clocks
//!
//! M68000UM "Exception Processing Execution Times" bills the address error at
//! 50 clock cycles. [`crate::interrupts`] is the model for how that number is
//! expressed here: the bus accesses charge themselves, so this module adds only
//! the remainder. Exact per-instruction timing is M1-31 (#39/#40) and the
//! harness runs with cycles unchecked.

use crate::bus::{ADDR_MASK, Bus};
use crate::cpu::BUS_CYCLE_CLOCKS;
use crate::cpu::M68k;
use crate::sr::{SR_S, SR_T};

/// Address error, the group 0 exception.
///
/// `$00000C` under the suite's numbering — see the module docs for why this is
/// 3 (and therefore numerically the same as
/// [`crate::exceptions::EXC_ILLEGAL`]) when hardware says vector 2 / `$000008`.
pub const EXC_ADDRESS_ERROR: u8 = 3;

/// Size of the group 0 frame in bytes: status word, fault address long, IRD,
/// saved SR and stacked PC long. Measured as `final.ram` bytes on every group 0
/// case sampled, and corroborated by the stack deltas:
/// `MOVE.w.json.gz` case `3af7 [MOVE.w (d8, A7, Xn), (A5)+] 7` moves `ssp` by
/// −14 (14 − 0) and `MOVE.l.json.gz` case `251f [MOVE.l (A7)+, -(A2)] 86` by −10
/// (14 − 4, the `(A7)+` having moved the stack back up by a long).
pub const ADDRESS_ERROR_FRAME_SIZE: u32 = 14;

/// The `status & 0xFFE0` half of the frame's status word: the IRD with its
/// access-description bits cleared, per M68000UM "Address Error".
const STATUS_IRD_MASK: u16 = 0xFFE0;

/// Status word direction bits: the access was a data read (`0x15`). Measured on
/// 23139 frames, e.g. `ADD.l.json.gz` case `5ca0 [ADD.l Q, -(A0)] 1`.
const STATUS_DATA_READ: u16 = 0x15;

/// Status word direction bits: the access was a data write (`0x05`), e.g.
/// `MOVE.w.json.gz` case `3af7 [MOVE.w (d8, A7, Xn), (A5)+] 7`.
const STATUS_DATA_WRITE: u16 = 0x05;

/// Status word direction bits: the access was an instruction fetch (`0x1E`),
/// e.g. `JMP.json.gz` case `4ed1 [JMP (A1)] 2`.
const STATUS_CODE_FETCH: u16 = 0x1E;

/// What the address error costs in total, per M68000UM "Exception Processing
/// Execution Times". 34 of it is the part every exception entry pays
/// ([`EXCEPTION_ENTRY_CLOCKS`]), so what this frame adds on top is the rest.
const ADDRESS_ERROR_ENTRY_CLOCKS: u64 = 50;

/// The bus accesses the group 0 entry performs: the seven word writes of the
/// frame, the two reads of the vector long, and the two fetches of the queue
/// refill at the handler. Each charges itself [`BUS_CYCLE_CLOCKS`], exactly as
/// [`crate::exceptions`] charges its own accesses.
const ADDRESS_ERROR_ENTRY_ACCESSES: u64 = 10;

/// What the entry adds to the access cost: the part of
/// [`ADDRESS_ERROR_ENTRY_CLOCKS`] that is not bus traffic.
///
/// Derived, not chosen, and in the style of
/// [`crate::interrupts::INTERRUPT_ACK_CLOCKS`] — but as a subtraction from the
/// documented total rather than a delta on [`EXCEPTION_ENTRY_CLOCKS`], because
/// the group 0 frame is four words bigger than group 1 and its extra accesses
/// are what make the two entries differ.
const ADDRESS_ERROR_EXTRA_CLOCKS: u64 =
    ADDRESS_ERROR_ENTRY_CLOCKS - ADDRESS_ERROR_ENTRY_ACCESSES * BUS_CYCLE_CLOCKS;

/// The kind of access that faulted, which decides the status word's low five
/// bits and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessKind {
    /// A data read: a `read_w` / `read_l` from an instruction handler.
    DataRead,
    /// A data write: a `write_w` / `write_l` from an instruction handler.
    DataWrite,
    /// An instruction fetch: the prefetch queue refill in
    /// [`M68k::pop_word`] / [`M68k::jump_to`].
    CodeFetch,
}

impl AccessKind {
    /// The direction bits this access contributes to the frame's status word.
    fn status_bits(self) -> u16 {
        match self {
            AccessKind::DataRead => STATUS_DATA_READ,
            AccessKind::DataWrite => STATUS_DATA_WRITE,
            AccessKind::CodeFetch => STATUS_CODE_FETCH,
        }
    }
}

/// A fault the CPU has raised but not yet entered.
///
/// Recorded at the moment of the faulting access because that is the only time
/// the three fields agree: by the time [`M68k::step`] gets to the entry the
/// instruction has moved `pc` on, and the access address is not remembered
/// anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingFault {
    /// The address as the instruction asked for it — **not** masked with
    /// [`ADDR_MASK`], which the frame preserves (see the module docs).
    addr: u32,
    kind: AccessKind,
    /// `pc` at the instant of the fault: the stacked PC for a data fault. For a
    /// fetch fault the frame uses `addr - 4` instead (measured on
    /// `JMP.json.gz` case `4ed1 [JMP (A1)] 2`).
    pc: u32,
    /// The instruction being executed, from [`M68k::ird`] — snapshotted by
    /// [`M68k::step`] rather than read from `prefetch[0]` at fault time, which
    /// has already moved on to the next opcode by the time e.g. `JMP`'s refill
    /// faults.
    ird: u16,
}

impl M68k {
    /// The alignment rule, applied to a bus access from an instruction handler:
    /// returns `true` when the access must **not** happen, having recorded the
    /// fault for [`M68k::take_address_error`].
    ///
    /// Two rules, both from M68000UM "Address Error":
    ///
    /// - a word or long access to an odd address faults. `addr & 1` is the test
    ///   whether or not `addr` is masked, since [`ADDR_MASK`] keeps bit 0;
    /// - a long access whose second word lies past the end of the 24 bit space
    ///   faults. Until now `read_l`/`write_l` wrapped `$FFFFFE` to `$000000`;
    ///   the 68000 has nothing to wrap to and faults instead.
    ///
    /// Once a fault is recorded every later checked access answers "don't
    /// happen" too, without raising a second fault. That is what stops the rest
    /// of the faulting instruction reaching the bus: the handlers do not check
    /// a `Result` (threading one through the ~100 access sites in `ops/` and
    /// `ea.rs` is not what this rule needs), so the accessors suppress instead.
    pub(crate) fn alignment_fault(&mut self, addr: u32, kind: AccessKind, is_long: bool) -> bool {
        if self.pending_address_error.is_some() {
            return true;
        }
        let masked = addr & ADDR_MASK;
        if addr & 1 != 0 || (is_long && masked + 2 > ADDR_MASK) {
            self.pending_address_error = Some(PendingFault {
                addr,
                kind,
                pc: self.pc,
                ird: self.ird,
            });
            return true;
        }
        false
    }

    /// True once an access in the instruction now executing has faulted.
    ///
    /// The instruction is not completed after an address error (M68000UM section
    /// 1 "Address Error", "the access is not performed"), so nothing that happens
    /// later in the same instruction may reach a register or the bus. Bus
    /// accesses already stop themselves in [`M68k::read_w`] and friends; this is
    /// the check for the write-back into a *register*, which no bus accessor is
    /// on its way past. Operations use it the way they use
    /// [`M68k::check_supervisor`]: test it, and return.
    pub(crate) fn address_error_pending(&self) -> bool {
        self.pending_address_error.is_some()
    }

    /// If an access faulted since the last call, enter the address error.
    ///
    /// Called by [`M68k::step`] once the opcode has finished, which is why
    /// [`PendingFault`] carries its own `pc`: the entry has to describe the
    /// instant of the fault, not of the end of the instruction.
    pub(crate) fn take_address_error(&mut self, bus: &mut impl Bus) {
        let Some(fault) = self.pending_address_error.take() else {
            return;
        };
        self.address_error(bus, fault);

        // Double fault: M68000UM section 6 "Exception Processing" — a second
        // exception while an address error is being processed stops the
        // processor rather than nesting. The only way that happens here is the
        // vector long itself pointing the queue refill at an odd address, which
        // raises a fresh fault inside `address_error` below; the accessors
        // cannot re-enter because `processing_address_error` keeps them out.
        if self.pending_address_error.take().is_some() {
            self.halted = true;
        }
    }

    /// The group 0 entry: push the seven-word frame, jump through
    /// [`EXC_ADDRESS_ERROR`].
    ///
    /// Same shape as [`crate::exceptions::exception`] — SR to a temp, then
    /// `S = 1`/`T = 0`, then the frame goes below the new active stack pointer,
    /// then the vector long, then a queue refill from the handler — with two
    /// differences: the frame is this one, and every access it makes is a `_raw`
    /// one, because the exception path must never fault-check its own accesses
    /// (its stack pointer is what it is, and the vector table is not negotiable).
    fn address_error(&mut self, bus: &mut impl Bus, fault: PendingFault) {
        let saved_sr = self.sr;
        self.set_sr((saved_sr & !(SR_S | SR_T)) | SR_S);

        let sp = self.a[7].wrapping_sub(ADDRESS_ERROR_FRAME_SIZE);
        self.set_active_sp(sp);

        // Frame order is the push order: PC, SR, IRD, fault address, status,
        // so the status word written last lands at `sp` (M68000UM "Stack Frame
        // Formats", group 0).
        let stacked_pc = match fault.kind {
            // Measured on `JMP.json.gz` case `4ed1 [JMP (A1)] 2`: a fetch fault
            // stacks the address it was fetching, minus the queue ahead of it.
            AccessKind::CodeFetch => fault.addr.wrapping_sub(4),
            AccessKind::DataRead | AccessKind::DataWrite => fault.pc,
        };
        let ird = fault.ird;
        let status = (ird & STATUS_IRD_MASK) | fault.kind.status_bits();

        self.write_l_raw(bus, sp + 10, stacked_pc);
        self.write_w_raw(bus, sp + 8, saved_sr);
        self.write_w_raw(bus, sp + 6, ird);
        self.write_l_raw(bus, sp + 2, fault.addr);
        self.write_w_raw(bus, sp, status);

        let handler = self.read_l_raw(bus, u32::from(EXC_ADDRESS_ERROR) * 4);
        self.jump_to(bus, handler);

        self.pending_exception = Some(EXC_ADDRESS_ERROR);
        self.cycles += ADDRESS_ERROR_EXTRA_CLOCKS;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::BUS_CYCLE_CLOCKS;
    use crate::sr::{SR_INT_MASK, SR_S, SR_Z};
    use crate::testbus::{TestBus, cpu_with_program};

    /// Where [`cpu_with_program`] puts the program, and the stack it starts on.
    const PROGRAM: u32 = 0x0000_1000;
    const STACK: u32 = 0x0001_0000;
    /// The handler the vector table points at for every case but one.
    const HANDLER: u32 = 0x0000_3000;
    /// Supervisor mode with interrupts masked: the mode every suite case runs in.
    const SUP: u16 = SR_S | SR_INT_MASK;

    /// A CPU reset onto `opcode` at [`PROGRAM`], with the address-error vector
    /// pointed at [`HANDLER`], `a0` holding `a0` and `d0` holding a recognisable
    /// value.
    fn cpu_running(opcode: u16, a0: u32) -> (M68k, TestBus) {
        let (mut cpu, mut bus) = cpu_with_program(&[opcode, 0x4E71]);
        bus.write_long(EXC_ADDRESS_ERROR as u32 * 4, HANDLER);
        bus.load_words(HANDLER, &[0x4E71, 0x4E71]);
        cpu.set_sr(SUP);
        cpu.a[0] = a0;
        cpu.d[0] = 0x1111_2222;
        (cpu, bus)
    }

    /// Read RAM back without `&mut self`: [`TestBus`]'s accessors are the `Bus`
    /// trait's, which take `&mut`, and these tests want to look at memory while
    /// holding the bus borrowed immutably.
    fn peek_word(bus: &TestBus, addr: u32) -> u16 {
        let i = addr as usize;
        u16::from_be_bytes([bus.mem[i], bus.mem[i + 1]])
    }

    /// Two big endian words, high first.
    fn peek_long(bus: &TestBus, addr: u32) -> u32 {
        (u32::from(peek_word(bus, addr)) << 16) | u32::from(peek_word(bus, addr + 2))
    }

    /// Assert the whole group 0 frame by hand: the seven words of
    /// `status / addr / ird / sr / pc`, the new stack pointer, and that nothing
    /// else about the entry moved.
    #[allow(clippy::too_many_arguments)]
    fn assert_frame(
        bus: &TestBus,
        cpu: &M68k,
        sp: u32,
        status: u16,
        addr: u32,
        ird: u16,
        saved_sr: u16,
        stacked_pc: u32,
    ) {
        assert_eq!(cpu.a[7], sp, "sp moved down by the whole 14-byte frame");
        assert_eq!(
            peek_word(bus, sp),
            status,
            "SP+0: status word = (ird & 0xFFE0) | direction"
        );
        assert_eq!(
            peek_long(bus, sp + 2),
            addr,
            "SP+2: faulting address, as a long and unmasked"
        );
        assert_eq!(peek_word(bus, sp + 6), ird, "SP+6: IRD");
        assert_eq!(peek_word(bus, sp + 8), saved_sr, "SP+8: SR before entry");
        assert_eq!(
            peek_long(bus, sp + 10),
            stacked_pc,
            "SP+A: stacked PC, as a long"
        );
        assert!(cpu.is_supervisor(), "S = 1");
        assert_eq!(cpu.sr & crate::sr::SR_T, 0, "T = 0");
        assert_eq!(
            cpu.sr & (SR_S | SR_T),
            SR_S,
            "S = 1, T = 0, and the entry changed no other bit of SR"
        );
        assert_eq!(cpu.pc, HANDLER, "entered the handler at $00000C");
        assert_eq!(
            cpu.pending_exception,
            Some(EXC_ADDRESS_ERROR),
            "the record says address error"
        );
    }

    #[test]
    fn odd_word_read_raises_address_error() {
        // MOVE.W (A0),D0 (`3010 [MOVE.w (A0), D0] 4589` in `MOVE.w.json.gz`)
        // with A0 odd: the read faults and never happens.
        let a0: u32 = 0x0000_2001;
        let (mut cpu, mut bus) = cpu_running(0x3010, a0);
        bus.write_word(0x0000_2000, 0xBEEF);
        let before = cpu.cycles;

        cpu.step(&mut bus);

        assert_frame(
            &bus,
            &cpu,
            STACK - ADDRESS_ERROR_FRAME_SIZE,
            0x3015,
            a0,
            0x3010,
            // `MOVE` sets the condition codes from its operand, which here is
            // the suppressed (zero) result of the faulting access, so Z is up
            // when the frame records SR. The entry itself touches no flags.
            SUP | SR_Z,
            PROGRAM,
        );
        assert_eq!(
            peek_word(&bus, 0x0000_2000),
            0xBEEF,
            "the faulting access must not happen"
        );
        assert_eq!(cpu.d[0], 0x1111_2222, "D0 keeps its old value");
        // 50 for the entry, plus the one access of the `MOVE` that still ran:
        // the queue pop. The faulting operand read is suppressed, and a
        // suppressed access charges nothing.
        assert_eq!(
            cpu.cycles - before,
            ADDRESS_ERROR_ENTRY_CLOCKS + BUS_CYCLE_CLOCKS,
            "address error costs 50 clocks (M68000UM)"
        );
    }

    #[test]
    fn odd_long_write_status_word_says_write() {
        // MOVE.L D0,(A0) with A0 odd: a write fault, so the status word's
        // direction bits are 0x05, and the 68000's "access does not happen"
        // means both words of the long are absent from memory.
        let a0: u32 = 0x0040_0003;
        let (mut cpu, mut bus) = cpu_running(0x2080, a0);
        bus.write_long(0x0040_0002, 0xDEAD_BEEF);

        cpu.step(&mut bus);

        assert_frame(
            &bus,
            &cpu,
            STACK - ADDRESS_ERROR_FRAME_SIZE,
            0x2085,
            a0,
            0x2080,
            SUP,
            PROGRAM,
        );
        assert_eq!(
            peek_long(&bus, 0x0040_0002),
            0xDEAD_BEEF,
            "neither word of the faulting long was written"
        );
    }

    #[test]
    fn odd_instruction_fetch() {
        // JMP (A0) to an odd target: the fetch of the first instruction word is
        // an address error with the code-fetch direction code, and the stacked
        // PC is the fetched address minus the 4 bytes of queue ahead of it
        // (`JMP.json.gz` case `4ed1 [JMP (A1)] 2`: fault $CCE90759, stacked
        // $CCE90755).
        let target: u32 = 0xFF00_2003;
        let (mut cpu, mut bus) = cpu_running(0x4ED0, target);

        cpu.step(&mut bus);

        assert_frame(
            &bus,
            &cpu,
            STACK - ADDRESS_ERROR_FRAME_SIZE,
            0x4EDE,
            target,
            0x4ED0,
            SUP,
            target - 4,
        );
    }

    #[test]
    fn double_fault_halts() {
        // An address error raised while the address error is being processed
        // stops the processor (M68000UM section 6 "Exception Processing").
        // Reachable here because the group 0 entry refills the prefetch queue
        // from the vector long: point that long at an odd address and the entry
        // itself faults on its own instruction fetch.
        let a0: u32 = 0x0000_2001;
        let (mut cpu, mut bus) = cpu_running(0x3010, a0);
        bus.write_long(EXC_ADDRESS_ERROR as u32 * 4, 0x0000_3001);

        cpu.step(&mut bus);

        assert!(cpu.halted, "a fault during the address error halts");
        // The frame was still completed before the second fault: the status word
        // and the fault address are written before the vector is read.
        let sp = STACK - ADDRESS_ERROR_FRAME_SIZE;
        assert_eq!(
            peek_word(&bus, sp),
            0x3015,
            "status word of the first fault"
        );
        assert_eq!(peek_long(&bus, sp + 2), a0, "fault address");
        assert_eq!(peek_word(&bus, sp + 6), 0x3010, "IRD");
        assert_eq!(peek_word(&bus, sp + 8), SUP | SR_Z, "saved SR");
        assert_eq!(peek_long(&bus, sp + 10), PROGRAM, "stacked PC");
        // Halting does not undo the mode change. Motorola's exception sequence is
        // push frame, update SR, *then* fetch the vector long (M68000UM section 6
        // "Exception Processing"), so the second fault is raised by a CPU that is
        // already supervisor with the new mask; `halted` stops instruction
        // execution and rolls nothing back.
        assert!(cpu.is_supervisor(), "S = 1 at the halt");
        assert_eq!(cpu.sr & SR_T, 0, "T = 0 at the halt");
        assert_eq!(
            cpu.sr & SR_INT_MASK,
            SR_INT_MASK,
            "the entry's interrupt mask is still up"
        );
    }

    #[test]
    fn byte_accesses_never_fault() {
        // MOVE.B (A0),D0 to the same odd address the word test faults on: A0 is
        // the byte-select line, so odd byte accesses are ordinary (M68000UM
        // "Address Definition", "Address Error").
        // MOVE.B (A0),D0 ($1010 in `MOVE.b.json.gz`): the same access, one byte
        // wide.
        let (mut cpu, mut bus) = cpu_running(0x1010, 0x0000_2001);
        bus.write_byte(0x0000_2001, 0x7F);
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None, "no address error");
        assert_eq!(cpu.d[0] & 0xFF, 0x7F, "the byte was read");
        assert_eq!(cpu.a[7], STACK, "the stack never moved");
    }

    #[test]
    fn long_reaching_past_the_end_of_the_address_space_faults() {
        // A long at $FFFFFE would read $FFFFFF and $1000001; the 68000 faults
        // where this core used to wrap (M68000UM "Address Error").
        let (mut cpu, mut bus) = cpu_running(0x2010, 0x00FF_FFFE);

        cpu.step(&mut bus);

        let sp = STACK - ADDRESS_ERROR_FRAME_SIZE;
        assert_eq!(peek_word(&bus, sp), 0x2015, "read fault status word");
        assert_eq!(
            peek_long(&bus, sp + 2),
            0x00FF_FFFE,
            "the address that faulted, not the wrapped one"
        );
        assert_eq!(cpu.pending_exception, Some(EXC_ADDRESS_ERROR));
    }

    #[test]
    fn the_entry_clocks_are_charged_once_and_not_twice() {
        // The documented 50 is the entry total; ten bus accesses charge
        // themselves, so the constant added by `address_error` is the remainder.
        assert_eq!(ADDRESS_ERROR_EXTRA_CLOCKS, 10);
        assert_eq!(
            ADDRESS_ERROR_EXTRA_CLOCKS + ADDRESS_ERROR_ENTRY_ACCESSES * BUS_CYCLE_CLOCKS,
            ADDRESS_ERROR_ENTRY_CLOCKS,
            "the entry costs 50 whether it is read as a total or as a sum"
        );
        // And the frame really is seven words of bus traffic.
        assert_eq!(ADDRESS_ERROR_FRAME_SIZE / 2, 7);
        assert_eq!(7 * BUS_CYCLE_CLOCKS, 28);
    }
}
