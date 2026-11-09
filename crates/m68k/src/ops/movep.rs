//! `MOVEP` — move between a data register and a peripheral's every-other-byte
//! register file (M1-25, #33).
//!
//! Encoding `0000 ddd 1 oo 001 aaa` plus one 16-bit displacement word
//! (M68000UM "MOVEP — Move to Peripheral Device"): `ddd` is the data register,
//! `aaa` the address register holding the peripheral's base, and `oo` the
//! direction and width together — bit 7 clear is memory to `Dn` and bit 6 clear
//! is a word, so `$0108` is `MOVEP.W (d16,A0),D0`, `$0148` the long, `$0188`
//! the word the other way round and `$01C8` the long. The mode field is fixed at
//! `001`, which is why MOVEP is the only instruction that reaches that mode of
//! the `$0000` group and why [`crate::decode`] splits it out before the
//! single-bit family.
//!
//! # Every other byte
//!
//! The 68000 has a 16-bit data bus, and the peripheral side of these
//! transactions is wired to it two bytes at a time: a MOVEP transfer lands on
//! `An + d16`, `+2`, `+4`, `+6` and nowhere else, high byte first (M68000UM
//! MOVEP: "displacements of 2, 4, and 6 … are automatically generated"). Each
//! transfer is still four *byte* accesses on the CPU side, so an odd base is
//! legal here — no MOVEP case in either suite file ends in the exception
//! handler, unlike MOVEM's.
//!
//! # A word load leaves the top of `Dn` alone
//!
//! The register side of a transfer is the whole of `Dn` for a long and the low
//! half for a word: `MOVEP.W (d16,A0),D0` merges its two bytes into `D0[15:0]`
//! and leaves `D0[31:16]` exactly as it was, with no sign extension at either
//! width (M68000UM MOVEP). A long load replaces the whole register. Both halves
//! of that are measured: every one of the 4047 word loads in `MOVEP.w.json.gz`
//! keeps the upper half of its register, and the stores write nothing outside
//! the two or four bytes the width calls for.
//!
//! # Cycles
//!
//! Nothing extra: the two or four byte accesses a MOVEP performs are already
//! charged by [`M68k::read_byte`] / [`M68k::write_byte`], and together with the
//! opcode word and the displacement word that is exactly M68000UM section 8's
//! 16 (word) and 24 (long) — `MOVEP.w.json.gz` and `MOVEP.l.json.gz` report one
//! `length` each, 16 and 24 over all 8065 cases of both files, and the core
//! reached both before M1-31b (#40) flipped them to cycle checking.

use crate::M68k;
use crate::bus::Bus;
use crate::ea::{Ea, Size, sign_extend_word};
use crate::ops::field;

/// Bit 7 of a MOVEP opcode: clear is memory to `Dn`, set is `Dn` to memory.
/// MOVEP is the only move in the core that puts the direction in the opcode
/// rather than in which operand is the source, because both operands are fixed.
const TO_MEMORY_BIT: u16 = 0x0080;
/// Bit 6 of a MOVEP opcode: set is a long transfer, clear a word. MOVEM spells
/// the same bit the same way for the same reason.
const LONG_SIZE_BIT: u16 = 0x0040;
/// Distance between the bytes of one MOVEP transfer: the peripheral's register
/// file is every other byte (M68000UM MOVEP).
const BYTE_STRIDE: u32 = 2;

impl M68k {
    /// `MOVEP.W/L <ea>,Dn` and `MOVEP.W/L Dn,<ea>` (`0000 ddd 1 oo 001 aaa`):
    /// the two or four every-other-byte transfers between `Dn` and `An + d16`
    /// (M68000UM MOVEP; M1-25, #33).
    ///
    /// The displacement is the instruction's only extension word, so it is
    /// popped before any operand traffic. The base is `An` plus that
    /// displacement as an *address*: nothing here is masked until the bus sees
    /// it, and the byte helpers mask on their own (#193). No flag moves — MOVEP
    /// is a move — and the clock count is 0 because every access charges itself:
    /// the extension word, and the two or four bytes. The instruction's own clock
    /// table is M1-31's (#39/#40).
    pub(crate) fn op_movep(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, 9);
        let an = usize::from(field(opcode, 0));
        let to_memory = opcode & TO_MEMORY_BIT != 0;
        let size = if opcode & LONG_SIZE_BIT == 0 {
            Size::Word
        } else {
            Size::Long
        };

        let displacement = sign_extend_word(self.fetch_ext_word(bus));
        let base = self.a[an].wrapping_add(displacement);
        let bytes = size.bytes();

        if to_memory {
            let value = self.d[usize::from(reg)];
            // High byte first: `Dn[31:24]` (or `[15:8]` for a word) lands at
            // `base` and the rest follow every other byte.
            for n in 0..bytes {
                let shift = 8 * (bytes - 1 - n);
                let byte = ((value >> shift) & 0xFF) as u8;
                self.write_b(bus, base.wrapping_add(BYTE_STRIDE * n), byte);
            }
        } else {
            let mut value = 0u32;
            for n in 0..bytes {
                let byte = self.read_b(bus, base.wrapping_add(BYTE_STRIDE * n));
                value |= u32::from(byte) << (8 * (bytes - 1 - n));
            }
            // `Size::Word` writes the low half and leaves `Dn[31:16]` alone.
            self.write_ea(bus, Ea::DataReg(reg), size, value);
        }

        self.advance_opcode(bus);
        0
    }
}

#[cfg(test)]
mod tests {
    use crate::M68k;
    use crate::bus::Bus;
    use crate::sr::{SR_C, SR_INT_MASK, SR_N, SR_S, SR_V, SR_X, SR_Z};
    use crate::testbus::{TestBus, cpu_with_program};

    /// Where [`cpu_with_program`] loads the program, so a test can name the
    /// address `pc` should reach.
    const PROGRAM: u32 = 0x0000_1000;
    /// A data address every memory test can use, clear of the program.
    const MEM: u32 = 0x0000_2000;

    /// Every condition code as one CCR value, for "this instruction leaves the
    /// flags alone" tests.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as the
    /// following words.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    /// Write `bytes` to consecutive addresses from `addr`. MOVEP moves single
    /// bytes, so its tests say what is at each byte rather than packing words:
    /// `TestBus::write_word`/`write_long` are big endian and a test that set up
    /// a 32-bit pattern that way would be reading a different order back.
    fn poke(bus: &mut TestBus, addr: u32, bytes: &[u8]) {
        for (n, byte) in bytes.iter().enumerate() {
            bus.write_byte(addr + n as u32, *byte);
        }
    }

    /// `MOVEP.L D0,(d16,A0)` is `$01C8`: `0000 000 1 11 001 000`.
    const MOVEP_L_D0_TO_MEM: u16 = 0x01C8;
    /// `MOVEP.L (d16,A0),D0`, `$0148`: the same with the direction bit clear.
    const MOVEP_L_MEM_TO_D0: u16 = 0x0148;
    /// `MOVEP.W (d16,A0),D0`, `$0108`.
    const MOVEP_W_MEM_TO_D0: u16 = 0x0108;
    /// `MOVEP.W D3,(d16,A1)`, `$0789`: `0000 011 1 10 001 001`, a different
    /// register pair so a handler that read the wrong field fails.
    const MOVEP_W_D3_TO_A1: u16 = 0x0789;

    /// The rule the whole instruction is named for: a long store puts `D0` on
    /// `base`, `+2`, `+4`, `+6`, high byte first, and touches no byte in
    /// between (M68000UM MOVEP).
    #[test]
    fn movep_l_to_memory_every_other_byte() {
        let (mut cpu, mut bus) = run(MOVEP_L_D0_TO_MEM, &[0x0004, 0x4E75]);
        poke(&mut bus, MEM, &[0xAA; 16]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0x1234_5678;

        cpu.step(&mut bus);

        let base = MEM + 4;
        assert_eq!(bus.read_byte(base), 0x12, "D0[31:24] at the base");
        assert_eq!(bus.read_byte(base + 2), 0x34, "D0[23:16] two on");
        assert_eq!(bus.read_byte(base + 4), 0x56, "D0[15:8] four on");
        assert_eq!(bus.read_byte(base + 6), 0x78, "D0[7:0] six on");
        assert_eq!(bus.read_byte(base + 1), 0xAA, "nothing lands between them");
        assert_eq!(bus.read_byte(base + 3), 0xAA);
        assert_eq!(bus.read_byte(base + 5), 0xAA);
        assert_eq!(cpu.pc, PROGRAM + 4, "opcode and one extension word");
    }

    /// The same transfer read back: `$0148` collects the four every-other-byte
    /// values into `D0` with the same alignment, so a store and a load are
    /// exact inverses.
    #[test]
    fn movep_l_from_memory_reads_every_other_byte() {
        let (mut cpu, mut bus) = run(MOVEP_L_MEM_TO_D0, &[0x0000, 0x4E75]);
        poke(
            &mut bus,
            MEM,
            &[0x11, 0xFF, 0x22, 0xFF, 0x33, 0xFF, 0x44, 0xFF],
        );
        cpu.a[0] = MEM;
        cpu.d[0] = 0x0000_0000;

        cpu.step(&mut bus);

        assert_eq!(
            cpu.d[0], 0x1122_3344,
            "base, +2, +4, +6, and the bytes between them are not read"
        );
    }

    /// A word load writes `D0[15:0]` and leaves `D0[31:16]` exactly as it was
    /// (M68000UM MOVEP; see the module docs for why this is not just MOVE's
    /// word rule).
    #[test]
    fn movep_w_from_memory_keeps_upper() {
        let (mut cpu, mut bus) = run(MOVEP_W_MEM_TO_D0, &[0x0000, 0x4E75]);
        poke(&mut bus, MEM, &[0xAB, 0xFF, 0xCD, 0xFF]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0xCAFE_BABE;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xCAFE_ABCD, "the upper half is untouched");
    }

    /// And a word store takes its two bytes out of the *low* half of `Dn`:
    /// `MOVEP.W D3,(d16,A1)` is `$0789`, and `D3[31:16]` never reaches the bus.
    #[test]
    fn movep_w_to_memory_takes_the_low_half() {
        let (mut cpu, mut bus) = run(MOVEP_W_D3_TO_A1, &[0x0000, 0x4E75]);
        poke(&mut bus, MEM, &[0xAA; 8]);
        cpu.a[1] = MEM;
        cpu.d[3] = 0xDEAD_BEEF;

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM), 0xBE, "D3[15:8] at the base");
        assert_eq!(bus.read_byte(MEM + 2), 0xEF, "D3[7:0] two on");
        assert_eq!(
            bus.read_byte(MEM + 4),
            0xAA,
            "a word is two bytes, not four"
        );
        assert_eq!(cpu.d[3], 0xDEAD_BEEF, "the register is unchanged");
        assert_eq!(cpu.a[1], MEM, "and so is the address register");
    }

    /// MOVEP is a move: no condition code moves, in either direction and at
    /// either width, even out of an `SR` that has everything set. The check
    /// covers the whole `SR`, so the upper byte — mode and interrupt mask — has
    /// to stay put as well.
    #[test]
    fn movep_changes_no_flags() {
        for opcode in [
            MOVEP_L_D0_TO_MEM,
            MOVEP_L_MEM_TO_D0,
            MOVEP_W_MEM_TO_D0,
            MOVEP_W_D3_TO_A1,
        ] {
            let (mut cpu, mut bus) = run(opcode, &[0x0000, 0x4E75]);
            poke(&mut bus, MEM, &[0x11, 0x22, 0x33, 0x44]);
            cpu.a[0] = MEM;
            cpu.a[1] = MEM;
            // `SR.T` is deliberately not in here: since #38 it is a live trace
            // enable, so setting it would take the trace exception and this test
            // is not about trace (see [`crate::trace`]). Every other bit of the
            // `SR`, flags and upper byte included, is still set and still checked.
            cpu.set_sr(SR_S | SR_INT_MASK | ALL_FLAGS);
            let sr = cpu.sr;

            cpu.step(&mut bus);

            assert_eq!(cpu.sr, sr, "{opcode:#06X} leaves SR alone");
        }
    }

    /// The displacement is signed, so a peripheral below the base register is
    /// reachable: `-2($2000)` is `$1FFE`, and the four bytes run from there.
    #[test]
    fn movep_uses_a_signed_displacement() {
        let (mut cpu, mut bus) = run(MOVEP_L_D0_TO_MEM, &[0xFFFE, 0x4E75]);
        poke(&mut bus, MEM - 4, &[0xAA; 16]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0x1122_3344;

        cpu.step(&mut bus);

        assert_eq!(bus.read_byte(MEM - 2), 0x11, "-2 from the base");
        assert_eq!(bus.read_byte(MEM), 0x22);
        assert_eq!(bus.read_byte(MEM + 2), 0x33);
        assert_eq!(bus.read_byte(MEM + 4), 0x44);
    }

    /// MOVEP's transfers are byte accesses, so an odd base address is ordinary,
    /// not an address error — the one way MOVEP differs from every other
    /// memory-moving instruction here (M68000UM MOVEP).
    #[test]
    fn movep_allows_an_odd_base() {
        let (mut cpu, mut bus) = run(MOVEP_L_D0_TO_MEM, &[0x0001, 0x4E75]);
        poke(&mut bus, MEM, &[0xAA; 16]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0x9988_7766;

        cpu.step(&mut bus);

        let base = MEM + 1;
        assert_eq!(bus.read_byte(base), 0x99);
        assert_eq!(bus.read_byte(base + 2), 0x88);
        assert_eq!(bus.read_byte(base + 4), 0x77);
        assert_eq!(bus.read_byte(base + 6), 0x66);
        assert_eq!(
            cpu.pc,
            PROGRAM + 4,
            "the instruction finished, so no address error was taken"
        );
    }
}
