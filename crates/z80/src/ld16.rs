//! 16-bit loads, the stack, and the exchanges (M2-05, #48).
//!
//! Nothing in this group computes flags. The one exception is `POP AF`, which
//! loads `F` from memory rather than deriving it, and even that leaves `q` at 0
//! (`f1`: 1000 cases with `final.f` written and `final.q == 0`), so every
//! instruction here ends in [`Z80::clear_latches`].
//!
//! `wz` is per-instruction and comes from the suite rather than from a rule of
//! thumb:
//!
//! | form | `wz` |
//! |---|---|
//! | `LD rr,nn`, `LD IX,nn`, `LD SP,HL`, `PUSH`, `POP`, `EX AF,AF'`, `EXX`, `EX DE,HL` | unchanged |
//! | `LD HL,(nn)`, `LD (nn),HL`, `ED LD rr,(nn)`, `ED LD (nn),rr`, `DD 22`, `DD 2A` | `nn + 1` |
//! | `EX (SP),HL`, `EX (SP),IX` | the value that was just read off the stack |
//!
//! Every helper here returns the instruction's whole T-state cost, prefixed
//! forms included, so each number can be checked straight against the suite's
//! `cycles` array.

use crate::decode::IndexMode;
use crate::{Z80, Z80Bus};

impl Z80 {
    /// `LD rr,nn` (`01`/`11`/`21`/`31`): 10 T-states, `wz` unchanged (1000 cases
    /// of each). Bits 5-4 pick the pair, and there `11` means `SP`.
    pub(crate) fn ld_rr_nn(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let value = self.fetch_word(bus);
        self.set_pair_y(op, value);
        self.clear_latches();
        10
    }

    /// `LD IX,nn` / `LD IY,nn` (`DD/FD 21`): 14 T-states, `wz` unchanged.
    pub(crate) fn ld_index_nn(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let value = self.fetch_word(bus);
        self.set_index(mode, value);
        self.clear_latches();
        14
    }

    /// `LD HL,(nn)` (`2A`): 16 T-states, `wz = nn + 1`.
    pub(crate) fn ld_hl_ind_nn(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let value = self.read_word_at_nn(bus);
        self.set_hl(value);
        self.clear_latches();
        16
    }

    /// `LD (nn),HL` (`22`): 16 T-states, `wz = nn + 1`.
    pub(crate) fn ld_ind_nn_hl(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let value = self.hl();
        self.write_word_at_nn(bus, value);
        self.clear_latches();
        16
    }

    /// `ED 4B/5B/6B/7B` — `LD BC/DE/HL/SP,(nn)`: 20 T-states, `wz = nn + 1`.
    pub(crate) fn ld_rr_ind_nn(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let value = self.read_word_at_nn(bus);
        self.set_pair_y(op, value);
        self.clear_latches();
        20
    }

    /// `ED 43/53/63/73` — `LD (nn),BC/DE/HL/SP`: 20 T-states, `wz = nn + 1`.
    pub(crate) fn ld_ind_nn_rr(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let value = match (op >> 4) & 3 {
            0 => self.bc(),
            1 => self.de(),
            2 => self.hl(),
            _ => self.sp(),
        };
        self.write_word_at_nn(bus, value);
        self.clear_latches();
        20
    }

    /// `DD/FD 2A` — `LD IX,(nn)`: 20 T-states, `wz = nn + 1`, `HL` untouched.
    ///
    /// Worth stating plainly, because most decode tables print `DD 2A` as
    /// `LD HL,(IX+d)`. The suite's `dd 2a`/`fd 2a` say otherwise in all 1000
    /// cases: four bytes fetched (`PC + 4`), 20 T-states, the word read from
    /// `nn` lands in `IX`/`IY`, and `HL` does not move. Same for `DD 22`/`FD 22`
    /// below, which store the index pair to `nn`.
    pub(crate) fn ld_index_ind_nn(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let value = self.read_word_at_nn(bus);
        self.set_index(mode, value);
        self.clear_latches();
        20
    }

    /// `DD/FD 22` — `LD (nn),IX`: 20 T-states, `wz = nn + 1`. See
    /// [`Z80::ld_index_ind_nn`] for why this is not the indexed store the
    /// decode tables advertise.
    pub(crate) fn ld_ind_nn_index(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let value = match mode {
            IndexMode::Ix => self.ix(),
            IndexMode::Iy => self.iy(),
            IndexMode::Hl => self.hl(),
        };
        self.write_word_at_nn(bus, value);
        self.clear_latches();
        20
    }

    /// Fetch `nn`, read the little-endian word there, leave `wz` at `nn + 1`.
    ///
    /// Every `nn`-taking form here ends with `nn + 1` in `wz` - the low byte
    /// from the operand fetch, the high byte whatever the fetch of `nn + 1`
    /// left behind - so both directions go through here and cannot drift apart.
    fn read_word_at_nn(&mut self, bus: &mut impl Z80Bus) -> u16 {
        let addr = self.fetch_word(bus);
        let lo = bus.read(addr);
        let hi = bus.read(addr.wrapping_add(1));
        self.set_wz(addr.wrapping_add(1));
        ((hi as u16) << 8) | u16::from(lo)
    }

    /// Fetch `nn` and store a little-endian word there, again leaving `wz` at
    /// `nn + 1`.
    fn write_word_at_nn(&mut self, bus: &mut impl Z80Bus, value: u16) {
        let addr = self.fetch_word(bus);
        bus.write(addr, value as u8);
        bus.write(addr.wrapping_add(1), (value >> 8) as u8);
        self.set_wz(addr.wrapping_add(1));
    }

    /// Load one of `BC`, `DE`, `HL`, `SP` from the pair field (bits 5-4) that
    /// `LD rr,nn` and the `ED` word forms share.
    fn set_pair_y(&mut self, op: u8, value: u16) {
        match (op >> 4) & 3 {
            0 => self.set_bc(value),
            1 => self.set_de(value),
            2 => self.set_hl(value),
            _ => self.set_sp(value),
        }
    }

    /// Load `IX` or `IY`. `Hl` means the prefix has already rotted off, so the
    /// `HL` form is what runs.
    fn set_index(&mut self, mode: IndexMode, value: u16) {
        match mode {
            IndexMode::Ix => self.set_ix(value),
            IndexMode::Iy => self.set_iy(value),
            IndexMode::Hl => self.set_hl(value),
        }
    }

    /// `LD SP,HL` (`F9`): 6 T-states. `wz` is *not* loaded with `HL`, despite
    /// what several references claim (`f9`: 1000 cases, `wz` unchanged).
    pub(crate) fn ld_sp_hl(&mut self) -> u32 {
        let value = self.hl();
        self.set_sp(value);
        self.clear_latches();
        6
    }

    /// `LD SP,IX` / `LD SP,IY` (`DD/FD F9`): 10 T-states, `wz` unchanged.
    pub(crate) fn ld_sp_index(&mut self, mode: IndexMode) -> u32 {
        let value = match mode {
            IndexMode::Ix => self.ix(),
            IndexMode::Iy => self.iy(),
            IndexMode::Hl => self.hl(),
        };
        self.set_sp(value);
        self.clear_latches();
        10
    }

    /// `PUSH qq` (`C5`/`D5`/`E5`/`F5`): 11 T-states.
    ///
    /// `SP` drops by two, then the high byte goes to `SP - 1` first and the low
    /// byte to `SP - 2` second. The suite's traces show the writes in that
    /// order, and for `F5` the high byte is `A` (`c5`: `SP-1` = `B`, `SP-2` = `C`).
    pub(crate) fn push_qq(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let value = match (op >> 4) & 3 {
            0 => self.bc(),
            1 => self.de(),
            2 => self.hl(),
            // `PUSH AF` is the one that pushes the flags, and it pushes the
            // whole register: A is the high byte.
            _ => self.af(),
        };
        self.push16(bus, value);
        self.clear_latches();
        11
    }

    /// `PUSH IX` / `PUSH IY` (`DD/FD E5`): 15 T-states.
    pub(crate) fn push_index(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let value = match mode {
            IndexMode::Ix => self.ix(),
            IndexMode::Iy => self.iy(),
            IndexMode::Hl => self.hl(),
        };
        self.push16(bus, value);
        self.clear_latches();
        15
    }

    /// The stack half of `PUSH`: two writes, high byte first.
    fn push16(&mut self, bus: &mut impl Z80Bus, value: u16) {
        let sp = self.sp().wrapping_sub(2);
        self.set_sp(sp);
        bus.write(sp.wrapping_add(1), (value >> 8) as u8);
        bus.write(sp, value as u8);
    }

    /// `POP BC/DE/HL/AF` (`C1`/`D1`/`E1`/`F1`): 10 T-states, `wz` unchanged.
    pub(crate) fn pop_qq(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let value = self.pop16(bus);
        match (op >> 4) & 3 {
            0 => self.set_bc(value),
            1 => self.set_de(value),
            2 => self.set_hl(value),
            // `POP AF` is the only form in this group that writes the flag
            // register, straight out of memory. `q` still ends at 0.
            _ => self.set_af(value),
        }
        self.clear_latches();
        10
    }

    /// `POP IX` / `POP IY` (`DD/FD E1`): 14 T-states, `wz` unchanged.
    pub(crate) fn pop_index(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let value = self.pop16(bus);
        self.set_index(mode, value);
        self.clear_latches();
        14
    }

    /// The stack half of `POP`: low byte at `SP`, high byte at `SP + 1`, then
    /// `SP` moves up by two (`c1` trace: read `SP`, then `SP + 1`).
    fn pop16(&mut self, bus: &mut impl Z80Bus) -> u16 {
        let sp = self.sp();
        let lo = bus.read(sp);
        let hi = bus.read(sp.wrapping_add(1));
        self.set_sp(sp.wrapping_add(2));
        ((hi as u16) << 8) | u16::from(lo)
    }

    /// `EX AF,AF'` (`08`): 4 T-states, and only `AF` moves.
    pub(crate) fn ex_af_af(&mut self) -> u32 {
        let main = self.af();
        self.set_af(self.af_shadow());
        self.set_af_shadow(main);
        self.clear_latches();
        4
    }

    /// `EXX` (`D9`): 4 T-states. Swaps `BC`, `DE` and `HL` with their shadows
    /// and nothing else - `AF`, `AF'`, `SP`, `IX`, `IY` all stay put, and so
    /// does `wz` (`d9`: 1000 cases).
    pub(crate) fn exx(&mut self) -> u32 {
        let bc = self.bc();
        self.set_bc(self.bc_shadow());
        self.set_bc_shadow(bc);

        let de = self.de();
        self.set_de(self.de_shadow());
        self.set_de_shadow(de);

        let hl = self.hl();
        self.set_hl(self.hl_shadow());
        self.set_hl_shadow(hl);

        self.clear_latches();
        4
    }

    /// `EX DE,HL` (`EB`): 4 T-states.
    ///
    /// The index prefix cannot reach it, so `DD EB` wastes the prefix and swaps
    /// `DE` and `HL` anyway (`dd eb`/`fd eb`: 8 T-states, `PC + 2`, swapped in
    /// all 1000 cases).
    pub(crate) fn ex_de_hl(&mut self) -> u32 {
        let de = self.de();
        self.set_de(self.hl());
        self.set_hl(de);
        self.clear_latches();
        4
    }

    /// `EX (SP),HL` (`E3`): 19 T-states.
    ///
    /// `EX (SP),IX` (`DD/FD E3`) is the same dance with the index pair, 4
    /// T-states slower. Both read the low byte at `SP` and the high byte at
    /// `SP + 1`, write the pair back high byte first, leave `SP` alone, and end
    /// with `wz` holding the value that came off the stack (`e3`/`dd e3`: 1000
    /// cases each).
    pub(crate) fn ex_sp_index(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let sp = self.sp();
        let lo = bus.read(sp);
        let hi = bus.read(sp.wrapping_add(1));
        let from_stack = ((hi as u16) << 8) | u16::from(lo);

        let (high, low) = match mode {
            IndexMode::Hl => (self.h(), self.l()),
            IndexMode::Ix => ((self.ix() >> 8) as u8, self.ix() as u8),
            IndexMode::Iy => ((self.iy() >> 8) as u8, self.iy() as u8),
        };
        bus.write(sp.wrapping_add(1), high);
        bus.write(sp, low);

        self.set_index(mode, from_stack);
        self.set_wz(from_stack);
        self.clear_latches();
        if mode == IndexMode::Hl { 19 } else { 23 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::RamBus;

    const PC: u16 = 0x0100;

    /// Reads one of the 16-bit registers back out of a CPU, so the table-driven
    /// cases below can say which one they expect an opcode to have touched.
    type Read = fn(&Z80) -> u16;

    #[test]
    fn ld_rr_nn_loads_pairs_and_sp() {
        let cases: [(u8, &str, Read); 4] = [
            (0x01, "BC", |c| c.bc()),
            (0x11, "DE", |c| c.de()),
            (0x21, "HL", |c| c.hl()),
            (0x31, "SP", |c| c.sp()),
        ];
        for (op, name, read) in cases {
            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_wz(0x2345);
            let mut bus = RamBus::at(PC, &[op, 0x34, 0x12]);
            assert_eq!(cpu.step(&mut bus), 10, "LD {name},nn T-states");
            assert_eq!(read(&cpu), 0x1234, "LD {name},nn value");
            assert_eq!(cpu.pc(), PC + 3, "LD {name},nn PC");
            assert_eq!(cpu.wz(), 0x2345, "LD {name},nn leaves wz alone");
        }
    }

    #[test]
    fn ld_hl_ind_nn_and_ld_ind_nn_hl_agree_on_layout_and_wz() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_wz(0x2345);
        let mut bus = RamBus::at(PC, &[0x2A, 0x00, 0x40])
            .with_byte(0x4000, 0x78)
            .with_byte(0x4001, 0x56);
        assert_eq!(cpu.step(&mut bus), 16);
        assert_eq!(cpu.hl(), 0x5678, "little-endian: low byte at nn");
        assert_eq!(cpu.wz(), 0x4001, "LD HL,(nn) leaves wz at nn + 1");
        assert_eq!(cpu.pc(), PC + 3);

        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_hl(0x1234);
        let mut bus = RamBus::at(PC, &[0x22, 0x00, 0x40]);
        assert_eq!(cpu.step(&mut bus), 16);
        assert_eq!(bus.peek(0x4000), 0x34);
        assert_eq!(bus.peek(0x4001), 0x12);
        assert_eq!(cpu.wz(), 0x4001);
        assert_eq!(cpu.pc(), PC + 3);
    }

    #[test]
    fn ed_ld_rr_ind_nn_covers_all_four_pairs() {
        // ED 4B/5B/6B/7B load BC/DE/HL/SP from (nn); ED 43/53/63/73 store them.
        let cases: [(u8, u8, Read); 4] = [
            (0x4B, 0x43, |c| c.bc()),
            (0x5B, 0x53, |c| c.de()),
            (0x6B, 0x63, |c| c.hl()),
            (0x7B, 0x73, |c| c.sp()),
        ];
        for (load, store, read) in cases {
            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_wz(0x2345);
            let mut bus = RamBus::at(PC, &[0xED, load, 0x00, 0x40])
                .with_byte(0x4000, 0x78)
                .with_byte(0x4001, 0x56);
            assert_eq!(cpu.step(&mut bus), 20, "ED {load:02X} T-states");
            assert_eq!(read(&cpu), 0x5678);
            assert_eq!(cpu.pc(), PC + 4);
            assert_eq!(cpu.wz(), 0x4001, "ED {load:02X} leaves wz at nn + 1");

            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_bc(0x1234);
            cpu.set_de(0x2345);
            cpu.set_hl(0x3456);
            cpu.set_sp(0x4567);
            let mut bus = RamBus::at(PC, &[0xED, store, 0x00, 0x40]);
            assert_eq!(cpu.step(&mut bus), 20, "ED {store:02X} T-states");
            let want = read(&cpu);
            assert_eq!(bus.peek(0x4000), want as u8);
            assert_eq!(bus.peek(0x4001), (want >> 8) as u8);
            assert_eq!(cpu.wz(), 0x4001);
        }
    }

    #[test]
    fn ld_sp_hl_is_six_states_and_does_not_touch_wz() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_hl(0xABCD);
        cpu.set_wz(0x2345);
        let mut bus = RamBus::at(PC, &[0xF9]);
        assert_eq!(cpu.step(&mut bus), 6);
        assert_eq!(cpu.sp(), 0xABCD);
        assert_eq!(cpu.wz(), 0x2345, "the suite says wz is NOT loaded with HL");

        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_ix(0x9876);
        cpu.set_wz(0x2345);
        let mut bus = RamBus::at(PC, &[0xDD, 0xF9]);
        assert_eq!(cpu.step(&mut bus), 10);
        assert_eq!(cpu.sp(), 0x9876);
        assert_eq!(cpu.wz(), 0x2345);
    }

    #[test]
    fn push_pop_roundtrip() {
        for (push, pop) in [(0xC5u8, 0xC1u8), (0xD5, 0xD1), (0xE5, 0xE1), (0xF5, 0xF1)] {
            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_sp(0x8000);
            cpu.set_bc(0x1234);
            cpu.set_de(0x2345);
            cpu.set_hl(0x3456);
            cpu.set_af(0x4556);
            let mut bus = RamBus::at(PC, &[push, pop]);
            assert_eq!(cpu.step(&mut bus), 11, "PUSH {push:02X} T-states");
            assert_eq!(cpu.step(&mut bus), 10, "POP {pop:02X} T-states");
            assert_eq!((cpu.bc(), cpu.de(), cpu.hl()), (0x1234, 0x2345, 0x3456));
            assert_eq!((cpu.a(), cpu.f()), (0x45, 0x56));
            assert_eq!(cpu.sp(), 0x8000, "SP comes back where it started");
        }
    }

    #[test]
    fn push_writes_high_first() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_sp(0x8002);
        cpu.set_bc(0x1234);
        let mut bus = RamBus::at(PC, &[0xC5]);
        cpu.step(&mut bus);
        assert_eq!(
            bus.writes,
            vec![(0x8001, 0x12), (0x8000, 0x34)],
            "high byte to SP-1 first, low byte to SP-2 second"
        );
        assert_eq!(cpu.sp(), 0x8000);
    }

    #[test]
    fn push_pop_af_move_both_halves() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_sp(0x8000);
        cpu.set_af(0x1234);
        let mut bus = RamBus::at(PC, &[0xF5, 0xC1]);
        assert_eq!(cpu.step(&mut bus), 11);
        assert_eq!(bus.peek(0x7FFF), 0x12, "A is the high byte");
        assert_eq!(bus.peek(0x7FFE), 0x34, "F is the low byte");

        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_sp(0x8000);
        cpu.set_wz(0x2345);
        let mut bus = RamBus::at(PC, &[0xF1])
            .with_byte(0x8000, 0xA3)
            .with_byte(0x8001, 0x9C);
        assert_eq!(cpu.step(&mut bus), 10);
        assert_eq!(cpu.af(), 0x9CA3);
        assert_eq!(cpu.wz(), 0x2345, "POP AF leaves wz alone");
    }

    #[test]
    fn pop_af_loads_f_but_leaves_q_clear() {
        // The one form here that writes the flag register. The suite still has
        // `final.q == 0` in all 1000 cases, so `q` tracks computed flags only.
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_sp(0x8000);
        let mut bus = RamBus::at(PC, &[0xF1])
            .with_byte(0x8000, 0xFF)
            .with_byte(0x8001, 0x11);
        cpu.step(&mut bus);
        assert_eq!(cpu.f(), 0xFF);
        assert_eq!(cpu.q(), 0);
        assert!(!cpu.p());
        assert!(!cpu.ei_pending());
    }

    #[test]
    fn push_pop_ix_carry_the_prefix_cost() {
        for prefix in [0xDDu8, 0xFD] {
            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_sp(0x8000);
            cpu.set_ix(0x1234);
            cpu.set_iy(0x2345);
            let mut bus = RamBus::at(PC, &[prefix, 0xE5, prefix, 0xE1]);
            assert_eq!(cpu.step(&mut bus), 15, "PUSH {prefix:02X} E5 T-states");
            assert_eq!(cpu.sp(), 0x7FFE);
            assert_eq!(cpu.step(&mut bus), 14, "POP {prefix:02X} E1 T-states");
            assert_eq!(cpu.sp(), 0x8000);
            let back = if prefix == 0xDD { cpu.ix() } else { cpu.iy() };
            assert_eq!(back, if prefix == 0xDD { 0x1234 } else { 0x2345 });
        }
    }

    #[test]
    fn ex_af_swaps_shadow() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_af(0x1234);
        cpu.set_af_shadow(0x5678);
        cpu.set_bc(0x9ABC);
        let mut bus = RamBus::at(PC, &[0x08]);
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(cpu.af(), 0x5678);
        assert_eq!(cpu.af_shadow(), 0x1234);
        assert_eq!(cpu.bc(), 0x9ABC, "EX AF,AF' touches nothing else");
        assert_eq!(cpu.pc(), PC + 1);
    }

    #[test]
    fn exx_swaps_bc_de_hl_only() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_bc(0x1111);
        cpu.set_de(0x2222);
        cpu.set_hl(0x3333);
        cpu.set_bc_shadow(0xAAAA);
        cpu.set_de_shadow(0xBBBB);
        cpu.set_hl_shadow(0xCCCC);
        cpu.set_af(0x1234);
        cpu.set_af_shadow(0x5678);
        cpu.set_sp(0x8000);
        cpu.set_ix(0x1234);
        cpu.set_wz(0x2345);
        let mut bus = RamBus::at(PC, &[0xD9]);
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!((cpu.bc(), cpu.de(), cpu.hl()), (0xAAAA, 0xBBBB, 0xCCCC));
        assert_eq!(
            (cpu.bc_shadow(), cpu.de_shadow(), cpu.hl_shadow()),
            (0x1111, 0x2222, 0x3333)
        );
        assert_eq!((cpu.a(), cpu.f()), (0x12, 0x34), "EXX leaves AF alone");
        assert_eq!(cpu.af_shadow(), 0x5678, "and AF' too");
        assert_eq!(cpu.sp(), 0x8000, "and SP");
        assert_eq!(cpu.ix(), 0x1234, "and IX");
        assert_eq!(cpu.wz(), 0x2345, "and wz");
    }

    #[test]
    fn ex_de_hl_ignores_dd_prefix() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_de(0x1234);
        cpu.set_hl(0x5678);
        let mut bus = RamBus::at(PC, &[0xEB]);
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!((cpu.de(), cpu.hl()), (0x5678, 0x1234));

        // Prefixed, the index mode cannot reach the swap, so the prefix decays
        // into a NOP and `EX DE,HL` still runs.
        for prefix in [0xDDu8, 0xFD] {
            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_de(0x1234);
            cpu.set_hl(0x5678);
            cpu.set_ix(0x9999);
            let mut bus = RamBus::at(PC, &[prefix, 0xEB]);
            assert_eq!(cpu.step(&mut bus), 8, "{prefix:02X} EB T-states");
            assert_eq!((cpu.de(), cpu.hl()), (0x5678, 0x1234));
            assert_eq!(cpu.pc(), PC + 2);
        }
    }

    #[test]
    fn ex_sp_hl_sets_wz() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_sp(0x8000);
        cpu.set_hl(0x1234);
        let mut bus = RamBus::at(PC, &[0xE3])
            .with_byte(0x8000, 0xCD)
            .with_byte(0x8001, 0xAB);
        assert_eq!(cpu.step(&mut bus), 19);
        assert_eq!(cpu.hl(), 0xABCD, "HL takes the word off the stack");
        assert_eq!(bus.peek(0x8000), 0x34, "old L goes to SP");
        assert_eq!(bus.peek(0x8001), 0x12, "old H goes to SP+1");
        assert_eq!(
            cpu.wz(),
            0xABCD,
            "wz takes the value that came off the stack"
        );
        assert_eq!(cpu.sp(), 0x8000, "SP itself does not move");
        assert_eq!(
            bus.writes,
            vec![(0x8001, 0x12), (0x8000, 0x34)],
            "high byte written first, like PUSH"
        );
    }

    #[test]
    fn ex_sp_ix_is_four_states_slower_and_wz_takes_ix() {
        for (prefix, want) in [(0xDDu16, 0x0102u16), (0xFD, 0x0304)] {
            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_sp(0x8000);
            cpu.set_ix(0x1234);
            cpu.set_iy(0x5678);
            cpu.set_hl(0x9ABC);
            let mut bus = RamBus::at(PC, &[prefix as u8, 0xE3])
                .with_byte(0x8000, want as u8)
                .with_byte(0x8001, (want >> 8) as u8);
            assert_eq!(cpu.step(&mut bus), 23);
            assert_eq!(cpu.wz(), want);
            assert_eq!(cpu.hl(), 0x9ABC, "HL is not involved");
            let got = if prefix == 0xDD { cpu.ix() } else { cpu.iy() };
            assert_eq!(got, want);
            assert_eq!(cpu.sp(), 0x8000);
        }
    }

    #[test]
    fn dd_index_nn_and_the_nn_pair_forms() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_wz(0x2345);
        let mut bus = RamBus::at(PC, &[0xDD, 0x21, 0x34, 0x12]);
        assert_eq!(cpu.step(&mut bus), 14);
        assert_eq!(cpu.ix(), 0x1234);
        assert_eq!(cpu.pc(), PC + 4);
        assert_eq!(cpu.wz(), 0x2345, "LD IX,nn leaves wz alone");

        // DD 22 / DD 2A move the index pair through memory, not HL: that is what
        // the suite's traces show, and HL stays where it was.
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_ix(0x1234);
        cpu.set_hl(0x9ABC);
        let mut bus = RamBus::at(PC, &[0xDD, 0x22, 0x00, 0x40]);
        assert_eq!(cpu.step(&mut bus), 20);
        assert_eq!(bus.peek(0x4000), 0x34);
        assert_eq!(bus.peek(0x4001), 0x12);
        assert_eq!(cpu.hl(), 0x9ABC, "HL is untouched by DD 22");
        assert_eq!(cpu.wz(), 0x4001);
        assert_eq!(cpu.pc(), PC + 4);

        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_hl(0x9ABC);
        let mut bus = RamBus::at(PC, &[0xDD, 0x2A, 0x00, 0x40])
            .with_byte(0x4000, 0x78)
            .with_byte(0x4001, 0x56);
        assert_eq!(cpu.step(&mut bus), 20);
        assert_eq!(cpu.ix(), 0x5678);
        assert_eq!(cpu.hl(), 0x9ABC, "HL is untouched by DD 2A");
        assert_eq!(cpu.wz(), 0x4001);
    }

    #[test]
    fn dd_d9_runs_exx_after_a_wasted_prefix() {
        let mut cpu = Z80::new();
        cpu.set_pc(PC);
        cpu.set_hl(0x1111);
        cpu.set_hl_shadow(0x2222);
        let mut bus = RamBus::at(PC, &[0xDD, 0xD9]);
        assert_eq!(cpu.step(&mut bus), 8);
        assert_eq!(cpu.hl(), 0x2222);
        assert_eq!(cpu.pc(), PC + 2);
    }

    #[test]
    fn nothing_here_touches_the_flags() {
        // `F` is a fixed pattern. Every file in this group shows 1000/1000 cases
        // with the flags unchanged. Two forms are deliberately not here: `F1`
        // (`POP AF`) loads `F` from memory, and `08` (`EX AF,AF'`) moves `F` into
        // the shadow pair - both are pinned by their own tests above.
        let ops = [
            0x01u8, 0x11, 0x21, 0x31, 0x22, 0x2A, 0xD9, 0xEB, 0xE3, 0xF9, 0xC1, 0xD1, 0xE1, 0xC5,
            0xD5, 0xE5, 0xF5,
        ];
        for op in ops {
            let mut cpu = Z80::new();
            cpu.set_pc(PC);
            cpu.set_af(0x003B);
            cpu.set_sp(0x8000);
            let mut bus = RamBus::at(PC, &[op, 0x00, 0x40, 0x00]);
            cpu.step(&mut bus);
            assert_eq!(cpu.f(), 0x3B, "{op:02X} left the flags alone");
            assert_eq!(cpu.q(), 0, "{op:02X} cleared q");
            assert!(!cpu.p(), "{op:02X} cleared p");
            assert!(!cpu.ei_pending(), "{op:02X} cleared ei_pending");
        }
    }
}
