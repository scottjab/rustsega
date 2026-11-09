//! Bit test, bit reset and bit set: `CB 40`-`FF` and the whole indexed
//! `DD CB d op` / `FD CB d op` space (M2-09, #52).
//!
//! Three instruction groups share the `CB` opcode space with the rotates, and
//! they differ in one structural way: `BIT` reads and writes nothing, while
//! `RES` and `SET` are read-modify-writes. That split is where all the
//! differences come from - `BIT` is the only one that touches the flags, and the
//! only one that is shorter when the operand is in memory.
//!
//! The indexed space is the same three groups applied to the byte at `IX + d`
//! / `IY + d`, with one undocumented twist: for `RES`, `SET` and the rotates,
//! `z` is also a destination, so the result lands in memory *and* in register
//! `z`. `BIT` has no destination at all.

#[cfg(test)]
mod tests {
    use crate::testutil::RamBus;
    use crate::{
        FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_X, FLAG_Y, FLAG_Z, Z80,
    };

    /// A CPU with `program` sitting at its `PC` and every flag clear, so a
    /// test's `set_flags` call is the whole starting flag state.
    fn cpu_at(pc: u16, program: &[u8]) -> (Z80, RamBus) {
        let mut cpu = Z80::new();
        cpu.set_pc(pc);
        cpu.set_flags(0);
        (cpu, RamBus::at(pc, program))
    }

    /// `BIT 7,r` (`CB 78`): `S` is the tested bit, and *only* for bit 7.
    #[test]
    fn bit7_sets_s() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x78]);
        cpu.set_b(0x80);

        assert_eq!(cpu.step(&mut bus), 8, "BIT b,r is 8 T-states");

        assert!(cpu.get_flag(FLAG_S), "bit 7 was set, and it is the sign");
        assert!(
            !cpu.get_flag(FLAG_Z),
            "Z says the tested bit was 1, not that the byte was 0"
        );
        assert!(!cpu.get_flag(FLAG_PV), "P/V follows Z, and Z is clear");
        assert!(cpu.get_flag(FLAG_H), "BIT always sets H");
        assert!(!cpu.get_flag(FLAG_N), "BIT always clears N");
        assert_eq!(cpu.b(), 0x80, "BIT never writes its operand back");
        assert_eq!(cpu.q(), cpu.f(), "BIT recomputed the flags, so q follows");

        // Bit 0 of a negative byte: S has to come down even though bit 7 is set,
        // because the tested bit - and therefore S - is bit 0.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x40]);
        cpu.set_b(0x81);
        cpu.set_flags(FLAG_S);

        assert_eq!(cpu.step(&mut bus), 8);

        assert!(!cpu.get_flag(FLAG_S), "only bit 7 can set S");
        assert!(!cpu.get_flag(FLAG_Z), "bit 0 of 0x81 was set");
    }

    /// `BIT b,r` on a clear bit: `Z` and `P/V` set together, and no write.
    #[test]
    fn bit_clear_bit_sets_z_and_pv() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x40]);
        cpu.set_b(0x00);
        cpu.set_flags(FLAG_Z | FLAG_PV | FLAG_C);

        assert_eq!(cpu.step(&mut bus), 8);

        assert!(cpu.get_flag(FLAG_Z), "bit 0 of 0x00 was clear");
        assert!(cpu.get_flag(FLAG_PV), "P/V always travels with Z here");
        assert!(cpu.get_flag(FLAG_C), "BIT never touches C");
        assert!(!cpu.get_flag(FLAG_S));
        assert_eq!(cpu.b(), 0x00);
        assert!(bus.writes.is_empty(), "BIT must not write memory");

        // The other half: a set bit takes both of them down again. `0xFF` keeps
        // S clear because the tested bit is 0, not because 0xFF is positive.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x78]);
        cpu.set_b(0xFF);

        assert_eq!(cpu.step(&mut bus), 8);

        assert!(cpu.get_flag(FLAG_S), "bit 7 of 0xFF was set");
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_PV));
    }

    /// `BIT b,(HL)` (`CB 46`): `X`/`Y` come from the high byte of `wz` - not
    /// from `HL` and not from the byte that was tested.
    #[test]
    fn bit_hl_xy_from_wz() {
        const PC: u16 = 0x0100;
        let (mut cpu, mut bus) = cpu_at(PC, &[0xCB, 0x46]);
        cpu.set_hl(0x9100);
        // 0x2A has bit 3 set and bit 5 clear; `HL`'s own high byte 0x91 has
        // neither, so an implementation reading `HL` fails both assertions.
        cpu.set_wz(0x2A5C);
        bus = bus.with_byte(0x9100, 0xFE);

        assert_eq!(
            cpu.step(&mut bus),
            12,
            "BIT b,(HL) reads but does not write, so it is 12 T-states"
        );

        assert!(cpu.get_flag(FLAG_Z), "bit 0 of 0xFE was clear");
        assert!(cpu.get_flag(FLAG_PV));
        assert!(cpu.get_flag(FLAG_X), "X is bit 3 of wz's high byte");
        assert!(!cpu.get_flag(FLAG_Y), "Y is bit 5 of wz's high byte");
        assert_eq!(bus.peek(0x9100), 0xFE, "BIT does not write back");
        assert_eq!(bus.writes, Vec::new());
        assert_eq!(cpu.wz(), 0x2A5C, "BIT b,(HL) leaves wz where it found it");
        assert_eq!(bus.reads, vec![PC, PC + 1, 0x9100]);
    }

    /// `RES`/`SET` on a register: the only thing that moves is the register.
    #[test]
    fn res_set_register_leaves_flags() {
        // `CB 9F` = RES 3,A, `CB C1` = SET 0,C.
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0x9F]);
        cpu.set_a(0x8B);
        let before = FLAG_C | FLAG_Z | FLAG_S | FLAG_PV | FLAG_H | FLAG_N | FLAG_X | FLAG_Y;
        cpu.set_flags(before);

        assert_eq!(cpu.step(&mut bus), 8, "RES b,r is 8 T-states");

        assert_eq!(cpu.a(), 0x83, "bit 3 of 0x8B cleared, the rest left alone");
        assert_eq!(cpu.f(), before, "RES leaves every flag bit alone");
        assert_eq!(cpu.q(), 0, "and does not feed the latch either");

        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xCB, 0xC1]);
        cpu.set_c(0x00);
        cpu.set_wz(0x1234);

        assert_eq!(cpu.step(&mut bus), 8);

        assert_eq!(cpu.c(), 0x01, "bit 0 set");
        assert_eq!(cpu.f(), 0);
        assert_eq!(cpu.q(), 0);
        assert_eq!(cpu.wz(), 0x1234, "RES/SET must not disturb wz");
    }

    /// `RES b,(HL)` (`CB A6`): a read-modify-write at `HL`, 15 T-states.
    #[test]
    fn res_hl_writes_memory() {
        const PC: u16 = 0x0200;
        let (mut cpu, mut bus) = cpu_at(PC, &[0xCB, 0xA6]);
        cpu.set_hl(0x9100);
        cpu.set_flags(FLAG_C | FLAG_H | FLAG_N);
        bus = bus.with_byte(0x9100, 0x8A);

        assert_eq!(
            cpu.step(&mut bus),
            15,
            "RES b,(HL) is a read-modify-write, 3 T-states more than BIT"
        );

        assert_eq!(bus.peek(0x9100), 0x82, "bit 3 of 0x8A cleared");
        assert_eq!(bus.writes, vec![(0x9100, 0x82)]);
        assert_eq!(cpu.f(), FLAG_C | FLAG_H | FLAG_N, "flags untouched");
        assert_eq!(cpu.q(), 0);
        assert_eq!(cpu.h(), 0x91, "HL itself is not the destination");
        assert_eq!(bus.reads, vec![PC, PC + 1, 0x9100]);

        // `SET 3,(HL)` (`CB D6`) is the same instruction with the bit the other
        // way, including the flags.
        let (mut cpu, mut bus) = cpu_at(PC, &[0xCB, 0xD6]);
        cpu.set_hl(0x9100);
        bus = bus.with_byte(0x9100, 0x82);

        assert_eq!(cpu.step(&mut bus), 15);

        assert_eq!(bus.peek(0x9100), 0x8A);
        assert_eq!(cpu.f(), 0);
    }

    /// `DD CB 01 00`: `RLC (IX+1)`, and the rotated byte also turns up in `B`.
    ///
    /// Undocumented: `z` is a *destination* as well as the source slot, so the
    /// result is written back to memory and copied into register `z`.
    #[test]
    fn ddcb_rlc_copies_to_register() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xDD, 0xCB, 0x01, 0x00]);
        cpu.set_ix(0x9000);
        // Whatever is in B has to be replaced, not combined with.
        cpu.set_b(0xFF);
        bus = bus.with_byte(0x9001, 0x85);

        assert_eq!(cpu.step(&mut bus), 23, "DD CB shifts are 23 T-states");

        assert_eq!(bus.peek(0x9001), 0x0B, "the rotate is written back");
        assert_eq!(cpu.b(), 0x0B, "and copied to B because z = 0");
        assert_eq!(cpu.wz(), 0x9001, "wz is the effective address");
        assert!(cpu.get_flag(FLAG_C), "bit 7 of the byte shifted out into C");
        assert_eq!(cpu.q(), cpu.f());
        assert_eq!(cpu.r(), 2, "the whole instruction is two M1 fetches");
    }

    /// `FD CB FE 7E`: `BIT 7,(IY-2)`, 20 T-states and no write at all.
    #[test]
    fn ddcb_bit_is_20t() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xFD, 0xCB, 0xFE, 0x7E]);
        cpu.set_iy(0x8100);
        cpu.set_a(0x00);
        bus = bus.with_byte(0x80FE, 0x80);

        assert_eq!(
            cpu.step(&mut bus),
            20,
            "BIT is 3 T-states cheaper than the write-back forms"
        );

        assert!(cpu.get_flag(FLAG_S), "bit 7 of the byte at IY-2");
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_H));
        assert!(!cpu.get_flag(FLAG_N));
        // X/Y come from the high byte of the address, 0x80: neither bit.
        assert!(!cpu.get_flag(FLAG_X));
        assert!(!cpu.get_flag(FLAG_Y));
        assert_eq!(bus.peek(0x80FE), 0x80);
        assert_eq!(bus.writes, Vec::new(), "BIT (IX+d) must not write");
        assert_eq!(cpu.wz(), 0x80FE, "wz is IY plus the signed displacement");
        assert_eq!(cpu.a(), 0, "BIT never copies into z, not even z = 7");
        assert_eq!(cpu.q(), cpu.f());
        assert_eq!(cpu.r(), 2);
    }

    /// `DD CB 02 FE`: `SET 7,(IX+2)` with `z = 6`, the memory-only form.
    #[test]
    fn ddcb_set_z6_writes_memory_only() {
        let (mut cpu, mut bus) = cpu_at(0x0100, &[0xDD, 0xCB, 0x02, 0xFE]);
        cpu.set_ix(0x9000);
        cpu.set_a(0x0F);
        bus = bus.with_byte(0x9002, 0x0F);

        assert_eq!(cpu.step(&mut bus), 23);

        assert_eq!(bus.peek(0x9002), 0x8F);
        assert_eq!(cpu.a(), 0x0F, "z = 6 is (IX+d), and there is no copy");
        assert_eq!(cpu.f(), 0, "RES/SET never move the flags");
        assert_eq!(cpu.q(), 0);
    }
}
