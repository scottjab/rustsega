//! Interrupt acceptance: `/INT` in all three modes, `/NMI`, the `EI` delay and
//! the `HALT` wake-up (M2-14, #57).
//!
//! ```text
//! NMI                 11 T    PC = 0x0066, IFF1 = 0, IFF2 kept
//! INT, IM 0 (0xFF)    13 T    the bus byte executes; on the Genesis it floats
//!                             to FF, which is RST 38h
//! INT, IM 1           13 T    PC = 0x0038
//! INT, IM 2           19 T    PC = word at I:int_vector
//! ```
//!
//! Acceptance happens between instructions, at the top of [`Z80::step`], and it
//! *replaces* the instruction that was about to be fetched: the acknowledge is
//! an M1 slot (so `R` counts it), no opcode is read, and the return address on
//! the stack is the address of the instruction that never ran. That is what
//! makes `EI` needing a delay and `HALT` ending on an interrupt two sides of the
//! same rule rather than two special cases.
//!
//! The `/INT` line is a level, not a pulse: the Genesis holds it for as long as
//! its own interrupt source is live (how long that is belongs to M3-14), so the
//! line staying high has to fire again the moment `EI` reopens the gate.

use crate::{FLAG_PV, IntMode, Z80, Z80Bus};

/// Where `/NMI` always goes. It is the only thing that can: no other
/// instruction and no other interrupt reaches it (Z80UM 2.2, "NMI").
const NMI_VECTOR: u16 = 0x0066;
/// The `IM 1` vector, which is `RST 38h` executed internally.
const MODE1_VECTOR: u16 = 0x0038;
/// `NMI` entry: one M1 plus the two stack writes.
const T_NMI: u32 = 11;
/// `IM 0` and `IM 1` entry: `NMI`'s shape plus the `/IORQ` `/RD` slot the bus
/// answers the vector byte in.
const T_INT: u32 = 13;
/// `IM 2` entry: `T_INT` plus the second `/IORQ` read for the vector's high
/// byte and the wait state between them.
const T_INT_IM2: u32 = 19;

impl Z80 {
    /// Take an interrupt if one is waiting, called from [`Z80::step`] before it
    /// fetches anything.
    ///
    /// `Some` is the T-states of the acknowledge sequence and means no
    /// instruction ran; `None` means the CPU was free to execute.
    ///
    /// `/NMI` wins when both are pending. It is edge-triggered, so the latch
    /// that [`Z80::nmi`] raised is consumed here whether or not the CPU was
    /// already sitting on a held `/INT` line.
    pub(crate) fn service_interrupts(&mut self, bus: &mut impl Z80Bus) -> Option<u32> {
        let nmi = self.nmi_pending;
        // `EI` guarantees the instruction after it runs before the gate opens,
        // and `DI` closes it outright (Z80UM 2.2, `EI`/`DI`).
        if !nmi && !(self.int_line && self.iff1 && !self.ei_pending) {
            return None;
        }

        // Resolve the vector before a single bit of state moves: an `IM 0` bus
        // byte that is not a `RST` has nowhere to go and panics, and a half
        // taken interrupt is worse than one that was never started.
        let (target, cycles) = if nmi {
            (NMI_VECTOR, T_NMI)
        } else {
            match self.im() {
                IntMode::Mode0 => (self.mode0_vector(), T_INT),
                IntMode::Mode1 => (MODE1_VECTOR, T_INT),
                IntMode::Mode2 => (self.mode2_vector(bus), T_INT_IM2),
            }
        };

        self.nmi_pending = false;
        self.set_iff1(false);
        if !nmi {
            // A maskable acknowledge shadows `IFF2` too. `NMI` deliberately
            // leaves it alone, so the `RETI` closing the handler can put
            // `IFF1` back the way it was (Z80UM 2.2, "NMI").
            self.set_iff2(false);
        }

        // The acknowledge is an M1 slot and `R` counts every M1 slot.
        self.inc_r();
        // `HALT` is a wait for exactly this. It stopped the fetch without pushing
        // anything, so the address the handler goes back to is the instruction
        // after the `HALT`, where `PC` already points, and not the `HALT` itself
        // (Z80UM 2.1, "HALT").
        self.set_halted(false);

        // The NMOS `LD A,I`/`LD A,R` bug. Those two copy `IFF2` into `P/V` on
        // the way out, and the copy is still live at the boundary, so an
        // interrupt accepted here - which has just shadowed `IFF2` to 0 - lands
        // on it. Documented as "if a LD A,I or LD A,R (which copy IFF2 to the
        // P/V flag) is interrupted, then the P/V flag is reset, even if
        // interrupts were enabled beforehand" (z80.info, "Interrupt Behaviour of
        // the Z80 CPU").
        if self.p() {
            self.set_flag(FLAG_PV, false);
        }

        let return_addr = self.pc();
        self.push16(bus, return_addr);
        self.set_pc(target);
        self.set_wz(target);
        self.clear_latches();
        Some(cycles)
    }

    /// The target an `IM 0` acknowledge builds from the byte the bus drives.
    ///
    /// A real `IM 0` places a whole instruction on the data bus and the CPU
    /// executes it, which needs bus access the `Z80Bus` trait does not model.
    /// Nothing on a Genesis can use it: the Z80's data bus floats high when
    /// nothing is driving it, so the byte is `FF` and the instruction that runs
    /// is `RST 38h`. Any other `RST` encoding is accepted on the same basis
    /// because it costs one line, and anything else is a genuine
    /// not-implemented rather than a silent `NOP`.
    fn mode0_vector(&self) -> u16 {
        let op = self.int_vector();
        if op & 0xC7 == 0xC7 {
            u16::from(op & 0x38)
        } else {
            self.unimplemented(&format!("{op:02X} (IM 0 interrupt vector)"))
        }
    }

    /// The vector an `IM 2` acknowledge reads from `(I << 8) | int_vector`, low
    /// byte first.
    ///
    /// Real hardware drives the low byte with bit 0 held high, since a
    /// 7-bit vector table is the common case; `int_vector` is the bus's
    /// business and this uses it exactly as given.
    fn mode2_vector(&mut self, bus: &mut impl Z80Bus) -> u16 {
        let base = (u16::from(self.i()) << 8) | u16::from(self.int_vector());
        let low = u16::from(bus.read(base));
        let high = u16::from(bus.read(base.wrapping_add(1)));
        (high << 8) | low
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::RamBus;
    use crate::{FLAG_PV, IntMode, Z80};

    /// A CPU at `0x0200` with `program` under it, a stack out of the way, and
    /// the interrupt state clear so each test only sets what it is about.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        let bus = RamBus::at(pc, program);
        cpu.set_pc(pc);
        cpu.set_sp(0x8000);
        (cpu, bus)
    }

    /// `IM 1` with the line asserted: `RST 38h` with no opcode fetched, 13
    /// T-states, and both flip-flops shadowed clear.
    #[test]
    fn im1_jumps_to_38_13t() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(true);
        cpu.set_iff2(true);
        cpu.set_int_line(true);

        assert_eq!(cpu.step(&mut bus), 13, "IM 1 entry is 13 T-states");

        assert_eq!(cpu.pc(), 0x0038);
        assert_eq!(cpu.wz(), 0x0038, "wz is the vector");
        assert_eq!(cpu.sp(), 0x7FFE);
        assert_eq!(
            bus.writes,
            vec![(0x7FFF, 0x02), (0x7FFE, 0x00)],
            "the interrupted PC is pushed high byte first"
        );
        assert!(
            bus.reads.is_empty(),
            "the acknowledge replaces the opcode fetch, reads: {:?}",
            bus.reads
        );
        assert_eq!(cpu.r(), 1, "the acknowledge is an M1 slot");
        assert!(!cpu.iff1() && !cpu.iff2(), "a maskable entry clears both");
        assert!(!cpu.halted());
    }

    /// `IM 2` reads the two bytes at `I:int_vector` and jumps there.
    #[test]
    fn im2_reads_vector_table() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode2);
        cpu.set_iff1(true);
        cpu.set_sp(0x8000);
        cpu.set_i(0xC1);
        cpu.set_int_vector(0x3E);
        bus.ram[0xC13E] = 0x00;
        bus.ram[0xC13F] = 0x40;
        cpu.set_int_line(true);

        assert_eq!(cpu.step(&mut bus), 19, "IM 2 entry is 19 T-states");

        assert_eq!(cpu.pc(), 0x4000);
        assert_eq!(cpu.wz(), 0x4000, "wz is the vector, not the table address");
        assert_eq!(bus.reads, vec![0xC13E, 0xC13F], "the table read, low first");
        assert_eq!(cpu.sp(), 0x7FFE);
        assert_eq!(bus.writes, vec![(0x7FFF, 0x02), (0x7FFE, 0x00)]);
    }

    /// The Genesis case: `IM 0` with a floating data bus is `RST 38h`.
    #[test]
    fn im0_ff_acts_as_rst38() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        // The default vector: nothing driving the bus reads back as 0xFF, which
        // is `RST 38h`. `IM 0` is also the mode the CPU comes out of reset in.
        assert_eq!(cpu.int_vector(), 0xFF);
        cpu.set_iff1(true);
        cpu.set_int_line(true);

        assert_eq!(cpu.step(&mut bus), 13);
        assert_eq!(cpu.pc(), 0x0038);
        assert_eq!(cpu.wz(), 0x0038);
        assert_eq!(bus.writes, vec![(0x7FFF, 0x02), (0x7FFE, 0x00)]);
        assert_eq!(cpu.r(), 1);
    }

    /// Every other `RST` encoding decodes the same way, since the byte is only
    /// read for its `y` field.
    #[test]
    fn im0_accepts_the_other_rst_encodings() {
        for (vector, target) in [
            (0xC7u8, 0x0000u16),
            (0xCF, 0x0008),
            (0xD7, 0x0010),
            (0xDF, 0x0018),
            (0xE7, 0x0020),
            (0xEF, 0x0028),
            (0xF7, 0x0030),
            (0xFF, 0x0038),
        ] {
            let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
            cpu.set_int_vector(vector);
            cpu.set_iff1(true);
            cpu.set_int_line(true);

            assert_eq!(cpu.step(&mut bus), 13, "{vector:02X} on the bus");
            assert_eq!(cpu.pc(), target, "{vector:02X} on the bus");
        }
    }

    /// A non-`RST` vector byte would have to be executed. That is out of scope
    /// (see [`Z80::mode0_vector`]) and says so rather than quietly doing
    /// nothing.
    #[test]
    fn im0_rejects_a_vector_that_is_not_a_rst() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_int_vector(0x00);
        cpu.set_iff1(true);
        cpu.set_int_line(true);

        let message = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cpu.step(&mut bus)))
            .expect_err("executing an arbitrary IM 0 bus byte is not implemented");
        let message = message
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(message.contains("IM 0 interrupt vector"), "{message}");
        assert_eq!(cpu.pc(), 0x0200, "the vector is resolved before PC moves");
        assert!(cpu.iff1(), "and before the flip-flops are shadowed");
        assert!(bus.writes.is_empty());
    }

    /// `DI` closes the gate: no interrupt, and the instruction under `PC` runs.
    #[test]
    fn di_blocks_int() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(false);
        cpu.set_int_line(true);

        assert_eq!(cpu.step(&mut bus), 4, "with IFF1 clear the NOP just runs");
        assert_eq!(cpu.pc(), 0x0201);
        assert_eq!(bus.reads, vec![0x0200]);
        assert!(bus.writes.is_empty());
        assert_eq!(cpu.wz(), 0, "nothing loaded wz");

        // And the mode alone is not enough either: IM 2 with the gate shut must
        // not read the vector table.
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode2);
        cpu.set_int_line(true);
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(bus.reads, vec![0x0200], "no vector read either");
    }

    /// `EI` opens the gate only after the *next* instruction has run.
    #[test]
    fn ei_delays_one_instruction() {
        // EI / NOP at 0x0200, the line held high the whole time.
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0xFB, 0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_int_line(true);

        assert_eq!(cpu.step(&mut bus), 4, "EI itself takes 4");
        assert!(cpu.iff1());
        assert_eq!(cpu.pc(), 0x0201);

        assert_eq!(cpu.step(&mut bus), 4, "the instruction after EI still runs");
        assert_eq!(cpu.pc(), 0x0202, "the NOP ran, it was not displaced");

        assert_eq!(cpu.step(&mut bus), 13, "and the interrupt lands after it");
        assert_eq!(cpu.pc(), 0x0038);
        assert_eq!(
            bus.writes,
            vec![(0x7FFF, 0x02), (0x7FFE, 0x02)],
            "the return address is the instruction that never ran"
        );
    }

    /// `HALT` ends on an interrupt, and the handler returns to the byte after
    /// the `HALT` because the halted CPU never fetched the next opcode.
    #[test]
    fn halt_exits_on_int_and_returns_after_halt() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x76]);
        cpu.set_im(IntMode::Mode1);

        assert_eq!(cpu.step(&mut bus), 4, "HALT itself");
        assert!(cpu.halted());
        assert_eq!(cpu.pc(), 0x0201, "PC sits past the HALT");

        cpu.set_iff1(true);
        cpu.set_int_line(true);
        assert_eq!(cpu.step(&mut bus), 13, "the wake-up costs an entry");
        assert!(!cpu.halted(), "and it is over");
        assert_eq!(cpu.pc(), 0x0038);
        assert_eq!(
            bus.writes,
            vec![(0x7FFF, 0x02), (0x7FFE, 0x01)],
            "return address is HALT + 1"
        );
        assert_eq!(cpu.r(), 2, "the HALT fetch and the acknowledge M1 slot");
    }

    /// A halted CPU with nothing to wake it keeps spinning at 4 T a go, which is
    /// what a frame loop pacing on T-states needs.
    #[test]
    fn halt_spins_until_the_line_goes_high() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x76]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(true);

        for _ in 0..3 {
            assert_eq!(cpu.step(&mut bus), 4);
            assert_eq!(cpu.pc(), 0x0201);
        }
        assert_eq!(
            bus.reads,
            vec![0x0200],
            "one opcode fetch for the HALT, and nothing after it"
        );

        cpu.set_int_line(true);
        assert_eq!(cpu.step(&mut bus), 13);
        assert_eq!(cpu.pc(), 0x0038);
    }

    /// `NMI` ignores `IFF1` entirely, and keeps `IFF2` for the handler.
    #[test]
    fn nmi_ignores_iff1_and_preserves_iff2() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(false);
        cpu.set_iff2(true);
        cpu.nmi();

        assert_eq!(cpu.step(&mut bus), 11, "NMI entry is 11 T-states");

        assert_eq!(cpu.pc(), 0x0066);
        assert_eq!(cpu.wz(), 0x0066);
        assert_eq!(
            bus.writes,
            vec![(0x7FFF, 0x02), (0x7FFE, 0x00)],
            "no opcode was executed to be undone"
        );
        assert!(bus.reads.is_empty());
        assert_eq!(cpu.r(), 1);
        assert!(!cpu.iff1(), "IFF1 is cleared");
        assert!(cpu.iff2(), "IFF2 keeps its value for RETI");
    }

    /// `NMI` is not held off by `EI` either, and it takes precedence when both
    /// interrupt sources are asking at once.
    #[test]
    fn nmi_beats_a_held_int_and_the_ei_latch() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(true);
        cpu.set_iff2(true);
        cpu.set_ei_pending(true);
        cpu.set_int_line(true);
        cpu.nmi();

        assert_eq!(
            cpu.step(&mut bus),
            11,
            "neither the EI latch nor a held INT holds off NMI"
        );
        assert_eq!(cpu.pc(), 0x0066, "NMI wins the slot");
        assert!(
            cpu.iff2(),
            "and keeps the IFF2 a maskable entry would have shadowed"
        );
        assert!(bus.reads.is_empty(), "no vector table read either");
    }

    /// The other half of the `NMI` rule: the `RETI` at the end of the handler
    /// puts `IFF1` back from the `IFF2` the `NMI` preserved.
    #[test]
    fn retn_restores_iff1_after_nmi() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0xED, 0x45]);
        cpu.set_iff1(false);
        cpu.set_iff2(true);
        cpu.nmi();

        assert_eq!(cpu.step(&mut bus), 11, "NMI takes the slot the RETN was in");
        assert_eq!(cpu.pc(), 0x0066);
        assert!(!cpu.iff1());
        assert!(cpu.iff2());

        cpu.set_pc(0x0200);
        assert_eq!(cpu.step(&mut bus), 14, "and now the handler's RETN runs");
        assert!(cpu.iff1(), "RETI restores IFF1 from IFF2");
        assert!(cpu.iff2());
    }

    /// `/INT` is a level: while it is held, every time the gate reopens the
    /// interrupt fires again.
    #[test]
    fn int_line_level_triggered() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(true);
        cpu.set_int_line(true);

        assert_eq!(cpu.step(&mut bus), 13, "first fire");
        assert_eq!(cpu.pc(), 0x0038);
        assert_eq!(cpu.step(&mut bus), 4, "IFF1 is clear, so it cannot refire");
        assert_eq!(cpu.pc(), 0x0039);

        // The handler's EI, then one more instruction, then the line gets
        // through again because nothing ever dropped it.
        cpu.set_pc(0x0200);
        let mut bus = RamBus::at(0x0200, &[0xFB]);
        assert_eq!(cpu.step(&mut bus), 4, "EI");
        assert_eq!(cpu.step(&mut bus), 4, "and one instruction of the delay");
        assert_eq!(cpu.step(&mut bus), 13, "the held line fires again");
        assert_eq!(cpu.pc(), 0x0038);
    }

    /// Dropping the line is what stops it: a level that went away is not
    /// remembered.
    #[test]
    fn int_line_is_dropped_when_the_bus_releases_it() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(true);
        cpu.set_int_line(true);
        cpu.set_int_line(false);

        assert_eq!(cpu.step(&mut bus), 4, "no longer asserted, so no entry");
        assert_eq!(cpu.pc(), 0x0201);
    }

    /// The NMOS `LD A,I` bug: an interrupt taken straight after one of the two
    /// instructions that copy `IFF2` into `P/V` clears the flag.
    #[test]
    fn int_after_ld_a_i_resets_pv() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0xED, 0x57]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(true);
        cpu.set_iff2(true);
        cpu.set_flags(0);

        assert_eq!(cpu.step(&mut bus), 9, "LD A,I");
        assert!(cpu.p(), "and the latch that says so");
        assert!(cpu.get_flag(FLAG_PV), "P/V mirrors IFF2 = 1");

        cpu.set_int_line(true);
        assert_eq!(cpu.step(&mut bus), 13);
        assert!(
            !cpu.get_flag(FLAG_PV),
            "the interrupt reset P/V, as it does on NMOS silicon"
        );
        assert!(!cpu.p(), "and the latch is consumed like any instruction's");

        // With the latch clear the flag is nobody's to reset.
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_im(IntMode::Mode1);
        cpu.set_iff1(true);
        cpu.set_flags(FLAG_PV);
        cpu.set_int_line(true);
        assert_eq!(cpu.step(&mut bus), 13);
        assert!(cpu.get_flag(FLAG_PV), "an ordinary entry leaves the flags");
    }

    /// Reset drops pending pin state: the bus has to ask again.
    #[test]
    fn reset_drops_pending_interrupts() {
        let (mut cpu, mut bus) = cpu_at(0x0200, &[0x00]);
        cpu.set_int_line(true);
        cpu.nmi();

        cpu.reset();
        cpu.set_pc(0x0200);

        assert_eq!(cpu.step(&mut bus), 4, "the pending NMI is gone");
        assert_eq!(cpu.pc(), 0x0201);
        assert_eq!(cpu.int_vector(), 0xFF, "the floating-bus default survives");
    }
}
