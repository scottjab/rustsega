//! 8-bit loads (M2-04, #47): `LD r,r'`, `LD r,n`, the `(HL)` and `(IX+d)`
//! forms, and the four accumulator-only memory forms.
//!
//! None of them touch the flags, and every one of them leaves `q`, `p` and the
//! EI latch clear - that is what all 1000 cases of every LD file in the
//! conformance suite show, so it is done in one place ([`Z80::clear_latches`]).
//!
//! The undocumented part of this ticket is `wz`. The Z80 leaves a near miss of
//! the address it last touched there, and the suite compares it, so each form
//! gets the exact value the suite expects; the rule is documented at each
//! helper and every one was checked against 1000 hardware cases.

use crate::decode::{y, z};
use crate::{IndexMode, Z80, Z80Bus};

impl Z80 {
    /// Read register slot `idx` (the 3-bit `r` field of an opcode), where `6`
    /// means the memory at `HL` or `IX+d`. Callers resolve slot `6` themselves.
    pub(crate) fn reg_slot(&self, idx: u8) -> u8 {
        match idx {
            0 => self.b(),
            1 => self.c(),
            2 => self.d(),
            3 => self.e(),
            4 => self.h(),
            5 => self.l(),
            7 => self.a(),
            _ => unreachable!("slot 6 is a memory access, not a register"),
        }
    }

    /// Write register slot `idx`, as [`Z80::reg_slot`].
    pub(crate) fn set_reg_slot(&mut self, idx: u8, value: u8) {
        match idx {
            0 => self.set_b(value),
            1 => self.set_c(value),
            2 => self.set_d(value),
            3 => self.set_e(value),
            4 => self.set_h(value),
            5 => self.set_l(value),
            7 => self.set_a(value),
            _ => unreachable!("slot 6 is a memory access, not a register"),
        }
    }

    /// Fetch an `(IX+d)` / `(IY+d)` displacement and return the address.
    ///
    /// Leaves `wz` at that address: every indexed access in the suite ends with
    /// `wz == base + d` (`dd 46`, `fd 77`, `dd 36`: 1000 cases each). The
    /// displacement is signed, so `d = 0xFE` reaches two bytes below the base.
    pub(crate) fn index_address(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u16 {
        let disp = i16::from(self.fetch_byte(bus) as i8);
        let base = match mode {
            IndexMode::Hl => self.hl(),
            IndexMode::Ix => self.ix(),
            IndexMode::Iy => self.iy(),
        };
        let addr = base.wrapping_add(disp as u16);
        self.set_wz(addr);
        addr
    }

    /// `LD r,r'` (opcode `40`-`7F`, `76` excluded: that is HALT), where slot `6`
    /// on either side is the memory at `HL`. No flags and no `wz` change; 4
    /// T-states for a register-to-register move, 7 once memory is involved.
    pub(crate) fn ld_rr(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let src = z(op);
        let value = if src == 6 {
            bus.read(self.hl())
        } else {
            self.reg_slot(src)
        };
        let dst = y(op);
        if dst == 6 {
            bus.write(self.hl(), value);
        } else {
            self.set_reg_slot(dst, value);
        }
        self.clear_latches();
        // A register-to-register move is 4 T-states; routing one side through
        // the memory at HL costs a memory access on top (`46`, `70`: 7 each).
        if src == 6 || dst == 6 { 7 } else { 4 }
    }

    /// `LD r,n` (`06 0E 16 1E 26 2E 3E`; `36` is `LD (HL),n`). 7 T-states.
    pub(crate) fn ld_r_n(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let value = self.fetch_byte(bus);
        self.set_reg_slot(y(op), value);
        self.clear_latches();
        7
    }

    /// `LD (HL),n` (`36`): 10 T-states, and `wz` does *not* move.
    pub(crate) fn ld_hl_ind_n(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let value = self.fetch_byte(bus);
        let addr = self.hl();
        bus.write(addr, value);
        self.clear_latches();
        10
    }

    /// `LD A,(BC)` / `LD A,(DE)`: 7 T-states, `wz = addr + 1` (both files, 1000
    /// cases each).
    pub(crate) fn ld_a_ind_rr(&mut self, bus: &mut impl Z80Bus, addr: u16) -> u32 {
        let value = bus.read(addr);
        self.set_a(value);
        self.set_wz(addr.wrapping_add(1));
        self.clear_latches();
        7
    }

    /// `LD (BC),A` / `LD (DE),A`: 7 T-states. `wz` ends up with the low byte of
    /// `addr + 1` and the byte written in the high byte (`02`, `12`: 1000 each).
    pub(crate) fn ld_ind_rr_a(&mut self, bus: &mut impl Z80Bus, addr: u16) -> u32 {
        let value = self.a();
        bus.write(addr, value);
        self.set_wz((u16::from(value) << 8) | (addr.wrapping_add(1) & 0x00FF));
        self.clear_latches();
        7
    }

    /// `LD A,(nn)` (`3A`): 13 T-states, `wz = nn + 1`.
    pub(crate) fn ld_a_ind_nn(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let addr = self.fetch_word(bus);
        let value = bus.read(addr);
        self.set_a(value);
        self.set_wz(addr.wrapping_add(1));
        self.clear_latches();
        13
    }

    /// `LD (nn),A` (`32`): 13 T-states, `wz = ((nn + 1) & 0xFF) | (A << 8)`.
    pub(crate) fn ld_ind_nn_a(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let addr = self.fetch_word(bus);
        let value = self.a();
        bus.write(addr, value);
        self.set_wz((u16::from(value) << 8) | (addr.wrapping_add(1) & 0x00FF));
        self.clear_latches();
        13
    }

    /// `LD r,(IX+d)` / `LD r,(IY+d)`: 19 T-states. The destination is a *real*
    /// register - `LD H,(IX+d)` writes H, not IXH (the IXH/IXL forms are
    /// M2-15, #58).
    pub(crate) fn ld_r_ind_index(&mut self, bus: &mut impl Z80Bus, mode: IndexMode, op: u8) -> u32 {
        let addr = self.index_address(bus, mode);
        let value = bus.read(addr);
        self.set_reg_slot(y(op), value);
        self.clear_latches();
        19
    }

    /// `LD (IX+d),r`: 19 T-states. `r` is a real register, and never `(HL)`:
    /// `DD 76` / `FD 76` are a rotted prefix plus HALT, per the suite, and
    /// `exec_index` keeps them out of here.
    pub(crate) fn ld_index_ind_r(&mut self, bus: &mut impl Z80Bus, mode: IndexMode, op: u8) -> u32 {
        let addr = self.index_address(bus, mode);
        let value = self.reg_slot(z(op));
        bus.write(addr, value);
        self.clear_latches();
        19
    }

    /// `LD (IX+d),n` (`DD 36 d n`): 19 T-states, `wz = IX + d`.
    pub(crate) fn ld_index_ind_n(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let addr = self.index_address(bus, mode);
        let value = self.fetch_byte(bus);
        bus.write(addr, value);
        self.clear_latches();
        19
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::RamBus;
    use crate::{FLAG_C, FLAG_Z, IntMode, Z80};

    /// `LD B,C` (`41`): 4 T-states, and nothing else moves.
    #[test]
    fn ld_b_c() {
        const PC: u16 = 0x1000;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_b(0x12);
        cpu.set_c(0x34);
        let mut bus = RamBus::at(PC, &[0x41]);

        assert_eq!(cpu.step(&mut bus), 4);

        assert_eq!((cpu.b(), cpu.c()), (0x34, 0x34));
        assert_eq!(cpu.pc(), 0x1001);
        assert_eq!(cpu.r(), 1);
        assert!(bus.writes.is_empty(), "a register load must not write RAM");
        assert_eq!(bus.reads, vec![0x1000]);
    }

    /// `LD A,n` (`3E nn`): 7 T-states, two bytes consumed.
    #[test]
    fn ld_a_n() {
        const PC: u16 = 0x2000;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        let mut bus = RamBus::at(PC, &[0x3E, 0x9A]);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x9A);
        assert_eq!(cpu.pc(), 0x2002);
    }

    /// `LD (HL),n` (`36 n nn`): 10 T-states, and `wz` stays put.
    #[test]
    fn ld_hl_ind_n() {
        const PC: u16 = 0x0500;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_hl(0x8000);
        cpu.set_wz(0x1122);
        let mut bus = RamBus::at(PC, &[0x36, 0x77]);

        assert_eq!(cpu.step(&mut bus), 10);

        assert_eq!(bus.peek(0x8000), 0x77);
        assert_eq!(bus.writes, vec![(0x8000, 0x77)]);
        assert_eq!(cpu.wz(), 0x1122, "LD (HL),n must not disturb wz");
        assert_eq!(cpu.pc(), 0x0502);
    }

    /// `LD A,(BC)` (`0A`): 7 T-states, and `wz` ends up one past the address.
    #[test]
    fn ld_a_bc_sets_wz() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_bc(0x9010);
        let mut bus = RamBus::at(PC, &[0x0A]).with_byte(0x9010, 0x5C);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(cpu.a(), 0x5C);
        assert_eq!(cpu.wz(), 0x9011);
        assert_eq!(bus.reads, vec![0x0100, 0x9010]);
    }

    /// `LD (nn),A` (`32 nn nn`): 13 T-states, and `wz` is the low byte of
    /// `nn + 1` with the stored value on top of it.
    #[test]
    fn ld_nn_a_sets_wz_high_to_a() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_a(0xCD);
        // nn = 0x6002, so nn + 1 = 0x6003 and only its low byte survives.
        let mut bus = RamBus::at(PC, &[0x32, 0x02, 0x60]);

        assert_eq!(cpu.step(&mut bus), 13);

        assert_eq!(bus.writes, vec![(0x6002, 0xCD)]);
        assert_eq!(cpu.wz(), 0xCD03);
        assert_eq!(cpu.pc(), 0x0103);
    }

    /// `LD A,(DE)` (`1A`): 7 T-states, `wz = DE + 1`.
    #[test]
    fn ld_a_de_reads_and_sets_wz() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_de(0x01FF);
        let mut bus = RamBus::at(PC, &[0x1A]).with_byte(0x01FF, 0x77);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!((cpu.a(), cpu.wz()), (0x77, 0x0200));
    }

    /// `LD (DE),A` (`12`) with `DE = 0x01FF`: `DE + 1` wraps to `0x0200`, so only
    /// `0x00` survives in `wz`'s low byte and the stored byte sits on top of it.
    #[test]
    fn ld_de_a_stores_and_splits_wz() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_de(0x01FF);
        cpu.set_a(0x31);
        let mut bus = RamBus::at(PC, &[0x12]);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(bus.writes, vec![(0x01FF, 0x31)]);
        assert_eq!(cpu.wz(), 0x3100);
    }

    /// `LD B,(HL)` (`46`) reads memory and leaves `wz` where it was.
    #[test]
    fn ld_b_hl_reads_and_leaves_wz() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_hl(0x2000);
        cpu.set_wz(0x4000);
        let mut bus = RamBus::at(PC, &[0x46]).with_byte(0x2000, 0x11);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!((cpu.b(), cpu.wz()), (0x11, 0x4000));
    }

    /// `LD (HL),D` (`72`) writes memory and, like the read, leaves `wz` alone.
    #[test]
    fn ld_hl_d_writes_and_leaves_wz() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_hl(0x2000);
        cpu.set_d(0xAB);
        cpu.set_wz(0x4000);
        let mut bus = RamBus::at(PC, &[0x72]);

        assert_eq!(cpu.step(&mut bus), 7);

        assert_eq!(bus.writes, vec![(0x2000, 0xAB)]);
        assert_eq!(cpu.wz(), 0x4000);
    }

    /// `LD A,(nn)` (`3A nn nn`): 13 T-states, `wz = nn + 1`.
    #[test]
    fn ld_a_nn_sets_wz() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        let mut bus = RamBus::at(PC, &[0x3A, 0xFF, 0x7F]).with_byte(0x7FFF, 0x64);

        assert_eq!(cpu.step(&mut bus), 13);

        assert_eq!((cpu.a(), cpu.wz(), cpu.pc()), (0x64, 0x8000, 0x0103));
    }

    /// `LD B,(IX+d)` with `d = 0xFE` reaches *below* the index register: IX is
    /// 0x4000, so the byte lives at 0x3FFE, and `wz` gets that address.
    #[test]
    fn ld_ix_d_negative_displacement() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_ix(0x4000);
        let mut bus = RamBus::at(PC, &[0xDD, 0x46, 0xFE]).with_byte(0x3FFE, 0x2B);

        assert_eq!(cpu.step(&mut bus), 19);

        assert_eq!(cpu.b(), 0x2B);
        assert_eq!(cpu.wz(), 0x3FFE);
        assert_eq!(cpu.ix(), 0x4000, "reading (IX+d) must not move IX");
        assert_eq!(
            bus.reads,
            vec![PC, PC + 1, PC + 2, 0x3FFE],
            "prefix, opcode, displacement, then the byte it points at"
        );
        assert_eq!(cpu.pc(), 0x0103);
    }

    /// `LD (IY+d),C` (`FD 71 d`): 19 T-states, `wz = IY + d`, and the source is
    /// the real C.
    #[test]
    fn ld_iy_d_store() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_iy(0x3100);
        cpu.set_c(0x9E);
        let mut bus = RamBus::at(PC, &[0xFD, 0x71, 0x05]);

        assert_eq!(cpu.step(&mut bus), 19);

        assert_eq!(bus.writes, vec![(0x3105, 0x9E)]);
        assert_eq!(cpu.wz(), 0x3105);
    }

    /// `LD L,(IX+d)` (`DD 6E d`) writes the *real* L. Only the `LD IXH,r` style
    /// forms are undocumented (M2-15, #58); the destination of an indexed read
    /// is an ordinary register.
    #[test]
    fn ld_l_ix_d_writes_real_l() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_ix(0x1000);
        cpu.set_l(0x11);
        let mut bus = RamBus::at(PC, &[0xDD, 0x6E, 0x01]).with_byte(0x1001, 0x22);

        assert_eq!(cpu.step(&mut bus), 19);

        assert_eq!(cpu.l(), 0x22);
        assert_eq!(cpu.h(), 0, "H must not be touched by LD L,(IX+d)");
    }

    /// The accumulator form, `LD A,(IX+d)` (`DD 7E d`).
    #[test]
    fn ld_a_ix_d() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_ix(0xFF00);
        let mut bus = RamBus::at(PC, &[0xDD, 0x7E, 0x00]).with_byte(0xFF00, 0x4D);

        assert_eq!(cpu.step(&mut bus), 19);

        assert_eq!((cpu.a(), cpu.wz()), (0x4D, 0xFF00));
    }

    /// `DD 36 d n`: 19 T-states, and `wz` is the written address even though the
    /// value operand is fetched afterwards.
    #[test]
    fn ld_ix_d_n() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_ix(0x0200);
        let mut bus = RamBus::at(PC, &[0xDD, 0x36, 0x10, 0x80]);

        assert_eq!(cpu.step(&mut bus), 19);

        assert_eq!(bus.writes, vec![(0x0210, 0x80)]);
        assert_eq!(cpu.wz(), 0x0210);
        assert_eq!(cpu.pc(), 0x0104);
    }

    /// The trap in this opcode block. The decode tables make `DD 76` an
    /// `LD (IX+d),(HL)`, and that is how I wrote it first, but the suite's
    /// `dd 76` and `fd 76` cases are both 8 T-states with `PC` up by 2: a rotted
    /// prefix and a HALT. Reading it as a load also eats a displacement byte, so
    /// pin the shape instead - prefix and `76` consumed, nothing written, and the
    /// opcode still unimplemented for HALT's ticket (M2-13, #56).
    #[test]
    fn dd_76_is_halt_with_a_wasted_prefix() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_ix(0x2000);
        cpu.set_hl(0x1234);
        let mut bus = RamBus::at(PC, &[0xDD, 0x76, 0x05]).with_byte(0x1234, 0xAB);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cpu.step(&mut bus);
        }))
        .is_err();

        assert!(panicked, "HALT is M2-13's (#56), so DD 76 must still panic");
        assert_eq!(
            cpu.pc(),
            PC + 2,
            "DD 76 must not fetch a displacement, or it was decoded as a load"
        );
        assert!(bus.writes.is_empty(), "DD 76 must not write memory");
    }

    /// LD never touches the flags, and clears `q`, `p` and the EI latch. The
    /// suite shows exactly that for every LD file, in all 1000 cases of each.
    #[test]
    fn loads_leave_the_flags_alone() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_a(0x01);
        cpu.set_c(0x02);
        cpu.set_flags(FLAG_Z | FLAG_C);
        cpu.set_q(0x80);
        cpu.set_p(true);
        cpu.set_im(IntMode::Mode2);

        for opcode in [
            vec![0x41],             // LD B,C
            vec![0x3E, 0x00],       // LD A,n
            vec![0x46],             // LD B,(HL)
            vec![0x0A],             // LD A,(BC)
            vec![0x3A, 0x00, 0x40], // LD A,(nn)
            vec![0xDD, 0x46, 0x00], // LD B,(IX+d)
        ] {
            let mut bus = RamBus::at(PC, &opcode);
            cpu.step(&mut bus);
            assert_eq!(cpu.f(), FLAG_Z | FLAG_C, "flags changed on {opcode:02X?}");
            assert_eq!(cpu.q(), 0, "q not cleared on {opcode:02X?}");
            assert!(!cpu.p(), "p not cleared on {opcode:02X?}");
            assert!(!cpu.ei_pending(), "EI latch not cleared on {opcode:02X?}");
            assert_eq!(cpu.im(), IntMode::Mode2, "IM must survive an LD");
        }
    }

    /// `76` alone is HALT and belongs to M2-13 (#56): still a named panic.
    #[test]
    fn halt_is_left_for_its_ticket() {
        const PC: u16 = 0x0100;
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        let mut bus = RamBus::at(PC, &[0x76]);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cpu.step(&mut bus)));
        assert!(result.is_err(), "76 must still be unimplemented");
    }
}
