//! Effective address (EA) calculation and operand access (M1-05, #13).
//!
//! The EA field of an opcode is 6 bits: mode in bits 5..3, register in bits
//! 2..0 (M68000UM, section 3 "Effective Address Computation"). This module
//! covers the five register-based modes — `000` Dn, `001` An, `010` (An),
//! `011` (An)+, `100` -(An) — and leaves `101`–`111` to M1-06 (#14).
//!
//! The three entry points are used in this order, and the order matters for
//! read-modify-write instructions:
//!
//! 1. [`M68k::resolve_ea`] computes the address *and* applies the
//!    `(An)+` / `-(An)` register update. Call it exactly once per operand.
//! 2. [`M68k::read_ea`] / [`M68k::write_ea`] then access that [`Ea`]. An
//!    `ADD.W #x, (A0)+`-style instruction reads and writes the same `Ea`;
//!    resolving twice would increment A0 twice.
//!
//! No EA calculation here reads memory, so none of them take a bus: extension
//! words (displacements, absolute addresses, immediate data) come from the
//! prefetch queue via [`M68k::fetch_ext_word`], never from `bus.read_word(pc)`
//! — see [`M68k::pc`] for why. The bus only appears in the accessors.

use crate::bus::{ADDR_MASK, Bus};
use crate::cpu::M68k;

/// Operand width of an instruction or an EA access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Byte (8 bit).
    Byte,
    /// Word (16 bit).
    Word,
    /// Long (32 bit).
    Long,
}

impl Size {
    /// Width in bytes: 1, 2, 4.
    #[inline]
    pub const fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }

    /// Bit mask covering this width: `0xFF`, `0xFFFF`, `0xFFFF_FFFF`.
    #[inline]
    pub const fn mask(self) -> u32 {
        match self {
            Size::Byte => 0x0000_00FF,
            Size::Word => 0x0000_FFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }

    /// Most significant bit of this width: `0x80`, `0x8000`, `0x8000_0000`.
    /// Used by the flag rules (N is the MSB of the result, V is signed overflow).
    #[inline]
    pub const fn msb(self) -> u32 {
        match self {
            Size::Byte => 0x0000_0080,
            Size::Word => 0x0000_8000,
            Size::Long => 0x8000_0000,
        }
    }

    /// Decode the common 2-bit size field (`00` byte, `01` word, `10` long).
    /// `11` — and anything wider than the field — is invalid, so this returns
    /// `None` rather than guessing.
    #[inline]
    pub const fn from_bits(bits: u16) -> Option<Size> {
        match bits {
            0 => Some(Size::Byte),
            1 => Some(Size::Word),
            2 => Some(Size::Long),
            _ => None,
        }
    }
}

/// A resolved effective address: where an operand lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ea {
    /// Data register direct (mode 000).
    DataReg(u8),
    /// Address register direct (mode 001).
    AddrReg(u8),
    /// Memory, at an address already computed and masked to 24 bits.
    Mem(u32),
}

impl Ea {
    /// True when `size` is a legal access width for this EA.
    ///
    /// Byte access to an address register is the only illegal combination:
    /// mode 001 is only listed for byte-free operand types, so no 68000
    /// instruction encodes it (M68000UM, section 3 "Effective Address
    /// Computation", mode 001, and section 1 "Address Registers", which gives
    /// An a word or long operand width). [`M68k::resolve_ea`] rejects it, so
    /// `read_ea` / `write_ea` never see it.
    #[inline]
    pub fn accepts_size(self, size: Size) -> bool {
        !matches!(self, Ea::AddrReg(_) if size == Size::Byte)
    }
}

/// Extra internal clocks charged by the `-(An)` address calculation.
///
/// M68000UM, Appendix C "Instruction Timing": predecrement EAs carry 2 internal
/// clock cycles beyond the access itself, because the decrement has to complete
/// before the bus cycle can start. TODO(M1-31a, #39): confirm against the
/// SingleStepTests cycle counts and add the remaining internal-clock
/// differences (`(An)+` with A7 and the register indirect pair).
const PREDEC_CALC_CLOCKS: u64 = 2;

/// Increment or decrement [`M68k::a`] applies for `(An)+` / `-(An)`.
///
/// The step is the operand size, except for a byte access through A7, which
/// steps by 2 so the stack pointer stays word aligned even for byte operands
/// (M68000UM, section 3 "Address Register Indirect" and "Address Register
/// Indirect with Postdecrement": A7 used as a stack pointer steps by 2 for a
/// byte operand, while A0–A6 step by 1).
const fn ea_step(reg: u8, size: Size) -> u32 {
    if reg == 7 && matches!(size, Size::Byte) {
        2
    } else {
        size.bytes()
    }
}

impl M68k {
    /// Resolve addressing modes 000–100 to an [`Ea`], applying the `(An)+` /
    /// `-(An)` register update as a side effect.
    ///
    /// `mode` is bits 5..3 of the EA field and `reg` bits 2..0 (for MOVE's
    /// destination field the two are swapped — handled by M1-09, #17).
    ///
    /// Returns `None` for a mode this layer cannot resolve: `101`–`111`, which
    /// land in M1-06 (#14), and byte size with An direct, which no instruction
    /// encodes (see [`Ea::accepts_size`]). Callers turn `None` into an illegal
    /// instruction or an unimplemented-mode path; it never panics.
    ///
    /// The address registers stay full 32-bit values — the suite keeps nonzero
    /// upper bytes in them (`MOVEA.w.json.gz` case `364a [MOVEA.w A2, A3] 3`
    /// ends with `a3 = 0xFFFF_E780`) — and only [`Ea::Mem`] is masked to
    /// [`ADDR_MASK`], which is the address that actually reaches the bus.
    #[allow(
        dead_code,
        reason = "called by the instruction tickets from M1-09 (#17) onward; only the tests use it today"
    )]
    pub(crate) fn resolve_ea(&mut self, mode: u8, reg: u8, size: Size) -> Option<Ea> {
        let r = usize::from(reg);
        match mode {
            0b000 => Some(Ea::DataReg(reg)),
            0b001 => Ea::AddrReg(reg)
                .accepts_size(size)
                .then_some(Ea::AddrReg(reg)),
            0b010 => Some(Ea::Mem(self.a[r] & ADDR_MASK)),
            0b011 => {
                // Postincrement: the current value is the address, then the
                // register moves past the operand.
                let addr = self.a[r] & ADDR_MASK;
                self.a[r] = self.a[r].wrapping_add(ea_step(reg, size));
                Some(Ea::Mem(addr))
            }
            0b100 => {
                // Predecrement: the register moves first, then the new value is
                // the address, and the calculation itself costs 2 clocks.
                self.a[r] = self.a[r].wrapping_sub(ea_step(reg, size));
                self.cycles += PREDEC_CALC_CLOCKS;
                Some(Ea::Mem(self.a[r] & ADDR_MASK))
            }
            // 101 d16(An), 110 d8/d16(An,Xi) and the PC forms, 111 absolute and
            // immediate: M1-06 (#14).
            _ => None,
        }
    }

    /// Read the operand at `ea`: `size` bits, zero extended into the returned
    /// `u32` (sign extension is the instruction's business, not the EA layer's).
    ///
    /// Memory goes through [`M68k::read_b`] / [`M68k::read_w`] / [`M68k::read_l`]
    /// so every access charges its bus cycles and is masked to 24 bits; a
    /// register access costs nothing.
    #[allow(
        dead_code,
        reason = "called by the instruction tickets from M1-09 (#17) onward; only the tests use it today"
    )]
    pub(crate) fn read_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: Size) -> u32 {
        debug_assert!(
            ea.accepts_size(size),
            "illegal {size:?} access to {ea:?}: resolve_ea is the gate"
        );
        match ea {
            Ea::DataReg(r) => self.d[usize::from(r)] & size.mask(),
            Ea::AddrReg(r) => self.a[usize::from(r)] & size.mask(),
            Ea::Mem(addr) => match size {
                Size::Byte => u32::from(self.read_b(bus, addr)),
                Size::Word => u32::from(self.read_w(bus, addr)),
                Size::Long => self.read_l(bus, addr),
            },
        }
    }

    /// Write `value` to the operand at `ea`, affecting only `size` bits.
    ///
    /// A byte or word write to Dn keeps the upper bits (M68000UM, section 3
    /// "Data Registers": narrower operands occupy the least significant bits).
    /// A word write to An is sign extended to 32 bits, so `MOVEA.W` of a
    /// negative value lands at `0xFFFF_xxxx`. Byte access to An cannot reach
    /// here: [`M68k::resolve_ea`] rejects it.
    ///
    /// TODO(M1-29, #37): a word or long access at an odd address is an address
    /// error (vector 2) before anything is read or written; this layer just
    /// passes the address on and `Ea::Mem` is where that check will hang.
    #[allow(
        dead_code,
        reason = "called by the instruction tickets from M1-09 (#17) onward; only the tests use it today"
    )]
    pub(crate) fn write_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: Size, value: u32) {
        debug_assert!(
            ea.accepts_size(size),
            "illegal {size:?} access to {ea:?}: resolve_ea is the gate"
        );
        match ea {
            Ea::DataReg(r) => {
                let i = usize::from(r);
                self.d[i] = (self.d[i] & !size.mask()) | (value & size.mask());
            }
            Ea::AddrReg(r) => {
                let i = usize::from(r);
                self.a[i] = match size {
                    // Not a legal encoding (see `resolve_ea`); the debug_assert
                    // above fires on any path that gets here, and the low-byte
                    // merge keeps an unreachable release build well defined
                    // instead of panicking inside an instruction.
                    Size::Byte => (self.a[i] & !0xFF) | (value & 0xFF),
                    // M68000UM, section 1 "Address Registers": word operands
                    // written to an address register are sign extended.
                    Size::Word => (value as i16) as i32 as u32,
                    Size::Long => value,
                };
            }
            Ea::Mem(addr) => match size {
                Size::Byte => self.write_b(bus, addr, value as u8),
                Size::Word => self.write_w(bus, addr, value as u16),
                Size::Long => self.write_l(bus, addr, value),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Addressing mode numbers (M68000UM, "Effective Address Calculation").
    const DN: u8 = 0b000;
    const AN: u8 = 0b001;
    const AN_IND: u8 = 0b010;
    const AN_POST: u8 = 0b011;
    const AN_PRE: u8 = 0b100;

    const SIZES: [Size; 3] = [Size::Byte, Size::Word, Size::Long];

    /// Test-LOCAL RAM that records every address it was asked about, copied
    /// from the pattern in `cpu.rs`'s test module: `testbus::TestBus`'s public
    /// shape is deliberately fixed (no access recorder, see #10 / #183), so the
    /// EA tests that need to count bus accesses keep their own.
    struct Ram {
        mem: Vec<u8>,
        reads: Vec<u32>,
        writes: Vec<u32>,
    }

    impl Ram {
        fn new() -> Self {
            Self {
                mem: vec![0; 1 << 16],
                reads: Vec::new(),
                writes: Vec::new(),
            }
        }

        fn peek_byte(&self, addr: u32) -> u8 {
            self.mem[addr as usize]
        }

        fn peek_word(&self, addr: u32) -> u16 {
            let i = addr as usize;
            u16::from_be_bytes([self.mem[i], self.mem[i + 1]])
        }

        fn peek_long(&self, addr: u32) -> u32 {
            let i = addr as usize;
            u32::from_be_bytes([
                self.mem[i],
                self.mem[i + 1],
                self.mem[i + 2],
                self.mem[i + 3],
            ])
        }

        fn poke_word(&mut self, addr: u32, value: u16) {
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }

        fn poke_long(&mut self, addr: u32, value: u32) {
            let i = addr as usize;
            self.mem[i..i + 4].copy_from_slice(&value.to_be_bytes());
        }
    }

    impl Bus for Ram {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.reads.push(addr);
            self.peek_byte(addr)
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.reads.push(addr);
            self.peek_word(addr)
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.writes.push(addr);
            let i = addr as usize;
            self.mem[i] = value;
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.writes.push(addr);
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }
    }

    // ---- mode 000: Dn ----

    #[test]
    fn read_dn_byte_word_long() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();
        cpu.d[0] = 0x1234_5678;

        let want = [0x78u32, 0x5678, 0x1234_5678];
        for (size, want) in SIZES.iter().zip(want) {
            let ea = cpu.resolve_ea(DN, 0, *size).expect("Dn always resolves");
            assert_eq!(ea, Ea::DataReg(0));
            assert_eq!(
                cpu.read_ea(&mut bus, ea, *size),
                want,
                "D0 = 0x12345678 reads {size:?} as {want:#x}"
            );
        }
        assert!(
            bus.reads.is_empty(),
            "register direct never touches the bus"
        );
        assert_eq!(cpu.cycles, 0, "nor does it charge bus cycles");
    }

    #[test]
    fn write_dn_byte_keeps_upper() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();
        cpu.d[0] = 0xFFFF_FFFF;

        let ea = cpu.resolve_ea(DN, 0, Size::Byte).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Byte, 0x00);
        assert_eq!(cpu.d[0], 0xFFFF_FF00, "byte write replaces only bit 7..0");

        let ea = cpu.resolve_ea(DN, 0, Size::Word).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Word, 0x1234_0000);
        assert_eq!(cpu.d[0], 0xFFFF_0000, "word write replaces only bit 15..0");

        let ea = cpu.resolve_ea(DN, 0, Size::Long).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Long, 0xA5A5_5A5A);
        assert_eq!(cpu.d[0], 0xA5A5_5A5A, "long write replaces all 32 bits");
        assert!(bus.writes.is_empty());
    }

    // ---- mode 001: An ----

    #[test]
    fn read_an_direct_truncates_to_size() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();
        cpu.a[2] = 0x1234_5678;

        // Byte is deliberately absent: byte size with An direct is rejected,
        // see `byte_addr_reg_direct_is_rejected`.
        let want = [(Size::Word, 0x5678u32), (Size::Long, 0x1234_5678)];
        for (size, want) in want {
            let ea = cpu.resolve_ea(AN, 2, size).expect("An resolves");
            assert_eq!(ea, Ea::AddrReg(2));
            assert_eq!(cpu.read_ea(&mut bus, ea, size), want, "{size:?} read");
        }
    }

    #[test]
    fn write_word_to_an_sign_extends() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();

        let ea = cpu.resolve_ea(AN, 3, Size::Word).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Word, 0xFFFF_FFFE);
        assert_eq!(cpu.a[3], 0xFFFF_FFFE, "a negative word fills the top");

        let ea = cpu.resolve_ea(AN, 3, Size::Word).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Word, 0x0000_1000);
        assert_eq!(cpu.a[3], 0x0000_1000, "a positive word zeroes the top");

        // A long write is the register value verbatim, top byte included: the
        // suite keeps nonzero upper bytes in An (`MOVEA.w.json.gz` case
        // `3062 [MOVEA.w -(A2), A0] 8` ends with `a2 = 0x9DB1_C21F`).
        let ea = cpu.resolve_ea(AN, 3, Size::Long).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Long, 0x9DB1_C221);
        assert_eq!(cpu.a[3], 0x9DB1_C221);
    }

    #[test]
    fn byte_addr_reg_direct_is_rejected() {
        let mut cpu = M68k::new();
        cpu.a[3] = 0x1000;

        assert_eq!(
            cpu.resolve_ea(AN, 3, Size::Byte),
            None,
            "no 68000 instruction encodes byte size with An direct"
        );
        assert_eq!(cpu.a[3], 0x1000, "rejection changes nothing");
        assert!(
            !Ea::AddrReg(3).accepts_size(Size::Byte),
            "and the EA itself knows it"
        );
        for size in [Size::Word, Size::Long] {
            assert!(Ea::AddrReg(3).accepts_size(size));
        }
        for size in SIZES {
            assert!(Ea::DataReg(3).accepts_size(size));
            assert!(Ea::Mem(0x1000).accepts_size(size));
        }
    }

    // ---- mode 010: (An) ----

    #[test]
    fn addr_reg_indirect_reads_and_writes() {
        let mut bus = Ram::new();
        bus.poke_long(0x1000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[0] = 0x1000;

        let ea = cpu.resolve_ea(AN_IND, 0, Size::Long).unwrap();
        assert_eq!(ea, Ea::Mem(0x1000));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Long), 0xDEAD_BEEF);
        assert_eq!(bus.reads, [0x1000, 0x1002], "a long is two words");

        cpu.write_ea(&mut bus, ea, Size::Word, 0x1111_2222);
        assert_eq!(bus.writes, [0x1000]);
        assert_eq!(
            bus.peek_long(0x1000),
            0x2222_BEEF,
            "a word write replaces only the addressed half"
        );
        assert_eq!(cpu.a[0], 0x1000, "(An) does not move the register");
    }

    // ---- mode 011: (An)+ ----

    #[test]
    fn postinc_steps_by_size() {
        for (i, size) in SIZES.into_iter().enumerate() {
            let mut bus = Ram::new();
            bus.poke_long(0x1000, 0xDEAD_BEEF);
            let mut cpu = M68k::new();
            cpu.a[0] = 0x1000;

            let want = [0xDEu32, 0xDEAD, 0xDEAD_BEEF][i];
            let ea = cpu.resolve_ea(AN_POST, 0, size).unwrap();
            assert_eq!(ea, Ea::Mem(0x1000), "the old value is the address");
            assert_eq!(
                cpu.read_ea(&mut bus, ea, size),
                want,
                "big endian: a narrow read takes the {size:?} at the address"
            );
            assert_eq!(
                cpu.a[0],
                0x1000 + size.bytes(),
                "A0 steps by {} for {size:?}",
                size.bytes()
            );
        }
    }

    #[test]
    fn byte_postinc_a7_steps_by_2() {
        let mut bus = Ram::new();
        bus.poke_long(0x1000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[7] = 0x1000;

        let ea = cpu.resolve_ea(AN_POST, 7, Size::Byte).unwrap();
        assert_eq!(ea, Ea::Mem(0x1000));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Byte), 0xDE);
        assert_eq!(
            cpu.a[7], 0x1002,
            "A7 keeps the stack word aligned: a byte still steps by 2"
        );

        // It is the byte step that is special, not A7 always stepping by 2.
        cpu.a[7] = 0x1000;
        let ea = cpu.resolve_ea(AN_POST, 7, Size::Long).unwrap();
        cpu.read_ea(&mut bus, ea, Size::Long);
        assert_eq!(cpu.a[7], 0x1004);
    }

    // ---- mode 100: -(An) ----

    #[test]
    fn predec_long() {
        let mut bus = Ram::new();
        bus.poke_long(0x2000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[1] = 0x2004;

        let ea = cpu.resolve_ea(AN_PRE, 1, Size::Long).unwrap();
        assert_eq!(ea, Ea::Mem(0x2000), "the decremented value is the address");
        assert_eq!(cpu.a[1], 0x2000, "A1 = 0x2004 - 4");
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Long), 0xDEAD_BEEF);
        assert_eq!(bus.reads, [0x2000, 0x2002]);
    }

    #[test]
    fn byte_predec_a7_steps_by_2() {
        let mut bus = Ram::new();
        bus.poke_long(0x2000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[7] = 0x2004;

        let ea = cpu.resolve_ea(AN_PRE, 7, Size::Byte).unwrap();
        assert_eq!(ea, Ea::Mem(0x2002));
        assert_eq!(
            cpu.a[7], 0x2002,
            "A7 keeps the stack word aligned: a byte still steps by 2"
        );
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Byte), 0xBE);

        // A0 is not special: a byte predecrement steps by 1.
        cpu.a[0] = 0x2004;
        let ea = cpu.resolve_ea(AN_PRE, 0, Size::Byte).unwrap();
        assert_eq!(ea, Ea::Mem(0x2003));
        assert_eq!(cpu.a[0], 0x2003);
    }

    #[test]
    fn predec_charges_two_extra_clocks() {
        let mut cpu = M68k::new();
        cpu.a[1] = 0x2004;

        let ea = cpu.resolve_ea(AN_PRE, 1, Size::Long).unwrap();
        assert_eq!(
            cpu.cycles, PREDEC_CALC_CLOCKS,
            "-(An) charges 2 internal clocks for the address calculation"
        );

        let mut bus = Ram::new();
        cpu.read_ea(&mut bus, ea, Size::Long);
        assert_eq!(
            cpu.cycles,
            PREDEC_CALC_CLOCKS + 8,
            "then the long read charges its own two bus cycles"
        );

        cpu.cycles = 0;
        cpu.resolve_ea(AN_POST, 1, Size::Long);
        cpu.resolve_ea(DN, 1, Size::Long);
        assert_eq!(cpu.cycles, 0, "(An)+ and Dn charge no extra clocks");
    }

    #[test]
    fn predec_wraps_below_zero() {
        let mut cpu = M68k::new();
        cpu.a[7] = 0;

        let ea = cpu.resolve_ea(AN_PRE, 7, Size::Word).unwrap();
        assert_eq!(
            cpu.a[7], 0xFFFF_FFFE,
            "the address register itself is a full 32-bit register"
        );
        assert_eq!(
            ea,
            Ea::Mem(0x00FF_FFFE),
            "the address that reaches the bus is masked to 24 bits"
        );
    }

    #[test]
    fn mem_ea_is_masked_to_24_bits() {
        let mut bus = Ram::new();
        bus.poke_word(0x1000, 0xABCD);
        let mut cpu = M68k::new();
        cpu.a[0] = 0xFF00_1000;

        let ea = cpu.resolve_ea(AN_IND, 0, Size::Word).unwrap();
        assert_eq!(ea, Ea::Mem(0x0000_1000));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Word), 0xABCD);
        assert_eq!(bus.reads, [0x1000]);
    }

    // ---- read-modify-write ----

    #[test]
    fn read_modify_write_resolves_postinc_once() {
        let mut bus = Ram::new();
        bus.poke_long(0x1000, 0x1111_2222);
        let mut cpu = M68k::new();
        cpu.a[0] = 0x1000;

        // What an `ADD.W #x, (A0)+` does: resolve once, then read and write the
        // same Ea. Resolving twice would increment A0 twice.
        let ea = cpu.resolve_ea(AN_POST, 0, Size::Word).unwrap();
        let value = cpu.read_ea(&mut bus, ea, Size::Word);
        assert_eq!(value, 0x1111, "the word at 0x1000 is the high half");
        cpu.write_ea(&mut bus, ea, Size::Word, value ^ 0xFFFF);

        assert_eq!(cpu.a[0], 0x1002, "one increment, not two");
        assert_eq!(bus.reads, [0x1000]);
        assert_eq!(bus.writes, [0x1000]);
        assert_eq!(
            bus.peek_long(0x1000),
            0xEEEE_2222,
            "only the low word moved"
        );
    }

    // ---- unimplemented modes ----

    #[test]
    fn modes_101_to_111_are_not_implemented_yet() {
        for mode in 0b101..0b1000u8 {
            for reg in 0..8u8 {
                for size in SIZES {
                    let mut cpu = M68k::new();
                    cpu.a[7] = 0x2000;
                    let before = cpu.clone();

                    assert_eq!(
                        cpu.resolve_ea(mode, reg, size),
                        None,
                        "mode {mode:b} reg {reg} is M1-06 (#14) territory"
                    );
                    assert_eq!(cpu, before, "and must not disturb the CPU");
                }
            }
        }
    }

    // ---- Size helpers ----

    #[test]
    fn size_helpers() {
        assert_eq!(Size::Byte.bytes(), 1);
        assert_eq!(Size::Word.bytes(), 2);
        assert_eq!(Size::Long.bytes(), 4);

        assert_eq!(Size::Byte.mask(), 0xFF);
        assert_eq!(Size::Word.mask(), 0xFFFF);
        assert_eq!(Size::Long.mask(), 0xFFFF_FFFF);

        assert_eq!(Size::Byte.msb(), 0x80);
        assert_eq!(Size::Word.msb(), 0x8000);
        assert_eq!(Size::Long.msb(), 0x8000_0000);

        assert_eq!(Size::from_bits(0), Some(Size::Byte));
        assert_eq!(Size::from_bits(1), Some(Size::Word));
        assert_eq!(Size::from_bits(2), Some(Size::Long));
        assert_eq!(Size::from_bits(3), None, "11 is the invalid size code");
        assert_eq!(Size::from_bits(0xFFFF), None, "and so is anything wider");
    }
}
