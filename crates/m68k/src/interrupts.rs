//! Interrupts: the IPL pins, the interrupt mask, autovector entry, and the
//! `STOP` wake-up (M1-28, #36).
//!
//! The 68000 has seven interrupt request inputs, IPL0–IPL6, read as one 3-bit
//! level 0..=7 (M68000UM, section 6 "Interrupt Processing"). The CPU samples
//! them *between* instructions, so an interrupt is never taken in the middle of
//! an opcode and the interrupted instruction is resumable: [`M68k::check_interrupts`]
//! runs at exactly one point in [`M68k::step`], before the queue is decoded, and
//! the instruction it pre-empts is left untouched in memory with `pc` still on it.
//!
//! The level is accepted when it is strictly above the mask in SR (bits 10–8,
//! [`crate::sr::SR_INT_MASK`]): a level-4 request waits while the mask is 4 or
//! higher and is served the moment the mask drops, because the pins are still
//! asserted. Level 7 is the NMI and ignores the mask, which costs an extra
//! rule — see [`M68k::check_interrupts`].
//!
//! A Genesis never puts a vector number on the bus: the VDP and the 68k side of
//! the chip only drive the IPL pins, and the CPU falls back to the *autovector*,
//! vector `24 + level` — slots `$60` (spurious) through `$7C` (level 7), the
//! table in [`crate::exceptions`]. [`Bus::int_ack`] is the hook that lets a real
//! vectored bus take over, and it returns `None` here.
//!
//! Entry is [`M68k::exception`] — the same group 1/2 sequence every other
//! exception uses, six bytes at the supervisor stack pointer and the handler long
//! read from the vector — plus the acknowledge's clocks: M68000UM's "Interrupt
//! Acknowledge" timing bills a one-device autovector acknowledge at 44 clocks,
//! 34 of which [`M68k::exception`] already charges, so this module adds
//! [`INTERRUPT_ACK_CLOCKS`] to reach 44.

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::exceptions::EXCEPTION_ENTRY_CLOCKS;
use crate::sr::SR_INT_MASK;

/// First autovector number: level `n` autovectors to `AUTO_VECTOR_BASE + n`,
/// which is slot `$60 + 4 * n` (M68000UM, section 6 "Interrupt Processing":
/// "Autovector vectors ... vector numbers 24 through 31").
///
/// Level 0 never gets here — no interrupt is asserted — so the base is only
/// ever added to a level of 1..=7, which lands on 25..31. Vector 24, the
/// spurious interrupt, is what the CPU uses when an acknowledge finds no device
/// willing to answer; [`Bus::int_ack`] returning `None` means "autovector", not
/// "nobody", so nothing here produces it.
pub(crate) const AUTO_VECTOR_BASE: u8 = 24;

/// What the interrupt acknowledge adds on top of [`EXCEPTION_ENTRY_CLOCKS`].
///
/// M68000UM section 6, "Interrupt Acknowledge", bills an autovectoring
/// acknowledge with a single device on the line at 44 clock cycles (its timing
/// note is 4 + 6 + 27 + 7). 34 of those are the group 1/2 entry, which
/// [`M68k::exception`] already charges, so what the acknowledge adds is the 10
/// left over — 44 in total, which is the number the tests assert.
pub(crate) const INTERRUPT_ACK_CLOCKS: u64 = 44 - EXCEPTION_ENTRY_CLOCKS;

/// Level 7 — the non-maskable interrupt.
const NMI_LEVEL: u8 = 7;

impl M68k {
    /// Sample the IPL pins and take the interrupt if one is live: returns
    /// whether it took one.
    ///
    /// [`M68k::step`] calls this between instructions and never in the middle of
    /// one, which is what makes an interrupt resumable: nothing of the
    /// interrupted instruction has been consumed, so its frame pushes `self.pc`
    /// and the instruction is still sitting in memory (M68000UM section 6
    /// "Interrupt Processing", and see [`M68k::pc`] for what `pc` is).
    ///
    /// A request is live when the level is strictly above the mask, or when it
    /// is level 7 — the NMI, which no mask can block. Level 7 is triggered by
    /// the edge into 7 rather than by the level: [`M68k::ipl_sampled`] holds the
    /// previous sample, so a bus that keeps IPL at 7 is acknowledged once and
    /// its handler gets to run. Every sample updates that field, including the
    /// ones that take nothing, so 7 → 0 → 7 is a new edge.
    ///
    /// Taking one is [`Bus::int_ack`] first — a vectored bus answers with its own
    /// vector number and `None`, which is all a Genesis ever gives, autovectors
    /// to `AUTO_VECTOR_BASE + level` — then [`M68k::exception`] with
    /// [`INTERRUPT_ACK_CLOCKS`] charged on top of the entry's 34. The mask goes
    /// up to the accepted level last: `exception` deliberately leaves it alone,
    /// because most exceptions are not interrupts, and raising it here is what
    /// stops a second acknowledge before the handler has run. `RTE` puts the old
    /// mask back from the frame.
    pub(crate) fn check_interrupts(&mut self, bus: &mut impl Bus) -> bool {
        let level = self.ipl;
        // Read and record the pins in one move: the NMI rule below needs the
        // level the previous sample saw, and every step samples them whether or
        // not it takes the interrupt.
        let previous = std::mem::replace(&mut self.ipl_sampled, level);

        // Level 7 is the NMI: taken on the edge into 7 whatever the mask says.
        let nmi = level == NMI_LEVEL && previous != NMI_LEVEL;
        if level == 0 || !(level > self.int_mask() || nmi) {
            return false;
        }

        let vector = bus.int_ack(level).unwrap_or(AUTO_VECTOR_BASE + level);
        self.cycles += INTERRUPT_ACK_CLOCKS;

        // `pc` has not moved, so it is still the address of the instruction
        // that would have run next — the value `exception` puts in the frame.
        let pushed_pc = self.pc;
        self.exception(bus, vector, pushed_pc);

        // Only bits 10..8 change here, so `set_sr` cannot see an S transition
        // and cannot swap the stack pointers underneath the frame just pushed.
        self.set_sr((self.sr & !SR_INT_MASK) | (u16::from(level) << 8));

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::STOPPED_CLOCKS;
    use crate::sr::SR_S;
    use crate::testbus::{TestBus, cpu_with_program};

    /// Where [`cpu_with_program`] loads its program, and therefore the address
    /// an interrupt taken on the first step has to push.
    const PROGRAM: u32 = 0x0000_1000;
    /// Handler every autovector slot is aimed at in these tests.
    const HANDLER: u32 = 0x0000_3000;
    /// `NOP`, so a handler or a program word can be stepped harmlessly.
    const NOP: u16 = 0x4E71;
    /// The 68k pushes a 6-byte group 1/2 frame: SR at `sp`, PC long at `sp + 2`.
    const FRAME_SIZE: u32 = 6;

    /// A CPU reset onto `program` with every level 1..=7 autovector slot aimed
    /// at [`HANDLER`], whose first two words are `NOP`s.
    ///
    /// One long per vector, so level `n` lives at `(24 + n) * 4`.
    fn cpu_pointed_at_handler(program: &[u16]) -> (M68k, TestBus) {
        let (cpu, mut bus) = cpu_with_program(program);
        for level in 1..=NMI_LEVEL {
            bus.write_long(u32::from(AUTO_VECTOR_BASE + level) * 4, HANDLER);
        }
        bus.load_words(HANDLER, &[NOP, NOP]);
        (cpu, bus)
    }

    /// [`cpu_pointed_at_handler`] on two `NOP`s, in supervisor mode with the
    /// interrupt mask set to `mask` (S is already set by the reset, so this
    /// cannot swap the stack pointer).
    fn cpu_masking(mask: u8) -> (M68k, TestBus) {
        let (mut cpu, bus) = cpu_pointed_at_handler(&[NOP, NOP]);
        cpu.set_sr(SR_S | (u16::from(mask) << 8));
        (cpu, bus)
    }

    /// Level 6 with the mask at 3: vector 30 = `24 + 6` at `$78`, the frame
    /// holds the old SR, and the mask goes up to the level that was taken.
    #[test]
    fn level_6_autovector() {
        let (mut cpu, mut bus) = cpu_masking(3);
        cpu.ipl = 6;
        let before = cpu.cycles;

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(AUTO_VECTOR_BASE + 6));
        assert_eq!(cpu.pc, HANDLER, "the long at $78");
        assert!(cpu.is_supervisor());
        assert_eq!(cpu.int_mask(), 6, "the mask rises to the level taken");
        let sp = cpu.a[7];
        assert_eq!(bus.read_long(sp + 2), PROGRAM, "frame PC");
        assert_eq!(
            bus.read_word(sp),
            SR_S | (3 << 8),
            "frame SR is the SR the interrupt interrupted"
        );
        let total = cpu.cycles - before;
        assert_eq!(total, EXCEPTION_ENTRY_CLOCKS + INTERRUPT_ACK_CLOCKS);
        assert_eq!(total, 44, "M68000UM 'Interrupt Acknowledge Timing'");
    }

    /// A request at or below the mask does nothing at all — the instruction
    /// runs, no frame, no vector — and the same request is served as soon as
    /// the mask drops below it, because the pins are still asserted.
    #[test]
    fn masked_interrupt_ignored() {
        let (mut cpu, mut bus) = cpu_masking(4);
        let stack = cpu.a[7];
        cpu.ipl = 4;

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, None);
        assert_eq!(cpu.pc, PROGRAM + 2, "the NOP ran");
        assert_eq!(cpu.int_mask(), 4, "mask untouched");
        assert_eq!(cpu.a[7], stack, "no frame pushed");

        cpu.set_sr(SR_S | (3 << 8));
        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(AUTO_VECTOR_BASE + 4));
        assert_eq!(cpu.pc, HANDLER, "unmasked one instruction later");
        assert_eq!(cpu.int_mask(), 4);
    }

    /// Level 7 is the NMI: taken even with the mask at 7, but only on the edge
    /// into 7 — while the pins sit at 7 it must not be acknowledged again every
    /// instruction, or the handler it jumps to could never run two opcodes.
    #[test]
    fn level_7_is_nmi_edge_triggered() {
        let (mut cpu, mut bus) = cpu_masking(7);
        cpu.ipl = NMI_LEVEL;

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(AUTO_VECTOR_BASE + 7));
        assert_eq!(cpu.pc, HANDLER, "vector 31 → $7C");
        let sp = cpu.a[7];

        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None, "still 7: no second NMI");
        assert_eq!(cpu.a[7], sp, "and no second frame");

        cpu.ipl = 0;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None, "level 0 is never an interrupt");

        cpu.ipl = NMI_LEVEL;
        cpu.step(&mut bus);
        assert_eq!(
            cpu.pending_exception,
            Some(AUTO_VECTOR_BASE + 7),
            "0 → 7 is a new edge, so the NMI fires again"
        );
        assert_eq!(cpu.a[7], sp - FRAME_SIZE);
    }

    /// `STOP` waits for an interrupt, so the stopped branch of [`M68k::step`]
    /// samples the pins too: a level above the mask clears `stopped`, enters the
    /// exception, and pushes the address after `STOP` — the instruction the
    /// stopped CPU was about to run.
    #[test]
    fn stop_wakes_on_interrupt() {
        // STOP #$2300 — supervisor, mask 3 — then a NOP.
        let (mut cpu, mut bus) = cpu_pointed_at_handler(&[0x4E72, 0x2300, NOP]);
        cpu.step(&mut bus);
        assert!(cpu.stopped, "STOP stopped it");
        assert_eq!(cpu.int_mask(), 3);

        // Nothing to wake it yet: the stopped path burns its clocks as before.
        assert_eq!(cpu.step(&mut bus), STOPPED_CLOCKS);
        assert!(cpu.stopped);

        cpu.ipl = 6;
        let before = cpu.cycles;
        cpu.step(&mut bus);

        assert!(!cpu.stopped, "the level-6 request woke it");
        assert_eq!(cpu.pending_exception, Some(AUTO_VECTOR_BASE + 6));
        assert_eq!(cpu.pc, HANDLER);
        assert_eq!(
            cpu.int_mask(),
            6,
            "waking up enters like any other interrupt"
        );
        let sp = cpu.a[7];
        assert_eq!(
            bus.read_long(sp + 2),
            PROGRAM + 4,
            "the frame PC is the instruction STOP was stopped in front of"
        );
        assert_eq!(
            cpu.cycles - before,
            EXCEPTION_ENTRY_CLOCKS + INTERRUPT_ACK_CLOCKS
        );
    }

    /// The frame carries the address of the instruction that *would* have run:
    /// the interrupt pre-empts an instruction that has not executed at all, so
    /// the frame PC is `pc` itself and the instruction's effect has not happened.
    #[test]
    fn interrupt_pushes_next_instruction_pc() {
        let (mut cpu, mut bus) = cpu_pointed_at_handler(&[0x7001, NOP]); // MOVEQ #1,D0
        cpu.set_sr(SR_S | (3 << 8));
        cpu.d[0] = 0xFFFF_FFFF;
        cpu.ipl = 6;

        cpu.step(&mut bus);

        let sp = cpu.a[7];
        assert_eq!(bus.read_long(sp + 2), PROGRAM);
        assert_eq!(cpu.d[0], 0xFFFF_FFFF, "the MOVEQ never ran");
    }

    /// [`Bus::int_ack`] is the escape hatch from autovectoring: a bus that
    /// answers with a vector number gets that vector, not `24 + level`.
    #[test]
    fn int_ack_vector_override() {
        /// A vector number no autovector can produce.
        const VECTORED: u8 = 100;
        const VECTORED_HANDLER: u32 = 0x0000_4000;

        let (mut cpu, mut bus) = cpu_masking(3);
        bus.int_vector = Some(VECTORED);
        bus.write_long(u32::from(VECTORED) * 4, VECTORED_HANDLER);
        bus.load_words(VECTORED_HANDLER, &[NOP, NOP]);
        cpu.ipl = 6;

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(VECTORED));
        assert_eq!(cpu.pc, VECTORED_HANDLER, "not the level-6 autovector");
        assert_eq!(cpu.int_mask(), 6);
    }

    /// `RTE` is the way out: back to the interrupted instruction with the SR —
    /// and therefore the old, lower interrupt mask — from the frame.
    #[test]
    fn rte_returns_and_restores_mask() {
        let (mut cpu, mut bus) = cpu_masking(3);
        bus.load_words(HANDLER, &[0x4E73, 0x4E73]); // RTE
        cpu.ipl = 6;

        cpu.step(&mut bus);
        assert_eq!(
            cpu.int_mask(),
            6,
            "inside the handler the mask is the level"
        );

        cpu.step(&mut bus);

        assert_eq!(cpu.pc, PROGRAM, "back on the interrupted instruction");
        assert_eq!(cpu.int_mask(), 3, "RTE restored the masked SR");
        assert_eq!(cpu.pending_exception, None);

        // A real handler clears the device that raised IPL; these tests just
        // drop the pin, because a still-level 6 request is taken again.
        cpu.ipl = 0;
        cpu.step(&mut bus);
        assert_eq!(cpu.pc, PROGRAM + 2, "and the interrupted NOP ran");
        assert_eq!(cpu.int_mask(), 3);
    }

    /// A reset re-samples the pins, so the NMI's edge state goes with it: after
    /// `reset` a level 7 that was already sitting there is a fresh edge.
    #[test]
    fn reset_clears_the_sampled_level() {
        let (mut cpu, mut bus) = cpu_masking(7);
        cpu.ipl = NMI_LEVEL;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(AUTO_VECTOR_BASE + 7));

        cpu.reset(&mut bus);
        assert_eq!(cpu.ipl, 0, "reset clears the pins too");
        cpu.ipl = NMI_LEVEL;

        cpu.step(&mut bus);

        assert_eq!(
            cpu.pending_exception,
            Some(AUTO_VECTOR_BASE + 7),
            "reset re-armed the NMI edge"
        );
    }
}
