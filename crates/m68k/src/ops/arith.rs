//! Arithmetic instructions: `ADD`, `ADDA`, `ADDI`, `ADDQ`, `ADDX`, and their
//! subtract twins `SUB`, `SUBA`, `SUBI`, `SUBQ`, `SUBX`.
//!
//! Opcodes (M68000UM section 8, "Instruction Formats"):
//!
//! | Group | Bits | Operation |
//! |---|---|---|
//! | `ADD` | `1101 ddd ooo mmm rrr` | `ooo` 000/001/010: `<ea>` + Dd → Dd; 100/101/110: Dd + `<ea>` → `<ea>`; `oo` = size |
//! | `ADDA` | `1101 aaa 011 mmm rrr` / `1101 aaa 111 mmm rrr` | `<ea>` + Aa → Aa, word / long |
//! | `ADDX` | `1101 xxx 1 ss 00 0 yyy` | Dy + Dx + X → Dx |
//! | `ADDX` | `1101 xxx 1 ss 00 1 yyy` | `-(Ay)` + `-(Ax)` + X → `(Ax)` |
//! | `ADDI` | `0000 0110 ss mmm rrr` | `<#data>` + `<ea>` → `<ea>` |
//! | `ADDQ` | `0101 ddd 0 ss mmm rrr` | data + `<ea>` → `<ea>`, data = 1..8, `000` means 8 |
//! | `SUB` | `1001 ddd ooo mmm rrr` | `ooo` 000/001/010: Dd − `<ea>` → Dd; 100/101/110: `<ea>` − Dd → `<ea>`; `oo` = size |
//! | `SUBA` | `1001 aaa 011 mmm rrr` / `1001 aaa 111 mmm rrr` | Aa − `<ea>` → Aa, word (sign-extended) / long |
//! | `SUBX` | `1001 xxx 1 ss 00 0 yyy` | Dx − Dy − X → Dx |
//! | `SUBX` | `1001 xxx 1 ss 00 1 yyy` | `(Ax)` − `-(Ay)` − X → `(Ax)` |
//! | `SUBI` | `0000 0100 ss mmm rrr` | `<ea>` − `<#data>` → `<ea>` |
//! | `SUBQ` | `0101 ddd 1 ss mmm rrr` | `<ea>` − data → `<ea>`, data = 1..8, `000` means 8 |
//!
//! Flags (M68000UM, "68000 CCR Affected by Instruction Set"). `SUB`, `SUBI` and
//! `SUBQ` to a data or memory destination set `X = C`, `N`, `Z`, `V`, `C`, with
//! `C` the borrow. `SUBQ` to an address register and `SUBA` leave all five
//! alone. `SUBX` is the odd one, exactly as `ADDX` is: the incoming `X` is a
//! third operand — a subtrahend, not a carry-in — and `Z` is cleared by a
//! non-zero result but *left unchanged* by a zero one, so a chain of byte
//! subtracts can report zero across the whole width.
//!
//! The two classes are siblings in more than the opcode map: every handler here
//! shares [`M68k::resolve_arith_ea`], every flag rule is one helper per direction
//! ([`M68k::add_flags`]/[`M68k::add_extend_flags`] and
//! [`M68k::sub_flags`]/[`M68k::sub_extend_flags`]), and the size/field constants
//! below are named `ALU_*` because both groups read them.
//!
//! Illegal operands are rejected by [`crate::decode`], not here: `ADD.B`/`SUB.B`
//! from or to `An`, `ADDQ.B`/`SUBQ.B` to `An`, and every non-data destination for
//! `ADDI`/`SUBI`.

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::ea::{Ea, Size, sign_extend_word};
use crate::ops::field;
use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};

/// Shift of the register field of the `1101`/`1001` groups and of `ADDQ`/
/// `SUBQ`'s data field, bits 11..9.
const ALU_REG_SHIFT: u32 = 9;
/// Shift of the operation/size field: bits 8..6 for `ADD`/`ADDA`/`SUB`/`SUBA`
/// (`ooo`), 7..6 (`ss`) for `ADDX`/`ADDI`/`ADDQ` and their subtract twins. Both
/// are three-field-wide extractions, and [`alu_size`] keeps only the two size
/// bits.
const ALU_OP_SHIFT: u32 = 6;
/// Shift of an EA mode, bits 5..3; the EA register is bits 2..0.
const EA_MODE_SHIFT: u32 = 3;
/// `ADD`/`ADDA`/`SUB`/`SUBA` operation bit, bit 8: with bit 6 or 7 set it
/// selects the `Dn op <ea> → <ea>` direction, and for `ADDA`/`SUBA` it selects
/// word or long.
const ALU_MODE_TO_EA: u8 = 0b100;
/// EA mode `001`, an address register.
const EA_MODE_AN: u8 = 0b001;
/// EA mode `100`, `-(An)`.
const EA_PREDEC: u8 = 0b100;
/// EA mode `111`, the mode whose register picks absolute/PC-relative/immediate.
const EA_MODE_111: u8 = 0b111;
/// EA register `100` inside mode `111`: immediate.
const EA_REG_IMMEDIATE: u8 = 0b100;

/// Bit 3 of `ADDX`/`SUBX`: 0 = register mode, 1 = `-(Ay),-(Ax)` memory mode.
const ALU_X_MEMORY: u16 = 0b1000;

/// Size from the two `ss` bits of `ADDI`/`ADDQ`/`SUBI`/`SUBQ` and from `oo` in
/// `ADD`'s and `SUB`'s `ooo` operation field.
///
/// `None` for `0b11`, which is `Scc`/`DBcc` in the `0101` group and undefined
/// in `0000 0110` and `0000 0100`; decode rejects all three, so `None` here is a
/// core bug.
const fn alu_size(bits: u8) -> Option<Size> {
    match bits & 0b11 {
        0b00 => Some(Size::Byte),
        0b01 => Some(Size::Word),
        0b10 => Some(Size::Long),
        _ => None,
    }
}

impl M68k {
    /// `ADD`: `1101 ddd ooo mmm rrr`.
    ///
    /// `ooo` 000/001/010 adds the effective address into data register `ddd`;
    /// 100/101/110 adds data register `ddd` *into* the effective address, a
    /// read-modify-write of that one location (M68000UM section 8, "ADD").
    pub(crate) fn op_add(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, ALU_REG_SHIFT);
        let op_mode = field(opcode, ALU_OP_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(op_mode) else {
            // `011`/`111` are ADDA and `11`-sized `ADD` does not exist, so
            // `classify` never routes them here.
            return self.op_illegal(bus);
        };

        if op_mode & ALU_MODE_TO_EA == 0 {
            // ADD <ea>,Dn: the source first, so `(A0)+` steps once.
            let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
                return self.op_illegal(bus);
            };
            let value = self.read_ea(bus, src, size);
            let result = self.add_flags(value, self.d[usize::from(reg)], false, size);
            self.write_ea(bus, Ea::DataReg(reg), size, result);
        } else {
            // ADD Dn,<ea>: one read-modify-write of the resolved destination.
            // Resolving the EA twice would step `(A0)+` twice, so it is
            // resolved once and the same `Ea` is read and written.
            let source = self.d[usize::from(reg)];
            let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
                return self.op_illegal(bus);
            };
            let value = self.read_ea(bus, dest, size);
            let result = self.add_flags(source, value, false, size);
            self.write_ea(bus, dest, size, result);
        }

        self.advance_opcode(bus);
        0
    }

    /// `ADDA.W` (`1101 aaa 011 mmm rrr`) and `ADDA.L` (`1101 aaa 111 mmm rrr`).
    ///
    /// The word form sign-extends the source to 32 bits before adding, and no
    /// flag is touched — that is the only difference from `ADD` (M68000UM
    /// section 8, "ADDA").
    pub(crate) fn op_adda(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, ALU_REG_SHIFT);
        let op_mode = field(opcode, ALU_OP_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let size = if op_mode & ALU_MODE_TO_EA == 0 {
            Size::Word
        } else {
            Size::Long
        };

        let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, size);
        let addend = if size == Size::Word {
            sign_extend_word(value as u16)
        } else {
            value
        };
        // Written as a long so the full 32-bit sum lands in An: the word form
        // must not re-sign-extend the low word of the result on the way in.
        // No flag is touched.
        let sum = self.a[usize::from(reg)].wrapping_add(addend);
        self.write_ea(bus, Ea::AddrReg(reg), Size::Long, sum);

        self.advance_opcode(bus);
        0
    }

    /// `ADDX`: `1101 xxx 1 ss 00 m yyy`, `ss != 11`.
    ///
    /// Register mode adds `Dy + Dx + X` into `Dx`; memory mode adds the bytes,
    /// words or longs at `-(Ay)` and `-(Ax)` with `X` and stores into `(Ax)`
    /// (M68000UM section 8, "ADDX").
    pub(crate) fn op_addx(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let dest = field(opcode, ALU_REG_SHIFT);
        let src = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            // `ss = 11` is ADDA.L, which decode routes away before the ADDX
            // overlap check, so this cannot happen.
            return self.op_illegal(bus);
        };

        if opcode & ALU_X_MEMORY == 0 {
            let left = self.d[usize::from(src)];
            let right = self.d[usize::from(dest)];
            let result = self.add_extend_flags(left, right, size);
            self.write_ea(bus, Ea::DataReg(dest), size, result);
            self.advance_opcode(bus);
            return 0;
        }

        // Memory mode: `-(Ay)` is resolved and read before `-(Ax)`, so
        // `ADDX.B -(A0),-(A0)` adds the byte to itself with both pointers
        // moving once, and each `Ea` is only resolved once.
        let Some(src_ea) = self.resolve_arith_ea(bus, EA_PREDEC, src, size) else {
            return self.op_illegal(bus);
        };
        let left = self.read_ea(bus, src_ea, size);
        let Some(dest_ea) = self.resolve_arith_ea(bus, EA_PREDEC, dest, size) else {
            return self.op_illegal(bus);
        };
        let right = self.read_ea(bus, dest_ea, size);
        let result = self.add_extend_flags(left, right, size);
        self.write_ea(bus, dest_ea, size, result);

        self.advance_opcode(bus);
        0
    }

    /// `ADDI`: `0000 0110 ss mmm rrr`.
    ///
    /// The immediate is the operand the EA field encodes as mode 111 register
    /// 100, and it is fetched before the destination's own extension words, so
    /// `ADDI.L #$12345678,d8(A0)` reads `$1234`, `$5678`, then `$d8` in that
    /// order (M68000UM section 8, "ADDI").
    pub(crate) fn op_addi(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            return self.op_illegal(bus);
        };

        // The immediate is mode 111 register 100 and it is fetched first, so
        // its words come off the queue before the destination's own extension
        // word (`resolve_ea` pops them as it resolves).
        let Some(immediate) = self.resolve_arith_ea(bus, EA_MODE_111, EA_REG_IMMEDIATE, size)
        else {
            return self.op_illegal(bus);
        };
        let addend = self.read_ea(bus, immediate, size);

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.add_flags(addend, value, false, size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        0
    }

    /// `ADDQ`: `0101 ddd 0 ss mmm rrr`, `ddd` = 1..8 and `000` meaning 8.
    ///
    /// To an address register the operation is always 32 bits whatever the size
    /// field says, and no flag changes (M68000UM section 8, "ADDQ").
    pub(crate) fn op_addq(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let quick = field(opcode, ALU_REG_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            return self.op_illegal(bus);
        };
        // The data field holds 1..8 and `000` means 8.
        let data = u32::from(if quick == 0 { 8 } else { quick });

        if mode == EA_MODE_AN {
            // Always a 32-bit add into An, whatever `ss` says, and no flag is
            // touched — which is why this cannot reuse `add_flags`.
            let sum = self.a[usize::from(ea_reg)].wrapping_add(data);
            self.write_ea(bus, Ea::AddrReg(ea_reg), Size::Long, sum);
            self.advance_opcode(bus);
            return 0;
        }

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.add_flags(data, value, false, size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        0
    }

    /// `SUB`: `1001 ddd ooo mmm rrr`.
    ///
    /// `ooo` 000/001/010 subtracts the effective address *from* data register
    /// `ddd` (`Dn - <ea> → Dn`); 100/101/110 subtract data register `ddd` *from*
    /// the effective address (`<ea> - Dn → <ea>`), a read-modify-write of that
    /// one location (M68000UM section 8, "SUB"). Which of the two an opcode is
    /// decides which operand is the minuend, and getting it the other way round
    /// is the single easiest way to fail every `SUB.W D0,(A0)` case in the suite.
    pub(crate) fn op_sub(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, ALU_REG_SHIFT);
        let op_mode = field(opcode, ALU_OP_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(op_mode) else {
            // `011`/`111` are SUBA and `11`-sized `SUB` does not exist, so
            // `classify` never routes them here.
            return self.op_illegal(bus);
        };

        if op_mode & ALU_MODE_TO_EA == 0 {
            // SUB <ea>,Dn: the source is read first, so `(A0)+` steps once, and
            // the register is the minuend.
            let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
                return self.op_illegal(bus);
            };
            let value = self.read_ea(bus, src, size);
            let result = self.sub_flags(value, self.d[usize::from(reg)], false, size);
            self.write_ea(bus, Ea::DataReg(reg), size, result);
        } else {
            // SUB Dn,<ea>: one read-modify-write of the resolved destination.
            // Resolving the EA twice would step `(A0)+` twice, so it is resolved
            // once and the same `Ea` is read and written. Here the memory operand
            // is the minuend and the register is taken away from it.
            let subtrahend = self.d[usize::from(reg)];
            let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
                return self.op_illegal(bus);
            };
            let value = self.read_ea(bus, dest, size);
            let result = self.sub_flags(subtrahend, value, false, size);
            self.write_ea(bus, dest, size, result);
        }

        self.advance_opcode(bus);
        0
    }

    /// `SUBA.W` (`1001 aaa 011 mmm rrr`) and `SUBA.L` (`1001 aaa 111 mmm rrr`).
    ///
    /// The word form sign-extends the source to 32 bits before subtracting, and
    /// no flag is touched — the only difference from `SUB` is that the
    /// destination is an address register and the borrow is thrown away
    /// (M68000UM section 8, "SUBA").
    pub(crate) fn op_suba(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, ALU_REG_SHIFT);
        let op_mode = field(opcode, ALU_OP_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let size = if op_mode & ALU_MODE_TO_EA == 0 {
            Size::Word
        } else {
            Size::Long
        };

        let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, size);
        let subtrahend = if size == Size::Word {
            sign_extend_word(value as u16)
        } else {
            value
        };
        // The address register is the minuend, and the difference is written as a
        // long so a borrow out of bit 31 wraps the whole register rather than
        // re-sign-extending its low word. No flag is touched, which is why this
        // cannot reuse `sub_flags`.
        let difference = self.a[usize::from(reg)].wrapping_sub(subtrahend);
        self.write_ea(bus, Ea::AddrReg(reg), Size::Long, difference);

        self.advance_opcode(bus);
        0
    }

    /// `SUBX`: `1001 xxx 1 ss 00 m yyy`, `ss != 11`.
    ///
    /// Register mode subtracts `Dy` and the incoming `X` from `Dx`; memory mode
    /// subtracts the bytes, words or longs at `-(Ay)` and `-(Ax)` with `X` and
    /// stores into `(Ax)` (M68000UM section 8, "SUBX"). As for `ADDX`, bits 11..9
    /// are the *destination* and bits 2..0 the source, and the decrements happen
    /// before each access, so `-(A0),-(A0)` reads two consecutive locations.
    pub(crate) fn op_subx(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let dest = field(opcode, ALU_REG_SHIFT);
        let src = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            // `ss = 11` is SUBA.L, which decode routes away before the SUBX
            // overlap check, so this cannot happen.
            return self.op_illegal(bus);
        };

        if opcode & ALU_X_MEMORY == 0 {
            let subtrahend = self.d[usize::from(src)];
            let minuend = self.d[usize::from(dest)];
            let result = self.sub_extend_flags(subtrahend, minuend, size);
            self.write_ea(bus, Ea::DataReg(dest), size, result);
            self.advance_opcode(bus);
            return 0;
        }

        // Memory mode: `-(Ay)` is resolved and read before `-(Ax)`, so
        // `SUBX.B -(A0),-(A0)` subtracts the byte from itself with both pointers
        // moving once, and each `Ea` is only resolved once.
        let Some(src_ea) = self.resolve_arith_ea(bus, EA_PREDEC, src, size) else {
            return self.op_illegal(bus);
        };
        let subtrahend = self.read_ea(bus, src_ea, size);
        let Some(dest_ea) = self.resolve_arith_ea(bus, EA_PREDEC, dest, size) else {
            return self.op_illegal(bus);
        };
        let minuend = self.read_ea(bus, dest_ea, size);
        let result = self.sub_extend_flags(subtrahend, minuend, size);
        self.write_ea(bus, dest_ea, size, result);

        self.advance_opcode(bus);
        0
    }

    /// `SUBI`: `0000 0100 ss mmm rrr`.
    ///
    /// The immediate is the operand the EA field encodes as mode 111 register
    /// 100, and it is fetched before the destination's own extension words, so
    /// `SUBI.L #$12345678,d8(A0)` reads `$1234`, `$5678`, then `$d8` in that
    /// order (M68000UM section 8, "SUBI"). The destination is the minuend.
    pub(crate) fn op_subi(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            return self.op_illegal(bus);
        };

        // The immediate is mode 111 register 100 and it is fetched first, so its
        // words come off the queue before the destination's own extension word
        // (`resolve_ea` pops them as it resolves).
        let Some(immediate) = self.resolve_arith_ea(bus, EA_MODE_111, EA_REG_IMMEDIATE, size)
        else {
            return self.op_illegal(bus);
        };
        let subtrahend = self.read_ea(bus, immediate, size);

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.sub_flags(subtrahend, value, false, size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        0
    }

    /// `SUBQ`: `0101 ddd 1 ss mmm rrr`, `ddd` = 1..8 and `000` meaning 8.
    ///
    /// To an address register the operation is always 32 bits whatever the size
    /// field says, and no flag changes (M68000UM section 8, "SUBQ").
    pub(crate) fn op_subq(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let quick = field(opcode, ALU_REG_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            return self.op_illegal(bus);
        };
        // The data field holds 1..8 and `000` means 8.
        let data = u32::from(if quick == 0 { 8 } else { quick });

        if mode == EA_MODE_AN {
            // Always a 32-bit subtract from An, whatever `ss` says, and no flag
            // is touched — which is why this cannot reuse `sub_flags`.
            let difference = self.a[usize::from(ea_reg)].wrapping_sub(data);
            self.write_ea(bus, Ea::AddrReg(ea_reg), Size::Long, difference);
            self.advance_opcode(bus);
            return 0;
        }

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.sub_flags(data, value, false, size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        0
    }

    /// Add `src` and `dst` at `size`, fold `extend` in as a carry-in, set
    /// X/N/Z/V/C from the result, and return the masked result.
    ///
    /// `extend` is `ADDX`'s incoming X, so callers that pass `true` get an
    /// `ADDX`-flavoured sum and must not also add 1 themselves; callers that
    /// pass `false` are plain `ADD`/`ADDI`/`ADDQ`. X and C both come out as the
    /// carry out of the top bit of `size`, N and V from the masked result, and
    /// V from signed overflow of `src + dst`: same sign on both operands and a
    /// different sign on the result, i.e. `(!(src ^ dst) & (result ^ dst))` at
    /// the size's most significant bit (M68000UM, "Integer Arithmetic Status
    /// Bits").
    ///
    /// `ADDX`'s sticky Z is layered on by [`M68k::add_extend_flags`]; the
    /// subtract side is the same pair of steps, [`M68k::sub_flags`] and
    /// [`M68k::sub_extend_flags`].
    pub(crate) fn add_flags(&mut self, src: u32, dst: u32, extend: bool, size: Size) -> u32 {
        // `u64` so the carry out of the top operand bit survives the add; the
        // operands are masked first so a caller can hand over a full register.
        let mask = u64::from(size.mask());
        let left = u64::from(src) & mask;
        let right = u64::from(dst) & mask;
        let sum = left + right + u64::from(extend);
        let result = (sum & mask) as u32;
        let carry = (sum & !mask) != 0;

        self.set_flag(SR_X, carry);
        self.set_flag(SR_C, carry);
        self.set_flag(SR_N, result & size.msb() != 0);
        self.set_flag(SR_Z, result == 0);
        // Signed overflow: both operands the same sign, result the other.
        let left = left as u32;
        let right = right as u32;
        let overflow = (!(left ^ right) & (result ^ right)) & size.msb();
        self.set_flag(SR_V, overflow != 0);
        result
    }

    /// `add_flags` with `extend` taken from the current X and `ADDX`'s Z rule
    /// applied: a non-zero result clears Z, a zero result leaves it alone
    /// (M68000UM section 8, "ADDX").
    pub(crate) fn add_extend_flags(&mut self, src: u32, dst: u32, size: Size) -> u32 {
        let z_before = self.flag_z();
        let x_before = self.flag_x();
        let result = self.add_flags(src, dst, x_before, size);
        if result == 0 {
            self.set_flag(SR_Z, z_before);
        }
        result
    }

    /// Subtract `src` (and `extend`, as an entry borrow) from `dst` at `size`,
    /// set N/Z/V/C from the result, and return the masked result — **without
    /// touching X**.
    ///
    /// This is the half of the subtract flag logic that does not own X, and it
    /// exists as a seam on its own because `CMP` (M1-12) performs exactly this
    /// subtraction and must leave X alone, while `NEG`/`NEGX`/`SBCD` (M1-14,
    /// M1-20) need it with X. Keeping it separate is what lets them share the
    /// borrow, overflow and zero rules without any of them setting a flag they
    /// are not allowed to touch.
    ///
    /// `dst - src - extend` is done as `dst + !src + 1 - extend` in `u64` so the
    /// borrow out of the top operand bit survives; the carry out is the inverse
    /// of the borrow, so C is set when there is *no* carry, i.e. when
    /// `src + extend > dst` unsigned. V is signed overflow of the subtraction:
    /// the operands have opposite signs and the result has the sign of the
    /// subtrahend, i.e. `((src ^ dst) & (result ^ dst))` at the size's most
    /// significant bit (M68000UM, "Integer Arithmetic Status Bits" and "SUB").
    pub(crate) fn sub_flags_no_x(&mut self, src: u32, dst: u32, extend: bool, size: Size) -> u32 {
        // The operands are masked first so a caller can hand over a full
        // register, whatever the operation size.
        let mask = u64::from(size.mask());
        let subtrahend = u64::from(src) & mask;
        let minuend = u64::from(dst) & mask;
        // `!subtrahend` is the one's complement inside `size`, and the `+ 1` is
        // the two's complement that `extend` then takes a 1 back out of.
        let difference = minuend + (!subtrahend & mask) + 1 - u64::from(extend);
        let result = (difference & mask) as u32;
        // A carry out of the top bit means the subtraction did *not* borrow.
        let borrow = (difference & !mask) == 0;

        self.set_flag(SR_C, borrow);
        self.set_flag(SR_N, result & size.msb() != 0);
        self.set_flag(SR_Z, result == 0);
        // Signed overflow: operands of opposite sign, result with the sign of
        // the subtrahend.
        let subtrahend = subtrahend as u32;
        let minuend = minuend as u32;
        let overflow = ((subtrahend ^ minuend) & (result ^ minuend)) & size.msb();
        self.set_flag(SR_V, overflow != 0);
        result
    }

    /// [`M68k::sub_flags_no_x`] plus the X copy that `SUB`, `SUBI`, `SUBQ`,
    /// `NEG` and friends all want: X ends up as the borrow, the same value C
    /// holds (M68000UM, "68000 CCR Affected by Instruction Set").
    pub(crate) fn sub_flags(&mut self, src: u32, dst: u32, extend: bool, size: Size) -> u32 {
        let result = self.sub_flags_no_x(src, dst, extend, size);
        let borrow = self.flag_c();
        self.set_flag(SR_X, borrow);
        result
    }

    /// `sub_flags` with `extend` taken from the current X and `SUBX`'s Z rule
    /// applied: a non-zero result clears Z, a zero result leaves it alone, so a
    /// chain of `SUBX`s reports zero across the whole width (M68000UM section 8,
    /// "SUBX"). X still comes out as the borrow, which is what makes the next
    /// link in the chain subtract it.
    pub(crate) fn sub_extend_flags(&mut self, src: u32, dst: u32, size: Size) -> u32 {
        let z_before = self.flag_z();
        let x_before = self.flag_x();
        let result = self.sub_flags(src, dst, x_before, size);
        if result == 0 {
            self.set_flag(SR_Z, z_before);
        }
        result
    }

    /// Reads an arithmetic operand EA: any data operand, plus an address
    /// register for `ADDA`/`SUBA` and `ADDQ`/`SUBQ`, which [`crate::decode`] has
    /// already checked. A `None` here means the decode table and this handler
    /// disagree; the debug assert makes that loud in tests and the illegal-instruction
    /// route is what a release build takes (see [`M68k::resolve_move_ea`],
    /// which this mirrors for the move class).
    fn resolve_arith_ea(
        &mut self,
        bus: &mut impl Bus,
        mode: u8,
        reg: u8,
        size: Size,
    ) -> Option<Ea> {
        let ea = self.resolve_ea(bus, mode, reg, size);
        debug_assert!(
            ea.is_some(),
            "decode let an invalid add operand through: mode {mode:b} reg {reg:b} {size:?}"
        );
        ea
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exceptions::EXC_ILLEGAL;
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::cpu_with_program;

    /// Where [`cpu_with_program`] loads the program, so a test can name the
    /// address `pc` should reach.
    const PROGRAM: u32 = 0x0000_1000;
    /// A data address every memory test can use: [`cpu_with_program`] puts the
    /// program at `$1000` and the stack at `$10000`.
    const MEM: u32 = 0x0000_2000;

    /// Every condition code as one CCR value, for "this instruction must leave
    /// the flags alone" tests.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as
    /// the following words.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, crate::testbus::TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    /// `ADD.B D1,D0` (`$D001`) with `$FF + $01`: zero result, carry and extend
    /// set, no signed overflow, and only the low byte of D0 changes.
    #[test]
    fn add_b_carry_and_zero() {
        let (mut cpu, mut bus) = run(0xD001, &[0x4E71]);
        cpu.d[0] = 0xDEAD_00FF;
        cpu.d[1] = 0x0000_0001;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_0000, "byte add keeps the upper 24 bits");
        assert!(cpu.flag_z(), "Z from the zero byte result");
        assert!(cpu.flag_c(), "C from the carry out of bit 7");
        assert!(cpu.flag_x(), "X copies C");
        assert!(!cpu.flag_v(), "-1 + 1 does not overflow");
        assert!(!cpu.flag_n());
    }

    /// `ADD.W D1,D0` (`$D041`) with `$7FFF + $1`: signed overflow and N, no
    /// carry out.
    #[test]
    fn add_w_signed_overflow() {
        let (mut cpu, mut bus) = run(0xD041, &[0x4E71]);
        cpu.d[0] = 0x1234_7FFF;
        cpu.d[1] = 0x0000_0001;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x1234_8000);
        assert!(cpu.flag_v(), "$7FFF + $1 is negative");
        assert!(cpu.flag_n());
        assert!(!cpu.flag_c(), "no carry out of bit 15");
        assert!(!cpu.flag_x());
        assert!(!cpu.flag_z());
    }

    /// `ADD.L D1,D0` (`$D081`) with `$FFFFFFFF + $1`: the carry out of bit 31
    /// reaches C and X, and a two-operand add of opposite signs cannot
    /// overflow.
    #[test]
    fn add_l_carry_out_of_bit_31() {
        let (mut cpu, mut bus) = run(0xD081, &[0x4E71]);
        cpu.d[0] = 0xFFFF_FFFF;
        cpu.d[1] = 0x0000_0001;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x0000_0000);
        assert!(cpu.flag_z());
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(!cpu.flag_v());
    }

    /// `ADD.W A1,D0` (`$D049`) and `ADD.L A1,D0` (`$D089`) read the address
    /// register at the operand size, and the size reaches the flags too
    /// (M68000UM section 8, "ADD": `An` is a valid source at every size, only a
    /// byte *destination* is illegal).
    #[test]
    fn add_from_an_uses_the_selected_size() {
        // A1 = $1FFFE, D0 = $DEAD0003: the word form sees `$FFFE`, which
        // carries out of bit 15, the long form sees `$1FFFE`, which does not.
        let (mut cpu, mut bus) = run(0xD049, &[0x4E71]);
        cpu.a[1] = 0x0001_FFFE;
        cpu.d[0] = 0xDEAD_0003;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_0001, "only the low word is added");
        assert!(cpu.flag_c(), "$FFFE + $3 carries out of bit 15");
        assert!(!cpu.flag_v(), "-2 + 3 does not overflow");

        let (mut cpu, mut bus) = run(0xD089, &[0x4E71]);
        cpu.a[1] = 0x0001_FFFE;
        cpu.d[0] = 0xDEAD_0003;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAF_0001, "the whole of A1 is added");
        assert!(!cpu.flag_c(), "and it does not carry out of bit 31");
    }

    /// `ADD.B #imm,D0` (`$D03C`) takes the low byte of one extension word and
    /// consumes exactly that word.
    #[test]
    fn add_immediate_source_uses_the_operand_width() {
        let (mut cpu, mut bus) = run(0xD03C, &[0x1234, 0x4E71]);
        cpu.d[0] = 0x0000_00CC;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x0000_0000, "$CC + low byte $34 wraps");
        assert!(cpu.flag_z());
        assert!(cpu.flag_c());
        assert_eq!(cpu.pc, PROGRAM + 4, "one extension word");
    }

    /// `ADD.W D0,(A0)+` (`$D158`) is a read-modify-write of one location: the
    /// post-increment moves A0 exactly once and the old location holds the
    /// sum.
    #[test]
    fn add_dn_to_ea_is_rmw_on_one_resolved_ea() {
        let (mut cpu, mut bus) = run(0xD158, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0xDEAD_0002;
        bus.write_word(MEM, 0x0001);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0x0003_0000, "sum at the old address");
        assert_eq!(cpu.a[0], MEM + 2, "one step, not two");
        assert!(!cpu.flag_c());
        assert!(!cpu.flag_z());
    }

    /// `ADD.B D1,(A0)` (`$D310`) touches one byte of memory.
    #[test]
    fn add_b_to_memory_touches_only_its_byte() {
        let (mut cpu, mut bus) = run(0xD310, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[1] = 0x0000_0001;
        bus.mem[MEM as usize] = 0xAA;
        bus.mem[MEM as usize + 1] = 0x55;

        cpu.step(&mut bus);

        assert_eq!(&bus.mem[MEM as usize..MEM as usize + 2], &[0xAB, 0x55]);
        assert_eq!(cpu.a[0], MEM);
    }

    /// `ADDA.W D0,A1` (`$D2C0`) sign-extends the word source into a 32-bit add
    /// and leaves the flags exactly as they were (M68000UM section 8, "ADDA").
    #[test]
    fn adda_w_sign_extends_no_flags() {
        let (mut cpu, mut bus) = run(0xD2C0, &[0x4E71]);
        cpu.d[0] = 0xDEAD_FFFF;
        cpu.a[1] = 0x0000_1000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[1], 0x0000_0FFF, "$FFFF sign extends to -1");
        assert_eq!(cpu.ccr(), ALL_FLAGS, "ADDA affects no flag");
    }

    /// `ADDA.L D1,A0` (`$D1C1`) stores the full 32-bit sum: a result whose low
    /// word looks negative must not be re-sign-extended on the way into A0,
    /// and no flag changes.
    #[test]
    fn adda_l_stores_the_full_32_bit_sum() {
        let (mut cpu, mut bus) = run(0xD1C1, &[0x4E71]);
        cpu.d[1] = 0xFFFF_0000;
        cpu.a[0] = 0x0002_0000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0x0001_0000, "the carry out of bit 31 is dropped");
        assert_eq!(cpu.ccr(), ALL_FLAGS);
    }

    /// `ADDQ.L #8,D0` (`$5080`): the `000` data field means 8, not 0.
    #[test]
    fn addq_8_encoding() {
        let (mut cpu, mut bus) = run(0x5080, &[0x4E71]);
        cpu.d[0] = 0xFFFF_FFF8;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x0000_0000, "adds 8");
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(cpu.flag_z());
    }

    /// `ADDQ.W #1,A0` (`$5248`) adds 1 to all 32 bits of A0 and touches no
    /// flag; a word-only add would have produced `$00000000` here.
    #[test]
    fn addq_to_an_is_long_and_flagless() {
        let (mut cpu, mut bus) = run(0x5248, &[0x4E71]);
        cpu.a[0] = 0x0000_FFFF;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0x0001_0000, "32-bit add whatever the size field");
        assert_eq!(cpu.ccr(), ALL_FLAGS, "ADDQ to An affects no flag");
    }

    /// `ADDQ.B #8,(A0)` (`$5010`) is a normal byte add for the flags.
    #[test]
    fn addq_to_memory_sets_the_add_flags() {
        let (mut cpu, mut bus) = run(0x5010, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.mem[MEM as usize] = 0xF8;

        cpu.step(&mut bus);

        assert_eq!(bus.mem[MEM as usize], 0x00);
        assert!(cpu.flag_z());
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(!cpu.flag_v(), "-8 + 8 is not a signed overflow");
    }

    /// `ADDQ.B #1,A0` (`$5208`) is not an instruction, and neither is
    /// `ADDI.W #imm,A0` (`$0648`): both decode to Illegal, so running one
    /// raises vector 3 and leaves the register alone.
    #[test]
    fn byte_addq_and_immediate_to_an_are_illegal() {
        let (mut cpu, mut bus) = run(0x5208, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        assert_eq!(cpu.a[0], MEM);

        let (mut cpu, mut bus) = run(0x0648, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        assert_eq!(cpu.a[0], MEM);
    }

    /// `ADDX.B D1,D0` (`$D101`) leaves Z alone when the result is zero and
    /// clears it when the result is not, in both directions from a preset Z.
    #[test]
    fn addx_z_sticky() {
        let (mut cpu, mut bus) = run(0xD101, &[0x4E71]);
        cpu.d[0] = 0;
        cpu.d[1] = 1;
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x01);
        assert!(!cpu.flag_z(), "a non-zero result clears Z");

        let (mut cpu, mut bus) = run(0xD101, &[0x4E71]);
        cpu.d[0] = 0;
        cpu.d[1] = 0;
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x00);
        assert!(cpu.flag_z(), "a zero result leaves a set Z alone");

        let (mut cpu, mut bus) = run(0xD101, &[0x4E71]);
        cpu.d[0] = 0;
        cpu.d[1] = 0;
        cpu.set_ccr(0);

        cpu.step(&mut bus);

        assert!(!cpu.flag_z(), "a zero result leaves a clear Z alone");
    }

    /// `ADDX.B D1,D0` (`$D101`) folds X in as a carry and writes the carry out
    /// back to X.
    #[test]
    fn addx_extend_is_the_third_operand() {
        let (mut cpu, mut bus) = run(0xD101, &[0x4E71]);
        cpu.d[0] = 0x0000_00FF;
        cpu.d[1] = 0x0000_0000;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x00, "$FF + $00 + X wraps");
        assert!(cpu.flag_c());
        assert!(cpu.flag_x(), "X comes back out as the carry");

        let (mut cpu, mut bus) = run(0xD101, &[0x4E71]);
        cpu.d[0] = 0x0000_000F;
        cpu.d[1] = 0x0000_0000;
        cpu.set_ccr(SR_X | SR_Z | SR_C | SR_V | SR_N);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x10);
        assert!(!cpu.flag_c(), "no carry out, so X clears too");
        assert!(!cpu.flag_x());
        assert!(!cpu.flag_z(), "non-zero result clears Z");
    }

    /// `ADDX.B -(A1),-(A0)` (`$D109`) reads and writes at the decremented
    /// addresses, each pointer moving exactly one byte (M68000UM section 8,
    /// "ADDX": "An is decremented ... before the operation").
    #[test]
    fn addx_predecrement_uses_the_decremented_addresses_once() {
        let (mut cpu, mut bus) = run(0xD109, &[0x4E71]);
        cpu.a[0] = MEM + 2;
        cpu.a[1] = MEM + 4;
        bus.mem[MEM as usize + 3] = 0x40;
        bus.mem[MEM as usize + 1] = 0x02;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], MEM + 1, "Ax decremented by one byte");
        assert_eq!(cpu.a[1], MEM + 3, "Ay decremented by one byte");
        assert_eq!(bus.mem[MEM as usize + 1], 0x43, "$40 + $02 + X at (Ax)");
        assert_eq!(
            bus.mem[MEM as usize + 3],
            0x40,
            "the source byte is not moved"
        );
        assert_eq!(bus.mem[MEM as usize], 0x00);
    }

    /// `ADDI.B #imm,D0` (`$0600`) adds the low byte of one extension word.
    #[test]
    fn addi_b_uses_the_low_byte_of_one_extension_word() {
        let (mut cpu, mut bus) = run(0x0600, &[0x1234, 0x4E71]);
        cpu.d[0] = 0xDEAD_00D0;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_0004, "$D0 + $34");
        assert!(cpu.flag_c());
        assert_eq!(cpu.pc, PROGRAM + 4, "one extension word");
    }

    /// `ADDI.L #imm,($2000).W` (`$06B8` $8000 $0000 $2000`): two extension
    /// words for the immediate, high word first, *before* the destination's
    /// own address word — the order is visible because adding `$80000000` to
    /// the memory long wraps to zero only if the immediate was read first.
    #[test]
    fn addi_l_to_memory() {
        let (mut cpu, mut bus) = run(0x06B8, &[0x8000, 0x0000, MEM as u16, 0x4E71]);
        bus.write_long(MEM, 0x8000_0000);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0x0000_0000, "$80000000 + $80000000");
        assert!(cpu.flag_z());
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(cpu.flag_v(), "two negatives give a positive");
        assert!(!cpu.flag_n());
        assert_eq!(cpu.pc, PROGRAM + 8, "opcode + 3 extension words");
    }

    // ── The subtract class (M1-11, #19) ──────────────────────────────────────
    //
    // Mirrors the add tests above opcode for opcode, because `SUB` is `ADD`'
    // twin: the same fields, the same illegal combinations, the same
    // read-modify-write and extension-word ordering rules. Only the arithmetic
    // and the direction of the two-operand forms differ, so those are what the
    // assertions below pin hardest.

    /// `SUB.B D1,D0` (`$9001`) with `$00 - $01`: the byte wraps up to `$FF`, and
    /// a borrow sets C, X and N (M68000UM section 8, "SUB").
    #[test]
    fn sub_b_borrow() {
        let (mut cpu, mut bus) = run(0x9001, &[0x4E71]);
        cpu.d[0] = 0xDEAD_0000;
        cpu.d[1] = 0x0000_0001;

        cpu.step(&mut bus);

        assert_eq!(
            cpu.d[0], 0xDEAD_00FF,
            "byte subtract keeps the upper 24 bits"
        );
        assert!(cpu.flag_c(), "$00 - $01 borrows out of bit 7");
        assert!(cpu.flag_x(), "X copies C");
        assert!(cpu.flag_n(), "the wrapped byte is negative");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v(), "0 - 1 cannot overflow");
    }

    /// `SUB.W D1,D0` (`$9041`) with `$8000 - $1`: the signed minimum minus one
    /// is the positive maximum, so V is set while no borrow leaves bit 15.
    #[test]
    fn sub_w_overflow() {
        let (mut cpu, mut bus) = run(0x9041, &[0x4E71]);
        cpu.d[0] = 0x1234_8000;
        cpu.d[1] = 0x0000_0001;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x1234_7FFF);
        assert!(cpu.flag_v(), "-32768 - 1 is +32767: signed overflow");
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_c(), "no borrow out of bit 15");
        assert!(!cpu.flag_x());
        assert!(!cpu.flag_z());
    }

    /// `SUB.L D1,D0` (`$9081`) with `$0 - $1`: the borrow out of bit 31 reaches
    /// both C and X, and Z is cleared by the all-ones result.
    #[test]
    fn sub_l_borrow_out_of_bit_31() {
        let (mut cpu, mut bus) = run(0x9081, &[0x4E71]);
        cpu.d[0] = 0x0000_0000;
        cpu.d[1] = 0x0000_0001;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xFFFF_FFFF);
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(cpu.flag_n());
        assert!(!cpu.flag_z());
    }

    /// `SUB.B D1,D0` (`$9001`) with equal operands: unlike `SUBX`, plain `SUB`
    /// sets Z on a zero result rather than leaving it alone.
    #[test]
    fn sub_zero_result_sets_z() {
        let (mut cpu, mut bus) = run(0x9001, &[0x4E71]);
        cpu.d[0] = 0xDEAD_0042;
        cpu.d[1] = 0x0000_0042;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_0000);
        assert!(cpu.flag_z(), "SUB sets Z, it does not leave it");
        assert!(!cpu.flag_c(), "equal operands do not borrow");
        assert!(!cpu.flag_x());
        assert!(!cpu.flag_n());
    }

    /// `SUB.W A1,D0` (`$9049`) and `SUB.L A1,D0` (`$9089`) read the address
    /// register at the operand size, and the size reaches the borrow too: the
    /// word form sees `$FFFE`, which borrows, the long form sees `$1FFFE`,
    /// which does not.
    #[test]
    fn sub_from_an_uses_the_selected_size() {
        let (mut cpu, mut bus) = run(0x9049, &[0x4E71]);
        cpu.a[1] = 0x0001_FFFE;
        cpu.d[0] = 0xDEAD_0003;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_0005, "only the low word is subtracted");
        assert!(cpu.flag_c(), "$3 - $FFFE borrows out of bit 15");
        assert!(!cpu.flag_v(), "3 - -2 does not overflow");

        let (mut cpu, mut bus) = run(0x9089, &[0x4E71]);
        cpu.a[1] = 0x0001_FFFE;
        cpu.d[0] = 0xDEAD_0003;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAB_0005, "the whole of A1 is subtracted");
        assert!(!cpu.flag_c(), "and it does not borrow out of bit 31");
    }

    /// `SUB.B #imm,D0` (`$903C`) takes the low byte of one extension word and
    /// consumes exactly that word.
    #[test]
    fn sub_immediate_source_uses_the_operand_width() {
        let (mut cpu, mut bus) = run(0x903C, &[0x1234, 0x4E71]);
        cpu.d[0] = 0xDEAD_00CC;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_0098, "$CC - $34");
        assert!(!cpu.flag_c(), "$CC is the larger operand");
        assert!(cpu.flag_n());
        assert_eq!(cpu.pc, PROGRAM + 4, "one extension word");
    }

    /// `SUB.W D0,(A0)+` (`$9158`) is a read-modify-write of one location, and
    /// the operand the EA names is the *minuend*: `$0005 - $0002`, never
    /// `$0002 - $0005`, and the post-increment moves A0 exactly once.
    #[test]
    fn sub_dn_from_ea_is_rmw_on_one_resolved_ea() {
        let (mut cpu, mut bus) = run(0x9158, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0xDEAD_0002;
        bus.write_word(MEM, 0x0005);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_long(MEM),
            0x0003_0000,
            "(A0) - D0 at the old address, not D0 - (A0)"
        );
        assert_eq!(cpu.a[0], MEM + 2, "one step, not two");
        assert!(!cpu.flag_c());
        assert!(!cpu.flag_z());
    }

    /// `SUB.B D1,(A0)` (`$9310`) touches one byte of memory, and it is the
    /// memory byte that the register is taken away from.
    #[test]
    fn sub_b_to_memory_touches_only_its_byte() {
        let (mut cpu, mut bus) = run(0x9310, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[1] = 0x0000_00AA;
        bus.mem[MEM as usize] = 0x01;
        bus.mem[MEM as usize + 1] = 0x55;

        cpu.step(&mut bus);

        assert_eq!(
            &bus.mem[MEM as usize..MEM as usize + 2],
            &[0x57, 0x55],
            "$01 - $AA wraps to $57 and the next byte is untouched"
        );
        assert!(cpu.flag_c(), "$01 - $AA borrows");
        assert!(cpu.flag_x());
        assert!(!cpu.flag_n(), "$57 is positive");
        assert_eq!(cpu.a[0], MEM);
    }

    /// `SUBA.W D0,A1` (`$92C0`) sign-extends the word source into a 32-bit
    /// subtract and leaves the flags exactly as they were (M68000UM section 8,
    /// "SUBA").
    #[test]
    fn suba_w_sign_extends() {
        let (mut cpu, mut bus) = run(0x92C0, &[0x4E71]);
        cpu.d[0] = 0xDEAD_FFFF;
        cpu.a[1] = 0x0000_1000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[1], 0x0000_1001, "$FFFF sign extends to -1");
        assert_eq!(cpu.ccr(), ALL_FLAGS, "SUBA affects no flag");
    }

    /// `SUBA.L D1,A0` (`$91C1`) stores the full 32-bit difference: A0 is the
    /// minuend, and a borrow out of bit 31 is dropped rather than reaching the
    /// flags or the upper bits.
    #[test]
    fn suba_l_stores_the_full_32_bit_difference() {
        let (mut cpu, mut bus) = run(0x91C1, &[0x4E71]);
        cpu.d[1] = 0xFFFF_0000;
        cpu.a[0] = 0x0002_0000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0x0003_0000, "A0 - D1, not D1 - A0");
        assert_eq!(cpu.ccr(), ALL_FLAGS);
    }

    /// `SUBQ.L #8,D0` (`$5180`): the `000` data field means 8, not 0.
    #[test]
    fn subq_8_encoding() {
        let (mut cpu, mut bus) = run(0x5180, &[0x4E71]);
        cpu.d[0] = 0x0000_0003;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xFFFF_FFFB, "subtracts 8");
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(cpu.flag_n());
        assert!(!cpu.flag_v(), "3 - 8 is not a signed overflow");
    }

    /// `SUBQ.W #1,A0` (`$5348`) subtracts 1 from all 32 bits of A0 and touches
    /// no flag; a word-only subtract would have left `$0001FFFF` here.
    #[test]
    fn subq_8_from_an_flagless() {
        let (mut cpu, mut bus) = run(0x5348, &[0x4E71]);
        cpu.a[0] = 0x0001_0000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], 0x0000_FFFF, "32-bit subtract, not a 16-bit one");
        assert_eq!(cpu.ccr(), ALL_FLAGS, "SUBQ to An affects no flag");
    }

    /// `SUBQ.B #8,(A0)` (`$5110`) is a normal byte subtract for the flags.
    #[test]
    fn subq_to_memory_sets_the_sub_flags() {
        let (mut cpu, mut bus) = run(0x5110, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.mem[MEM as usize] = 0x00;

        cpu.step(&mut bus);

        assert_eq!(bus.mem[MEM as usize], 0xF8);
        assert!(!cpu.flag_z());
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(cpu.flag_n());
        assert!(!cpu.flag_v(), "0 - 8 is not a signed overflow");
    }

    /// `SUBQ.B #1,A0` (`$5308`) is not an instruction, and neither is
    /// `SUBI.W #imm,A0` (`$0448`): both decode to Illegal, so running one
    /// raises vector 3 and leaves the register alone.
    #[test]
    fn byte_subq_and_immediate_to_an_are_illegal() {
        let (mut cpu, mut bus) = run(0x5308, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        assert_eq!(cpu.a[0], MEM);

        let (mut cpu, mut bus) = run(0x0448, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        assert_eq!(cpu.a[0], MEM);
    }

    /// `SUBX.B D1,D0` (`$9101`) leaves Z alone when the result is zero and
    /// clears it when the result is not, in both directions from a preset Z —
    /// the rule that keeps a multi-precision chain's Z correct.
    #[test]
    fn subx_z_sticky() {
        let (mut cpu, mut bus) = run(0x9101, &[0x4E71]);
        cpu.d[0] = 0x0000_0001;
        cpu.d[1] = 0x0000_0000;
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x01);
        assert!(!cpu.flag_z(), "a non-zero result clears Z");

        let (mut cpu, mut bus) = run(0x9101, &[0x4E71]);
        cpu.d[0] = 0;
        cpu.d[1] = 0;
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x00);
        assert!(cpu.flag_z(), "a zero result leaves a set Z alone");

        let (mut cpu, mut bus) = run(0x9101, &[0x4E71]);
        cpu.d[0] = 0;
        cpu.d[1] = 0;
        cpu.set_ccr(0);

        cpu.step(&mut bus);

        assert!(!cpu.flag_z(), "a zero result leaves a clear Z alone");
    }

    /// `SUBX.B D1,D0` (`$9101`) folds X in as a third operand — an extra
    /// subtrahend, not a carry — and writes the borrow out back to X.
    #[test]
    fn subx_extend_is_the_third_operand() {
        let (mut cpu, mut bus) = run(0x9101, &[0x4E71]);
        cpu.d[0] = 0x0000_0000;
        cpu.d[1] = 0x0000_00FF;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x00, "$00 - $FF - X wraps to zero");
        assert!(cpu.flag_c());
        assert!(cpu.flag_x(), "X comes back out as the borrow");

        let (mut cpu, mut bus) = run(0x9101, &[0x4E71]);
        cpu.d[0] = 0x0000_0010;
        cpu.d[1] = 0x0000_0000;
        cpu.set_ccr(SR_X | SR_Z | SR_C | SR_V | SR_N);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x0F, "$10 - $00 - X");
        assert!(!cpu.flag_c(), "no borrow, so X clears too");
        assert!(!cpu.flag_x());
        assert!(!cpu.flag_z(), "non-zero result clears Z");
    }

    /// `SUBX.B -(A1),-(A0)` (`$9109`) reads and writes at the decremented
    /// addresses, each pointer moving exactly one byte (M68000UM section 8,
    /// "SUBX": "An is decremented ... before the operation").
    #[test]
    fn subx_predecrement_uses_the_decremented_addresses_once() {
        let (mut cpu, mut bus) = run(0x9109, &[0x4E71]);
        cpu.a[0] = MEM + 2;
        cpu.a[1] = MEM + 4;
        bus.mem[MEM as usize + 3] = 0x40;
        bus.mem[MEM as usize + 1] = 0x02;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], MEM + 1, "Ax decremented by one byte");
        assert_eq!(cpu.a[1], MEM + 3, "Ay decremented by one byte");
        assert_eq!(
            bus.mem[MEM as usize + 1],
            0xC1,
            "$02 - $40 - X at (Ax), not the other way round"
        );
        assert_eq!(
            bus.mem[MEM as usize + 3],
            0x40,
            "the source byte is not moved"
        );
        assert_eq!(bus.mem[MEM as usize], 0x00);
        assert!(cpu.flag_c(), "$02 - $40 - X borrows");
        assert!(cpu.flag_x());
    }

    /// `SUBI.B #imm,D0` (`$0400`) subtracts the low byte of one extension word.
    #[test]
    fn subi_b_uses_the_low_byte_of_one_extension_word() {
        let (mut cpu, mut bus) = run(0x0400, &[0x1234, 0x4E71]);
        cpu.d[0] = 0xDEAD_00D0;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_009C, "$D0 - low byte $34");
        assert!(!cpu.flag_c());
        assert_eq!(cpu.pc, PROGRAM + 4, "one extension word");
    }

    /// `SUBI.L #imm,($2000).W` (`$04B8 $1234 $5678 $2000`): two extension words
    /// for the immediate, high word first, *before* the destination's own
    /// address word. The operands are deliberately asymmetric so a wrong order
    /// cannot still land at `$2000` with `$12345678` in hand.
    #[test]
    fn subi_l_memory() {
        let (mut cpu, mut bus) = run(0x04B8, &[0x1234, 0x5678, MEM as u16, 0x4E71]);
        bus.write_long(MEM, 0x0000_0001);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0xEDCB_A989, "$1 - $12345678");
        assert!(cpu.flag_c());
        assert!(cpu.flag_x());
        assert!(cpu.flag_n());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v(), "1 - a positive does not overflow");
        assert_eq!(cpu.pc, PROGRAM + 8, "opcode + 3 extension words");
    }

    /// The seam CMP (M1-12) and NEG (M1-14) reuse: `sub_flags_no_x` sets
    /// N/Z/V/C from the difference and leaves X exactly where it was — even when
    /// the difference does not borrow, which is the case that would silently
    /// clear X under CMP. `sub_flags`, the entry SUB itself uses, still copies C
    /// into X.
    #[test]
    fn sub_flags_no_x_leaves_x_alone() {
        let (mut cpu, _bus) = run(0x4E71, &[]);
        cpu.set_ccr(SR_X);

        let result = cpu.sub_flags_no_x(0x0000_0100, 0x0000_0001, false, Size::Word);

        assert_eq!(result, 0x0000_FF01, "$1 - $100, never $100 - $1");
        assert!(cpu.flag_c(), "$1 - $100 borrows");
        assert!(cpu.flag_n(), "the result is negative at word size");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v());
        assert!(cpu.flag_x(), "a borrow must not clear a set X here either");

        let (mut cpu, _bus) = run(0x4E71, &[]);
        cpu.set_ccr(SR_X);

        let result = cpu.sub_flags_no_x(0x0000_0001, 0x0000_0100, false, Size::Word);

        assert_eq!(result, 0x0000_00FF);
        assert!(!cpu.flag_c(), "$100 - $1 does not borrow");
        assert!(
            cpu.flag_x(),
            "and X keeps the value it had: CMP leaves X alone"
        );

        let (mut cpu, _bus) = run(0x4E71, &[]);
        cpu.set_ccr(SR_X);

        let result = cpu.sub_flags(0x0000_0001, 0x0000_0100, false, Size::Word);

        assert_eq!(result, 0x0000_00FF);
        assert!(!cpu.flag_c());
        assert!(
            !cpu.flag_x(),
            "SUB takes X from the borrow, so it clears here"
        );
    }
}
