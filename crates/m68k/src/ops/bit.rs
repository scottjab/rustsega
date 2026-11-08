//! Single-bit test and modify instructions: `BTST`, `BCHG`, `BCLR` and `BSET`
//! (M1-20, #28) — M68000UM "BTST — Bit Test", "BCHG — Bit Change",
//! "BCLR — Bit Clear", "BSET — Bit Set".
//!
//! All four live in the `$0000` group and share one encoding in two shapes:
//!
//! | Bits | 15..12 | 11..9 | 8 | 7..6 | 5..0 |
//! |---|---|---|---|---|---|
//! | dynamic, `0000 ddd 1 tt eeeeee` | `0000` | the `Dn` holding the bit number | `1` | `tt` | the operand |
//! | static, `0000 1000 tt eeeeee` | `0000` | `100` | `0` | `tt` | the operand |
//!
//! `tt` is `00` `BTST`, `01` `BCHG`, `10` `BCLR`, `11` `BSET`. It is a mnemonic
//! selector and not a size, so unlike the immediate operations of the same group
//! none of these encodings has an `ss` = `11` hole.
//!
//! # Size, and the two bit-number modulos
//!
//! There is no size field: the operand's own kind decides it. A `Dn` operand is
//! a **long** and its bit number is taken **modulo 32**; a memory operand is a
//! **byte** and its bit number is taken **modulo 8** (M68000UM "BTST" Operation:
//! "the bit number … modulo 32 or … modulo 8"). The static form's bit number is
//! the low eight bits of the first extension word, which either modulo subsumes.
//!
//! # Flags
//!
//! `Z` and nothing else: `Z` is set when the tested bit — as it was *before* any
//! modification — was 0. `N`, `V`, `C` and `X` are untouched (M68000UM, the same
//! four "Flags" lines; measured over all 32 260 cases of the four suite files,
//! where the only `sr` bit that ever differs between `initial` and `final` is
//! bit 2).
//!
//! # Cycles
//!
//! `0`, like every other handler (see [`crate::ops`]). Two numbers are owed by
//! M1-31 (#39/#40): the widths of M68000PM section 6 for this class, and
//! `BCLR`'s two extra internal clocks when the bit number is 16 or more on a
//! `Dn` operand (M68000UM "BCLR", "Timing").

#[cfg(test)]
mod tests {
    use crate::bus::Bus;
    use crate::cpu::M68k;
    use crate::decode::{decode, Op};
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::{TestBus, cpu_with_program};

    /// The five condition code bits, for the test that has to leave four of
    /// them alone.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Memory operand for the byte-size tests: well clear of the program at
    /// `$1000` and of the stack at `$1_0000`.
    const MEM: u32 = 0x0020_00;

    // The four `tt` values.
    const BTST: u8 = 0b00;
    const BCHG: u8 = 0b01;
    const BCLR: u8 = 0b10;
    const BSET: u8 = 0b11;

    // The eight addressing modes a bit operation may meet.
    const DN: u8 = 0b000;
    const AN: u8 = 0b001;
    const A_INDIRECT: u8 = 0b010;
    const POST_INC: u8 = 0b011;
    const D16_AN: u8 = 0b101;
    const ABS_W: u8 = 0b111;

    /// `0000 ddd 1 tt mmm rrr`: the bit number is in `D{bit_reg}` and the operand
    /// is `mode`/`reg`.
    const fn dynamic(bit_reg: u8, tt: u8, mode: u8, reg: u8) -> u16 {
        0x0100 | ((bit_reg as u16) << 9) | ((tt as u16) << 6) | ((mode as u16) << 3) | reg as u16
    }

    /// `0000 1000 tt mmm rrr`: the bit number is the low eight bits of the
    /// instruction's first extension word.
    const fn static_(tt: u8, mode: u8, reg: u8) -> u16 {
        0x0800 | ((tt as u16) << 6) | ((mode as u16) << 3) | reg as u16
    }

    /// One instruction at `$1000` followed by `rest` (its extension words, and a
    /// terminating `NOP`), reset and ready for one `step`.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    // ---- decode ----

    /// Mode `001` inside the `$0000` group is `MOVEP`, not a bit operation:
    /// `MOVEP.L D0,(A0)` is `$0108`, whose EA field is exactly the field
    /// `BTST D0,(A0)` would use. `MOVEP` is M1-25 (#33), so the whole mode `001`
    /// column of this group stays unclaimed here (M68000PM/AD section 8,
    /// "MOVEP — Move to/from Peripheral Device"; the ticket's own example).
    #[test]
    fn ea_001_is_not_bitop() {
        for tt in [BTST, BCHG, BCLR, BSET] {
            for reg in 0..8u8 {
                for bit_reg in 0..8u8 {
                    // MOVEP is `0000 ddd 1 d1 d0 001 rrr`: bits 7..6 are its
                    // register-pair selector, not `tt`, so all four of those too.
                    let opcode = dynamic(bit_reg, tt, AN, reg);
                    assert_eq!(
                        decode(opcode),
                        Op::Unimplemented,
                        "${opcode:04X}: An direct in the bit-op group is MOVEP (M1-25)"
                    );
                }
            }
        }
        // The static form's mode `001` is not MOVEP — MOVEP has bit 8 set — but
        // it is unassigned on the 68000 too, so it stays unclaimed until MOVEP's
        // ticket has been through this group.
        assert_eq!(
            decode(static_(BTST, AN, 0)),
            Op::Unimplemented,
            "static, An direct"
        );
        // And the neighbours that *are* bit operations do decode, so the rule
        // above is a hole in the family rather than the family being missing.
        assert_ne!(decode(dynamic(0, BTST, DN, 0)), Op::Unimplemented, "BTST D0,D0");
        assert_ne!(decode(static_(BTST, DN, 0)), Op::Unimplemented, "BTST #,D0");
    }

    // ---- BTST ----

    /// `BTST D1,D2` on bit 31 — the top of the long, which only the
    /// `Dn`-operand-is-a-long rule reaches. A set bit 31 clears `Z`, a clear one
    /// sets it, and `BTST` writes nothing back (M68000UM "BTST", and #28).
    #[test]
    fn btst_dn_bit_31() {
        for (value, expect_z) in [
            (0x8000_0000u32, false),
            (0xFFFF_FFFF, false),
            (0x7FFF_FFFF, true),
            (0x0000_0000, true),
        ] {
            let (mut cpu, mut bus) = run(dynamic(1, BTST, DN, 2), &[0x4E71]);
            cpu.d[1] = 31;
            cpu.d[2] = value;

            cpu.step(&mut bus);

            assert_eq!(cpu.flag_z(), expect_z, "Z from bit 31 of {value:08X}");
            assert_eq!(cpu.d[2], value, "BTST writes nothing back");
        }
    }

    /// A `Dn` bit number is taken modulo 32: `BTST D1,D2` with `D1` = 33 tests
    /// bit 1, and 32 wraps all the way back to bit 0 (M68000UM "BTST":
    /// "modulo 32").
    #[test]
    fn btst_dn_wraps_the_bit_number_modulo_32() {
        for (number, bit) in [(0u32, 0), (31, 31), (32, 0), (33, 1), (63, 31), (255, 31)] {
            let (mut cpu, mut bus) = run(dynamic(1, BTST, DN, 2), &[0x4E71]);
            cpu.d[1] = number;
            cpu.d[2] = 1 << bit;

            cpu.step(&mut bus);

            assert!(!cpu.flag_z(), "{number} reaches bit {bit}, which is set");

            let (mut cpu, mut bus) = run(dynamic(1, BTST, DN, 2), &[0x4E71]);
            cpu.d[1] = number;
            cpu.d[2] = !(1 << bit);

            cpu.step(&mut bus);

            assert!(cpu.flag_z(), "{number} reaches bit {bit}, which is clear");
        }
    }

    /// A memory operand is a **byte**, so its bit number is taken modulo 8 and
    /// `BTST D1,(A0)` with `D1` = 12 tests bit 4 of the addressed byte — not bit
    /// 12 of a long, and not the byte above the address (M68000UM "BTST",
    /// "modulo 8"). Asserted byte by byte because `TestBus` is big endian.
    #[test]
    fn btst_memory_modulo_8() {
        let opcode = dynamic(1, BTST, A_INDIRECT, 0);
        for number in [4u32, 12, 20, 100] {
            let bit = (number % 8) as u8;

            let (mut cpu, mut bus) = run(opcode, &[0x4E71]);
            cpu.a[0] = MEM;
            bus.write_byte(MEM, 0xFF ^ (1 << bit));

            cpu.step(&mut bus);

            assert!(!cpu.flag_z(), "{number} reaches bit {bit}, which is clear");

            let (mut cpu, mut bus) = run(opcode, &[0x4E71]);
            cpu.a[0] = MEM;
            bus.write_byte(MEM, 1 << bit);

            cpu.step(&mut bus);

            assert!(cpu.flag_z(), "{number} reaches bit {bit}, which is set");
            assert_eq!(bus.read_byte(MEM), 1 << bit, "BTST wrote nothing back");
        }
    }

    /// `BTST` only reads, so the two read-only operands the modify forms are
    /// refused — an immediate and `d16(PC)` — are legal for it (M68000PM/AD
    /// 2.2.12 gives `BTST` the whole data column, and `BTST.json.gz` carries 138
    /// immediate cases and 166 `d16(PC)` ones).
    ///
    /// `BTST #3,d16(PC)` also pins the *order* of the extension words: the bit
    /// number is the first word after the opcode and the displacement the second,
    /// so the displacement is measured from `$1004` — the address of the
    /// displacement word itself — and not from `$1002`.
    #[test]
    fn btst_reads_its_source_only() {
        // `BTST D1,#$00FB`: bit 2 of the immediate is clear, so Z is set.
        let (mut cpu, mut bus) = run(dynamic(1, BTST, 0b111, 0b100), &[0x00FB, 0x4E71]);
        cpu.d[1] = 2;
        cpu.set_ccr(ALL_FLAGS & !SR_Z);

        cpu.step(&mut bus);

        assert!(cpu.flag_z(), "bit 2 of #$00FB is clear");

        // `BTST #3,d16(PC)` with displacement +4, i.e. from `$1004` to `$1008`:
        // one byte past the four words of this very program.
        let (mut cpu, mut bus) = run(static_(BTST, 0b111, 0b010), &[0x0003, 0x0004, 0x4E71]);
        bus.write_byte(0x0000_1008, 0x07);

        cpu.step(&mut bus);

        assert!(cpu.flag_z(), "bit 3 of the byte at $1008 is clear");
        assert_eq!(bus.read_byte(0x0000_1008), 0x07, "BTST wrote nothing");
    }

    // ---- BCHG ----

    /// `BCHG` complements the bit and takes `Z` from the bit as it was **before**
    /// the complement: bit set → cleared and `Z` = 1; bit clear → set and
    /// `Z` = 0 (M68000UM "BCHG": "The Z flag is set to the complement of the bit
    /// specified").
    #[test]
    fn bchg_toggles_and_sets_z_from_old() {
        let (mut cpu, mut bus) = run(dynamic(1, BCHG, DN, 2), &[0x4E71]);
        cpu.d[1] = 5;
        cpu.d[2] = 0x0000_0020;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[2], 0x0000_0000, "bit 5 went away");
        assert!(cpu.flag_z(), "the bit was set before, so Z is set");

        let (mut cpu, mut bus) = run(dynamic(1, BCHG, DN, 2), &[0x4E71]);
        cpu.d[1] = 5;
        cpu.d[2] = 0x0000_0000;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[2], 0x0000_0020, "bit 5 appeared");
        assert!(!cpu.flag_z(), "the bit was clear before, so Z is clear");
    }

    /// `BCHG` on a memory operand is a byte read-modify-write at the byte the EA
    /// names, and nothing at ±1 of it moves.
    #[test]
    fn bchg_memory_toggles_one_byte() {
        let (mut cpu, mut bus) = run(static_(BCHG, POST_INC, 0), &[0x0003, 0x4E71]);
        cpu.a[0] = MEM;
        bus.write_byte(MEM - 1, 0x5A);
        bus.write_byte(MEM, 0x11);
        bus.write_byte(MEM + 1, 0xA5);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x19, "bit 3 of $11 is $19");
        assert_eq!(bus.read_byte(MEM - 1), 0x5A, "the byte below is untouched");
        assert_eq!(bus.read_byte(MEM + 1), 0xA5, "the byte above is untouched");
        assert_eq!(cpu.a[0], MEM + 1, "a byte operand steps A0 by one");
    }

    // ---- BCLR ----

    /// `BCLR #3,(A0)`: the static form's bit number is the low eight bits of the
    /// extension word, and on a byte operand it clears exactly that one bit of
    /// the addressed byte (M68000UM "BCLR" with an immediate bit number).
    #[test]
    fn bclr_static() {
        let opcode = static_(BCLR, A_INDIRECT, 0);
        let (mut cpu, mut bus) = run(opcode, &[0x0003, 0x4E71]);
        cpu.a[0] = MEM;
        bus.write_byte(MEM, 0x8B);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x83, "only bit 3 of $8B went away");
        assert!(cpu.flag_z(), "bit 3 was set before the clear");

        let (mut cpu, mut bus) = run(opcode, &[0x0003, 0x4E71]);
        cpu.a[0] = MEM;
        bus.write_byte(MEM, 0x87);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x87, "clearing a clear bit changes nothing");
        assert!(!cpu.flag_z(), "bit 3 was clear before, so Z is clear");
    }

    /// …and the same static form on a `Dn` is a **long** operation: `BCLR #31,D2`
    /// clears bit 31, which is the case a byte-only reading of the static form
    /// gets wrong.
    #[test]
    fn bclr_static_dn_is_a_long() {
        let (mut cpu, mut bus) = run(static_(BCLR, DN, 2), &[0x001F, 0x4E71]);
        cpu.d[2] = 0xFFFF_FFFF;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[2], 0x7FFF_FFFF, "bit 31 of the long, not bit 7");
        assert!(cpu.flag_z());
    }

    // ---- BSET ----

    /// `BSET D1,(A0)+`: a dynamic bit number on a memory operand sets one bit of
    /// one byte, and the postincrement steps `A0` by the *byte* size (M68000UM
    /// "BSET" and section 3 "Address Register Indirect with Postincrement").
    #[test]
    fn bset_dynamic_memory() {
        let (mut cpu, mut bus) = run(dynamic(1, BSET, POST_INC, 0), &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[1] = 7;
        bus.write_byte(MEM, 0x0F);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x8F, "bit 7 of $0F is $8F");
        assert!(!cpu.flag_z(), "the bit was clear before the set");
        assert_eq!(cpu.a[0], MEM + 1, "a byte operand steps A0 by one, not four");
    }

    /// `BSET D1,D2` on a `Dn` is a long write: bit 31 of `D2`, with the low three
    /// bytes left exactly as they were.
    #[test]
    fn bset_dynamic_dn_sets_a_long_bit() {
        let (mut cpu, mut bus) = run(dynamic(1, BSET, DN, 2), &[0x4E71]);
        cpu.d[1] = 31;
        cpu.d[2] = 0x1234_5678;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[2], 0x9234_5678);
        assert!(cpu.flag_z(), "bit 31 was clear before the set");
    }

    // ---- flags ----

    /// **Only `Z` changes.** With all five flags preset and the tested bit clear
    /// — so that `Z` does move — `N`, `V`, `C` and `X` have to survive. That is
    /// the easy rule to get wrong in a handler written from a template that sets
    /// `N` and `Z` from the result (M68000UM "68000 CCR Affected by Instruction
    /// Set": these four touch `Z` only).
    #[test]
    fn only_z_changes() {
        for opcode in [
            dynamic(1, BTST, DN, 2),
            dynamic(1, BCHG, DN, 2),
            dynamic(3, BCLR, DN, 2),
            dynamic(1, BSET, A_INDIRECT, 0),
            static_(BTST, DN, 2),
            static_(BSET, DN, 2),
            static_(BCHG, POST_INC, 0),
            static_(BCLR, A_INDIRECT, 0),
        ] {
            let (mut cpu, mut bus) = run(opcode, &[0x0000, 0x4E71]);
            cpu.d[1] = 0;
            cpu.d[2] = 0;
            cpu.d[3] = 0;
            cpu.a[0] = MEM;
            bus.write_byte(MEM, 0);
            cpu.set_ccr(ALL_FLAGS);

            cpu.step(&mut bus);

            assert!(cpu.flag_z(), "{opcode:04X}: bit 0 was clear, so Z is set");
            assert!(cpu.flag_n(), "{opcode:04X}: N is untouched");
            assert!(cpu.flag_v(), "{opcode:04X}: V is untouched");
            assert!(cpu.flag_c(), "{opcode:04X}: C is untouched");
            assert!(cpu.flag_x(), "{opcode:04X}: X is untouched");
        }
    }

    // ---- read-modify-write resolves the EA once ----

    /// `BSET D1,(A0)+` resolves its destination EA **once**: one byte is read at
    /// the old `A0`, modified, and written back to that same byte, and `A0` moves
    /// by one byte in total (#13, the rule `CLR`/`NEG`/`NOT` were given too).
    #[test]
    fn bset_postincrement_resolves_its_destination_once() {
        let (mut cpu, mut bus) = run(dynamic(1, BSET, POST_INC, 0), &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[1] = 0;
        bus.write_byte(MEM, 0x00);
        bus.write_byte(MEM + 1, 0xFF);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x01, "written back where it read");
        assert_eq!(bus.read_byte(MEM + 1), 0xFF, "not twice, so $20001 was never named");
        assert_eq!(cpu.a[0], MEM + 1, "A0 moved once");
    }

    /// The same for `BCLR`, whose second read would see what the first write
    /// left behind; resolving twice would step `A0` to `MEM + 2` and clear a bit
    /// of the second byte.
    #[test]
    fn bclr_postincrement_resolves_its_destination_once() {
        let (mut cpu, mut bus) = run(dynamic(3, BCLR, POST_INC, 0), &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[3] = 0;
        bus.write_byte(MEM, 0x03);
        bus.write_byte(MEM + 1, 0xFF);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x02, "bit 0 cleared once");
        assert_eq!(bus.read_byte(MEM + 1), 0xFF, "the next byte was never named");
        assert_eq!(cpu.a[0], MEM + 1, "A0 moved once");
    }

    /// …and for `BCHG`, where a double resolve would toggle the same bit twice
    /// and leave the byte untouched: the clearest possible signature of a
    /// read-modify-write that asked the EA twice.
    #[test]
    fn bchg_postincrement_resolves_its_destination_once() {
        let (mut cpu, mut bus) = run(dynamic(1, BCHG, POST_INC, 0), &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[1] = 0;
        bus.write_byte(MEM, 0x02);
        bus.write_byte(MEM + 1, 0xFF);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x03, "bit 0 set once, not toggled twice");
        assert_eq!(bus.read_byte(MEM + 1), 0xFF, "the next byte was never named");
        assert_eq!(cpu.a[0], MEM + 1, "A0 moved once");
    }

    /// `BTST` is a *test*: a memory operand it cannot write is still fine, and a
    /// `d16(An)` operand leaves `An` where it was.
    #[test]
    fn btst_leaves_the_address_register_alone() {
        let (mut cpu, mut bus) = run(dynamic(1, BTST, D16_AN, 0), &[0x0010, 0x4E71]);
        cpu.a[0] = MEM;
        bus.write_byte(MEM + 0x10, 0xFF);

        cpu.step(&mut bus);

        assert!(cpu.flag_z(), "bit 0 of $FF is set");
        assert_eq!(cpu.a[0], MEM, "d16(An) does not move An");
    }

    /// The absolute word form, which is the only mode-`111` operand the modify
    /// instructions can use besides Dn and the index modes: `BSET #6,($2000).W`.
    #[test]
    fn bset_absolute_word() {
        let (mut cpu, mut bus) = run(static_(BSET, ABS_W, 0), &[0x0006, 0x2000, 0x4E71]);
        bus.write_byte(MEM, 0x00);

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0x40);
        assert!(cpu.flag_z(), "the bit was clear before the set");
    }
}
