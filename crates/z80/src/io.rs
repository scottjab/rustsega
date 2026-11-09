//! `IN`/`OUT` and the block I/O group (M2-12, #55).
//!
//! Two families share almost nothing. `IN A,(n)` and `OUT (n),A` are plain
//! accumulator shuffles with a whole 16-bit port address in the opcode, while
//! `IN r,(C)` and `OUT (C),r` address the port with `BC` and only survive in the
//! `ED` space. Both leave the flags alone except for the `IN` forms, which write
//! S/Z/P/V/X/Y of the value read and clear `H` and `N`.
//!
//! The block group is the one with the undocumented behaviour, and it is the same
//! story as [`crate::block`]: the flags do not describe a result, they describe
//! a 9-bit sum `k = value + delta` that is never stored anywhere, where `delta`
//! is `C + 1`/`C - 1` for the `IN` forms and the low byte of `HL` *after* it
//! stepped for the `OUT` forms. `B` decrements before `OUT` chooses its port and
//! after `IN` reads one, which is the difference between `wz = BC + 1` reading
//! the old `B` and reading the new one.
//!
//! A repeating pass runs once and then rewinds, exactly as the block transfers
//! do, and takes `X`/`Y` from its own address rather than from `B`. On top of
//! that it adjusts `H` and `P/V` a second time from the low bits of the new `B`
//! ([`Z80::io_repeat_adjust`]), which no other instruction does; every rule here
//! is checked against all 1000 hardware cases of `ed b2`/`ba`/`b3`/`bb`.

use crate::decode::y;
use crate::{
    FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_X, FLAG_Y, SZXY_PARITY, Z80, Z80Bus, parity,
    szxy_flags,
};

/// `IN A,(n)` and `OUT (n),A`: 11 T-states, being 4 for the opcode fetch, 3 for
/// the operand and 4 for the port cycle.
const T_PORT_N: u32 = 11;
/// `IN r,(C)` and `OUT (C),r`: 12 T-states, the `ED` fetch included.
const T_PORT_C: u32 = 12;
/// One pass of a block I/O instruction, `ED` fetch included.
const T_IO_BLOCK: u32 = 16;
/// A block I/O pass that repeats: the 5 T-states of the rewind on top.
const T_IO_BLOCK_REPEAT: u32 = 21;

/// `ED x3`: the direction bit, set for `OUT` and clear for `IN`.
const BIT_OUT: u8 = 0x01;
/// `ED xA`: the step bit, set for the `D` forms and clear for the `I` forms.
/// The same opcode field as `block::BIT_DOWN`, and the direction both `HL` and
/// the port step by.
const BIT_DOWN: u8 = 0x08;
/// `ED B_`: loop back while `B` survives the pass. Same field as the transfer
/// group, so the same constant applies.
const BIT_REPEAT: u8 = 0x10;

/// The `Y`/`X` bits, which a repeating pass takes from the instruction address.
const FLAG_YX: u8 = FLAG_Y | FLAG_X;

impl Z80 {
    /// `IN A,(n)`, opcode `DB`.
    ///
    /// The port is `A` in the high byte with the operand in the low one, so the
    /// accumulator addresses 256 ports at a time. No flags are outputs, and `wz`
    /// ends up one past the port address (all 1000 cases of `db`).
    pub(crate) fn in_a_n(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let port_low = self.fetch_byte(bus);
        let port = (u16::from(self.a()) << 8) | u16::from(port_low);
        let value = bus.io_read(port);
        self.set_wz(port.wrapping_add(1));
        self.set_a(value);
        self.clear_latches();
        T_PORT_N
    }

    /// `OUT (n),A`, opcode `D3`.
    ///
    /// Same port rule as [`Z80::in_a_n`], but `wz` only steps its low byte: the
    /// high byte stays `A`, which all 1000 cases of `d3` show and is what the
    /// address bus is still holding one cycle later.
    pub(crate) fn out_n_a(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let port_low = self.fetch_byte(bus);
        let port = (u16::from(self.a()) << 8) | u16::from(port_low);
        bus.io_write(port, self.a());
        self.set_wz((port & 0xFF00) | u16::from(port_low.wrapping_add(1)));
        self.clear_latches();
        T_PORT_N
    }

    /// `IN r,(C)`, `ED 40`/`48`/`50`/`58`/`60`/`68`/`70`/`78`.
    ///
    /// The port is `BC` whole. `S`/`Z`/`P/V` and the `X`/`Y` copies come from the
    /// value read, `H` and `N` clear, and `C` survives untouched. Slot 6 is the
    /// undocumented `IN (C)`: the read still happens and the flags still follow
    /// it, but nothing is written back to a register (`ed 70`, 1000 cases).
    pub(crate) fn in_r_c(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let port = self.bc();
        let value = bus.io_read(port);
        self.set_wz(port.wrapping_add(1));
        self.update_flags(!FLAG_C, SZXY_PARITY[usize::from(value)]);
        let slot = y(op);
        if slot != 6 {
            self.set_reg_slot(slot, value);
        }
        self.latch_flags();
        T_PORT_C
    }

    /// `OUT (C),r`, `ED 41`/`49`/`51`/`59`/`61`/`69`/`71`/`79`.
    ///
    /// No flags, and `wz = BC + 1` like the `IN` forms. Slot 6 is `ED 71`, which
    /// on the NMOS part writes a zero rather than `(HL)` or a floating `0xFF`.
    pub(crate) fn out_c_r(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let slot = y(op);
        let port = self.bc();
        let value = if slot == 6 { 0 } else { self.reg_slot(slot) };
        bus.io_write(port, value);
        self.set_wz(port.wrapping_add(1));
        self.clear_latches();
        T_PORT_C
    }

    /// The eight block I/O instructions: `ED A2` `INI`, `AA` `IND`, `A3` `OUTI`,
    /// `AB` `OUTD`, and the `B_` repeating form of each.
    ///
    /// One pass per `step`, and a repeating form rewinds onto its own
    /// instruction while `B` survives, exactly as the block transfers do
    /// (M2-11, #54). The order of the side effects is what separates the four
    /// variants:
    ///
    /// - `IN`: the port is `BC` as it stands, the value is read, `B` steps, and
    ///   the value lands at `HL` before `HL` itself steps.
    /// - `OUT`: the value comes from `HL`, then `HL` steps, then `B` steps, and
    ///   only then is the port assembled - so it uses the new `B`.
    pub(crate) fn block_io(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        // `PC` is already past `ED op`, so the instruction starts two bytes back.
        let address = self.pc().wrapping_sub(2);
        let down = op & BIT_DOWN != 0;
        let left = self.b().wrapping_sub(1);
        let hl = self.hl();
        let counted = if down {
            hl.wrapping_sub(1)
        } else {
            hl.wrapping_add(1)
        };

        let (value, delta) = if op & BIT_OUT != 0 {
            // `OUTI`/`OUTD`: the byte leaves memory, `HL` steps, `B` steps, and
            // only then is the port put together - from the new `B`. The delta of
            // the flag sum is `HL`'s low byte as it now stands.
            let value = bus.read(hl);
            self.set_hl(counted);
            self.set_b(left);
            let port = self.bc();
            bus.io_write(port, value);
            self.set_wz(if down {
                port.wrapping_sub(1)
            } else {
                port.wrapping_add(1)
            });
            (value, counted as u8)
        } else {
            // `INI`/`IND`: the port is `BC` with `B` still to step, and `C + 1`
            // (or `C - 1`) is the delta.
            let port = self.bc();
            self.set_wz(if down {
                port.wrapping_sub(1)
            } else {
                port.wrapping_add(1)
            });
            let value = bus.io_read(port);
            self.set_b(left);
            bus.write(hl, value);
            self.set_hl(counted);
            let delta = if down {
                self.c().wrapping_sub(1)
            } else {
                self.c().wrapping_add(1)
            };
            (value, delta)
        };

        // Every flag of a block I/O pass, none of them from a result: S/Z/X/Y of
        // the new `B`, N of the value's bit 7, and H/C/P-V from the 9-bit `k`.
        let k = u16::from(value) + u16::from(delta);
        let mut flags = szxy_flags(left);
        if parity(((k & 0x07) as u8) ^ left) {
            flags |= FLAG_PV;
        }
        if value & FLAG_S != 0 {
            flags |= FLAG_N;
        }
        if k > 0xFF {
            flags |= FLAG_C | FLAG_H;
        }

        if op & BIT_REPEAT == 0 || left == 0 {
            self.set_flags(flags);
            self.latch_flags();
            return T_IO_BLOCK;
        }
        self.io_repeat_adjust(flags, address, value, left)
    }

    /// Finish a repeating block I/O pass that is going round again: `PC` back
    /// onto the instruction with `wz` one past it, `X`/`Y` from that address
    /// rather than from `B`, and then the two adjustments no other instruction
    /// makes.
    ///
    /// With a carry out of `k`, `H` stops being the carry and becomes "the low
    /// nibble of the new `B` is `0`" when the value's bit 7 was set and `0xF`
    /// when it was clear. `P/V` flips once unless the three low bits the rule
    /// looks at - `B`, or `B - 1`/`B + 1` on a carry - already had even parity.
    /// Together that is the `post_IN_O_R()` quirk, and these four rules hold
    /// across all 3990 repeating cases of `ed b2`/`ba`/`b3`/`bb`.
    fn io_repeat_adjust(&mut self, mut flags: u8, address: u16, value: u8, left: u8) -> u32 {
        self.set_pc(address);
        self.set_wz(address.wrapping_add(1));
        flags = (flags & !FLAG_YX) | ((address >> 8) as u8 & FLAG_YX);
        if flags & FLAG_C == 0 {
            if !parity(left & 0x07) {
                flags ^= FLAG_PV;
            }
        } else if value & FLAG_S != 0 {
            if !parity(left.wrapping_sub(1) & 0x07) {
                flags ^= FLAG_PV;
            }
            flags = (flags & !FLAG_H) | (u8::from(left & 0x0F == 0x00) * FLAG_H);
        } else {
            if !parity(left.wrapping_add(1) & 0x07) {
                flags ^= FLAG_PV;
            }
            flags = (flags & !FLAG_H) | (u8::from(left & 0x0F == 0x0F) * FLAG_H);
        }
        self.set_flags(flags);
        self.latch_flags();
        T_IO_BLOCK_REPEAT
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::RamBus;
    use crate::{FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_X, FLAG_Y, FLAG_Z, Z80};

    /// Sentinel `wz`, to tell "the instruction wrote `wz`" from "nothing
    /// happened to it".
    const WZ0: u16 = 0x2345;

    /// A CPU at `pc` with `program` under it, `wz` preloaded and no flags.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        cpu_at_bus(pc, RamBus::at(pc, program))
    }

    /// As [`cpu_at`], for a bus that needs port values or memory set up first.
    fn cpu_at_bus(pc: u16, bus: RamBus) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        cpu.set_wz(WZ0);
        (cpu, bus)
    }

    #[test]
    fn in_a_n_port_uses_a_high() {
        let bus = RamBus::at(0x0100, &[0xDB, 0x34]).with_port(0x5A);
        let (mut cpu, mut bus) = cpu_at_bus(0x0100, bus);
        cpu.set_a(0xFE);
        cpu.set_flags(FLAG_S | FLAG_Z | FLAG_C);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(
            bus.port_reads,
            [(0xFE34, 0x5A)],
            "the port is A in the high byte, n in the low"
        );
        assert_eq!(cpu.a(), 0x5A);
        assert_eq!(cpu.f(), FLAG_S | FLAG_Z | FLAG_C, "no flags are outputs");
        assert_eq!(cpu.wz(), 0xFE35, "wz = port + 1");
        assert_eq!(cpu.pc(), 0x0102);
        assert_eq!(cpu.q(), 0, "a load does not latch q");
    }

    #[test]
    fn out_n_a_writes_a_under_a_high() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xD3, 0xFF]);
        cpu.set_a(0x66);
        cpu.set_flags(FLAG_S | FLAG_PV);

        assert_eq!(cpu.step(&mut bus), 11);

        assert_eq!(bus.port_writes, [(0x66FF, 0x66)]);
        assert_eq!(cpu.wz(), 0x6600, "only the low byte of wz steps");
        assert_eq!(cpu.f(), FLAG_S | FLAG_PV);
        assert!(bus.writes.is_empty(), "OUT never writes memory");
    }

    #[test]
    fn in_c_sets_flags() {
        // `IN B,(C)` reading 0x81: S set, Z clear, even parity, X/Y clear.
        let bus = RamBus::at(0x0100, &[0xED, 0x40]).with_port(0x81);
        let (mut cpu, mut bus) = cpu_at_bus(0x0100, bus);
        cpu.set_b(0x12);
        cpu.set_c(0x34);
        cpu.set_flags(FLAG_C | FLAG_X | FLAG_Y | FLAG_H | FLAG_N);

        assert_eq!(cpu.step(&mut bus), 12);

        assert_eq!(bus.port_reads, [(0x1234, 0x81)], "the port is BC");
        assert_eq!(cpu.b(), 0x81);
        assert!(cpu.get_flag(FLAG_S));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_PV), "0x81 holds an even number of bits");
        assert!(
            !cpu.get_flag(FLAG_X) && !cpu.get_flag(FLAG_Y),
            "X/Y follow the value, not the previous instruction"
        );
        assert!(
            !cpu.get_flag(FLAG_H) && !cpu.get_flag(FLAG_N),
            "IN always clears H and N"
        );
        assert!(cpu.get_flag(FLAG_C), "C is not an output of IN");
        assert_eq!(cpu.wz(), 0x1235, "wz = BC + 1");
        assert_eq!(cpu.q(), cpu.f(), "IN does set flags, so q latches them");
    }

    #[test]
    fn in_c_discard_keeps_the_register_and_flags() {
        // `IN (C)`, `ED 70`: the read happens, the value does not land anywhere.
        let bus = RamBus::at(0x0100, &[0xED, 0x70]).with_port(0x0F);
        let (mut cpu, mut bus) = cpu_at_bus(0x0100, bus);
        cpu.set_b(0x12);
        cpu.set_c(0x34);

        assert_eq!(cpu.step(&mut bus), 12);

        assert_eq!(bus.port_reads, [(0x1234, 0x0F)]);
        assert_eq!(cpu.b(), 0x12, "nothing is written back");
        assert_eq!(cpu.hl(), 0, "slot 6 is not (HL) here either");
        assert_eq!(
            cpu.f(),
            FLAG_X | FLAG_PV,
            "flags still come from the discarded value"
        );
    }

    #[test]
    fn out_c_0_writes_zero() {
        // `ED 71`: on the NMOS part the bus is left at 0, so a zero is written.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0x71]);
        cpu.set_b(0x40);
        cpu.set_c(0xFF);
        cpu.set_flags(FLAG_S | FLAG_C);

        assert_eq!(cpu.step(&mut bus), 12);

        assert_eq!(bus.port_writes, [(0x40FF, 0x00)]);
        assert_eq!(cpu.wz(), 0x4100);
        assert_eq!(cpu.f(), FLAG_S | FLAG_C, "OUT leaves the flags alone");
        assert_eq!(cpu.q(), 0);
        assert!(bus.port_reads.is_empty());
    }

    #[test]
    fn out_c_r_writes_the_register() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0x49]);
        cpu.set_b(0xAB);
        cpu.set_c(0x34);

        assert_eq!(cpu.step(&mut bus), 12);

        assert_eq!(bus.port_writes, [(0xAB34, 0x34)], "OUT (C),C");
        assert_eq!(cpu.wz(), 0xAB35);
    }

    #[test]
    fn ini_flags_k_overflow() {
        // B = 2, C = 0xFE, port value 0x01: k = 0x01 + (0xFE + 1) = 0x100, so
        // both H and C come out set even though nothing was added to a register.
        let bus = RamBus::at(0x0100, &[0xED, 0xA2]).with_port(0x01);
        let (mut cpu, mut bus) = cpu_at_bus(0x0100, bus);
        cpu.set_b(0x02);
        cpu.set_c(0xFE);
        cpu.set_hl(0x2000);

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(bus.port_reads, [(0x02FE, 0x01)], "the port is BC as read");
        assert_eq!(bus.peek(0x2000), 0x01, "the value lands at (HL)");
        assert_eq!(cpu.b(), 0x01, "B steps after the read");
        assert_eq!(cpu.hl(), 0x2001);
        assert_eq!(cpu.wz(), 0x02FF, "wz = BC + 1 with B still 2");
        assert_eq!(
            cpu.f(),
            FLAG_H | FLAG_C,
            "k = 0x100 sets H and C; N is bit 7 of 0x01 and P/V = parity((k & 7) ^ B) = parity(1)"
        );
        assert_eq!(cpu.pc(), 0x0102, "INI never repeats");
    }

    #[test]
    fn ini_flags_negative_case() {
        // The other side of both rules: no overflow, and a value whose bit 7
        // sets N while B is zero so Z goes up.
        let bus = RamBus::at(0x0100, &[0xED, 0xA2]).with_port(0x80);
        let (mut cpu, mut bus) = cpu_at_bus(0x0100, bus);
        cpu.set_b(0x01);
        cpu.set_c(0xFF);
        cpu.set_hl(0x2000);

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(cpu.b(), 0x00);
        assert_eq!(
            cpu.f(),
            FLAG_Z | FLAG_N | FLAG_PV,
            "k = 0x80 + 0 = 0x80: no carry, N from bit 7, P/V = parity(0) which is even"
        );
        assert_eq!(cpu.wz(), 0x0200);
    }

    #[test]
    fn indr_steps_down_and_stops_at_zero() {
        // B = 1 so the iteration leaves B = 0 and the repeat does not happen:
        // 16 T-states and PC moves off the instruction.
        let bus = RamBus::at(0x2000, &[0xED, 0xBA]).with_port(0x77);
        let (mut cpu, mut bus) = cpu_at_bus(0x2000, bus);
        cpu.set_b(0x01);
        cpu.set_c(0x00);
        cpu.set_hl(0x3000);

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(bus.port_reads, [(0x0100, 0x77)]);
        assert_eq!(bus.peek(0x3000), 0x77);
        assert_eq!(cpu.hl(), 0x2FFF, "the count runs down");
        assert_eq!(cpu.b(), 0);
        assert_eq!(cpu.pc(), 0x2002, "B = 0 ends the loop");
        assert_eq!(cpu.wz(), 0x00FF, "wz = BC - 1");
    }

    #[test]
    fn otir_repeats() {
        // One pass that leaves B = 2, so PC goes back to the instruction for
        // the next 21 T-states and X/Y come off its own address.
        let bus = RamBus::at(0x2800, &[0xED, 0xB3]).with_byte(0x2000, 0x81);
        let (mut cpu, mut bus) = cpu_at_bus(0x2800, bus);
        cpu.set_b(0x03);
        cpu.set_c(0x40);
        cpu.set_hl(0x2000);

        assert_eq!(cpu.step(&mut bus), 21);

        assert_eq!(
            bus.port_writes,
            [(0x0240, 0x81)],
            "B steps before the write"
        );
        assert_eq!(cpu.b(), 0x02);
        assert_eq!(cpu.hl(), 0x2001);
        assert_eq!(cpu.pc(), 0x2800, "the repeat rewinds onto the instruction");
        assert_eq!(cpu.wz(), 0x2801, "and wz follows the rewound PC");
        assert_eq!(
            cpu.f(),
            FLAG_N | FLAG_X | FLAG_Y,
            "N from the value, X/Y from 0x28; k = 0x81 + 0x01 does not carry and \
             the repeat flips P/V back off"
        );
        assert_eq!(cpu.q(), cpu.f());
    }

    #[test]
    fn otir_repeat_undocumented_h_and_pv() {
        // The two adjustments no other instruction makes: with a carry out of
        // `k`, H stops being the carry and becomes "low nibble of B is zero",
        // and P/V flips unless the low three bits of B have even parity.
        let bus = RamBus::at(0x2000, &[0xED, 0xB3]).with_byte(0x2080, 0x81);
        let (mut cpu, mut bus) = cpu_at_bus(0x2000, bus);
        cpu.set_b(0x13);
        cpu.set_c(0x40);
        cpu.set_hl(0x2080);

        assert_eq!(cpu.step(&mut bus), 21);

        // k = 0x81 + 0x81 = 0x102: carry, so a plain OUTI would set H as well.
        assert_eq!(cpu.b(), 0x12);
        assert!(cpu.get_flag(FLAG_C), "k carried");
        assert!(
            !cpu.get_flag(FLAG_H),
            "H is B & 0x0F == 0 on a repeat, not the carry"
        );
        assert!(
            cpu.get_flag(FLAG_PV),
            "P/V flips because (B - 1) & 7 = 1 has odd parity"
        );
        assert_eq!(
            cpu.f() & (FLAG_X | FLAG_Y),
            FLAG_Y,
            "0x20 has Y and not X, so neither bit is the one from B = 0x12"
        );
        assert_eq!(bus.port_writes, [(0x1240, 0x81)]);
    }

    #[test]
    fn indr_flags_track_the_count_down() {
        // The same instruction as INI with the count running down: k uses C - 1,
        // so C = 0 wraps to 0xFF and overflows with a small value.
        let bus = RamBus::at(0x0100, &[0xED, 0xAA]).with_port(0x01);
        let (mut cpu, mut bus) = cpu_at_bus(0x0100, bus);
        cpu.set_b(0x04);
        cpu.set_c(0x00);
        cpu.set_hl(0x2000);

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(bus.peek(0x2000), 0x01);
        assert_eq!(cpu.b(), 0x03);
        assert_eq!(cpu.hl(), 0x1FFF);
        assert_eq!(cpu.wz(), 0x03FF);
        // k = 0x01 + 0xFF = 0x100: H and C set, N clear, P/V = parity(0 ^ 3).
        assert_eq!(cpu.f(), FLAG_H | FLAG_C | FLAG_PV);
    }
}
