//! Exception entry: the group 1 / group 2 frame, the vector map, and the two
//! opcodes that raise an exception by themselves (M1-08, #16).
//!
//! [`M68k::exception`] is the single entry point every exception route uses:
//! TRAP (#16), illegal instruction / line A / line F (#15, wired up here), the
//! privilege check for the privileged instructions (this module's
//! [`M68k::check_supervisor`]), then CHK, TRAPV and divide-by-zero from their
//! own instruction tickets, and interrupts (M1-28, #36) on top of the mask
//! change. Group 0 — address error and bus error — pushes a bigger frame and lives
//! in [`crate::address_error`] (M1-29, #37); nothing here builds one.
//!
//! # The vector map, measured
//!
//! The convention throughout this crate is **the vector long for vector `n`
//! lives at `$000000 + 4 * n`** — the number is the slot address divided by
//! four. That is not a style choice: the pinned suite fetches from those slots,
//! and the numbers below are what its cases actually read. Motorola's
//! "Exception Vector Memory Map" figure labels the same slots one higher,
//! because it numbers the reset pair 1 and 2 instead of 0 and 1, and tickets
//! that quote the figure are off by one against this table. Take the slot
//! address from the hardware map, divide by four, and that is the number here.
//!
//! | Constant | Vector | Long at | Evidence |
//! |---|---|---|---|
//! | — | 0, 1 | `$00`, `$04` | reset SSP / PC ([`crate::M68k::reset`]) |
//! | — | 2 | `$08` | address error **on hardware**: the slot is claimed by [`crate::address_error::EXC_ADDRESS_ERROR`] at `$0C`, which is what the suite's frames read — see that module for the discrepancy |
//! | [`crate::address_error::EXC_ADDRESS_ERROR`] | 3 | `$0C` | address error on the hardware map (M68000UM Table 6-2) and in every group-0 case of the suite, which fetches `$0C`; the six #436 carts put halt stubs there, and `Warrior of Rome II` stores `#$3` into its exception-code word |
//! | [`EXC_ILLEGAL`] | 4 | `$10` | M68000UM Table 6-2 ("Exception Vector Assignment"), and all six #436 carts put their illegal-instruction handler there while halting on `$0C` (`Warrior of Rome II`'s `$10` stub stores `#$4`). The suite cannot arbitrate this slot: its illegal-effective-address cases fetch `$0C` because the recording core routed illegal to the address-error slot (`tests/harness/mod.rs` documents this), and the suite has no line-A/F file to pin the neighbouring rows either |
//! | [`EXC_DIV_ZERO`] | 5 | `$14` | `DIVU.json.gz` case `80ef [DIVU (d16, A7), D0] 5745`: divisor word at `$0000F0` is zero, the case reads the long at `$14`/`$16` and the frame goes on the stack. Only case in the suite that divides by zero |
//! | [`EXC_CHK`] | 6 | `$18` | 4272 of the 7389 exception cases in `CHK.json.gz` fetch `$18` (the rest are illegal-EA cases fetching the recording core's `$0C`) |
//! | [`EXC_TRAPV`] | 7 | `$1C` | all 4095 exception cases in `TRAPV.json.gz` fetch `$1C` |
//! | [`EXC_PRIVILEGE`] | 8 | `$20` | **not covered by the suite** — every case in every file runs in supervisor mode (`sr & $2000` set in all 8065 `TRAP.json.gz` cases, and `RESET`, `MOVEtoUSP`, `MOVEfromUSP` and friends have no exception cases at all), so no case can show which slot a privilege violation fetches. Chosen as the slot after [`EXC_TRAPV`], which is where both the ticket's map and the measured `CHK`/`TRAPV`/divide-by-zero sequence put it |
//! | [`EXC_TRACE`] | 9 | `$24` | **not covered by the suite** either: no case in the 125 files starts with `sr & $8000` set (checked over all 1 000 060 cases), so nothing fetches the trace vector. The slot is the hardware map's — M68000UM "Exception Vector Memory Map", the group 2 slot between privilege violation and the spurious-interrupt slot |
//! | [`EXC_LINE_A`] | 10 | `$28` | M68000UM Table 6-2, row 10, and all six #436 carts populate it with a real handler wherever they execute an A-line opcode on purpose (`Mamono Hunter Youko`'s `$28` handler is a compact VDP register write, see the line section below) |
//! | [`EXC_LINE_F`] | 11 | `$2C` | M68000UM Table 6-2, row 11, and the same six carts populate it (`Warrior of Rome II`'s `$2C` handler dispatches through a jump table) |
//! | — | 24, 25..31 | `$60`, `$64`–`$7C` | spurious and level 1–7 autovectors: M1-28 (#36), taken by [`crate::interrupts`] |
//! | [`trap_vector`] | 32 + n | `$80 + 4 * n` | all 8065 `TRAP.json.gz` cases: `4e40` reads `$80`, `4e42` reads `$88`, `4e4f` reads `$BC` |
//!
//! The suite pins the CHK/TRAPV/divide-by-zero/trap rows and — through the
//! recording core's own routing — the illegal and address-error rows share one
//! slot. It cannot settle the two rows between them: those come from M68000UM
//! Table 6-2 plus the one map every Genesis cart is written against (all six
//! #436 carts agree on it, down to storing exception codes 2/3/4 in their
//! `$08`/`$0C`/`$10` halt stubs). The slots the suite does pin are one slot
//! above where some Genesis documentation puts divide-by-zero, CHK and TRAPV;
//! that discrepancy stays on an issue for a hardware test ROM to settle, and
//! nothing outside this table depends on it.
//!
//! # The line-A and line-F vectors are the 68000's own
//!
//! M68000UM Table 6-2 ("Exception Vector Assignment", section 6-7) lists line
//! 1010 emulator at vector 10, long `$028`, and line 1111 emulator at vector
//! 11, long `$02C`, as plain rows: the only row in the whole table footnoted
//! "MC68010 only" is Format Error (14). Section 6.3.6 says it outright —
//! "word patterns with bits 15–12 equaling 1010 or 1111 are distinguished as
//! unimplemented instructions, and separate exception vectors are assigned to
//! these patterns to permit efficient emulation" — so the separate vectors are
//! a 68000 feature, not something the 68010 introduced. What the 68010 adds on
//! top is mid-instruction restart (the format/offset word and the
//! restart-capable frames) and the RTE-based emulation mode, and *those* this
//! crate still does not implement.
//!
//! Genesis carts treat the two vectors as part of the machine: all six of the
//! #436 black carts execute an A-line or F-line opcode on purpose and populate
//! the matching slot with a real handler while parking halt stubs on `$08`/
//! `$0C`/`$10`. `Mamono Hunter Youko`'s `$28` handler is the compact one, and
//! it shows the mechanism: `MOVEM.L D7/A4,-(A7)`, `MOVEA.L $A(A7),A0` (the
//! frame PC, the opcode's own address), `ADDQ.L #$2,$A(A7)` (step over the
//! one-word opcode), `MOVE.W (A0),D0`, apply `$8000`, write `$C00004`, `RTE` —
//! an A-line opcode used as a VDP register write. On the crate's old single
//! vector those carts fell into their own halt stub and never drew.
//!
//! # The frame, and which PC goes into it
//!
//! Group 1/2 sequence (M68000UM section 6 "Exception Processing", M68000PM
//! section 1.3): copy SR to a temp; set S = 1 and clear T; push the PC long,
//! then the saved SR word, so the final layout is **SR at SP, PC at SP+2**;
//! fetch the handler long from the vector and refill the queue from it.
//!
//! Measured against the suite, in this crate's `pc` convention (see
//! [`crate::M68k::pc`] — `pc` is the address of the executing opcode):
//!
//! - TRAP, TRAPV, CHK push the **next** instruction's address: all 8065
//!   `TRAP.json.gz` cases and all 4095 `TRAPV.json.gz` cases put `initial.pc +
//!   2` in the frame (both instructions are one word), and the `CHK.json.gz`
//!   frames put `initial.pc + 2`, `+4` or `+6` — the length of the CHK form, so
//!   "after the instruction", never a bare `+2`.
//! - divide-by-zero pushes the address of the **faulting instruction**: the one
//!   divide-by-zero case in the suite starts at `$000C00` and its frame holds
//!   `$000C00`. (The ticket guessed "next instruction" for this one; the data
//!   disagrees.)
//! - illegal instruction, line A, line F and privilege violation push the
//!   address of the faulting instruction itself, so a handler can decode it
//!   (M68000PM "Illegal Instruction"; the M68000UM's general rule says the
//!   stacked PC "usually" points at the next unexecuted instruction, but the
//!   family manual states the exception for these routes: the saved value is
//!   the address of the illegal or unimplemented instruction, and it is the
//!   handler's job to adjust it to skip the instruction on return). The carts
//!   do exactly that adjustment — Mamono Hunter Youko's `$28` handler does
//!   `ADDQ.L #$2,$A(A7)` after reading the opcode through the frame PC — so
//!   the opcode's own address is what these routes push, unchanged for all
//!   three of them. The suite never reaches these through a decoded opcode of
//!   ours, so the rule is documented rather than measured.
//!
//! The saved SR is the SR as it was *before* the mode change, S and T bits and
//! all — that is what makes `RTE` able to return to user mode. The suite has no
//! user-mode or traced case, so no case can contradict it, and its one
//! observable detail in the cases it does have is exactly right: in every
//! `TRAP.json.gz` case the stacked word equals `initial.sr`.
//!
//! # Clocks
//!
//! M68000UM "Exception Processing Execution Times" bills TRAP, illegal
//! instruction and privilege violation at 34 clocks, and all 8065
//! `TRAP.json.gz` cases have `length = 34`. [`M68k::exception`] charges all 34
//! itself and returns nothing, so a handler that calls it returns 0 clocks like
//! every other handler today: the bus accesses charge themselves and the rest
//! is added here, which leaves `M68k::cycles` correct while `M68k::step`'s
//! return value stays the not-yet-settled instruction-overhead channel
//! (M1-31, #39). Per-instruction extras (CHK's 40 plus EA, divide-by-zero's 38
//! plus EA) belong to the tickets that implement those instructions.

use crate::bus::{ADDR_MASK, Bus};
use crate::cpu::{BUS_CYCLE_CLOCKS, M68k, SP_EVEN_MASK};
use crate::sr::{SR_S, SR_T};

/// Illegal instruction, slot `$000010` (vector 4).
///
/// This used to be 3, the address-error slot — the suite's illegal-EA cases
/// fetch `$0C`, but only because the suite's recording core routed illegal to
/// the same slot as address error, which is the bug this constant carried for
/// years. The hardware map and all six #436 carts put illegal instruction at
/// vector 4 (see the module docs); `tests/harness/mod.rs` still reads the
/// suite's `$0C`, on purpose, because the recorded data is what it is.
pub const EXC_ILLEGAL: u8 = 4;

/// Line-1010 emulator (A-line), slot `$000028` (vector 10).
///
/// M68000UM Table 6-2 row 10, and the slot every A-line-using Genesis cart
/// puts its handler in. See the module docs for why this is a 68000 feature
/// and what the 68010 adds on top.
pub const EXC_LINE_A: u8 = 10;

/// Line-1111 emulator (F-line), slot `$00002C` (vector 11).
///
/// M68000UM Table 6-2 row 11, and the slot the F-line-using Genesis carts
/// populate. See [`EXC_LINE_A`].
pub const EXC_LINE_F: u8 = 11;

/// Divide by zero (CHK-style group 1 exception), slot `$000014`.
///
/// The instruction that raises it is not implemented yet (`DIVU`/`DIVS` are
/// M1-24/M1-25); the number is pinned here because it was measured from the
/// suite and the ticket that raises it should not have to re-derive it.
pub const EXC_DIV_ZERO: u8 = 5;

/// `CHK` operand out of range, slot `$000018`. See [`EXC_DIV_ZERO`] for why the
/// constant exists before the instruction does.
pub const EXC_CHK: u8 = 6;

/// `TRAPV` taken with `SR.V` and `SR.T` both set, slot `$00001C`. See
/// [`EXC_DIV_ZERO`] for why the constant exists before the instruction does.
pub const EXC_TRAPV: u8 = 7;

/// Privilege violation: a privileged instruction executed with `SR.S = 0`,
/// slot `$000020`.
///
/// The one vector number here that the pinned suite cannot pin (see the module
/// docs); raised by [`M68k::check_supervisor`].
pub const EXC_PRIVILEGE: u8 = 8;

/// Trace exception, slot `$000024`: taken at the end of an instruction that
/// started with `SR.T` set ([`crate::trace`], M68000UM section 6.3.8).
///
/// See [`EXC_DIV_ZERO`] for why the constant lives here rather than with the
/// code that takes it, and the module docs for why the number is the slot
/// address over four.
pub const EXC_TRACE: u8 = 9;

/// The slot TRAP #0 uses; TRAP #n uses [`TRAP_VECTOR_BASE`] + n, i.e. the long
/// at `$000080 + 4 * n`. Measured over all sixteen traps in `TRAP.json.gz`.
pub const TRAP_VECTOR_BASE: u8 = 32;

/// The vector number for `TRAP #number`, and therefore the slot
/// `4 * trap_vector(number)` = `$80 + 4 * number`.
///
/// `number` is the low nibble of the opcode (`$4E40..=$4E4F`).
pub const fn trap_vector(number: u8) -> u8 {
    TRAP_VECTOR_BASE + number
}

/// The address of the vector long for `vector`, under the convention "slot
/// address = number × 4" that the whole map follows.
const fn vector_address(vector: u8) -> u32 {
    // `as`, not `u32::from`: `From` is not a const trait, so a `const fn` in
    // this crate cannot call it — the same reason `ea.rs`'s sign-extension
    // helpers cast rather than convert.
    (vector as u32) * VECTOR_STRIDE
}

/// Vector slots are four bytes apart: one long per vector.
const VECTOR_STRIDE: u32 = 4;

/// Size of a group 1/2 exception frame: the PC long plus the saved SR word.
/// Measured on every framed case of `TRAP.json.gz`, `TRAPV.json.gz` and
/// `CHK.json.gz`: `initial.ssp - final.ssp == 6`.
const EXCEPTION_FRAME_SIZE: u32 = 6;

/// What M68000UM bills a group 1/2 exception entry at, and the number every
/// `TRAP.json.gz` case reports as its `length`.
pub(crate) const EXCEPTION_ENTRY_CLOCKS: u64 = 34;

/// What [`M68k::exception`] does **not** bill for an instruction that had
/// already fetched its opcode before it faulted: those
/// [`OPCODE_CLOCKS`](crate::cpu::OPCODE_CLOCKS) themselves.
///
/// A handler returns them so the step totals the fetch it really spent. The
/// first suite case that forced this was `80ef [DIVU (d16, A7), D0] 5745`:
/// `length` 46 = 4 fetch + 8 for the `(d16,An)` read + 34 entry (#238). Since
/// #241 `CHK`'s traps are cycle-checked and return it on both trap paths, with
/// the sign-only path one [`ALU_PASS_CLOCKS`] pass above it: the suite's 4 272
/// trapping `CHK` cases sit at `38 + ea` when the signed bound compare fires
/// the trap and `40 + ea` when only `Dn < 0` does — documented bus timing (see
/// [`M68k::op_chk`] for the law and its citations), not a fit.
///
/// Not for the traps that *are* the instruction: `TRAP`, `TRAPV` and an illegal
/// opcode fault before any operand is read, and cost the entry's 34 with nothing
/// of their own on top — 34 in every one of the 8065 `TRAP.json.gz` cases and
/// the 4095 trapping cases of `TRAPV.json.gz`. No illegal-instruction case is
/// dated by the suite at all (they all end at the vector `is_exception_case`
/// skips), which is what keeps this 4 away from [`M68k::op_illegal`].
///
/// [`M68k::op_illegal`]: crate::M68k::op_illegal
/// [`M68k::op_chk`]: crate::ops::arith::M68k::op_chk
/// [`ALU_PASS_CLOCKS`]: crate::ops::arith::ALU_PASS_CLOCKS
pub(crate) const FAULTED_FETCH_CLOCKS: u32 = crate::cpu::OPCODE_CLOCKS;

/// Bus accesses [`M68k`] performs during an exception entry: three writes for
/// the frame (PC high, PC low, SR), two reads for the vector long, and two
/// reads to refill the prefetch queue from the handler address. The suite's own
/// trace for a TRAP case is exactly these seven, plus 4 + 2 clocks of non-bus
/// time.
const EXCEPTION_BUS_ACCESSES: u64 = 7;

/// The part of the 34 clocks that the bus accesses do not account for.
const EXCEPTION_NONBUS_CLOCKS: u64 =
    EXCEPTION_ENTRY_CLOCKS - EXCEPTION_BUS_ACCESSES * BUS_CYCLE_CLOCKS;

impl M68k {
    /// Enter the exception `vector` with `pushed_pc` in the frame: the
    /// group 1/2 sequence in full.
    ///
    /// Afterwards the CPU is in supervisor mode with tracing off, the frame
    /// sits at the active (now supervisor) stack pointer with the saved SR at
    /// `SP` and `pushed_pc` at `SP + 2`, `pc` is the handler long read from
    /// [`vector_address`], and the prefetch queue is `[word(pc), word(pc + 2)]`
    /// exactly as [`M68k::jump_to`] leaves it.
    ///
    /// `pushed_pc` is a parameter rather than read from `self.pc` because which
    /// address that is depends on the caller: see the module docs for the
    /// per-exception rule. The caller computes it *before* calling, while `pc`
    /// still points at the instruction that faulted.
    ///
    /// Charges [`EXCEPTION_ENTRY_CLOCKS`] (34) into [`M68k::cycles`] and
    /// returns nothing; see the module docs for why the clocks do not travel
    /// back out through the handler's return value.
    pub(crate) fn exception(&mut self, bus: &mut impl Bus, vector: u8, pushed_pc: u32) {
        // 1. SR out to a temp, before anything is changed: the frame has to
        //    carry the mode and trace bits the exception is about to clear.
        let saved_sr = self.sr;

        // 2. Supervisor mode, trace off, condition codes and interrupt mask
        //    untouched. `set_sr` performs the stack pointer swap, and only
        //    swaps when S actually changes (M1-01, #9), so an exception taken
        //    in supervisor mode leaves USP and SSP where they were; taken from
        //    user mode, `a[7]` becomes the SSP and the USP is parked.
        self.set_sr((saved_sr & !(SR_S | SR_T)) | SR_S);

        // 3. The frame goes below the new active stack pointer, which step 2
        //    just made the SSP. Layout: SR at `sp`, PC long at `sp + 2`
        //    (M68000UM "Stack Frame Formats", group 0/1/2 vector frame). The
        //    stack address's bit 0 is forced off: the CPU's own stack traffic is
        //    word or long and the stack pointers are word-aligned registers, so
        //    an odd incoming SP pushes at the even address below it (#522,
        //    [`crate::cpu::SP_EVEN_MASK`]).
        let sp = self.a[7].wrapping_sub(EXCEPTION_FRAME_SIZE) & SP_EVEN_MASK;
        self.set_active_sp(sp);
        self.write_l_raw(bus, (sp + 2) & ADDR_MASK, pushed_pc);
        self.write_w_raw(bus, sp, saved_sr);

        // 4. New PC from the vector long, then a full queue refill from there:
        //    the words the queue held belong to a stream that is no longer
        //    running, so they are thrown away (M68000UM "Exception Processing":
        //    the processor "begins fetching instructions from the new PC").
        let handler = self.read_l_raw(bus, vector_address(vector));
        self.jump_to(bus, handler);

        // One step, one exception: the record is what the harness and the
        // scheduler can look at to see which route was taken.
        self.pending_exception = Some(vector);

        self.cycles += EXCEPTION_NONBUS_CLOCKS;
    }

    /// True when the caller may execute its instruction; false when this raised
    /// the privilege-violation exception and the caller must return immediately
    /// without performing any part of the operation.
    ///
    /// The privileged instructions are MOVE to SR/CCR from a memory source,
    /// ANDI/ORI/EORI to SR, MOVE from/to USP, RTE, STOP and RESET (M68000PM
    /// section 8, "supervisor programs only"). `RTE` is the first of them with a
    /// handler (M1-17, #25); the others start with this same check when their
    /// ticket lands.
    ///
    /// The frame carries `self.pc`, the address of the privileged instruction
    /// itself, so a handler can decode and re-execute it. Charges the exception
    /// entry's clocks like [`M68k::exception`], and a caller that gets `false`
    /// must have done nothing else yet: `RTE` checks before it pops, so the frame
    /// of a denied RTE is the stack the return address is still sitting on.
    pub(crate) fn check_supervisor(&mut self, bus: &mut impl Bus) -> bool {
        if self.is_supervisor() {
            return true;
        }
        let faulting_pc = self.pc;
        // The second of the two "the instruction never ran" routes in M68000UM
        // section 6.3.8: a denied instruction is not the instruction that traces.
        self.suppress_trace();
        self.exception(bus, EXC_PRIVILEGE, faulting_pc);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_error::EXC_ADDRESS_ERROR;
    use crate::sr::{SR_C, SR_INT_MASK, SR_N, SR_V, SR_X, SR_Z};
    use crate::{EXC_CHK, EXC_DIV_ZERO, EXC_TRAPV};

    /// 64 KB RAM that records every access it was asked about, in order, with
    /// its direction and value: the public `TestBus` logs addresses only
    /// (#184), so an assertion about what a bus saw needs a test-local bus (the
    /// pattern in `ea.rs` and `ops/movem.rs`).
    struct Ram {
        mem: Vec<u8>,
        /// `(kind, address, value)` for every access, in order.
        log: Vec<Access>,
    }

    /// One recorded bus access.
    #[derive(Debug, PartialEq, Eq)]
    enum Access {
        Read(u32),
        Write(u32, u16),
    }

    impl Ram {
        fn new() -> Self {
            Self {
                mem: vec![0; 1 << 16],
                log: Vec::new(),
            }
        }

        fn poke_word(&mut self, addr: u32, value: u16) {
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }

        fn poke_long(&mut self, addr: u32, value: u32) {
            self.poke_word(addr, (value >> 16) as u16);
            self.poke_word(addr + 2, value as u16);
        }

        fn peek_word(&self, addr: u32) -> u16 {
            let i = addr as usize;
            u16::from_be_bytes([self.mem[i], self.mem[i + 1]])
        }

        fn peek_long(&self, addr: u32) -> u32 {
            (u32::from(self.peek_word(addr)) << 16) | u32::from(self.peek_word(addr + 2))
        }

        /// The addresses the CPU read, in order.
        fn reads(&self) -> Vec<u32> {
            self.log
                .iter()
                .filter_map(|a| match a {
                    Access::Read(addr) => Some(*addr),
                    Access::Write(..) => None,
                })
                .collect()
        }

        fn writes(&self) -> Vec<(u32, u16)> {
            self.log
                .iter()
                .filter_map(|a| match a {
                    Access::Write(addr, value) => Some((*addr, *value)),
                    Access::Read(..) => None,
                })
                .collect()
        }
    }

    impl Bus for Ram {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.log.push(Access::Read(addr));
            self.mem[addr as usize]
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.log.push(Access::Read(addr));
            self.peek_word(addr)
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.log.push(Access::Write(addr, u16::from(value)));
            self.mem[addr as usize] = value;
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.log.push(Access::Write(addr, value));
            self.poke_word(addr, value);
        }
    }

    /// Where the program under test lives, and the stack top, so the tests can
    /// name the addresses the frame lands on.
    const PROGRAM: u32 = 0x0000_1000;
    const STACK: u32 = 0x0000_8000;
    /// The handler these tests point vectors at.
    const HANDLER: u32 = 0x0000_3000;

    /// A CPU executing `opcode` at [`PROGRAM`], in the mode `sr` selects, with
    /// both stack pointers set to distinguishable values and the vector table
    /// pointed at [`HANDLER`] everywhere that matters.
    fn cpu_at(opcode: u16, sr: u16) -> (M68k, Ram) {
        let mut bus = Ram::new();
        for slot in (0x00..=0xBC).step_by(4) {
            bus.poke_long(slot, HANDLER);
        }
        bus.poke_long(PROGRAM, 0xDEAD_BEEF);
        bus.poke_long(HANDLER, 0x1234_5678);

        let mut cpu = M68k::new();
        // SR first, while both pointers are zero, so the swap in `set_sr`
        // cannot move a loaded value into the wrong slot (`harness::apply`
        // does the same for the same reason).
        cpu.set_sr(sr);
        cpu.pc = PROGRAM;
        cpu.prefetch = [opcode, 0x4E71];
        cpu.ssp = STACK;
        cpu.usp = STACK + 0x100;
        cpu.a[7] = if sr & SR_S != 0 { cpu.ssp } else { cpu.usp };
        // Something recognisable in every register, so a test can see that the
        // exception left them alone.
        cpu.d[0] = 0x1111_2222;
        cpu.a[0] = 0x0020_0000;
        (cpu, bus)
    }

    /// Supervisor mode, interrupts masked, all condition codes clear — the
    /// state every suite case runs in.
    const SUP: u16 = SR_S | SR_INT_MASK;
    /// User mode with every condition code set and trace off: the mode no suite
    /// case ever exercises, so the S/T clearing and the swap are pinned here.
    const USER: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// The frame the suite says a group 1/2 exception leaves behind, asserted
    /// verbatim from `TRAP.json.gz`: SR word at the new `a[7]`, PC long above
    /// it, and the SSP six below where it started.
    fn assert_frame(cpu: &M68k, bus: &Ram, sp: u32, saved_sr: u16, pushed_pc: u32) {
        assert_eq!(
            cpu.a[7], sp,
            "the frame is six bytes below the stack pointer that was in use"
        );
        assert_eq!(
            bus.peek_word(sp),
            saved_sr,
            "SR word at SP (M68000UM 'Stack Frame Formats')"
        );
        assert_eq!(
            bus.peek_long(sp + 2),
            pushed_pc,
            "PC long at SP + 2, and nothing else in the frame"
        );
    }

    #[test]
    fn trap_pushes_frame_and_jumps() {
        // The ticket's test, in the mode the suite actually measures: all 8065
        // TRAP cases are supervisor with T = 0, so S does not change and the
        // frame lands on the SSP the CPU was already using.
        let (mut cpu, mut bus) = cpu_at(0x4E40, SUP);
        let before = cpu.cycles;

        cpu.step(&mut bus);

        assert_frame(&cpu, &bus, STACK - 6, SUP, PROGRAM + 2);
        assert_eq!(cpu.ssp, STACK - 6, "the stored SSP moved with a[7]");
        assert_eq!(cpu.usp, STACK + 0x100, "the USP is not the stack in use");
        assert!(cpu.is_supervisor(), "S = 1");
        assert_eq!(cpu.sr & SR_T, 0, "T = 0");
        assert_eq!(cpu.sr, SUP, "mask and condition codes untouched");
        assert_eq!(cpu.pc, HANDLER, "PC from the long at $000080 (vector 32)");
        assert_eq!(
            cpu.prefetch,
            [0x1234, 0x5678],
            "queue refilled from the handler: [word(pc), word(pc + 2)]"
        );
        assert_eq!(
            cpu.pending_exception,
            Some(TRAP_VECTOR_BASE),
            "TRAP #0 records vector 32"
        );
        assert_eq!(
            cpu.cycles - before,
            EXCEPTION_ENTRY_CLOCKS,
            "TRAP costs 34 clocks (M68000UM, and `length` in every suite case)"
        );
        // Registers are the instruction's business and the exception took none
        // of them.
        assert_eq!(cpu.d[0], 0x1111_2222);
        assert_eq!(cpu.a[0], 0x0020_0000);
    }

    #[test]
    fn trap_from_user_mode_switches_to_the_supervisor_stack() {
        // No suite case covers this: every TRAP case is already in supervisor
        // mode. The rule is M68000UM section 6 — the S=1/T=0 change happens
        // before the push, so the frame lands on the SSP — plus the #9 rule
        // that `set_sr` swaps only when S actually changes.
        let (mut cpu, mut bus) = cpu_at(0x4E40, USER);
        assert!(!cpu.is_supervisor());
        let usp_before = cpu.usp;

        cpu.step(&mut bus);

        assert!(cpu.is_supervisor(), "S set to 1");
        assert_eq!(cpu.sr & SR_T, 0, "T cleared");
        assert_eq!(cpu.sr, USER | SR_S, "only S changed");
        assert_eq!(cpu.a[7], STACK - 6, "the frame went on the SSP");
        assert_eq!(cpu.ssp, STACK - 6, "and the SSP moved with it");
        assert_eq!(cpu.usp, usp_before, "the parked USP is untouched");
        assert_frame(&cpu, &bus, STACK - 6, USER, PROGRAM + 2);
    }

    #[test]
    fn the_saved_sr_keeps_the_mode_and_trace_bits_being_cleared() {
        // The temp copy of SR is taken first, so the frame is what `RTE`
        // returns from: user mode with T set still has S=0, T=1 in the frame
        // even though the CPU is now supervisor with T clear.
        let sr = USER | SR_T;
        let (mut cpu, mut bus) = cpu_at(0x4E40, sr);
        assert_eq!(cpu.sr, sr, "the CPU starts with T set");

        cpu.step(&mut bus);

        assert_eq!(cpu.sr & (SR_S | SR_T), SR_S, "running now with S=1, T=0");
        assert_eq!(
            bus.peek_word(STACK - 6),
            sr,
            "the frame keeps the old S=0 and T=1"
        );
    }

    #[test]
    fn a_supervisor_exception_does_not_swap_the_stack_pointers() {
        // #9 semantics: the swap happens only when S changes, so the USP must
        // not move and the SSP must not pick up the USP.
        let (mut cpu, mut bus) = cpu_at(0x4E4F, SUP);
        let usp = cpu.usp;
        cpu.step(&mut bus);
        assert_eq!(cpu.usp, usp);
        assert_eq!(cpu.a[7], cpu.ssp, "a[7] is still the SSP");
    }

    #[test]
    fn trap_15_uses_vector_47() {
        let (mut cpu, mut bus) = cpu_at(0x4E4F, SUP);
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(47));
        assert_eq!(
            bus.reads(),
            vec![
                0x0000_00BC, // TRAP #15's vector long: 47 * 4
                0x0000_00BE,
                HANDLER,
                HANDLER + 2, // the queue refill
            ],
            "TRAP #15 fetches its handler from $0000BC"
        );
        assert_eq!(cpu.pc, HANDLER);
    }

    #[test]
    fn each_trap_number_uses_its_own_vector() {
        // Measured in `TRAP.json.gz`: `4e40` reads `$80`, `4e42` reads `$88`,
        // `4e4f` reads `$BC`, so the slot is `$80 + 4 * n`.
        for number in 0..=15u8 {
            let (mut cpu, mut bus) = cpu_at(0x4E40 | u16::from(number), SUP);
            cpu.step(&mut bus);
            assert_eq!(cpu.pending_exception, Some(trap_vector(number)));
            assert_eq!(
                bus.reads()[0],
                0x80 + 4 * u32::from(number),
                "TRAP #{number} must read its long from $80 + 4 * {number}"
            );
            assert_eq!(cpu.pc, HANDLER);
        }
    }

    #[test]
    fn illegal_pushes_own_address() {
        // `$4AFC` is the documented illegal opcode (M68000PM section 8). Its
        // frame points at the illegal instruction itself, so a handler can
        // decode it — one word before where TRAP would have resumed.
        let (mut cpu, mut bus) = cpu_at(0x4AFC, SUP);
        let before = cpu.cycles;

        cpu.step(&mut bus);

        assert_frame(&cpu, &bus, STACK - 6, SUP, PROGRAM);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        assert_eq!(cpu.pc, HANDLER);
        assert_eq!(
            bus.reads(),
            vec![0x0000_0010, 0x0000_0012, HANDLER, HANDLER + 2,],
            "the illegal-instruction long is at $000010 (vector {}) — six #436 carts\
             put their illegal handler there and halt on the old $00000C",
            EXC_ILLEGAL
        );
        assert_eq!(
            cpu.cycles - before,
            EXCEPTION_ENTRY_CLOCKS,
            "illegal instruction costs 34 clocks"
        );
    }

    /// The line-A and line-F emulators are vectors 10 and 11, slots `$000028`
    /// and `$00002C` (M68000UM Table 6-2, "Exception Vector Assignment": plain
    /// rows, no MC68010-only footnote). All six #436 black carts ship handler
    /// longs there and halt stubs at `$10`/`$0C`, which is what settled the
    /// numbering: on hardware an A-line opcode such as Mamono Hunter Youko's
    /// `DC.W $A004` lands on `$28`, where its handler reads the frame PC (the
    /// opcode's own address, see the frame docs) and steps over it.
    #[test]
    fn line_a_and_line_f_take_their_own_vectors() {
        // `$A004` is the word Mamono Hunter Youko executes at 0x0084B4; `$FF1F`
        // is the word Warrior of Rome II (and Caesar no Yabou II) executes.
        for (opcode, vector, slot, name) in [
            (0xA004u16, 10u8, 0x0000_0028u32, "line A"),
            (0xFF1Fu16, 11u8, 0x0000_002Cu32, "line F"),
        ] {
            let (mut cpu, mut bus) = cpu_at(opcode, SUP);
            let before = cpu.cycles;
            cpu.step(&mut bus);
            assert_eq!(
                cpu.pending_exception,
                Some(vector),
                "{name} (${opcode:04X}) must raise vector {vector}"
            );
            assert_frame(&cpu, &bus, STACK - 6, SUP, PROGRAM);
            assert_eq!(
                bus.peek_long(STACK - 4),
                PROGRAM,
                "{name}: the frame PC is the opcode's own address, so a handler can\
                 fetch the opcode and ADDQ #2 past it"
            );
            assert_eq!(cpu.pc, HANDLER);
            assert_eq!(
                bus.reads(),
                vec![slot, slot + 2, HANDLER, HANDLER + 2],
                "{name} must fetch its handler long from ${slot:08X}"
            );
            assert_eq!(
                cpu.cycles - before,
                EXCEPTION_ENTRY_CLOCKS,
                "{name} costs the same 34 clocks ILLEGAL costs"
            );
        }
    }

    /// The A-line group routes to vector [`EXC_LINE_A`], slot `$000028`, with
    /// the illegal route's frame (the opcode's own address) and its 34 clocks.
    /// `$A03B` is Star Cruiser's word.
    #[test]
    fn a_line_opcodes_take_vector_ten_across_the_group() {
        for opcode in [0xA123u16, 0xA000, 0xAFFF, 0xA004, 0xA03B] {
            let (mut cpu, mut bus) = cpu_at(opcode, SUP);
            let before = cpu.cycles;
            cpu.step(&mut bus);
            assert_eq!(
                cpu.pending_exception,
                Some(EXC_LINE_A),
                "${opcode:04X} must take the line-A emulator vector"
            );
            assert_frame(&cpu, &bus, STACK - 6, SUP, PROGRAM);
            assert_eq!(
                bus.reads()[0],
                0x0000_0028,
                "${opcode:04X} must fetch its handler long from $000028"
            );
            assert_eq!(
                cpu.cycles - before,
                EXCEPTION_ENTRY_CLOCKS,
                "${opcode:04X} costs the same 34 clocks ILLEGAL costs"
            );
        }
    }

    /// The F-line group routes to vector [`EXC_LINE_F`], slot `$00002C`, same
    /// frame and same 34 clocks. `$FF1F` is Warrior of Rome II's word (and
    /// Caesar no Yabou II's); `$F033` is Twinkle Tale's.
    #[test]
    fn f_line_opcodes_take_vector_eleven_across_the_group() {
        for opcode in [0xF123u16, 0xF000, 0xFFFF, 0xFF1F, 0xF033] {
            let (mut cpu, mut bus) = cpu_at(opcode, SUP);
            let before = cpu.cycles;
            cpu.step(&mut bus);
            assert_eq!(
                cpu.pending_exception,
                Some(EXC_LINE_F),
                "${opcode:04X} must take the line-F emulator vector"
            );
            assert_frame(&cpu, &bus, STACK - 6, SUP, PROGRAM);
            assert_eq!(
                bus.reads()[0],
                0x0000_002C,
                "${opcode:04X} must fetch its handler long from $00002C"
            );
            assert_eq!(
                cpu.cycles - before,
                EXCEPTION_ENTRY_CLOCKS,
                "${opcode:04X} costs the same 34 clocks ILLEGAL costs"
            );
        }
    }

    #[test]
    fn undefined_encodings_take_the_illegal_frame() {
        // The encodings #355 found carts executing: `$4741` is the `0100` group's
        // unassigned `101` operation slot, `$00FF`/`$00FA` are `ORI` at size `11`
        // and `$0AC6` is `EORI` at size 11. None of them is an instruction, so
        // the 68000 treats them exactly as it treats `ILLEGAL` — same vector, the
        // address of the encoding itself in the frame, the same four fetches and
        // the same 34 clocks — instead of leaving the core with nothing to do.
        for opcode in [0x4741u16, 0x00FF, 0x00FA, 0x0AC6, 0x0E00, 0x0808] {
            let (mut cpu, mut bus) = cpu_at(opcode, SUP);
            let before = cpu.cycles;
            cpu.step(&mut bus);
            assert_eq!(
                cpu.pending_exception,
                Some(EXC_ILLEGAL),
                "${opcode:04X} must take the illegal-instruction vector"
            );
            assert_frame(&cpu, &bus, STACK - 6, SUP, PROGRAM);
            assert_eq!(
                cpu.pc, HANDLER,
                "${opcode:04X} must jump to the $000010 long"
            );
            assert_eq!(
                cpu.cycles - before,
                EXCEPTION_ENTRY_CLOCKS,
                "${opcode:04X} costs the same 34 clocks ILLEGAL costs"
            );
            let mut illegal_bus_reads = vec![0x0000_0010, 0x0000_0012, HANDLER, HANDLER + 2];
            illegal_bus_reads.sort();
            let mut reads = bus.reads();
            reads.sort();
            assert_eq!(
                reads, illegal_bus_reads,
                "${opcode:04X} must read the same four words as ILLEGAL"
            );
        }
    }

    /// The unmaskable family: no exception except the interrupt consults the
    /// SR interrupt mask. M68000UM 6.2.5 is the citation - "For the reset and
    /// interrupt exceptions, the interrupt priority mask is also updated
    /// appropriately" is the only mention of the mask in the entry sequence -
    /// so illegal, the line emulators and the instruction traps are taken
    /// whatever the mask holds. Dyna Brothers 2 runs its whole main loop on
    /// this: `ORI.W #$700,SR` and then a 1111-format service call whose
    /// handler must be entered with the mask still at 7 (#494).
    ///
    /// The strongest state pins it: user mode with the mask left at 7 by the
    /// supervisor that set it, so S = 0 when the exception is taken and the
    /// entry has to swap to the SSP to push the frame and still deliver. The
    /// trap-based members (`CHK`, `TRAPV`, zero divide) share `exception` with
    /// these four, so one call site covers them by construction.
    #[test]
    fn the_unmaskable_family_is_taken_under_interrupt_mask_seven() {
        let sr = USER | SR_INT_MASK;
        for (opcode, vector, slot, pushed_pc, name) in [
            (0xFF1Fu16, EXC_LINE_F, 0x0000_002Cu32, PROGRAM, "line F"),
            (0xA004u16, EXC_LINE_A, 0x0000_0028u32, PROGRAM, "line A"),
            (0x4AFCu16, EXC_ILLEGAL, 0x0000_0010u32, PROGRAM, "ILLEGAL"),
            (
                0x4E40u16,
                TRAP_VECTOR_BASE,
                0x0000_0080u32,
                PROGRAM + 2,
                "TRAP #0",
            ),
        ] {
            let (mut cpu, mut bus) = cpu_at(opcode, sr);
            let before = cpu.cycles;

            cpu.step(&mut bus);

            assert_eq!(
                cpu.pending_exception,
                Some(vector),
                "{name} (${opcode:04X}) is taken under SR mask 7"
            );
            assert!(cpu.is_supervisor(), "{name}: the entry set S");
            assert_eq!(
                cpu.int_mask(),
                7,
                "{name}: the mask the entry found is the mask it leaves"
            );
            assert_frame(&cpu, &bus, STACK - 6, sr, pushed_pc);
            assert_eq!(cpu.pc, HANDLER, "{name}: the handler is entered");
            assert_eq!(
                bus.reads(),
                vec![slot, slot + 2, HANDLER, HANDLER + 2],
                "{name} fetches its handler long from ${slot:08X}"
            );
            assert_eq!(
                cpu.cycles - before,
                EXCEPTION_ENTRY_CLOCKS,
                "{name} costs the usual 34 clocks"
            );
        }
    }

    #[test]
    fn privilege_violation_helper() {
        // Called directly, as the privileged-instruction tickets will use it.
        let (mut cpu, mut bus) = cpu_at(0x4E70, USER); // RESET, whenever it lands
        assert!(bus.log.is_empty(), "the helper is the only bus user here");

        assert!(
            !cpu.check_supervisor(&mut bus),
            "user mode: the exception is taken, so the caller must not execute"
        );
        assert!(cpu.is_supervisor(), "the exception entered supervisor mode");
        // The frame goes on the SSP, which is where the swap moved `a[7]` from
        // the USP — the same rule `trap_from_user_mode...` pins.
        assert_frame(&cpu, &bus, STACK - 6, USER, PROGRAM);
        assert_eq!(cpu.pc, HANDLER);
        assert_eq!(cpu.pending_exception, Some(EXC_PRIVILEGE));

        // Supervisor mode: no exception, no bus traffic, nothing to return.
        let (mut cpu, mut bus) = cpu_at(0x4E70, SUP);
        assert!(cpu.check_supervisor(&mut bus));
        assert!(bus.log.is_empty(), "a supervisor check touches nothing");
        assert_eq!(cpu.pc, PROGRAM, "and the CPU is untouched");
        assert_eq!(cpu.pending_exception, None);
    }

    #[test]
    fn exception_writes_the_frame_below_the_stack_pointer() {
        // Pushes go down, and the frame's own words are the last thing written:
        // PC high at SP+2, PC low at SP+4, SR at SP (`TRAP.json.gz` traces
        // `$07FE`, `$07FA`, `$07FC` — the same three words).
        let (mut cpu, mut bus) = cpu_at(0x4E40, SUP);
        cpu.step(&mut bus);
        assert_eq!(
            bus.writes(),
            vec![
                (STACK - 4, 0x0000), // PC high
                (STACK - 2, 0x1002), // PC low: PROGRAM + 2
                (STACK - 6, SUP),    // SR
            ]
        );
    }

    #[test]
    fn exception_keeps_the_active_stack_pointer_in_both_places() {
        // `a[7]` mirrors whichever pointer SR.S selects and the stored field is
        // compared too (`tests/harness/mod.rs`), so a push must write both.
        let (mut cpu, mut bus) = cpu_at(0x4E40, SUP);
        cpu.step(&mut bus);
        assert_eq!(cpu.a[7], cpu.ssp, "supervisor: a[7] and ssp agree");
        assert_eq!(cpu.ssp, STACK - EXCEPTION_FRAME_SIZE);

        let (mut cpu, mut bus) = cpu_at(0x4E40, USER);
        cpu.step(&mut bus);
        assert_eq!(cpu.a[7], cpu.ssp, "after the swap, a[7] and ssp agree");
        assert_eq!(
            cpu.usp,
            STACK + 0x100,
            "and the USP stayed parked as it was"
        );
    }

    #[test]
    fn an_odd_stack_pointer_pushes_the_frame_at_an_even_address() {
        // The CPU's own stack traffic forces bit 0 of the address off (#522):
        // a frame taken with A7 odd pushes at the even address below it, and
        // the stored pointer comes back even, which is why the real chip cannot
        // walk its stack down one odd address at a time.
        let (mut cpu, mut bus) = cpu_at(0x4E40, SUP);
        cpu.ssp = STACK + 1;
        cpu.a[7] = cpu.ssp;
        cpu.step(&mut bus);

        let frame = (STACK + 1 - EXCEPTION_FRAME_SIZE) & crate::cpu::SP_EVEN_MASK;
        assert_eq!(
            frame,
            STACK - EXCEPTION_FRAME_SIZE,
            "bit 0 dropped, not wrapped"
        );
        assert_eq!(cpu.ssp, frame, "the stored SSP ends even");
        assert_eq!(cpu.a[7], frame, "and a[7] agrees");
        assert_eq!(
            bus.writes(),
            vec![
                (frame + 2, 0x0000), // PC high
                (frame + 4, 0x1002), // PC low: PROGRAM + 2
                (frame, SUP),        // SR
            ],
            "the frame lands at the even address"
        );
    }

    #[test]
    fn vector_numbers_are_slot_addresses_over_four() {
        // The convention, pinned on the numbers themselves: every constant
        // times four is the long address the suite measures — and, for rows the
        // suite cannot measure (the illegal slot and the two line rows), the
        // long address M68000UM Table 6-2 names and the six #436 carts write
        // their handlers into.
        assert_eq!(vector_address(EXC_ADDRESS_ERROR), 0x0000_000C);
        assert_eq!(vector_address(EXC_ILLEGAL), 0x0000_0010);
        assert_eq!(vector_address(EXC_DIV_ZERO), 0x0000_0014);
        assert_eq!(vector_address(EXC_CHK), 0x0000_0018);
        assert_eq!(vector_address(EXC_TRAPV), 0x0000_001C);
        assert_eq!(vector_address(EXC_PRIVILEGE), 0x0000_0020);
        assert_eq!(vector_address(EXC_TRACE), 0x0000_0024);
        assert_eq!(vector_address(EXC_LINE_A), 0x0000_0028);
        assert_eq!(vector_address(EXC_LINE_F), 0x0000_002C);
        assert_eq!(vector_address(trap_vector(0)), 0x0000_0080);
        assert_eq!(vector_address(trap_vector(15)), 0x0000_00BC);
        // Address error stays vector 3; the illegal route no longer shares it.
        assert_eq!(EXC_ADDRESS_ERROR, 3);
        assert_eq!(EXC_ILLEGAL, 4);
        assert_eq!(EXC_LINE_A, 10);
        assert_eq!(EXC_LINE_F, 11);
    }

    #[test]
    fn the_exception_clocks_split_into_bus_and_non_bus_time() {
        // 34 is not a magic number: seven bus cycles plus the two 4-clock
        // slots of non-bus time the suite's own trace shows.
        assert_eq!(EXCEPTION_BUS_ACCESSES * BUS_CYCLE_CLOCKS, 28);
        assert_eq!(EXCEPTION_NONBUS_CLOCKS, 6);
        assert_eq!(EXCEPTION_ENTRY_CLOCKS, 34);
    }
}
