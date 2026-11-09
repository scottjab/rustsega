//! Block transfer and search: `LDI`/`LDD`/`LDIR`/`LDDR` and
//! `CPI`/`CPD`/`CPIR`/`CPDR` (M2-11, #54).
//!
//! ```text
//! ED A0  LDI    (DE) <- (HL), HL +1, DE +1, BC -1     16
//! ED A8  LDD    (DE) <- (HL), HL -1, DE -1, BC -1     16
//! ED B0  LDIR   as LDI, repeat while BC != 0       16 / 21
//! ED B8  LDDR   as LDD, repeat while BC != 0       16 / 21
//! ED A1  CPI    A - (HL),  HL +1, BC -1               16
//! ED A9  CPD    A - (HL),  HL -1, BC -1               16
//! ED B1  CPIR   as CPI,   repeat while BC != 0 && !Z 16 / 21
//! ED B9  CPDR   as CPD,   repeat while BC != 0 && !Z 16 / 21
//! ```
//!
//! The opcode bits line up with the three choices, so two helpers cover the
//! group: bit 0 says search rather than transfer, bit 3 says count down rather
//! than up, bit 4 says the repeating form.
//!
//! One iteration per [`Z80::step`]: a repeat leaves `PC` on the instruction
//! itself (`PC -= 2`) rather than looping inside the core, so an interrupt can
//! land between two iterations exactly as it does on hardware. The suite
//! captures this directly - every one of the 1000 `ed b0` cases ends with `PC`
//! equal to its starting `PC` and 21 T-states, and the three `ed b1` cases that
//! find a match end 2 higher with 16.
//!
//! Two pieces of undocumented behaviour, both confirmed against all 1000 cases
//! of each of the eight `ed a0`-`ed b9` files (rule from the
//! "Z80 block instruction flags" write-up in the hoglet67/Z80Decoder wiki):
//!
//! * `X`/`Y` come from `n`, where `n` is `byte + A` for the transfers and
//!   `A - (HL) - H` for the searches; `X` is bit 3 of `n` and `Y` is bit 1 of
//!   `n` - not bit 5, which is why they are not a `szxy_flags` call.
//! * On an iteration that repeats, `n` is thrown away and `X`/`Y` come from
//!   bits 3 and 5 of the high byte of the instruction's own address instead,
//!   and `wz` becomes that address + 1.

use crate::{
    FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_X, FLAG_Y, FLAG_Z, SZXY_PARITY, Z80, Z80Bus,
};

/// One iteration of a block instruction, including the last iteration of a
/// repeating form.
const T_BLOCK: u32 = 16;
/// An iteration of a repeating form that repeats: `T_BLOCK` plus the two cycles
/// of the `PC -= 2` rewind.
const T_BLOCK_REPEAT: u32 = 21;

/// Opcode bit 0: the search half of the block group (`A1`/`A9`/`B1`/`B9`).
const BIT_SEARCH: u8 = 0x01;
/// Opcode bit 3: count down (`A8`/`B8`/`A9`/`B9`) rather than up.
const BIT_DOWN: u8 = 0x08;
/// Opcode bit 4: the repeating form (`B0`/`B8`/`B1`/`B9`).
const BIT_REPEAT: u8 = 0x10;

/// The flag bits a block transfer establishes: `H`, `N`, `P/V` and `X`/`Y`.
/// `S`, `Z` and `C` are left exactly as they were, which is what makes `LDI`
/// usable inside a search loop without disturbing a previous `CPI`.
const TRANSFER_FLAGS: u8 = FLAG_H | FLAG_N | FLAG_PV | FLAG_X | FLAG_Y;
/// The flag bits a block search establishes: everything but `C`, which it
/// leaves alone (Z80 CPU Family User's Manual UM0080, `CPI` description).
const SEARCH_FLAGS: u8 = !FLAG_C;

/// The undocumented `X`/`Y` of an iteration that does not repeat.
///
/// `X` is bit 3 of the source byte and `Y` is bit 1 of it - bit 1, not bit 5.
/// The flag bit `Y` lives in is bit 5, which is what makes this worth naming:
/// masking a plain `FLAG_X | FLAG_Y` off the source byte looks right and is
/// wrong whenever that byte's bit 1 and bit 5 disagree.
const fn block_xy(source: u8) -> u8 {
    (source & FLAG_X) | ((source & 0x02) << 4)
}

/// The `X`/`Y` of an iteration that repeats: bits 3 and 5 of the high byte of
/// the instruction's own address, which is also where `PC` ends up.
///
/// Verified against all 3997 repeating cases of `ed b0`, `ed b1`, `ed b8` and
/// `ed b9`; the three `ed b1` cases that stop take the `n` rule instead.
const fn block_xy_from_address(address: u16) -> u8 {
    ((address >> 8) as u8) & (FLAG_X | FLAG_Y)
}

impl Z80 {
    /// `ED A0`/`A8`/`B0`/`B8`: `LDI`, `LDD`, `LDIR`, `LDDR`.
    ///
    /// One pass moves `(HL)` to `(DE)` and steps `HL`, `DE` and `BC`; a repeat
    /// costs the rewind cycle and re-executes. `S`, `Z` and `C` are not outputs
    /// of a transfer at all, which is the whole reason a `LDIR` can sit inside a
    /// loop that is testing `C`.
    pub(crate) fn block_transfer(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let address = self.pc.wrapping_sub(2);
        let value = bus.read(self.hl());
        let dest = self.de();
        bus.write(dest, value);

        let left = self.block_counters(op);
        let repeat = left && op & BIT_REPEAT != 0;
        let mut affected = u8::from(left) * FLAG_PV;
        affected |= if repeat {
            block_xy_from_address(address)
        } else {
            block_xy(value.wrapping_add(self.a()))
        };
        self.update_flags(TRANSFER_FLAGS, affected);

        let tstates = self.finish_block(op, address, repeat);
        self.latch_flags();
        tstates
    }

    /// `ED A1`/`A9`/`B1`/`B9`: `CPI`, `CPD`, `CPIR`, `CPDR`.
    ///
    /// A compare against `(HL)` that never touches `A`: `S`, `Z` and `H` are the
    /// ones a `CP (HL)` would set, `N` is set, and `C` is left where it was.
    /// The search repeats while `BC != 0` and the bytes did not match, so a
    /// `CPIR` that finds its byte ends on `Z` - 16 T-states and a moved-on `PC`,
    /// not the 21 of the iterations that came before it.
    pub(crate) fn block_search(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        let address = self.pc.wrapping_sub(2);
        let value = bus.read(self.hl());
        let a = self.a();
        let difference = a.wrapping_sub(value);
        // The borrow out of bit 3, the same XOR trick as `CP` (see
        // `alu8::sub_flags`), kept because `n` below needs it as well as `H`.
        let half = (a ^ value ^ difference) & 0x10 != 0;

        let left = self.block_counters(op);
        let repeat = left && difference != 0 && op & BIT_REPEAT != 0;
        let mut affected = SZXY_PARITY[usize::from(difference)] & (FLAG_S | FLAG_Z) | FLAG_N;
        if half {
            affected |= FLAG_H;
        }
        affected |= u8::from(left) * FLAG_PV;
        affected |= if repeat {
            block_xy_from_address(address)
        } else {
            block_xy(difference.wrapping_sub(u8::from(half)))
        };
        self.update_flags(SEARCH_FLAGS, affected);

        let tstates = self.finish_block(op, address, repeat);
        self.latch_flags();
        tstates
    }

    /// Step `HL`, and `DE` for the transfers, in the direction bit 3 says, count
    /// `BC` down, and report whether any bytes are left.
    ///
    /// All three move on every iteration including the last: the transfer or
    /// compare happens before the repeat test, so a finished `LDIR` leaves `HL`,
    /// `DE` and `BC` one step along too (all 8000 cases of the eight files).
    fn block_counters(&mut self, op: u8) -> bool {
        let step = |value: u16| {
            if op & BIT_DOWN != 0 {
                value.wrapping_sub(1)
            } else {
                value.wrapping_add(1)
            }
        };
        self.set_hl(step(self.hl()));
        if op & BIT_SEARCH == 0 {
            self.set_de(step(self.de()));
        }
        self.set_bc(self.bc().wrapping_sub(1));
        self.bc() != 0
    }

    /// The `PC` and `wz` of an iteration that has just finished.
    ///
    /// Only the `B_` forms can ask for the rewind: `LDI` with `BC` still
    /// non-zero is still over, and all 1000 `ed a0` cases end 2 past the
    /// instruction like any other two-byte opcode.
    ///
    /// A repeat rewinds `PC` onto the instruction, so the next `step()` runs the
    /// rest of the block and an interrupt can be taken in between; `wz` becomes
    /// the instruction address + 1 on that iteration and only on that one
    /// (1000/1000 cases of `ed b0`, `ed b8` and `ed b9`, 997/1000 of `ed b1`).
    /// An iteration that does not repeat leaves `PC` where the fetches left it,
    /// and leaves `wz` alone for a transfer or steps it by one for a search,
    /// which is what `CPI`/`CPD` do and what the three stopping `CPIR` cases
    /// show. Nothing in the suite says what a *terminating* `LDIR` does to `wz`
    /// (`BC` is 16 bits, so none of its 1000 random starts ever got there), so
    /// it follows `LDI`, the one case we can see.
    fn finish_block(&mut self, op: u8, address: u16, repeat: bool) -> u32 {
        if repeat {
            self.set_pc(address);
            self.set_wz(address.wrapping_add(1));
            return T_BLOCK_REPEAT;
        }
        if op & BIT_SEARCH != 0 {
            let wz = self.wz();
            self.set_wz(if op & BIT_DOWN != 0 {
                wz.wrapping_sub(1)
            } else {
                wz.wrapping_add(1)
            });
        }
        T_BLOCK
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::RamBus;
    use crate::{FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_X, FLAG_Y, FLAG_Z, Z80};

    /// Sentinel `wz`: no address in these tests is ever `0x2345`, so a test can
    /// tell "this instruction loaded `wz`" from "this instruction left it be".
    const WZ0: u16 = 0x2345;

    /// A CPU at `pc` with `program` under it, `wz` preloaded and every flag
    /// clear so each test only sets the flags it means to prove something about.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        cpu.set_wz(WZ0);
        (cpu, RamBus::at(pc, program))
    }

    /// `LDI` (`ED A0`): the byte moves, all three counters move with it, and the
    /// three flags a transfer must not disturb stay put.
    #[test]
    fn ldi_moves_and_counts() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0xA0]);
        cpu.set_hl(0x2000);
        cpu.set_de(0x3000);
        cpu.set_bc(0x0004);
        bus.ram[0x2000] = 0xC3;
        // Every flag set, so "unchanged" cannot be mistaken for "cleared".
        cpu.set_flags(FLAG_S | FLAG_Z | FLAG_C);

        assert_eq!(cpu.step(&mut bus), 16, "one transfer pass is 16 T-states");

        assert_eq!(bus.peek(0x3000), 0xC3, "(DE) <- (HL)");
        assert_eq!(cpu.hl(), 0x2001, "HL steps up");
        assert_eq!(cpu.de(), 0x3001, "DE steps up");
        assert_eq!(cpu.bc(), 0x0003, "BC counts down");
        assert!(cpu.get_flag(FLAG_S), "S is not an output of LDI");
        assert!(cpu.get_flag(FLAG_Z), "Z is not an output of LDI");
        assert!(cpu.get_flag(FLAG_C), "C is not an output of LDI");
        assert!(!cpu.get_flag(FLAG_H), "H is always cleared");
        assert!(!cpu.get_flag(FLAG_N), "N is always cleared");
        assert!(cpu.get_flag(FLAG_PV), "P/V says BC != 0");
        assert_eq!(cpu.wz(), WZ0, "a transfer does not disturb wz");
        assert_eq!(cpu.q(), cpu.f(), "the flags were recomputed, so q follows");
    }

    /// `LDD` (`ED A8`): the same pass counting down, and down is the only thing
    /// that differs.
    #[test]
    fn ldd_moves_downwards() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0xA8]);
        cpu.set_hl(0x2000);
        cpu.set_de(0x3000);
        cpu.set_bc(0x0002);
        bus.ram[0x2000] = 0x5A;

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(bus.peek(0x3000), 0x5A, "the destination is the old DE");
        assert_eq!(cpu.hl(), 0x1FFF);
        assert_eq!(cpu.de(), 0x2FFF);
        assert_eq!(cpu.bc(), 0x0001);
        assert!(cpu.get_flag(FLAG_PV), "one byte still to go");
    }

    /// `LDI`'s undocumented `X`/`Y`: bit 3 of `byte + A` and bit 1 of `byte + A`.
    ///
    /// The carry out of the low nibble is the point: `0x80 + 0x08` has bit 3 set
    /// although the transferred byte has no bit 3, so this cannot be a copy of
    /// the transferred byte. `Y` from bit 1 of the sum, not bit 5, is the other
    /// half: `0x01 + 0x02` ends with `Y` set and `X` clear.
    #[test]
    fn ldi_xy_from_n() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0xA0]);
        cpu.set_hl(0x2000);
        cpu.set_de(0x3000);
        cpu.set_bc(0x0001);
        cpu.set_a(0x08);
        bus.ram[0x2000] = 0x80;

        assert_eq!(cpu.step(&mut bus), 16);

        assert!(cpu.get_flag(FLAG_X), "bit 3 of 0x80 + 0x08");
        assert!(!cpu.get_flag(FLAG_Y), "bit 1 of 0x88 is clear");

        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0xA0]);
        cpu.set_hl(0x2000);
        cpu.set_de(0x3000);
        cpu.set_bc(0x0001);
        cpu.set_a(0x02);
        bus.ram[0x2000] = 0x01;

        assert_eq!(cpu.step(&mut bus), 16);

        assert!(cpu.get_flag(FLAG_Y), "Y is bit 1 of 0x03, not bit 5");
        assert!(!cpu.get_flag(FLAG_X), "bit 3 of 0x03 is clear");
    }

    /// `LDIR` (`ED B0`) repeating: `PC` stays on the instruction, `wz` becomes
    /// that address + 1, and `X`/`Y` are taken from the instruction address too.
    ///
    /// The program sits at `0x2800` so the high byte of the instruction address
    /// has bits 3 and 5 set, while `A` and the transferred byte are both 0 - the
    /// `n` rule would leave both bits clear, so a test that sees them set can
    /// only have read them from `PC`.
    #[test]
    fn ldir_repeats_with_21t() {
        let (mut cpu, mut bus) = cpu_at(0x2800, &[0xED, 0xB0]);
        cpu.set_hl(0x2000);
        cpu.set_de(0x3000);
        cpu.set_bc(0x0003);
        bus.ram[0x2000] = 0x00;

        assert_eq!(cpu.step(&mut bus), 21, "a repeating iteration costs 21");

        assert_eq!(cpu.pc(), 0x2800, "PC rewinds by 2 onto the instruction");
        assert_eq!(cpu.wz(), 0x2801, "wz = instruction address + 1");
        assert!(
            cpu.get_flag(FLAG_X) && cpu.get_flag(FLAG_Y),
            "X/Y come from bits 3 and 5 of the new PC's high byte, 0x28"
        );
        assert!(cpu.get_flag(FLAG_PV), "BC is now 2, so the block goes on");
        assert_eq!(bus.peek(0x3000), 0x00);
        assert_eq!((cpu.hl(), cpu.de(), cpu.bc()), (0x2001, 0x3001, 0x0002));
    }

    /// `LDIR`'s last iteration: the same instruction, but `BC` reaches 0, so it
    /// costs 16 T-states, moves on, and goes back to the `n` rule for `X`/`Y`.
    #[test]
    fn ldir_last_iteration_16t() {
        let (mut cpu, mut bus) = cpu_at(0x2800, &[0xED, 0xB0]);
        cpu.set_a(0x00);
        cpu.set_hl(0x2000);
        cpu.set_de(0x3000);
        cpu.set_bc(0x0001);
        bus.ram[0x2000] = 0x80;

        assert_eq!(cpu.step(&mut bus), 16, "the final pass does not rewind");

        assert_eq!(cpu.pc(), 0x2802, "PC moves past the instruction");
        assert_eq!(cpu.wz(), WZ0, "a finished transfer leaves wz alone");
        assert!(!cpu.get_flag(FLAG_PV), "BC == 0 clears P/V");
        // n = 0x80 + 0 = 0x80: bit 3 clear, bit 1 clear, so the PC rule of the
        // test above must not apply here.
        assert!(!cpu.get_flag(FLAG_X));
        assert!(!cpu.get_flag(FLAG_Y));
        assert_eq!((cpu.hl(), cpu.de(), cpu.bc()), (0x2001, 0x3001, 0x0000));
    }

    /// `CPI` (`ED A1`): a compare that leaves `A` alone, sets `N`, and takes
    /// `S`/`Z`/`H` from the subtraction while `C` survives.
    #[test]
    fn cpi_compares_without_touching_a() {
        // A match: Z set, S clear, no half-borrow.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0xA1]);
        cpu.set_a(0x42);
        cpu.set_hl(0x2000);
        cpu.set_bc(0x0005);
        cpu.set_flags(FLAG_C | FLAG_S);
        bus.ram[0x2000] = 0x42;

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(cpu.a(), 0x42, "CPI is a compare, not a subtract");
        assert!(cpu.get_flag(FLAG_Z), "0x42 - 0x42 is zero");
        assert!(
            !cpu.get_flag(FLAG_S),
            "S follows the difference, not the old S"
        );
        assert!(!cpu.get_flag(FLAG_H), "no borrow out of bit 3");
        assert!(cpu.get_flag(FLAG_N), "CPI always sets N");
        assert!(cpu.get_flag(FLAG_C), "C is not an output of CPI");
        assert!(cpu.get_flag(FLAG_PV), "BC is 4, so the search may go on");
        assert_eq!((cpu.hl(), cpu.bc()), (0x2001, 0x0004));
        assert_eq!(cpu.wz(), WZ0 + 1, "CPI is CPIR with the search over");

        // No match one lower in the other direction: 0x42 - 0x43 = 0xFF, so S
        // sets and the borrow out of bit 3 sets H.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0xA1]);
        cpu.set_a(0x42);
        cpu.set_hl(0x2000);
        cpu.set_bc(0x0001);
        bus.ram[0x2000] = 0x43;

        assert_eq!(cpu.step(&mut bus), 16);

        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_S));
        assert!(cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_PV), "BC is 0 now");
    }

    /// `CPD` (`ED A9`): `HL` counts down and `wz` counts down with it.
    #[test]
    fn cpd_searches_downwards() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xED, 0xA9]);
        cpu.set_a(0x10);
        cpu.set_hl(0x2000);
        cpu.set_bc(0x0002);
        bus.ram[0x2000] = 0x10;

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(cpu.hl(), 0x1FFF, "HL steps down to the next byte");
        assert_eq!(cpu.bc(), 0x0001);
        assert!(cpu.get_flag(FLAG_Z), "found it");
        assert!(cpu.get_flag(FLAG_N));
        assert_eq!(cpu.wz(), WZ0 - 1, "wz follows HL down");
    }

    /// `CPIR` (`ED B1`) finding its byte: the search stops, so this iteration is
    /// a plain `CPI` - 16 T-states, `PC` moves on, no `PC`-based `X`/`Y`.
    #[test]
    fn cpir_stops_on_match() {
        // 0x2800 again: were X/Y taken from the instruction address this would
        // set both, and a match means the n rule leaves both clear (A - (HL) = 0).
        let (mut cpu, mut bus) = cpu_at(0x2800, &[0xED, 0xB1]);
        cpu.set_a(0x42);
        cpu.set_hl(0x2000);
        cpu.set_bc(0x0005);
        bus.ram[0x2000] = 0x42;

        assert_eq!(cpu.step(&mut bus), 16, "a match ends the search here");

        assert_eq!(cpu.pc(), 0x2802, "PC moves on, no rewind");
        assert!(cpu.get_flag(FLAG_Z), "the match is visible as Z");
        assert!(cpu.get_flag(FLAG_N));
        assert!(
            cpu.get_flag(FLAG_PV),
            "BC is 4, so a re-run would search on"
        );
        assert!(!cpu.get_flag(FLAG_X) && !cpu.get_flag(FLAG_Y), "n = 0 here");
        assert_eq!(cpu.wz(), WZ0 + 1, "a stopping CPIR is a CPI");
        assert_eq!((cpu.hl(), cpu.bc()), (0x2001, 0x0004));
    }

    /// `CPIR` not finding it: the rewind, and 21 T-states.
    #[test]
    fn cpir_repeats_while_it_has_bytes_left() {
        let (mut cpu, mut bus) = cpu_at(0x2800, &[0xED, 0xB1]);
        cpu.set_a(0x42);
        cpu.set_hl(0x2000);
        cpu.set_bc(0x0005);
        bus.ram[0x2000] = 0x43;

        assert_eq!(cpu.step(&mut bus), 21);

        assert_eq!(cpu.pc(), 0x2800);
        assert_eq!(cpu.wz(), 0x2801);
        assert!(!cpu.get_flag(FLAG_Z), "no match yet");
        assert!(
            cpu.get_flag(FLAG_X) && cpu.get_flag(FLAG_Y),
            "a repeating iteration reads X/Y from the instruction address"
        );
        assert_eq!((cpu.hl(), cpu.bc()), (0x2001, 0x0004));
    }

    /// `CPDR` (`ED B9`) running out of bytes: stops because `BC` hit 0 rather
    /// than because it matched, going down all the while.
    #[test]
    fn cpdr_stops_when_bc_hits_zero() {
        let (mut cpu, mut bus) = cpu_at(0x2800, &[0xED, 0xB9]);
        cpu.set_a(0x10);
        cpu.set_hl(0x2000);
        cpu.set_bc(0x0001);
        bus.ram[0x2000] = 0x11;

        assert_eq!(cpu.step(&mut bus), 16);

        assert_eq!(cpu.pc(), 0x2802);
        assert_eq!(cpu.hl(), 0x1FFF);
        assert_eq!(cpu.bc(), 0x0000);
        assert!(!cpu.get_flag(FLAG_Z), "0x10 - 0x11 is not zero");
        assert!(!cpu.get_flag(FLAG_PV), "the block is over");
        // n = 0x10 - 0x11 - H(1) = 0xFE: bit 3 set, bit 1 set, so this is the n
        // rule rather than the PC rule again.
        assert!(cpu.get_flag(FLAG_X) && cpu.get_flag(FLAG_Y));
        assert_eq!(cpu.wz(), WZ0 - 1, "a stopping CPDR is a CPD");
    }
}
