//! Arithmetic instructions: `ADD`, `ADDA`, `ADDI`, `ADDQ`, `ADDX`, and their
//! subtract twins `SUB`, `SUBA`, `SUBI`, `SUBQ`, `SUBX`; the compare and test
//! class `CMP`, `CMPA`, `CMPI`, `CMPM` and `TST`, which do the same arithmetic
//! (or simply a read) for the flags alone and store nothing back; and the three
//! single-operand read-modify-writes of the `$4000`/`$4200`/`$4400` opcodes —
//! `NEGX` and `NEG` are subtracts from zero and `CLR` is `set_logic_flags` with
//! the 68000's read-before-write kept.
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
//! | `CMP` | `1011 ddd 0ss mmm rrr` | Dd − `<ea>` → flags only |
//! | `CMPA` | `1011 aaa 011 mmm rrr` / `1011 aaa 111 mmm rrr` | Aa − `<ea>` → flags, word (sign-extended) / long |
//! | `CMPM` | `1011 xxx 1 ss 001 yyy` | `(Ax)+` − `(Ay)+` → flags, both pointers step |
//! | `CMPI` | `0000 1100 ss mmm rrr` | `<ea>` − `<#data>` → flags only |
//! | `TST` | `0100 1010 ss mmm rrr` | read `<ea>` → N/Z, V and C cleared |
//! | `NEGX` | `0100 0000 ss mmm rrr` | `0 − <ea> − X` → `<ea>`, `Z` sticky |
//! | `CLR` | `0100 0010 ss mmm rrr` | 0 → `<ea>`, reading it first |
//! | `NEG` | `0100 0100 ss mmm rrr` | `0 − <ea>` → `<ea>` |
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
//! [`M68k::sub_flags`]/[`M68k::sub_extend_flags`]) — which is why `NEG` and
//! `NEGX` need no arithmetic of their own beyond "minuend zero" — and the
//! size/field constants below are named `ALU_*` because both groups read them.
//!
//! Flags again, for the compare class (M68000UM, "68000 CCR Affected by
//! Instruction Set"): `CMP`/`CMPA`/`CMPI`/`CMPM` set `N`, `Z`, `V` and `C` and
//! leave `X` alone, which is why they call [`M68k::sub_flags_no_x`] and the
//! `sub_flags` wrapper would be wrong for every one of them. `TST` sets `N` and
//! `Z` from the operand and clears `V` and `C`, again leaving `X` alone, which is
//! [`M68k::set_logic_flags`].
//!
//! Illegal operands are rejected by [`crate::decode`], not here: `ADD.B`/`SUB.B`
//! from or to `An`, `ADDQ.B`/`SUBQ.B` to `An`, every non-data destination for
//! `ADDI`/`SUBI`/`CMPI`, `CMP.B` from `An`, and `TST`, `NEGX`, `CLR` and `NEG` of
//! `An`, a PC-relative operand or an immediate.

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::ea::{Ea, Size, sign_extend_word};
use crate::exceptions::EXC_CHK;
use crate::ops::field;
use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};

/// Clocks one pass of the 68000 ALU costs: the ALU is 16 bits wide, so an
/// operand or a result that is a long is a pass of its own, and a pass that no
/// bus cycle overlaps is 2 clocks.
///
/// Not a number the manual states in this form — M68000UM section 8 gives
/// per-instruction totals. What the totals leave after their bus cycles are
/// subtracted is this constant times a number of passes, measured over every
/// non-exception case of the pinned `SingleStepTests` files for M1-31a (#39).
/// [`one_operand_clocks`] is the count for the one-operand group; the byte and
/// word forms of every group here need nothing, which is why their `.b`/`.w`
/// files pass unchanged.
pub(crate) const ALU_PASS_CLOCKS: u32 = 2;

/// Clocks a one-operand ALU instruction (`NEG`, `NEGX`, `CLR`, `NOT`) pays on
/// top of its bus traffic: one long pass when the operand is a register, and
/// nothing when it is memory, whose read-then-write cycles hide the pass.
///
/// Measured: `NEG.l`/`NEGX.l`/`CLR.l`/`NOT.l` against `Dn` are 2 over (6 clocks,
/// 4 of them the fetch), and every memory case of those files is already exact.
pub(crate) const fn one_operand_clocks(size: Size, ea: Ea) -> u32 {
    match (size, ea) {
        (Size::Long, Ea::DataReg(_)) => ALU_PASS_CLOCKS,
        _ => 0,
    }
}

/// Clocks the two-operand ALU instructions (`ADD`, `SUB` and their `#imm`/`Q`
/// forms, and via [`crate::ops::logic`] `AND`, `OR` and `EOR`) pay on top of
/// their bus traffic: a long operation is two passes of [`ALU_PASS_CLOCKS`], and
/// every memory access the operand pair performs hides one.
///
/// That is the one rule the M68000UM section 8 totals reduce to once the bus
/// cycles the core already charges are subtracted, and it covers both
/// directions: a long `<ea>,Dn` is 2 over for a memory source (its read covers
/// one pass) and 4 over for a register, `An` or `#imm` source (which cover
/// neither), while a long read-modify-write — `ADD.L D0,(A0)`, `ADDI.L`,
/// `ADDQ.L`, `EOR.L` — does two accesses, is covered twice over, and pays
/// nothing. Byte and word operands are a single pass and pay nothing anywhere;
/// that is why those files were already exact.
///
/// Measured over every non-exception case of the pinned `ADD.b/w/l`,
/// `ADDA.w/l`, `SUB.b/w/l`, `SUBA.w/l`, `AND.b/w/l`, `OR.b/w/l` and `EOR.b/w/l`
/// suites for #225.
pub(crate) const fn alu_op_clocks(size: Size, accesses: u32) -> u32 {
    match size {
        Size::Long => (2 * ALU_PASS_CLOCKS).saturating_sub(accesses * ALU_PASS_CLOCKS),
        Size::Byte | Size::Word => 0,
    }
}

/// The bus accesses a *source* operand performs: one for a memory read, none for
/// a register and none for an immediate, which [`crate::ea`] already fetched off
/// the queue (see [`crate::ea::Ea::Imm`] and [`M68k::read_ea`]).
pub(crate) const fn source_accesses(ea: Ea) -> u32 {
    match ea {
        Ea::Mem(_) => 1,
        Ea::DataReg(_) | Ea::AddrReg(_) | Ea::Imm(_) => 0,
    }
}

/// The bus accesses a *destination* operand performs: a read then a write for a
/// memory location, none for a register. The `Dn op <ea> → <ea>` half of every
/// two-operand ALU instruction is why these count two where
/// [`source_accesses`] counts one, and it is what makes them exact.
pub(crate) const fn dest_accesses(ea: Ea) -> u32 {
    match ea {
        Ea::Mem(_) => 2,
        Ea::DataReg(_) | Ea::AddrReg(_) | Ea::Imm(_) => 0,
    }
}

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
/// EA mode `011`, `(An)+` — the only mode `CMPM` uses.
const EA_POSTINC: u8 = 0b011;
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

        let pass_clocks;
        if op_mode & ALU_MODE_TO_EA == 0 {
            // ADD <ea>,Dn: the source first, so `(A0)+` steps once.
            let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
                return self.op_illegal(bus);
            };
            let value = self.read_ea(bus, src, size);
            let result = self.add_flags(value, self.d[usize::from(reg)], false, size);
            self.write_ea(bus, Ea::DataReg(reg), size, result);
            pass_clocks = alu_op_clocks(size, source_accesses(src));
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
            pass_clocks = alu_op_clocks(size, dest_accesses(dest));
        }

        self.advance_opcode(bus);
        pass_clocks
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
        // `ADDA.W` pays for the sign extension as well as the long add in every
        // mode, memory included, so the operand's accesses do not hide it: 4
        // clocks flat. `ADDA.L` needs no extension pass and so follows the
        // two-pass rule of [`alu_op_clocks`] like `ADDA`'s plain-`ADD` sibling.
        // Measured over `ADDA.w` and `ADDA.l` (#225).
        if size == Size::Word {
            2 * ALU_PASS_CLOCKS
        } else {
            alu_op_clocks(size, source_accesses(src))
        }
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
            // Two registers, so no bus access covers the long add's passes; the
            // byte and word forms pay nothing ([`alu_op_clocks`], #225).
            return alu_op_clocks(size, 0);
        }

        // Memory mode: `-(Ay)` is resolved and read before `-(Ax)`, so
        // `ADDX.B -(A0),-(A0)` adds the byte to itself with both pointers
        // moving once, and each `Ea` is only resolved once. The destination is
        // resolved as a write: its predecrement happens in the write cycle, so
        // only `-(Ay)` pays the address calculation ([`crate::ea::EaUse`], and
        // what all 3 sizes of `ADDX.<mem>` measure once it is done that way).
        let Some(src_ea) = self.resolve_arith_ea(bus, EA_PREDEC, src, size) else {
            return self.op_illegal(bus);
        };
        let left = self.read_ea(bus, src_ea, size);
        let Some(dest_ea) = self.resolve_arith_ea_write(bus, EA_PREDEC, dest, size) else {
            return self.op_illegal(bus);
        };
        let right = self.read_ea(bus, dest_ea, size);
        let result = self.add_extend_flags(left, right, size);
        self.write_ea(bus, dest_ea, size, result);

        // The three accesses of the memory form — read `-(Ay)`, read `-(Ax)`,
        // write `-(Ax)` — cover its ALU passes, so nothing is added here even at
        // long (#225).
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
        alu_op_clocks(size, dest_accesses(dest))
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
            // Measured over `ADD.w` and `ADD.l`, which is where the suite puts
            // `ADDQ` cases: the word form pays for the width pass on top of the
            // long add, the long form for the add alone. There is no bus access
            // to hide either (#225).
            return if size == Size::Word {
                2 * ALU_PASS_CLOCKS
            } else {
                ALU_PASS_CLOCKS
            };
        }

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.add_flags(data, value, false, size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        alu_op_clocks(size, dest_accesses(dest))
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

        let pass_clocks;
        if op_mode & ALU_MODE_TO_EA == 0 {
            // SUB <ea>,Dn: the source is read first, so `(A0)+` steps once, and
            // the register is the minuend.
            let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
                return self.op_illegal(bus);
            };
            let value = self.read_ea(bus, src, size);
            let result = self.sub_flags(value, self.d[usize::from(reg)], false, size);
            self.write_ea(bus, Ea::DataReg(reg), size, result);
            pass_clocks = alu_op_clocks(size, source_accesses(src));
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
            pass_clocks = alu_op_clocks(size, dest_accesses(dest));
        }

        self.advance_opcode(bus);
        pass_clocks
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
        // As for [`M68k::op_adda`]: the word form pays the sign-extension pass in
        // every mode, the long form follows [`alu_op_clocks`] (#225).
        if size == Size::Word {
            2 * ALU_PASS_CLOCKS
        } else {
            alu_op_clocks(size, source_accesses(src))
        }
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
            // The register form of `ADDX`, including its 4 clocks for `SUBX.L`
            // (see [`alu_op_clocks]).
            return alu_op_clocks(size, 0);
        }

        // Memory mode: `-(Ay)` is resolved and read before `-(Ax)`, so
        // `SUBX.B -(A0),-(A0)` subtracts the byte from itself with both pointers
        // moving once, and each `Ea` is only resolved once. As in `ADDX`, the
        // destination is resolved as a write so its predecrement is free
        // ([`crate::ea::EaUse`]), and its three accesses cover its ALU passes,
        // so nothing is added after `advance_opcode` (#225).
        let Some(src_ea) = self.resolve_arith_ea(bus, EA_PREDEC, src, size) else {
            return self.op_illegal(bus);
        };
        let subtrahend = self.read_ea(bus, src_ea, size);
        let Some(dest_ea) = self.resolve_arith_ea_write(bus, EA_PREDEC, dest, size) else {
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
        alu_op_clocks(size, dest_accesses(dest))
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
            // is touched — which is why this cannot reuse `sub_flags`. Timing
            // too is the `ADDQ` rule, not the operand one: see [`M68k::op_addq`].
            let difference = self.a[usize::from(ea_reg)].wrapping_sub(data);
            self.write_ea(bus, Ea::AddrReg(ea_reg), Size::Long, difference);
            self.advance_opcode(bus);
            return if size == Size::Word {
                2 * ALU_PASS_CLOCKS
            } else {
                ALU_PASS_CLOCKS
            };
        }

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.sub_flags(data, value, false, size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        alu_op_clocks(size, dest_accesses(dest))
    }

    /// `CMP`: `1011 ddd 0ss mmm rrr`.
    ///
    /// `Dn - <ea>` at `size`, and only the flags survive: neither operand is
    /// written back (M68000UM section 8, "CMP"). `X` is unaffected, which is the
    /// whole reason this calls [`M68k::sub_flags_no_x`] and not [`M68k::sub_flags`]
    /// — the borrow goes to `C` and stops there.
    ///
    /// The source is read first, so `CMP.W (A0)+,D0` steps `A0` exactly once.
    pub(crate) fn op_cmp(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, ALU_REG_SHIFT);
        let op_mode = field(opcode, ALU_OP_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(op_mode) else {
            // `011`/`111` are CMPA and `11`-sized `CMP` does not exist, so
            // `classify` never routes them here.
            return self.op_illegal(bus);
        };

        let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, size);
        self.sub_flags_no_x(value, self.d[usize::from(reg)], false, size);

        self.advance_opcode(bus);
        // A long compare is one ALU pass the bus cycles do not cover: 2 clocks,
        // in every addressing mode except `#imm` (`ADD.l` and friends, which
        // also write a result, pay it only for a source that is not read from
        // memory — see [`ALU_PASS_CLOCKS]`). `CMP.b`/`CMP.w` are already exact.
        if size == Size::Long {
            ALU_PASS_CLOCKS
        } else {
            0
        }
    }

    /// `CMPA.W` (`1011 aaa 011 mmm rrr`) and `CMPA.L` (`1011 aaa 111 mmm rrr`).
    ///
    /// The word form sign-extends its source to 32 bits, and the comparison is
    /// then 32 bits wide whatever the size field said: `CMPA.W` against an `A0`
    /// of `$1FFFF` with a source word of `$FFFF` is *not* equal (M68000UM
    /// section 8, "CMPA"). So this is [`M68k::op_suba`]'s operand handling with
    /// [`M68k::sub_flags_no_x`] applied at [`Size::Long`] and the store dropped:
    /// `X` is unaffected and `A0` keeps its value.
    pub(crate) fn op_cmpa(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
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
        // The address register is the minuend, and the compare is a long one even
        // for `CMPA.W`, so a borrow out of bit 31 shows up in `C`.
        self.sub_flags_no_x(subtrahend, self.a[usize::from(reg)], false, Size::Long);

        self.advance_opcode(bus);
        // One long pass, and `CMPA.W` does not escape it either: the compare is
        // long whatever the operand size, so all 10 modes of `CMPA.W` and
        // `CMPA.L` measure 2 over ([`ALU_PASS_CLOCKS`], M1-31a #39).
        ALU_PASS_CLOCKS
    }

    /// `CMPM`: `1011 xxx 1 ss 001 yyy`, `(Ay)+` compared against `(Ax)+`.
    ///
    /// `(Ax) - (Ay)` at `size`, flags only, and **both** pointers step forward by
    /// the operand size (M68000UM section 8, "CMPM"). As in `SUBX`, bits 11..9 are
    /// the destination and bits 2..0 the source; each EA is resolved exactly once
    /// and `(Ay)` is read first, so `CMPM.B (A0)+,(A0)+` compares two consecutive
    /// bytes and leaves `A0` two bytes along.
    pub(crate) fn op_cmpm(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let dest = field(opcode, ALU_REG_SHIFT);
        let src = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            // `ss` = `11` is `CMPA.L`, which `classify` hands to `op_cmpa`
            // before it looks at the EA mode, so it cannot arrive here.
            return self.op_illegal(bus);
        };

        let Some(src_ea) = self.resolve_arith_ea(bus, EA_POSTINC, src, size) else {
            return self.op_illegal(bus);
        };
        let subtrahend = self.read_ea(bus, src_ea, size);
        let Some(dest_ea) = self.resolve_arith_ea(bus, EA_POSTINC, dest, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest_ea, size);
        self.sub_flags_no_x(subtrahend, value, false, size);

        self.advance_opcode(bus);
        0
    }

    /// `CMPI`: `0000 1100 ss mmm rrr`.
    ///
    /// `<ea>` − `<#data>`, flags only. The immediate is the operand the EA field
    /// encodes as mode 111 register 100 and it is fetched before the
    /// destination's own extension words, exactly as in [`M68k::op_subi`]
    /// (M68000UM section 8, "CMPI").
    pub(crate) fn op_cmpi(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            return self.op_illegal(bus);
        };

        // Fetched first, so its words come off the queue before the
        // destination's own extension word (`resolve_ea` pops them as it goes).
        let Some(immediate) = self.resolve_arith_ea(bus, EA_MODE_111, EA_REG_IMMEDIATE, size)
        else {
            return self.op_illegal(bus);
        };
        let subtrahend = self.read_ea(bus, immediate, size);

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        self.sub_flags_no_x(subtrahend, value, false, size);

        self.advance_opcode(bus);
        0
    }

    /// `TST`: `0100 1010 ss mmm rrr`, `ss` != `11`.
    ///
    /// Reads `<ea>` and sets N and Z from it, clears V and C and leaves X alone
    /// (M68000UM section 8, "TST") — [`M68k::set_logic_flags`], the rule `MOVE`
    /// and the logic group share. Nothing is stored, but the read is a real one:
    /// `TST.B (A0)+` steps `A0`, and an odd word address is still an address
    /// error (M1-29 will take that up).
    pub(crate) fn op_tst(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            // `ss` = `11` is the TAS sixth of `$4Axx`, which `classify` keeps
            // out of this handler.
            return self.op_illegal(bus);
        };

        let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, src, size);
        self.set_logic_flags(value, size);

        self.advance_opcode(bus);
        0
    }

    /// `TAS`: `1001 0110 00 mmm rrr` — read `<ea>` as a byte, set the flags from
    /// it, and write it back with bit 7 set (M68000UM section 8, "TAS — Test and
    /// Set"; #35).
    ///
    /// Two things distinguish it from the [`M68k::op_tst`] above it, which is
    /// otherwise the same shape:
    ///
    /// * the size is byte-only. `TAS` shares the `$4Axx` field with `TST` but
    ///   takes the `ss` = `11` slot that `TST` must refuse, so there is no
    ///   `alu_size` call here — `decode::classify_tas` already rejected every
    ///   operand mode that cannot be altered, which is why an address register
    ///   direct or an immediate reaches this file as `Op::Illegal` and never as
    ///   `Op::Tas`.
    /// * the read is a read-modify-write, so the same resolved [`Ea`] is read and
    ///   then written. Resolving twice would step `(A0)+` a second time.
    ///
    /// The flags are the `TST` rule — [`M68k::set_logic_flags`], which sets N and
    /// Z from the value, clears V and C, and leaves X alone — and they are set
    /// from the value **before** bit 7 is forced, which is what makes `TAS` of
    /// zero set `Z` while storing `$80`.
    ///
    /// The store is an ordinary byte write: M68000UM notes that the write-back is
    /// not locked against an interrupt, so there is no bus-level special case to
    /// model here.
    pub(crate) fn op_tas(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);

        let Some(ea) = self.resolve_arith_ea(bus, mode, ea_reg, Size::Byte) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, ea, Size::Byte);
        self.set_logic_flags(value, Size::Byte);
        self.write_ea(bus, ea, Size::Byte, value | 0x0080);

        self.advance_opcode(bus);
        0
    }

    /// `CHK.W Dn,<ea>`: `0100 ddd 110 mmm rrr` (M68000UM "CHK — Check Data
    /// Register Against Upper Bound"; M1-27, #35).
    ///
    /// Signed 16-bit comparison of `Dn` against the bound, with three outcomes
    /// and no write-back at all — `Dn` is read and left exactly as it was:
    ///
    /// * `Dn.w < 0` — `N = 1`, [`crate::EXC_CHK`] (vector 6, slot `$18`).
    /// * `Dn.w > <ea>.w` — `N = 0`, same vector.
    /// * otherwise — the instruction finishes normally.
    ///
    /// The bound test is signed on both sides, so a negative `Dn` traps whatever
    /// the bound is and never reaches the second comparison.
    ///
    /// # Flags
    ///
    /// M68000UM calls `C`, `V` and `Z` "undefined" for CHK (only `N` and `X` are
    /// spelled out: `N` from the trap condition above, `X` unaffected). They are
    /// not undefined in the data though, and `CHK.json.gz` pins one particular
    /// choice for each path:
    ///
    /// `Z`, `V` and `C` come out **cleared on every path**, trap or not, and `N`
    /// is written by the two trap paths only — it is what says *why* the trap
    /// fired. `X` is untouched throughout. The in-range cases are what make this
    /// visible, because there is no trap to blame for the flags: `CHK D4,D4` with
    /// `D4.w >= 0` clears an incoming `V` (so it is not "flags untouched"), and
    /// `CHK (d16, A4),D1` with `D1` below the bound keeps an incoming `N = 1`
    /// (so it is not `CMP Dn,<ea>` either, which would take `N` from the
    /// subtraction and `Z` from its being zero). Two thirds of the manual's
    /// "undefined" is therefore "the ALU result codes get dropped on the floor",
    /// and the third that survives is `N`.
    ///
    /// Which of the two trap reasons fired is what `N` reports, and the negative
    /// `Dn` test runs first: a negative `Dn` above a *negative* bound traps with
    /// `N = 1` even though its difference is positive.
    ///
    /// # The frame's PC
    ///
    /// Unlike a divide, CHK is *not* restarted: the frame carries the address of
    /// the next instruction, which is why `return_pc` is read from `pc` *after*
    /// the operand is resolved. Resolving an extension-word mode steps `pc` over
    /// the extension (see [`M68k::fetch_ext_word`]) but never over the opcode
    /// itself, so `pc + 2` is the instruction after the whole CHK whatever the
    /// mode — one word for `d16(An)`, two for `abs.L`.
    pub(crate) fn op_chk(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let reg = field(opcode, ALU_REG_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);

        let Some(src) = self.resolve_arith_ea(bus, mode, ea_reg, Size::Word) else {
            return self.op_illegal(bus);
        };
        // The bound is read before anything is decided: `CHK (A0)+,D0` steps `A0`
        // once whether or not it traps.
        let bound = self.read_ea(bus, src, Size::Word);
        let value = self.d[usize::from(reg)];
        // Both sides of the compare are the low word, sign-extended, and the
        // result is only ever 0, positive or negative — no wrap-around to worry
        // about, which is what lets the two tests below stay plain `if`s.
        let signed_value = (value as u16) as i16 as i32;
        let signed_bound = (bound as u16) as i16 as i32;

        // `Z`, `V` and `C` come out clear on every path, `X` never changes, and
        // only the two trap paths touch `N` — it is what says *why* the trap
        // fired. The in-range path leaves `N` exactly as it found it, which is
        // odd but measured: `CHK (d16, A4),D1` with `D1` in range expects a set
        // `N` carried in from earlier code, while `CHK D4,D4` with `D4.w >= 0`
        // expects a clear one taken from an incoming `V`.
        let cleared = self.ccr() & !(SR_N | SR_Z | SR_V | SR_C);
        let return_pc = self.pc.wrapping_add(2);

        if signed_value < 0 {
            self.set_ccr(cleared | SR_N);
            self.exception(bus, EXC_CHK, return_pc);
            return 0;
        }
        if signed_value > signed_bound {
            self.set_ccr(cleared);
            self.exception(bus, EXC_CHK, return_pc);
            return 0;
        }

        // In range: `CHK` writes nothing back, so clearing the three flags is the
        // whole rest of it.
        self.set_ccr(self.ccr() & !(SR_Z | SR_V | SR_C));
        self.advance_opcode(bus);
        0
    }

    /// `NEG`: `0100 0100 ss mmm rrr` — `0 - <ea> → <ea>` (M68000UM section 8,
    /// "NEG"; M1-14, #22).
    pub(crate) fn op_neg(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.neg_operand(bus, opcode, false)
    }

    /// `NEGX`: `0100 0000 ss mmm rrr` — `0 - <ea> - X → <ea>`, with `SUBX`'s
    /// sticky `Z` (M68000UM section 8, "NEGX"; M1-14, #22).
    pub(crate) fn op_negx(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.neg_operand(bus, opcode, true)
    }

    /// The `NEG`/`NEGX` body: one data-alterable operand, taken from zero and
    /// written back to the **same** resolved [`Ea`] — `NEG.W (A0)+` steps `A0`
    /// once, and `-(A0)` is read and written at the decremented address. That is
    /// `logic_to_ea` in `ops::logic`, which `NOT` and `EOR` use, with different
    /// arithmetic.
    ///
    /// `extend` selects `NEGX`: the incoming `X` becomes a third subtrahend and
    /// `Z` goes sticky, exactly as in `SUBX`. Both directions call
    /// [`M68k::sub_flags`]/[`M68k::sub_extend_flags`] with the operand as the
    /// subtrahend and zero as the minuend — the one operand order that makes
    /// "`C` is set unless the operand was zero" (M68000UM "NEG") fall out of the
    /// borrow rule instead of needing a case of its own, and `$80`/`$8000`/
    /// `$8000_0000` the only operand that sets `V`.
    fn neg_operand(&mut self, bus: &mut impl Bus, opcode: u16, extend: bool) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            // `ss` = `11`: `$40C0` is MOVE from SR's sixth (#34) and `$44C0`
            // unassigned, and `classify` stops both ranges at `$xBF`.
            return self.op_illegal(bus);
        };

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = if extend {
            self.sub_extend_flags(value, 0, size)
        } else {
            self.sub_flags(value, 0, false, size)
        };
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        one_operand_clocks(size, dest)
    }

    /// `CLR`: `0100 0010 ss mmm rrr` — zero to `<ea>` (M68000UM section 8,
    /// "CLR"; M1-14, #22).
    ///
    /// `Z` set, `N`, `V` and `C` cleared, `X` unaffected (M68000PM "CLR"), which
    /// is [`M68k::set_logic_flags`] applied to zero — the rule `MOVE`, `NOT` and
    /// `TST` share. Nothing is computed from the operand that was there, so the
    /// operand is read for its bus cycle alone.
    ///
    /// Which brings the one thing a plain "write zero" would miss: **on the 68000
    /// a memory destination is read first** (M68000PM "CLR", which lists the
    /// 68020's read-free `CLR` as a later-machine difference), so the read is
    /// resolved once with the write and charged like any other access. The suite
    /// cannot see this — it compares final state — so
    /// `clr_to_memory_reads_then_writes` is the test that pins it.
    pub(crate) fn op_clr(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = alu_size(field(opcode, ALU_OP_SHIFT)) else {
            // `ss` = `11` again: `$42C0..=$42FF` is unassigned on the 68000.
            return self.op_illegal(bus);
        };

        let Some(dest) = self.resolve_arith_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        // The 68000's dummy read: the value is thrown away, the bus cycle is not.
        self.read_ea(bus, dest, size);
        self.set_logic_flags(0, size);
        self.write_ea(bus, dest, size, 0);

        self.advance_opcode(bus);
        one_operand_clocks(size, dest)
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

    /// Reads an arithmetic or compare operand EA: any data operand, plus an
    /// address register for `ADDA`/`SUBA`, `ADDQ`/`SUBQ` and a word or long
    /// `CMP`, which [`crate::decode`] has already checked. A `None` here means the decode table and this handler
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

    /// The write side of [`M68k::resolve_arith_ea`], for the instruction that
    /// resolves the location it stores to: `ADDX`/`SUBX` in their `-(Ay),-(Ax)`
    /// form. See [`crate::ea::EaUse`] for why that one costs 2 clocks less than
    /// reading the same mode (#225).
    fn resolve_arith_ea_write(
        &mut self,
        bus: &mut impl Bus,
        mode: u8,
        reg: u8,
        size: Size,
    ) -> Option<Ea> {
        let ea = self.resolve_ea_write(bus, mode, reg, size);
        debug_assert!(
            ea.is_some(),
            "decode let an invalid add destination through: mode {mode:b} reg {reg:b} {size:?}"
        );
        ea
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exceptions::{EXC_CHK, EXC_ILLEGAL};
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

    /// `CMP.B D1,D0` (`$B001`) sets Z on equality and stores nothing: `Dn - <ea>`
    /// is computed for its flags and thrown away (M68000UM section 8, "CMP";
    /// M1-12, #20).
    #[test]
    fn cmp_equal_sets_z() {
        let (mut cpu, mut bus) = run(0xB001, &[0x4E71]);
        cpu.d[0] = 0xDEAD_00FF;
        cpu.d[1] = 0xCAFE_00FF;
        cpu.set_ccr(SR_C | SR_V | SR_N);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_00FF, "CMP never writes its destination");
        assert_eq!(cpu.d[1], 0xCAFE_00FF, "nor its source");
        assert!(cpu.flag_z(), "the difference is zero");
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_c(), "and equal operands do not borrow");
        assert!(!cpu.flag_v());
        assert!(!cpu.flag_x(), "X was clear, and a compare does not set it");
    }

    /// `CMP.W D1,D0` (`$B041`) sets C on a borrow and leaves X exactly as it was
    /// in *both* directions: a compare is a subtraction whose result is discarded,
    /// so the copy into X that `sub_flags` does for SUB must not happen (M68000UM
    /// "CMP": "X is unaffected"; M1-12, #20).
    #[test]
    fn cmp_borrow_sets_c_not_x() {
        let (mut cpu, mut bus) = run(0xB041, &[0x4E71]);
        cpu.d[0] = 0x0000_0000;
        cpu.d[1] = 0x0000_0001;

        cpu.step(&mut bus);

        assert!(cpu.flag_c(), "0 - 1 borrows");
        assert!(cpu.flag_n(), "the difference is $FFFF");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v(), "two positives cannot overflow");
        assert!(
            !cpu.flag_x(),
            "and X stays clear rather than taking the borrow"
        );

        let (mut cpu, mut bus) = run(0xB041, &[0x4E71]);
        cpu.d[0] = 0x0000_0001;
        cpu.d[1] = 0x0000_0000;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert!(!cpu.flag_c(), "1 - 0 does not borrow");
        assert!(cpu.flag_x(), "so a set X is not cleared either");
    }

    /// `CMPA.W D0,A0` (`$B0C0`) sign-extends the word source and compares it
    /// against the whole 32-bit address register, not just its low word, and
    /// leaves X alone (M68000UM "CMPA": "Word operation ... the source operand is
    /// extended to 32 bits ... sign extended"; M1-12, #20).
    #[test]
    fn cmpa_w_sign_extends() {
        let (mut cpu, mut bus) = run(0xB0C0, &[0x4E71]);
        cpu.a[0] = 0xFFFF_FFFF;
        cpu.d[0] = 0xDEAD_FFFF;

        cpu.step(&mut bus);

        assert!(cpu.flag_z(), "$FFFF sign extends to -1, which is A0");
        assert!(!cpu.flag_c(), "no borrow");
        assert!(!cpu.flag_v());

        let (mut cpu, mut bus) = run(0xB0C0, &[0x4E71]);
        cpu.a[0] = 0x8000_0000;
        cpu.d[0] = 0xDEAD_FFFF;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert!(!cpu.flag_z(), "the compare is 32 bits wide, not 16");
        assert!(cpu.flag_n(), "$80000001 is negative");
        assert!(cpu.flag_c(), "and does not borrow");
        assert!(cpu.flag_x(), "X unaffected");
        assert_eq!(cpu.a[0], 0x8000_0000, "CMPA stores nothing either");
    }

    /// `CMPM.B (A1)+,(A0)` (`$B109`) compares `(Ax) - (Ay)` and steps **both**
    /// pointers by one byte, writing nothing back; `(Ay)` is the source and is
    /// read first (M68000UM section 8, "CMPM"; measured in `CMP.b`; M1-12, #20).
    #[test]
    fn cmpm_increments_both() {
        let (mut cpu, mut bus) = run(0xB109, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.a[1] = MEM + 0x10;
        bus.mem[MEM as usize] = 0x01;
        bus.mem[MEM as usize + 0x10] = 0x02;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], MEM + 1, "Ax stepped by one byte");
        assert_eq!(cpu.a[1], MEM + 0x11, "and Ay stepped by one byte too");
        assert!(cpu.flag_c(), "$01 - $02 borrows, so it is (Ax) - (Ay)");
        assert!(cpu.flag_n(), "the difference is $FF");
        assert!(!cpu.flag_z());
        assert!(cpu.flag_x(), "X unaffected");
        assert_eq!(bus.mem[MEM as usize], 0x01, "CMPM stores nothing");
        assert_eq!(bus.mem[MEM as usize + 0x10], 0x02);
    }

    /// `CMPM.B (A0)+,(A0)` (`$B108`) is #19's `-(An)` test from the other side:
    /// each pointer is resolved exactly once per operand, so the two bytes
    /// compared are consecutive and `A0` finishes two ahead, and `(Ay)` is read
    /// first, which the borrow can see (M68000UM "CMPM", addressing modes).
    #[test]
    fn cmpm_same_register_reads_consecutive_bytes_in_order() {
        let (mut cpu, mut bus) = run(0xB108, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.mem[MEM as usize] = 0x01; // (Ay) = (A0), read first
        bus.mem[MEM as usize + 1] = 0x02; // (Ax) = (A0) one byte along

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], MEM + 2, "one step per operand, no double read");
        assert!(!cpu.flag_c(), "$02 - $01 does not borrow");
        assert!(!cpu.flag_z(), "and the difference is not zero");
    }

    /// `CMPI.L #imm,($2000).W` (`$0CB8`): two extension words for the immediate,
    /// high word first, *before* the destination's own address word — the same
    /// order `SUBI` uses, so the words are deliberately asymmetric and the
    /// comparison only comes out equal if they were read in the right order
    /// (M68000UM "CMPI"; M1-12, #20).
    #[test]
    fn cmpi_l_reads_its_immediate_before_the_destination() {
        let (mut cpu, mut bus) = run(0x0CB8, &[0x1234, 0x5678, MEM as u16, 0x4E71]);
        bus.write_long(MEM, 0x1234_5678);
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert!(cpu.flag_z(), "immediate and destination are equal");
        assert!(!cpu.flag_c());
        assert!(cpu.flag_x(), "X unaffected");
        assert_eq!(bus.read_long(MEM), 0x1234_5678, "CMPI stores nothing");
        assert_eq!(cpu.pc, PROGRAM + 8, "opcode + 3 extension words");
    }

    /// `TST.W D0` (`$4A40`) sets N and Z from the operand and clears V and C,
    /// leaving X alone (M68000UM "TST": N and Z are set according to the operand,
    /// V and C are cleared, X is unaffected; M1-12, #20).
    #[test]
    fn tst_clears_v_c() {
        let (mut cpu, mut bus) = run(0x4A40, &[0x4E71]);
        cpu.d[0] = 0xDEAD_0000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert!(cpu.flag_z(), "the low word is zero");
        assert!(
            !cpu.flag_n(),
            "N comes from the word, not the whole register"
        );
        assert!(!cpu.flag_v(), "V is cleared");
        assert!(!cpu.flag_c(), "C is cleared");
        assert!(cpu.flag_x(), "X is unaffected");
        assert_eq!(cpu.d[0], 0xDEAD_0000, "TST does not write");

        let (mut cpu, mut bus) = run(0x4A40, &[0x4E71]);
        cpu.d[0] = 0xDEAD_8000;

        cpu.step(&mut bus);

        assert!(cpu.flag_n(), "N comes from the operand at its own size");
        assert!(!cpu.flag_z());
    }

    /// `TST.B (A0)` (`$4A10`) tests one byte and still reads it: `$80` sets N even
    /// though the other bytes of the bus entry are zero (M68000UM "TST").
    #[test]
    fn tst_b_memory_tests_one_byte() {
        let (mut cpu, mut bus) = run(0x4A10, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.mem[MEM as usize] = 0x80;
        cpu.set_ccr(SR_C | SR_V);

        cpu.step(&mut bus);

        assert!(cpu.flag_n(), "the byte is negative");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_c());
        assert!(!cpu.flag_v());
        assert_eq!(cpu.a[0], MEM, "no pointer movement for a plain (An)");
    }

    /// `CHK.W Dd,<ea>` traps with vector 6 and `N = 1` when `Dd.w` is negative.
    ///
    /// The negative case comes first and is independent of the bound: M68000UM
    /// "CHK — Check Data Register Against Upper Bound" makes the test
    /// `Dd.w < 0 || Dd.w > <ea>.w`, so a negative operand traps against a
    /// negative bound it is arithmetically *below* (``-1 > -10`` is false in
    /// unsigned but the signed test still fires on the sign bit alone). The
    /// frame carries the address of the *next* instruction, so the CHK is not
    /// restarted, and only `N` is pinned on this path (#35).
    #[test]
    fn chk_negative_traps_with_n() {
        // `CHK.W D1,D0` (`0100 000 110 000 001` = `$4181`): D0.w = `$FFFF` = -1,
        // D1.w = 10, and the CHK traps on the sign bit before the bound matters.
        let (mut cpu, mut bus) = run(0x4181, &[0x4E71]);
        cpu.d[0] = 0x1234_FFFF;
        cpu.d[1] = 0x0000_000A;
        cpu.set_ccr(SR_Z | SR_V | SR_C); // N clear: the trap has to set it
        let sp = cpu.a[7];

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_CHK), "vector 6");
        assert!(cpu.flag_n(), "N = 1 whenever the operand is negative");
        assert_eq!(cpu.d[0], 0x1234_FFFF, "CHK never writes its destination");
        // Frame: SR at the new `sp`, the pushed PC as a long at `sp + 2`, and the
        // PC is the *next* instruction, `$1000 + 2`.
        let frame = sp.wrapping_sub(6);
        assert_eq!(
            bus.read_long((frame + 2) & crate::bus::ADDR_MASK),
            PROGRAM + 2,
            "CHK is not restarted, so the frame carries the next instruction"
        );

        // An extension-word source must still push the address after the whole
        // instruction: `CHK.W (A0,D0.w),D1` is `$45AE` plus an index word, four
        // bytes wide, and `resolve_ea` has already stepped `pc` over it when the
        // trap is raised.
        let (mut cpu, mut bus) = run(0x43B0, &[0x0000, 0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0;
        cpu.d[1] = 0xFFFF_FFFF;
        bus.write_word(MEM, 0x0010);
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_CHK));
        let frame = sp.wrapping_sub(6);
        assert_eq!(
            bus.read_long((frame + 2) & crate::bus::ADDR_MASK),
            PROGRAM + 4,
            "one extension word counts towards the pushed PC"
        );
    }

    /// `CHK.W Dd,<ea>` traps with vector 6 and `N = 0` when `0 <= Dd.w > <ea>.w`.
    #[test]
    fn chk_over_bound_traps_without_n() {
        // `CHK.W D1,D0` with D0.w = 5 and D1.w = 3: in range for unsigned,
        // over the bound for signed, and N must come out clear even though the
        // suite hands the case over with every flag set.
        let (mut cpu, mut bus) = run(0x4181, &[0x4E71]);
        cpu.d[0] = 0xABCD_0005;
        cpu.d[1] = 0x0000_0003;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, Some(EXC_CHK));
        assert!(!cpu.flag_n(), "N = 0 on the over-bound path");
        assert_eq!(cpu.d[0], 0xABCD_0005, "destination untouched");

        // Equality is *inside* the bound: `Dd.w > <ea>.w` is strict, so 3 vs 3
        // falls through to the no-trap path rather than trapping.
        let (mut cpu, mut bus) = run(0x4181, &[0x4E71]);
        cpu.d[0] = 3;
        cpu.d[1] = 3;
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, None, "Dn.w == bound is in range");
    }

    /// `CHK.W Dd,<ea>` with `0 <= Dd.w <= <ea>.w` does not trap, advances, and
    /// leaves the condition codes as `CMP` would: Z and V are "undefined" in
    /// M68000UM but deterministic in the data, and the pinned suite accepts the
    /// CMP values (checked against `CHK.json.gz`), so this uses
    /// [`M68k::sub_flags_no_x`] — `X` is unaffected.
    #[test]
    fn chk_in_range_no_trap() {
        // `CHK.W D1,D0` with D0.w = 2, D1.w = 10: no trap. The flags are *not*
        // those of `2 - 10`: `Z`, `V` and `C` come out clear and `N` keeps what it
        // came in with, which is what CHK.json.gz measures (its in-range cases want
        // `0x00` from an incoming `V`, and `0x18` from an incoming `N | Z`). Both
        // are asserted below: an incoming `N` survives, `Z`/`V`/`C` do not, and `X`
        // is never touched. The plain-CMP reading and the plain-do-nothing reading
        // each get one half of this wrong.
        let (mut cpu, mut bus) = run(0x4181, &[0x4E71]);
        cpu.d[0] = 0x0000_0002;
        cpu.d[1] = 0x0000_000A;
        cpu.set_ccr(SR_X | SR_N | SR_Z | SR_V | SR_C);

        cpu.step(&mut bus);

        assert_eq!(cpu.pending_exception, None, "in range: no CHK exception");
        assert_eq!(cpu.pc, PROGRAM + 2, "the instruction completed");
        assert_eq!(cpu.d[0], 2, "CHK never writes Dn");
        assert!(cpu.flag_n(), "N is left alone, set or clear");
        assert!(!cpu.flag_c(), "C is cleared even though nothing trapped");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v());
        assert!(cpu.flag_x(), "X is never touched by CHK");

        // `CHK.W (A0),D0` (`$4190`) reads the bound from memory, word only.
        let (mut cpu, mut bus) = run(0x4190, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.write_word(MEM, 0xFFF0); // -16 as a signed bound
        cpu.d[0] = 0xFFFF_FFFF; // -1: negative, so it traps either way
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_CHK));
        assert_eq!(cpu.a[0], MEM, "a plain (An) does not move");
    }

    /// `TAS.B <ea>` (`$4AC0 | <ea>`) reads the byte, sets N and Z from **that
    /// byte as read**, clears V and C, leaves X alone, and writes the byte back
    /// with bit 7 set. Both halves matter: the flags come from the old value and
    /// the store is the point of the instruction (M68000UM "TAS — Test and Set":
    /// "N and Z are set according to the operand value ... V and C are cleared;
    /// X is unaffected"; #35).
    #[test]
    fn tas_sets_bit7_and_flags() {
        // `TAS.B D0` on `$7F`: the write-back is `$FF`, so N is set — and it is
        // set from the operand's own bit 7 being cleared, which is why this case
        // is the one that separates "flags from the value read" from "flags
        // from the value stored".
        let (mut cpu, mut bus) = run(0x4AC0, &[0x4E71]);
        cpu.d[0] = 0xDEAD_007F;
        cpu.set_ccr(SR_Z | SR_V | SR_C); // X clear, and it must stay clear

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_00FF, "bit 7 set, upper 24 bits kept");
        assert!(
            !cpu.flag_n(),
            "N comes from the `$7F` that was read, not the `$FF` that was stored"
        );
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v(), "V is cleared");
        assert!(!cpu.flag_c(), "C is cleared");
        assert!(!cpu.flag_x(), "X is unaffected");
        assert_eq!(cpu.pc, PROGRAM + 2, "one opcode word, no extension");

        // `TAS.B D0` on zero: Z is set, from the value *read*, even though the
        // value stored back is `$80` — which is not zero and not negative as a
        // byte would make N come out set.
        let (mut cpu, mut bus) = run(0x4AC0, &[0x4E71]);
        cpu.d[0] = 0xDEAD_FF00;
        cpu.set_ccr(SR_X | SR_V | SR_C);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_FF80);
        assert!(cpu.flag_z(), "Z comes from the operand, not the result");
        assert!(cpu.flag_x(), "X is unaffected, so it stays set here");
        assert!(!cpu.flag_n(), "N too comes from the operand");

        // `TAS.B (A0)` (`$4AD0`): the byte really is read and really is written
        // back, at the same address — a read-modify-write, not a store of a
        // computed value over a stale read. `$4AC8` is not this instruction: mode
        // `001` is An direct, which `classify_tas` sends to `Op::Illegal`.
        let (mut cpu, mut bus) = run(0x4AD0, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.write_word(MEM, 0x1234);
        cpu.set_ccr(SR_Z | SR_V | SR_C);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_word(MEM),
            0x9234,
            "the byte at MEM — the high one, big-endian — is set; the other is left alone"
        );
        assert!(
            !cpu.flag_n(),
            "N comes from the `$12` read, not the `$92` stored"
        );
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_c() && !cpu.flag_v());
        assert_eq!(cpu.a[0], MEM, "a plain (An) does not move");

        // `TAS.B (A0)+` (`$4AD8`) steps the pointer exactly once: one byte read
        // and the same byte written, so `A0` ends one past the byte, not two.
        let (mut cpu, mut bus) = run(0x4AD8, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.write_word(MEM, 0x0102);

        cpu.step(&mut bus);

        assert_eq!(bus.read_word(MEM), 0x8102, "one byte set, one byte left");
        assert_eq!(cpu.a[0], MEM + 1, "A0 moved once");
    }

    /// Every condition code the `NEG`/`CLR` tests below start from, so a rule
    /// about "cleared" cannot pass by the flag having been clear all along.
    const NEGC: u16 = SR_N | SR_Z | SR_V | SR_C;

    /// `NEG.B D0` of zero: `0 - 0` borrows nothing, so `C` and `X` are clear and
    /// `Z` is set — the one operand for which NEG clears the carry (M68000UM
    /// "NEG": "C is cleared if the source operand is 0, set otherwise"; #22).
    #[test]
    fn neg_zero_clears_c() {
        let (mut cpu, mut bus) = run(0x4400, &[0x4E71]);
        cpu.d[0] = 0;
        cpu.set_ccr(NEGC);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0);
        assert!(cpu.flag_z());
        assert!(!cpu.flag_c(), "zero negated is the only case with no carry");
        assert!(!cpu.flag_x(), "NEG sets X from the carry");
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_v());

        let (mut cpu, mut bus) = run(0x4400, &[0x4E71]);
        cpu.d[0] = 1;
        cpu.set_ccr(0);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0xFF);
        assert!(cpu.flag_c(), "and any other operand borrows");
        assert!(cpu.flag_x());
    }

    /// `NEG` of the most negative value of each size overflows: the result is
    /// unchanged, `V` is set, and `C` is set because zero was still borrowed from
    /// (M68000UM "NEG": "V is set if the operand was $80, $8000 or $80000000";
    /// the `D7` byte/word/long triple of `NEG.b`/`NEG.w`/`NEG.l`).
    #[test]
    fn neg_min_sets_v() {
        for (opcode, value, mask, name) in [
            (0x4400u16, 0x80u32, 0xFFu32, "NEG.B $80"),
            (0x4440, 0x8000, 0xFFFF, "NEG.W $8000"),
            (0x4480, 0x8000_0000, 0xFFFF_FFFF, "NEG.L $80000000"),
        ] {
            let (mut cpu, mut bus) = run(opcode, &[0x4E71]);
            cpu.d[0] = value;
            cpu.set_ccr(0);

            cpu.step(&mut bus);

            assert_eq!(cpu.d[0] & mask, value & mask, "{name}: unchanged");
            assert!(cpu.flag_v(), "{name}: V set");
            assert!(cpu.flag_c(), "{name}: C set, zero minus a negative carries");
            assert!(cpu.flag_n(), "{name}: the result is still negative");
            assert!(!cpu.flag_z());

            // The same test without the overflow, one less than the boundary.
            let (mut cpu, mut bus) = run(opcode, &[0x4E71]);
            cpu.d[0] = value - 1;
            cpu.set_ccr(0);

            cpu.step(&mut bus);

            assert!(!cpu.flag_v(), "{name} minus one: no overflow");
        }
    }

    /// `NEGX` is `SUBX` with the subtrahend only: `Z` is sticky (set only by the
    /// incoming X and result, never *cleared* by a zero result), and X is a third
    /// subtrahend rather than a carry-in (M68000UM "NEGX" and "SUBX"; #22).
    #[test]
    fn negx_z_sticky() {
        // `$FF - X` with X set is zero: Z was clear, and a zero result must leave
        // it clear. `NEGX.B D0` is `$4000`; D0 is `$FF`, X is set.
        let (mut cpu, mut bus) = run(0x4000, &[0x4E71]);
        cpu.d[0] = 0xFF;
        cpu.set_ccr(SR_X);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x00);
        assert!(!cpu.flag_z(), "a zero result leaves a clear Z alone");
        assert!(cpu.flag_c(), "0 - $FF - X borrows");
        assert!(cpu.flag_x());

        // ... and with Z set going in, the same zero result leaves it set.
        let (mut cpu, mut bus) = run(0x4000, &[0x4E71]);
        cpu.d[0] = 0xFF;
        cpu.set_ccr(SR_X | SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x00);
        assert!(cpu.flag_z(), "a zero result leaves a set Z alone");

        // A non-zero result clears Z, which is what makes the stickiness a chain
        // rule rather than "NEGX never touches Z".
        let (mut cpu, mut bus) = run(0x4000, &[0x4E71]);
        cpu.d[0] = 0;
        cpu.set_ccr(SR_Z);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0xFF, 0x00);
        assert!(cpu.flag_z(), "0 - 0 - 0 is zero, so Z stays as it was");
    }

    /// `CLR.W D0`: `Z` set, `N`/`V`/`C` cleared, `X` untouched (M68000PM "CLR";
    /// #22). `CLR.B` of the byte that was $FF is `CLR.b`'s first measured case.
    #[test]
    fn clr_w_sets_z_keeps_x() {
        let (mut cpu, mut bus) = run(0x4243, &[0x4E71]);
        cpu.d[3] = 0x1234_5678;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[3], 0x1234_0000, "only the low word is cleared");
        assert!(cpu.flag_z());
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_v());
        assert!(!cpu.flag_c());
        assert!(cpu.flag_x(), "CLR leaves X alone");
    }

    /// The 68000 reads a memory destination before it writes `CLR`'s zero
    /// (M68000PM "CLR", which lists the 68020's read-free form as a later
    /// difference). Final state cannot show the difference — a read has no
    /// architectural effect — so this is the one test here that inspects the bus
    /// rather than the CPU, through the recording bus below.
    #[test]
    fn clr_to_memory_reads_then_writes() {
        let mut bus = RecordingBus::new();
        bus.load(PROGRAM, &[0x4258u16, 0x4E71] /* CLR.W (A0)+, NOP */);
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        bus.accesses.clear();
        cpu.a[0] = MEM;
        bus.poke(MEM, 0xBEEF);

        cpu.step(&mut bus);

        // The access after these two is the prefetch of the word at $1002, which
        // is why this compares a prefix rather than the whole log.
        assert_eq!(
            &bus.accesses[..2],
            [Access::Read(MEM), Access::Write(MEM)],
            "CLR.W (A0)+ reads the resolved word and writes it back once, \
             at the same address, in that order"
        );
        assert_eq!(bus.peek(MEM), 0, "the write really is zero");
    }

    /// `NEG` and `NEGX` are read-modify-writes too: `(An)+` steps exactly once and
    /// `-(An)` steps exactly once, because the operand is resolved once and both
    /// halves use the same [`Ea`] (M68000UM "NEG"; #22, and the same property
    /// `logic_to_ea` pins for AND and OR).
    #[test]
    fn neg_to_memory_moves_the_pointer_once() {
        let (mut cpu, mut bus) = run(0x4458, &[0x4E71]); // NEG.W (A0)+
        cpu.a[0] = MEM;
        bus.mem[MEM as usize..MEM as usize + 2].copy_from_slice(&0x0002u16.to_be_bytes());

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], MEM + 2, "one step, not two");
        let stored = u16::from_be_bytes([bus.mem[MEM as usize], bus.mem[MEM as usize + 1]]);
        assert_eq!(stored, 0xFFFE);

        let (mut cpu, mut bus) = run(0x4060, &[0x4E71]); // NEGX.W -(A0)
        cpu.a[0] = MEM + 4;
        bus.mem[(MEM + 2) as usize..(MEM + 4) as usize].copy_from_slice(&0x0001u16.to_be_bytes());

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], MEM + 2, "predecrement happens once");
        let stored = u16::from_be_bytes([bus.mem[(MEM + 2) as usize], bus.mem[(MEM + 3) as usize]]);
        assert_eq!(stored, 0xFFFF, "0 - 1 - X, X clear");
    }

    /// What [`RecordingBus`] records: one bus access, in the order the CPU made
    /// it. `Read` is a read of the bus, `Write` a write, and the address is what
    /// the CPU asked for before masking.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Access {
        Read(u32),
        Write(u32),
    }

    /// A bus that keeps the addresses it was asked about, which
    /// [`crate::testbus::TestBus`] deliberately does not: the only way to see a
    /// read that throws its value away is to count it (see
    /// `cpu::tests::Ram64k`, the same device for the same reason; #22).
    struct RecordingBus {
        mem: Vec<u8>,
        accesses: Vec<Access>,
    }

    impl RecordingBus {
        fn new() -> Self {
            Self {
                mem: vec![0; 1 << 20],
                accesses: Vec::new(),
            }
        }

        /// The reset vector the CPU needs: SSP at $000000 and the program at
        /// [`PROGRAM`].
        fn load(&mut self, addr: u32, words: &[u16]) {
            self.poke_long(0, 0x0000_F000);
            self.poke_long(4, PROGRAM);
            for (n, word) in words.iter().enumerate() {
                let where_ = addr + 2 * n as u32;
                self.mem[where_ as usize] = (word >> 8) as u8;
                self.mem[where_ as usize + 1] = *word as u8;
            }
        }

        fn poke(&mut self, addr: u32, value: u16) {
            self.mem[addr as usize] = (value >> 8) as u8;
            self.mem[addr as usize + 1] = value as u8;
        }

        fn poke_long(&mut self, addr: u32, value: u32) {
            self.mem[addr as usize..addr as usize + 4].copy_from_slice(&value.to_be_bytes());
        }

        fn peek(&self, addr: u32) -> u16 {
            u16::from_be_bytes([self.mem[addr as usize], self.mem[addr as usize + 1]])
        }
    }

    impl Bus for RecordingBus {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.accesses.push(Access::Read(addr));
            self.mem[addr as usize]
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.accesses.push(Access::Read(addr));
            let i = addr as usize;
            u16::from_be_bytes([self.mem[i], self.mem[i + 1]])
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.accesses.push(Access::Write(addr));
            self.mem[addr as usize] = value;
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.accesses.push(Access::Write(addr));
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }
    }
}
