//! Trace mode: the `SR.T` bit and the trace exception at the end of an
//! instruction (M1-30, #38).
//!
//! Trace is the 68000's single step. With `SR.T` set ([`crate::sr::SR_T`], bit
//! 15) the CPU takes the *trace exception* — [`EXC_TRACE`], vector 9, the long at
//! `$000024` — as soon as the instruction now executing has finished (M68000UM
//! section 6.3.8 "Trace"). It is an ordinary group 1/2 exception, so the handler
//! finds a six byte frame to resume from and returns with `RTE`; the handler
//! re-arms the next single step by setting T again, which is exactly the case the
//! "T is read before the instruction" rule below has to get right.
//!
//! # T is read before the instruction, not after
//!
//! The decision is taken from the T bit as it stands when the instruction
//! starts: [`M68k::trace_armed`] is called by [`M68k::step`] before the queue is
//! decoded, and [`M68k::take_trace`] is called with that snapshot once the
//! instruction is over. Both halves of that matter (M68000UM section 6.3.8):
//!
//! - an instruction that *sets* T — `ORI.W #$8000,SR`, or an `RTE` returning to a
//!   traced context — is not itself traced, because T was clear when it started.
//!   The next instruction is.
//! - an instruction that *clears* T — `RTE` restoring an SR without it, `MOVE` to
//!   SR, or a `STOP` whose operand drops it — still traces, because T was set when
//!   it started.
//!
//! # What does not trace
//!
//! An instruction that never ran does not trace. [`M68k::suppress_trace`] marks
//! the illegal-instruction / line A / line F route
//! ([`crate::ops::M68k::op_illegal`]), the privilege violation
//! ([`M68k::check_supervisor`]) and the address error
//! ([`M68k::take_address_error`]). The first two are M68000UM section 6.3.8; the
//! address error is an inference rather than a rule of its own, and it is stated
//! here because it is not in the ticket: the address error leaves the faulting
//! access unperformed and the instruction incomplete (M68000UM "Address Error",
//! and [`crate::address_error`]), and trace is taken at the *end* of a completed
//! instruction.
//!
//! # Ordering: trap, then trace, then interrupts
//!
//! A group 2 trap is processed first and the trace exception second, so the trace
//! frame holds the *trap handler's* address. Nothing here has to know that:
//! [`M68k::take_trace`] pushes `self.pc`, and by the time it runs the trap entry
//! has already pushed its own frame and moved `pc` to the handler. Two frames,
//! trap then trace, and the trace handler can `RTE` into the trap handler.
//!
//! Interrupts are checked after the trace exception, and that falls out of the
//! same placement: [`M68k::step`] samples the IPL pins before decoding
//! ([`crate::interrupts`]) and traces after the instruction, so a request that was
//! asserted during the traced instruction is acknowledged against the trace
//! handler, one step later, never instead of the trace.
//!
//! `STOP` is the one instruction whose own effect the trace exception cancels:
//! `STOP` sets [`M68k::stopped`] and taking the trace clears it again, so the CPU
//! runs the handler instead of waiting for the interrupt that the trace has
//! already delivered (M68000UM section 6.3.8, "STOP with the trace bit set").
//!
//! # Clocks
//!
//! The entry charges itself like any other group 1/2 entry — 34 clocks,
//! [`crate::exceptions::EXCEPTION_ENTRY_CLOCKS`] — and nothing is added on top.
//! Whether trace costs extra overhead beyond the entry is M1-31 (#39/#40) timing
//! work, and the traced instruction's own clocks were charged before this ran.

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::exceptions::EXC_TRACE;
use crate::sr::SR_T;

impl M68k {
    /// Whether the instruction [`M68k::step`] is about to run must be traced:
    /// `SR.T` as it stands *before* the instruction, which is the whole of the
    /// "`ORI` to SR setting `T` is not itself traced, and the next instruction
    /// is" rule (M68000UM section 6.3.8).
    pub(crate) fn trace_armed(&self) -> bool {
        self.sr & SR_T != 0
    }

    /// Mark the instruction now executing as one that does not trace: the
    /// illegal-instruction / line A / line F route, a privilege violation, or an
    /// address error. [`M68k::take_trace`] then does nothing for it.
    ///
    /// Every caller has just entered an exception for an instruction that did not
    /// complete, so there is no completed instruction left to trace; see the
    /// module docs for the route-by-route reasoning.
    pub(crate) fn suppress_trace(&mut self) {
        self.trace_suppressed = true;
    }

    /// Take the trace exception for the instruction that just finished, if
    /// [`M68k::trace_armed`] said so before it started and nothing suppressed it.
    ///
    /// `armed` is `step`'s snapshot of T rather than `self.sr` read here, because
    /// the instruction (and the trap entry it took) has already had its way with
    /// the T bit: an `RTE` restoring an SR without T still traces, and a `STOP`
    /// that dropped T traces too (M68000UM section 6.3.8).
    ///
    /// The frame pushes `self.pc`, which is *whatever the instruction left behind*
    /// and that is exactly what makes the trap interaction work: a group 2 trap has
    /// already entered its handler and moved `pc` there, so the trace frame holds
    /// the trap handler's address (M68000UM section 6.3.8, and
    /// [`M68k::exception`] takes the pushed PC from its caller for this reason).
    pub(crate) fn take_trace(&mut self, bus: &mut impl Bus, armed: bool) {
        if !armed || self.trace_suppressed {
            return;
        }
        let pushed_pc = self.pc;

        // `STOP` is the only way `stopped` can be set at this point — every other
        // path into a stopped CPU leaves `step` through the stopped branch — and
        // the trace exception is the interrupt-equivalent event `STOP` waits for,
        // so the processor runs the handler instead of staying halted
        // (M68000UM section 6.3.8: "STOP with the trace bit set").
        self.stopped = false;

        self.exception(bus, EXC_TRACE, pushed_pc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_error::{ADDRESS_ERROR_FRAME_SIZE, EXC_ADDRESS_ERROR};
    use crate::exceptions::{EXC_ILLEGAL, EXC_PRIVILEGE};
    use crate::interrupts::AUTO_VECTOR_BASE;
    use crate::sr::{SR_INT_MASK, SR_S};
    use crate::testbus::{TestBus, cpu_with_program};

    /// Where [`cpu_with_program`] loads its program, and the stack it starts on.
    const PROGRAM: u32 = 0x0000_1000;
    const STACK: u32 = 0x0001_0000;
    /// Handler the trace vector points at.
    const TRACE_HANDLER: u32 = 0x0000_3000;
    /// Handler the `TRAP #0` vector points at.
    const TRAP_HANDLER: u32 = 0x0000_4000;
    /// Handler the illegal-instruction vector points at.
    const ILLEGAL_HANDLER: u32 = 0x0000_5000;
    /// Handler the privilege-violation vector points at.
    const PRIV_HANDLER: u32 = 0x0000_6000;
    /// Handler the level-6 autovector points at.
    const IRQ_HANDLER: u32 = 0x0000_7000;
    /// Where a traced `RTE` returns to.
    const RETURN: u32 = 0x0000_8000;
    /// `NOP`, so a handler or a program word steps harmlessly.
    const NOP: u16 = 0x4E71;
    /// A group 1/2 frame: SR at `sp`, PC long at `sp + 2`.
    const FRAME: u32 = 6;

    /// Aim `vector` at `handler` and make the handler two `NOP`s.
    fn point_at(bus: &mut TestBus, vector: u8, handler: u32) {
        bus.write_long(u32::from(vector) * 4, handler);
        bus.load_words(handler, &[NOP, NOP]);
    }

    /// A CPU running `program` with T set on top of the post-reset SR (S = 1 and
    /// mask 7, so nothing else interferes with the step under test) and the trace
    /// vector aimed at [`TRACE_HANDLER`].
    fn cpu_tracing(program: &[u16]) -> (M68k, TestBus) {
        let (mut cpu, mut bus) = cpu_with_program(program);
        point_at(&mut bus, EXC_TRACE, TRACE_HANDLER);
        cpu.set_sr(cpu.sr | SR_T);
        (cpu, bus)
    }

    /// The ticket's test: `T` set, one `NOP`, and the trace exception is the
    /// whole of what that `NOP` did to the control flow — a frame holding the
    /// address of the *next* instruction and the SR that ran it, T and all.
    #[test]
    fn trace_after_nop() {
        let (mut cpu, mut bus) = cpu_tracing(&[NOP, NOP]);
        let sr = cpu.sr;

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_TRACE));
        assert_eq!(cpu.pc, TRACE_HANDLER, "the long at $24 (vector 9)");
        assert_eq!(
            cpu.prefetch,
            [NOP, NOP],
            "the queue refilled from the handler"
        );
        let sp = cpu.a[7];
        assert_eq!(sp, STACK - FRAME, "one 6-byte frame");
        assert_eq!(
            bus.read_word(sp),
            sr,
            "the frame SR is the SR the NOP ran with, T included"
        );
        assert_eq!(
            bus.read_long(sp + 2),
            PROGRAM + 2,
            "the frame PC is the instruction after the NOP"
        );
        assert_eq!(cpu.sr & SR_T, 0, "the trace entry clears T");
        assert_eq!(cpu.sr, sr & !SR_T, "and nothing else");

        // Single step, not "trace for ever": the handler's own NOP is untraced.
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None);
        assert_eq!(cpu.pc, TRACE_HANDLER + 2, "the handler ran");
    }

    /// The ticket's test: an instruction that *sets* T is not traced itself, and
    /// the instruction after it is — the T bit is read before the instruction,
    /// not after (M68000UM section 6.3.8).
    #[test]
    fn trace_not_taken_when_t_set_by_instruction() {
        // ORI.W #$8000,SR — sets T (mask 7 and S stay as they are) — then a NOP.
        let (mut cpu, mut bus) = cpu_with_program(&[0x007C, 0x8000, NOP, NOP]);
        point_at(&mut bus, EXC_TRACE, TRACE_HANDLER);
        assert_eq!(cpu.sr & SR_T, 0, "starts untraced");

        cpu.step(&mut bus);

        assert_eq!(cpu.sr & SR_T, SR_T, "the ORI set T");
        assert_eq!(
            cpu.pending_exception, None,
            "the instruction that sets T is not the one traced"
        );
        assert_eq!(cpu.pc, PROGRAM + 4, "the ORI completed normally");
        assert_eq!(cpu.a[7], STACK, "and pushed no frame");

        cpu.step(&mut bus);

        assert_eq!(
            cpu.pending_exception,
            Some(EXC_TRACE),
            "the next instruction is traced"
        );
        let sp = cpu.a[7];
        assert_eq!(
            bus.read_long(sp + 2),
            PROGRAM + 6,
            "the traced instruction is the NOP after the ORI's extension word"
        );
        assert_eq!(cpu.pc, TRACE_HANDLER);
    }

    /// The ticket's test: a group 2 trap is processed first, then trace, so the
    /// trace frame holds the trap handler's address (M68000UM section 6.3.8).
    #[test]
    fn trace_after_trap_sees_trap_handler_pc() {
        let (mut cpu, mut bus) = cpu_tracing(&[0x4E40, NOP]); // TRAP #0
        point_at(&mut bus, crate::exceptions::TRAP_VECTOR_BASE, TRAP_HANDLER);
        let sr = cpu.sr;

        cpu.step(&mut bus);

        let trap_sp = STACK - FRAME;
        let trace_sp = STACK - 2 * FRAME;
        assert_eq!(
            cpu.a[7], trace_sp,
            "two frames: the trap's, then the trace's below it"
        );
        assert_eq!(
            bus.read_long(trap_sp + 2),
            PROGRAM + 2,
            "the trap frame still returns past the TRAP"
        );
        assert_eq!(
            bus.read_long(trace_sp + 2),
            TRAP_HANDLER,
            "the trace frame holds the trap handler's address, not the TRAP's"
        );
        assert_eq!(
            bus.read_word(trace_sp),
            sr & !SR_T,
            "the SR the trap entry left behind is what trace saves"
        );
        assert_eq!(
            cpu.pc, TRACE_HANDLER,
            "and execution goes on at the trace handler"
        );
        assert_eq!(
            cpu.pending_exception,
            Some(EXC_TRACE),
            "the trace exception is the last one entered"
        );
    }

    /// The ticket's test: `STOP` with T set traces, and the trace exception
    /// leaves the processor running rather than stopped.
    #[test]
    fn trace_stop_does_not_stay_stopped() {
        // STOP #$2300 — supervisor, mask 3, T clear — then a NOP.
        let (mut cpu, mut bus) = cpu_tracing(&[0x4E72, 0x2300, NOP]);

        cpu.step(&mut bus);

        assert!(
            !cpu.stopped,
            "the trace exception is taken after STOP, so STOP did not stick"
        );
        assert_eq!(cpu.pending_exception, Some(EXC_TRACE));
        assert_eq!(cpu.pc, TRACE_HANDLER);
        let sp = cpu.a[7];
        assert_eq!(
            bus.read_long(sp + 2),
            PROGRAM + 4,
            "the frame PC is the instruction after STOP"
        );
        assert_eq!(
            bus.read_word(sp),
            0x2300,
            "the frame SR is the SR STOP loaded, not the SR before it"
        );
        assert_eq!(
            cpu.int_mask(),
            3,
            "STOP's mask is the one trace entered with"
        );

        assert_eq!(
            cpu.step(&mut bus),
            0,
            "the next step runs the handler, not the stopped-clock path"
        );
        assert_eq!(cpu.pc, TRACE_HANDLER + 2);
    }

    /// An illegal instruction — and the line A / line F routes with it — never
    /// traced: the instruction did not execute, so there is nothing to have
    /// completed (M68000UM section 6.3.8).
    #[test]
    fn no_trace_for_the_illegal_instruction() {
        for opcode in [0x4AFCu16, 0xA123, 0xF123] {
            let (mut cpu, mut bus) = cpu_tracing(&[opcode, NOP]);
            point_at(&mut bus, EXC_ILLEGAL, ILLEGAL_HANDLER);

            cpu.step(&mut bus);

            assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL), "${opcode:04X}");
            assert_eq!(
                cpu.pc, ILLEGAL_HANDLER,
                "${opcode:04X}: one exception, not two"
            );
            assert_eq!(
                cpu.a[7],
                STACK - FRAME,
                "${opcode:04X}: only the illegal frame was pushed"
            );
        }
    }

    /// A privilege violation does not trace either, and it is the second of the
    /// two "the instruction never ran" routes in M68000UM section 6.3.8.
    #[test]
    fn no_trace_for_a_privilege_violation() {
        let (mut cpu, mut bus) = cpu_tracing(&[0x4E73, NOP]); // RTE
        point_at(&mut bus, EXC_PRIVILEGE, PRIV_HANDLER);
        cpu.usp = STACK;
        cpu.set_sr(SR_T); // user mode, T still set

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_PRIVILEGE));
        assert_eq!(cpu.pc, PRIV_HANDLER, "the trace vector was not taken");
        assert_eq!(cpu.a[7], STACK - FRAME, "only the privilege frame");
    }

    /// The address error is not a completed instruction either, so the trace
    /// exception does not follow it — see the module docs for why this is an
    /// inference rather than a ticket rule.
    #[test]
    fn no_trace_for_an_address_error() {
        let (mut cpu, mut bus) = cpu_tracing(&[0x3010, NOP]); // MOVE.W (A0),D0
        point_at(&mut bus, EXC_ADDRESS_ERROR, ILLEGAL_HANDLER);
        cpu.a[0] = 0x0000_2001;

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_ADDRESS_ERROR));
        assert_eq!(cpu.pc, ILLEGAL_HANDLER, "one exception, not two");
        assert_eq!(
            cpu.a[7],
            STACK - ADDRESS_ERROR_FRAME_SIZE,
            "only the group 0 frame"
        );
    }

    /// "Interrupts are checked after the trace exception": the pins are sampled
    /// once per [`M68k::step`], before the decode, and the trace exception is taken
    /// at the end of the instruction that step ran — so a request asserted while a
    /// traced instruction is executing is acknowledged against the trace handler,
    /// after the trace, never instead of it.
    #[test]
    fn interrupt_is_taken_after_the_trace_exception() {
        let (mut cpu, mut bus) = cpu_tracing(&[NOP, NOP]);
        point_at(&mut bus, AUTO_VECTOR_BASE + 6, IRQ_HANDLER);
        cpu.set_sr((cpu.sr & !SR_INT_MASK) | (2 << 8)); // keep T, accept level 6
        assert_eq!(cpu.ipl, 0, "the request arrives during the traced NOP");

        cpu.step(&mut bus);

        assert_eq!(
            cpu.pending_exception,
            Some(EXC_TRACE),
            "the traced instruction came first"
        );
        assert_eq!(cpu.pc, TRACE_HANDLER);

        cpu.ipl = 6;
        cpu.step(&mut bus);

        assert_eq!(
            cpu.pending_exception,
            Some(AUTO_VECTOR_BASE + 6),
            "the interrupt is checked after the trace exception"
        );
        let sp = cpu.a[7];
        assert_eq!(
            bus.read_long(sp + 2),
            TRACE_HANDLER,
            "so the interrupt pre-empts the trace handler, not the traced NOP"
        );
        assert_eq!(cpu.pc, IRQ_HANDLER);
    }

    /// `RTE` restoring an SR without T still traces: the trace was armed before
    /// the `RTE` ran, which is the only thing trace looks at.
    #[test]
    fn traced_rte_restoring_an_sr_without_t_still_traces() {
        let (mut cpu, mut bus) = cpu_tracing(&[0x4E73, NOP]); // RTE
        let restored: u16 = SR_S | (3 << 8); // supervisor, mask 3, T clear
        let frame = cpu.a[7];
        bus.write_word(frame, restored);
        bus.write_long(frame + 2, RETURN);

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_TRACE), "RTE traces");
        assert_eq!(cpu.pc, TRACE_HANDLER);
        let sp = cpu.a[7];
        assert_eq!(
            bus.read_long(sp + 2),
            RETURN,
            "the frame PC is the address RTE loaded"
        );
        assert_eq!(
            cpu.int_mask(),
            3,
            "RTE's mask is what the trace entered with"
        );
        assert_eq!(cpu.sr & SR_T, 0, "T stayed clear through both");
    }
}
