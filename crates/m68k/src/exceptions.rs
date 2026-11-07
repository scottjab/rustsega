//! Exception entry: the group 1 / group 2 frame, the vector map, and the two
//! opcodes that raise an exception by themselves (M1-08, #16).
//!
//! [`M68k::exception`] is the single entry point every exception route uses:
//! TRAP (#16), illegal instruction / line A / line F (#15, wired up here), the
//! privilege check for the privileged instructions (this module's
//! [`M68k::check_supervisor`]), then CHK, TRAPV and divide-by-zero from their
//! own instruction tickets, and interrupts (M1-28, #36) on top of the mask
//! change. Group 0 — address error and bus error — pushes a bigger frame and is
//! M1-29 (#37); nothing here builds one.
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
//! | — | 2 | `$08` | address error: M1-29 (#37), not raised here |
//! | [`EXC_ILLEGAL`] | 3 | `$0C` | every exception case of 62 suite files fetches `$0C` (illegal EA), and `tests/harness/mod.rs` installs its handler there |
//! | [`EXC_DIV_ZERO`] | 5 | `$14` | `DIVU.json.gz` case `80ef [DIVU (d16, A7), D0] 5745`: divisor word at `$0000F0` is zero, the case reads the long at `$14`/`$16` and the frame goes on the stack. Only case in the suite that divides by zero |
//! | [`EXC_CHK`] | 6 | `$18` | 4272 of the 7389 exception cases in `CHK.json.gz` fetch `$18` (the rest are illegal-EA cases fetching `$0C`) |
//! | [`EXC_TRAPV`] | 7 | `$1C` | all 4095 exception cases in `TRAPV.json.gz` fetch `$1C` |
//! | [`EXC_PRIVILEGE`] | 8 | `$20` | **not covered by the suite** — every case in every file runs in supervisor mode (`sr & $2000` set in all 8065 `TRAP.json.gz` cases, and `RESET`, `MOVEtoUSP`, `MOVEfromUSP` and friends have no exception cases at all), so no case can show which slot a privilege violation fetches. Chosen as the slot after [`EXC_TRAPV`], which is where both the ticket's map and the measured `CHK`/`TRAPV`/divide-by-zero sequence put it |
//! | — | 9 | `$24` | trace: M1-30 (#38) |
//! | — | 24, 25..31 | `$60`, `$64`–`$7C` | spurious and level 1–7 autovectors: M1-28 (#36) |
//! | [`trap_vector`] | 32 + n | `$80 + 4 * n` | all 8065 `TRAP.json.gz` cases: `4e40` reads `$80`, `4e42` reads `$88`, `4e4f` reads `$BC` |
//!
//! The three slots the suite pins are one slot above where some Genesis
//! documentation puts divide-by-zero, CHK and TRAPV. The suite is what this
//! crate is scored against, so its slots win here and the discrepancy is on a
//! issue for a hardware test ROM to settle; nothing outside this table depends
//! on the difference.
//!
//! # There is no line A or line F vector on the 68000
//!
//! M68000UM lists Format I ("Line 1010 emulator instruction") and Format J
//! ("Line 1111 emulator instruction"), but the MC68000 implements neither, so
//! the `1010` and `1111` groups are reserved encodings and a reserved opcode
//! raises the illegal-instruction exception ([`EXC_ILLEGAL`]) — one vector,
//! three routes. The separate line-A and line-F emulator vectors are a 68010
//! addition (M1-12, #20), which is why [`crate::decode::Op::LineA`] and
//! [`crate::decode::Op::LineF`] stay distinct in the decode table even though
//! they route identically here. There is deliberately **no line-A or line-F
//! vector constant in this crate**: the pinned suite has no A-line or F-line
//! file, so nothing could pin one, and a number nobody can check is worse than
//! none.
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
//!   (M68000PM "Illegal Instruction"). The suite never reaches these through a
//!   decoded opcode of ours, so the rule is documented rather than measured.
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
use crate::cpu::{BUS_CYCLE_CLOCKS, M68k};
use crate::sr::{SR_S, SR_T};

/// Illegal instruction — and, on the 68000, line A and line F as well.
///
/// Slot `$00000C`. See the module docs for why this is 3 here and 4 in
/// Motorola's figure.
pub const EXC_ILLEGAL: u8 = 3;

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
        //    (M68000UM "Stack Frame Formats", group 0/1/2 vector frame).
        let sp = self.a[7].wrapping_sub(EXCEPTION_FRAME_SIZE);
        self.set_active_sp(sp);
        self.write_l(bus, (sp + 2) & ADDR_MASK, pushed_pc);
        self.write_w(bus, sp, saved_sr);

        // 4. New PC from the vector long, then a full queue refill from there:
        //    the words the queue held belong to a stream that is no longer
        //    running, so they are thrown away (M68000UM "Exception Processing":
        //    the processor "begins fetching instructions from the new PC").
        let handler = self.read_l(bus, vector_address(vector));
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
    /// section 8, "supervisor programs only"). They are not implemented yet;
    /// this is the check they all start with, so the ticket that lands each one
    /// has the exception path without re-deriving it.
    ///
    /// The frame carries `self.pc`, the address of the privileged instruction
    /// itself, so a handler can decode and re-execute it. Charges the exception
    /// entry's clocks like [`M68k::exception`].
    // Called by the privileged-instruction tickets (MOVE to SR, ANDI/ORI/EORI
    // to SR, MOVE from/to USP, RTE, STOP, RESET); only the tests here use it
    // today, which is the same arrangement `advance_opcode` has.
    #[allow(
        dead_code,
        reason = "called by the privileged-instruction tickets; only the tests use it today"
    )]
    pub(crate) fn check_supervisor(&mut self, bus: &mut impl Bus) -> bool {
        if self.is_supervisor() {
            return true;
        }
        let faulting_pc = self.pc;
        self.exception(bus, EXC_PRIVILEGE, faulting_pc);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sr::{SR_C, SR_INT_MASK, SR_N, SR_V, SR_X, SR_Z};
    use crate::{EXC_CHK, EXC_DIV_ZERO, EXC_TRAPV};

    /// 64 KB RAM that also records every address it was asked about, in order:
    /// the public `TestBus` deliberately records nothing, so bus-activity
    /// assertions need a test-local bus (the pattern in `cpu.rs` and `ea.rs`).
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
            vec![0x0000_000C, 0x0000_000E, HANDLER, HANDLER + 2,],
            "the illegal-instruction long is at $00000C (vector {})",
            EXC_ILLEGAL
        );
        assert_eq!(
            cpu.cycles - before,
            EXCEPTION_ENTRY_CLOCKS,
            "illegal instruction costs 34 clocks"
        );
    }

    #[test]
    fn line_a_and_line_f_take_the_illegal_frame() {
        // The 68000 has no line emulator: Formats I and J are reserved, so the
        // `1010` and `1111` groups take the illegal-instruction exception with
        // the illegal instruction's frame — same vector, same pushed PC, same
        // cost. The 68010's own line-A/F vectors are #20's, and there is no
        // constant for them to use here.
        for opcode in [0x4AFCu16, 0xA123, 0xF123, 0xA000, 0xFFFF] {
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
                "${opcode:04X} must jump to the $00000C long"
            );
            assert_eq!(
                cpu.cycles - before,
                EXCEPTION_ENTRY_CLOCKS,
                "${opcode:04X} costs the same 34 clocks"
            );
            let mut illegal_bus_reads = vec![0x0000_000C, 0x0000_000E, HANDLER, HANDLER + 2];
            illegal_bus_reads.sort();
            let mut reads = bus.reads();
            reads.sort();
            assert_eq!(
                reads, illegal_bus_reads,
                "${opcode:04X} must read the same four words as ILLEGAL"
            );
        }
    }

    #[test]
    fn no_line_a_or_line_f_vector_number_is_taken() {
        // The anti-test for the ticket's `line_a_vector_10` / `line_f_vector_11`:
        // those slots belong to the 68010's line emulators, so on the 68000
        // nothing may reach them. Assert the slots the 68010 would use
        // (`$24`..`$2C` is where the ticket's numbers 9/10/11 would land) are
        // never fetched, by checking the routes that would use them.
        for opcode in [0x4AFCu16, 0xA123, 0xF123] {
            let (mut cpu, mut bus) = cpu_at(opcode, SUP);
            cpu.step(&mut bus);
            let forbidden: Vec<u32> = (0x10..=0x30).step_by(4).collect();
            for addr in bus.reads() {
                assert!(
                    !forbidden.contains(&addr),
                    "${opcode:04X} fetched its handler from ${addr:08X}, \
                     which is one of the reserved/68010-only slots"
                );
            }
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
    fn vector_numbers_are_slot_addresses_over_four() {
        // The convention, pinned on the numbers themselves: every constant
        // times four is the long address the suite measures.
        assert_eq!(vector_address(EXC_ILLEGAL), 0x0000_000C);
        assert_eq!(vector_address(EXC_DIV_ZERO), 0x0000_0014);
        assert_eq!(vector_address(EXC_CHK), 0x0000_0018);
        assert_eq!(vector_address(EXC_TRAPV), 0x0000_001C);
        assert_eq!(vector_address(EXC_PRIVILEGE), 0x0000_0020);
        assert_eq!(vector_address(trap_vector(0)), 0x0000_0080);
        assert_eq!(vector_address(trap_vector(15)), 0x0000_00BC);
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
