//! Textual disassembler for debug logging (M1-33, #43).
//!
//! [`disassemble`] turns the words at an address into one line of Motorola
//! source — `MOVE.W D1,D0`, `BNE.S $001004` — the way the manual writes them,
//! without running the instruction and without a bus. It asks [`crate::decode`]
//! what the opcode is and formats what the decoder already knows; an opcode the
//! decoder does not implement has no operands to name, so it prints as data
//! (`DC.W $FFFF`) and occupies one word.
//!
//! # Syntax
//!
//! Numbers are hexadecimal and `$`-prefixed. The two widths the ticket's
//! examples leave open are settled here:
//!
//! * immediates, displacements and bit numbers carry no leading zeros
//!   (`MOVEQ #$1,D0`, `ADDQ.L #$8,A0`, `$10(PC)`);
//! * addresses are padded to the field that holds them — four digits for an
//!   absolute word, six for an absolute long or a branch target
//!   (`MOVE.L $012345,D0`, `JSR $012345`).
//!
//! Branch and `DBcc` targets are computed (`pc + 2 + displacement`, since a
//! 68000 displacement is relative to the *following* instruction), `.S` for a
//! byte displacement and `.W` for a displacement word. Every other
//! displacement is printed as the unsigned word the encoding holds, and
//! `d16(PC)` names the displacement rather than the address it points at:
//! neither can become a symbol from here, and a displacement is what you
//! actually want to see when stepping.
//!
//! # Truncated input
//!
//! A debugger logs from memory it may not have filled in yet, so a slice that
//! stops before the instruction does is not an error: a missing extension word
//! reads as `0` so the text stays complete, and the word count never claims
//! more than the caller handed over. Nothing in here panics, and the caller's
//! own PC arithmetic stays right either way.

use crate::bus::ADDR_MASK;
use crate::decode::{self, Op};
use crate::ea::Size;

/// Disassemble the instruction at `words[0]`, as though it were at `pc`.
///
/// Returns the text and the number of words the instruction occupies: one for
/// `NOP`, three for `JSR $123456`. `pc` is only used for the branch-family
/// target, which is where it looks like a jump rather than an offset.
///
/// ```text
/// MOVEQ #$1,D0          MOVE.L $012345,D0     MOVEM.L D0-D1/A0,-(A7)
/// BNE.S $001004         JSR $012345            MOVEM.W $10(A2),D0/A2
/// ```
pub fn disassemble(words: &[u16], pc: u32) -> (String, usize) {
    let Some(&opcode) = words.first() else {
        return (data(0), 1);
    };
    let mut r = Reader { words, next: 1, pc };
    let text = format_op(&mut r, opcode);
    // An instruction reads exactly the words its operands name (see
    // `Reader`), which is how execution lengths itself too: `Cpu::fetch_ext_word`
    // / `fetch_ext_long` are called once per extension word and no further.
    (text, r.next.min(words.len()).max(1))
}

/// One word the disassembler has no instruction for.
fn data(word: u16) -> String {
    format!("DC.W ${word:04X}")
}

/// The extension-word cursor: a format reads the words its operands name, in
/// encoding order, and stops when the caller's slice does.
///
/// Counting this way rather than from a table is what keeps the length honest
/// — a format that names an operand it does not have reads a word it should
/// not, and the test suite's word-count checks catch it.
struct Reader<'a> {
    words: &'a [u16],
    next: usize,
    pc: u32,
}

impl Reader<'_> {
    /// The next word, or `0` where the slice stopped before it.
    fn word(&mut self) -> u16 {
        let word = self.words.get(self.next).copied().unwrap_or(0);
        self.next += 1;
        word
    }

    /// A long as the two words the encoding carries it in: high half first.
    fn long(&mut self) -> u32 {
        (u32::from(self.word()) << 16) | u32::from(self.word())
    }

    /// Where a branch with signed displacement `d` lands: relative to the
    /// instruction after this one, and 24-bit like every other 68000 address.
    fn target(&self, d: i16) -> String {
        let at = self.pc.wrapping_add(2).wrapping_add(d as u32) & ADDR_MASK;
        format!("${at:06X}")
    }
}

/// The three-bit field of `opcode` starting at `shift`.
const fn field(opcode: u16, shift: u32) -> u8 {
    ((opcode >> shift) & 0b111) as u8
}

/// `D0`-`D7`.
fn dn(n: u8) -> String {
    format!("D{n}")
}

/// `A0`-`A7`.
fn an(n: u8) -> String {
    format!("A{n}")
}

/// A `$`-prefixed, upper-case number with no leading zeros.
fn num(value: u32) -> String {
    format!("${value:X}")
}

/// `.B`/`.W`/`.L` — empty for an opcode whose size field is the invalid `11`,
/// and for the instructions that carry no size at all.
fn size_suffix(size: Option<Size>) -> &'static str {
    match size {
        Some(Size::Byte) => ".B",
        Some(Size::Word) => ".W",
        Some(Size::Long) => ".L",
        None => "",
    }
}

/// The size of a `MOVE`/`MOVEA`/`MOVEQ`, whose field is numbered differently
/// from everybody else's (see [`Size::from_move_bits`]).
fn move_size(opcode: u16) -> Option<Size> {
    Size::from_move_bits((opcode >> 12) & 0b11)
}

/// The size of an ALU instruction: bits 7..6.
fn alu_size(opcode: u16) -> Option<Size> {
    Size::from_bits((opcode >> 6) & 0b11)
}

/// `#imm`: one word, two for a long, the low half of a word for a byte
/// (M68000UM, "Immediate"). `size` is `None` where a `#` operand cannot appear
/// at all, which reads one word like a word-sized immediate.
fn immediate(r: &mut Reader<'_>, size: Option<Size>) -> String {
    let value = match size {
        Some(Size::Long) => r.long(),
        Some(Size::Byte) => u32::from(r.word() & 0x00FF),
        Some(Size::Word) | None => u32::from(r.word()),
    };
    format!("#{}", num(value))
}

/// The operand the six-bit `ea` field names, reading its extension words from
/// `r` (M68000UM, "Effective Addressing", Table 8-1).
fn ea_text(r: &mut Reader<'_>, ea: u8, size: Option<Size>) -> String {
    let mode = (ea >> 3) & 0b111;
    let reg = ea & 0b111;
    match (mode, reg) {
        (0b000, n) => dn(n),
        (0b001, n) => an(n),
        (0b010, n) => format!("({})", an(n)),
        (0b011, n) => format!("({})+", an(n)),
        (0b100, n) => format!("-({})", an(n)),
        (0b101, n) => format!("{},{}", num(u32::from(r.word())), an(n)),
        (0b110, n) => indexed(r, an(n)),
        // Absolute: a word for `.W` — which is what an unprefixed mode means —
        // and a long for `.L`.
        (0b111, 0b000) => format!("${:04X}", r.word()),
        (0b111, 0b001) => format!("${:06X}", r.long()),
        (0b111, 0b010) => format!("{},PC", num(u32::from(r.word()))),
        (0b111, 0b011) => indexed(r, String::from("PC")),
        (0b111, 0b100) => immediate(r, size),
        // Mode `111` with `reg` above `100` is not a mode at all and is never
        // decoded (`ea::ea_mode_valid`), so this is unreachable from a decoded
        // opcode.
        _ => String::from("???"),
    }
}

/// `d8(An,Xn)` and `d8(PC,Xn)`: one brief word holding both the displacement
/// and the index (M68000UM, "Index Extended").
fn indexed(r: &mut Reader<'_>, base: String) -> String {
    let brief = r.word();
    let d8 = num(u32::from(brief & 0xFF));
    format!("{d8},{base}({})", index(brief))
}

/// The index named by a brief extended word: bit 15 selects `An` over `Dn`,
/// bits 14..12 the number, and bit 11 the width (`ea::indexed_addr`).
fn index(brief: u16) -> String {
    let bank = if brief & 0x8000 == 0 { "D" } else { "A" };
    let width = if brief & 0x0800 == 0 { "W" } else { "L" };
    format!("{bank}{}.{width}", field(brief, 12))
}

/// Condition-code names, indexed by the `cccc` field: `0000` is `T`, `1111`
/// `LE` (M68000UM, "Condition Codes", Table 3-4).
const CC: [&str; 16] = [
    "T", "F", "HI", "LS", "CC", "CS", "NE", "EQ", "VC", "VS", "PL", "MI", "GE", "LT", "GT", "LE",
];

/// The four bit instructions, selected by `tt`.
const BIT: [&str; 4] = ["BTST", "BCHG", "BCLR", "BSET"];

/// Shift families, selected by `tt`, suffixed with the direction: `ASL`, `LSR`,
/// `ROXL`, `ROR` (`ops::shift::Kind`).
const SHIFT: [&str; 4] = ["AS", "LS", "ROX", "RO"];

/// The mnemonic, operands and length of one instruction.
fn format_op(r: &mut Reader<'_>, opcode: u16) -> String {
    let ea = (opcode & 0x3F) as u8;
    // The `xxx` field, which names a destination for most of the ALU group and
    // the register for the move-to-an-address-register formats.
    let dx = field(opcode, 9);
    // The MOVE-class destination: mode in bits 8..6, register in 11..9.
    let dst = (field(opcode, 6) << 3) | dx;

    match decode::decode(opcode) {
        // Nothing to name: print the word and step over it.
        Op::Illegal | Op::Unimplemented | Op::LineA | Op::LineF => data(opcode),

        Op::Nop => String::from("NOP"),
        Op::Reset => String::from("RESET"),
        Op::Rte => String::from("RTE"),
        Op::Rts => String::from("RTS"),
        Op::Rtr => String::from("RTR"),
        Op::Trapv => String::from("TRAPV"),

        Op::Trap => format!("TRAP #{}", num(u32::from(opcode & 0xF))),
        Op::Stop => format!("STOP {}", immediate(r, Some(Size::Word))),

        // --- moves ---
        Op::Move => {
            let size = move_size(opcode);
            let from = ea_text(r, ea, size);
            let to = ea_text(r, dst, size);
            format!("MOVE{} {from},{to}", size_suffix(size))
        }
        Op::Movea => {
            // A `MOVEA` immediate is a word however wide the instruction is
            // (M68000UM MOVEA: it is sign-extended to the long form).
            let from = ea_text(r, ea, Some(Size::Word));
            format!("MOVEA{} {from},{}", size_suffix(move_size(opcode)), an(dx))
        }
        Op::Moveq => {
            let imm = num(u32::from(opcode & 0xFF));
            format!("MOVEQ {imm},{}", dn(dx))
        }
        Op::MoveFromSr => {
            let to = ea_text(r, ea, Some(Size::Word));
            format!("MOVE.W SR,{to}")
        }
        Op::MoveToCcr => {
            let from = ea_text(r, ea, Some(Size::Byte));
            format!("MOVE.B {from},CCR")
        }
        Op::MoveToSr => {
            let from = ea_text(r, ea, Some(Size::Word));
            format!("MOVE.W {from},SR")
        }
        Op::MoveUsp => {
            let a = an(field(opcode, 0));
            if opcode & 0b1000 == 0 {
                format!("MOVE {a},USP")
            } else {
                format!("MOVE USP,{a}")
            }
        }
        Op::Movep => movep(r, opcode),

        // --- ALU ---
        op @ (Op::Add | Op::Sub | Op::And | Op::Or | Op::Eor) => alu(r, opcode, op, ea, dx),
        op @ (Op::Adda | Op::Suba | Op::Cmpa) => adda(r, opcode, op, ea),
        Op::Cmp => {
            let size = alu_size(opcode);
            let from = ea_text(r, ea, size);
            format!("CMP{} {from},{}", size_suffix(size), dn(dx))
        }
        op @ (Op::Addx | Op::Subx) => {
            double(opcode, if op == Op::Addx { "ADDX" } else { "SUBX" })
        }
        Op::Cmpm => {
            let size = size_suffix(alu_size(opcode));
            format!("CMPM{size} ({})+,({})+", an(field(opcode, 0)), an(dx))
        }
        op @ (Op::Addi | Op::Subi | Op::Andi | Op::Ori | Op::Eori | Op::Cmpi) => {
            immediate_alu(r, opcode, op, ea)
        }
        Op::LogicImmCcr => logic_imm(r, opcode, "CCR"),
        Op::LogicImmSr => logic_imm(r, opcode, "SR"),
        op @ (Op::Addq | Op::Subq) => quick(op, opcode),
        op @ (Op::Neg | Op::Negx | Op::Clr | Op::Not | Op::Tst | Op::Tas | Op::Chk) => {
            unary(r, opcode, op, ea)
        }
        op @ (Op::Abcd | Op::Sbcd) => {
            let name = if op == Op::Abcd { "ABCD" } else { "SBCD" };
            if opcode & 0b1000 == 0 {
                format!("{name} {},{}", dn(field(opcode, 0)), dn(dx))
            } else {
                format!("{name} -({}),-({})", an(field(opcode, 0)), an(dx))
            }
        }
        Op::Nbcd => format!("NBCD {}", ea_text(r, ea, Some(Size::Byte))),

        // --- bit manipulations ---
        Op::BitDynamic => {
            let bit = dn(dx);
            format!(
                "{} {bit},{}",
                BIT[bit_kind(opcode)],
                ea_text(r, ea, Some(Size::Word))
            )
        }
        Op::BitStatic => {
            let bit = immediate(r, Some(Size::Word));
            format!(
                "{} {bit},{}",
                BIT[bit_kind(opcode)],
                ea_text(r, ea, Some(Size::Word))
            )
        }
        Op::Swap => format!("SWAP {}", dn(field(opcode, 0))),
        Op::Ext => {
            let size = if opcode & 0b100_0000 == 0 { ".W" } else { ".L" };
            format!("EXT{size} {}", dn(field(opcode, 0)))
        }
        Op::Exg => {
            // Operation `101` at mode `000` is `Dx,Dy`, at `001` it is `Ax,Ay`,
            // and `110`+`001` is `Dx,Ay` (M68000UM EXG; `decode::is_exg`).
            let right = field(opcode, 0);
            let pair = match (field(opcode, 6), field(opcode, 3)) {
                (0b101, 0b001) => (an(dx), an(right)),
                (0b110, 0b001) => (dn(dx), an(right)),
                _ => (dn(dx), dn(right)),
            };
            format!("EXG {},{}", pair.0, pair.1)
        }
        op @ (Op::Mulu | Op::Muls | Op::Divu | Op::Divs) => {
            let name = match op {
                Op::Mulu => "MULU",
                Op::Muls => "MULS",
                Op::Divu => "DIVU",
                _ => "DIVS",
            };
            let from = ea_text(r, ea, Some(Size::Word));
            format!("{name}.W {from},{}", dn(dx))
        }

        // --- shifts ---
        Op::Shift => {
            let kind = SHIFT[usize::from((opcode >> 3) & 0b11)];
            let dir = direction(opcode);
            let count = field(opcode, 9);
            // A `000` field is not no shift, it is a full width of it; and the
            // count is in a data register when bit 5 is set.
            let n = if opcode & 0b10_0000 == 0 {
                num(u32::from(if count == 0 { 8 } else { count }))
            } else {
                dn(count)
            };
            format!(
                "{kind}{dir}{} {n},{}",
                size_suffix(alu_size(opcode)),
                dn(field(opcode, 0))
            )
        }
        Op::ShiftMemory => {
            let kind = SHIFT[usize::from((opcode >> 9) & 0b11)];
            let dir = direction(opcode);
            // Memory shifts are word wide, always (M68000UM ASL/ASR/LSL/LSR).
            format!("{kind}{dir}.W {}", ea_text(r, ea, Some(Size::Word)))
        }

        // --- branches, and the condition-code friends ---
        Op::Bcc => {
            let cc = CC[usize::from((opcode >> 8) & 0b1111)];
            // Condition `0000` is `BRA`, not `BT` (M68000UM Bcc/BRA/BSR).
            let name = if cc == "T" {
                String::from("BRA")
            } else {
                format!("B{cc}")
            };
            branch(r, opcode, &name)
        }
        Op::Bsr => branch(r, opcode, "BSR"),
        Op::Scc => {
            let cc = CC[usize::from((opcode >> 8) & 0b1111)];
            format!("S{cc} {}", ea_text(r, ea, Some(Size::Byte)))
        }
        Op::Dbcc => {
            let cc = CC[usize::from((opcode >> 8) & 0b1111)];
            let d = r.word() as i16;
            let at = r.target(d);
            format!("DB{cc} {},{at}", dn(field(opcode, 0)))
        }

        // --- jumps, and the address-taking handful ---
        Op::Jmp => format!("JMP {}", ea_text(r, ea, None)),
        Op::Jsr => format!("JSR {}", ea_text(r, ea, None)),
        Op::Lea => format!("LEA {},{}", ea_text(r, ea, None), an(dx)),
        Op::Pea => format!("PEA {}", ea_text(r, ea, None)),
        Op::Movem => movem(r, opcode),
        Op::Link => {
            let d = immediate(r, Some(Size::Word));
            format!("LINK {},{d}", an(field(opcode, 0)))
        }
        Op::Unlk => format!("UNLK {}", an(field(opcode, 0))),
    }
}

/// `MOVEP`: one displacement word, and bit 7 says which way the byte went.
///
/// The displacement is a plain offset off an address register, so it is read by
/// hand rather than through [`ea_text`] — the encoding puts `An` in the `reg`
/// field with no mode to name (M68000UM MOVEP).
fn movep(r: &mut Reader<'_>, opcode: u16) -> String {
    let size = if opcode & 0b100_0000 == 0 { ".W" } else { ".L" };
    let mem = format!("{},{}", num(u32::from(r.word())), an(field(opcode, 0)));
    let d = dn(field(opcode, 9));
    // Bit 7 set is `MOVEP Dx,d16(An)`; clear is the other direction.
    if opcode & 0b1000_0000 == 0 {
        format!("MOVEP{size} {mem},{d}")
    } else {
        format!("MOVEP{size} {d},{mem}")
    }
}

/// `ADD`/`SUB`/`AND`/`OR`/`EOR`: op mode `000`-`010` is `<ea>,Dn` and
/// `100`-`110` is `Dn,<ea>` (M68000UM, "Instruction Formats", Format IV).
fn alu(r: &mut Reader<'_>, opcode: u16, op: Op, ea: u8, dx: u8) -> String {
    let name = match op {
        Op::Add => "ADD",
        Op::Sub => "SUB",
        Op::And => "AND",
        Op::Or => "OR",
        _ => "EOR",
    };
    let size = alu_size(opcode);
    let suffix = size_suffix(size);
    let to_memory = field(opcode, 6) & 0b100 != 0;
    let (left, right) = if to_memory {
        (dn(dx), ea_text(r, ea, size))
    } else {
        (ea_text(r, ea, size), dn(dx))
    };
    format!("{name}{suffix} {left},{right}")
}

/// `ADDA`/`SUBA`/`CMPA`, printed the way the manual writes them — as `ADD`,
/// `SUB` and `CMP` with a size, and an address register destination.
fn adda(r: &mut Reader<'_>, opcode: u16, op: Op, ea: u8) -> String {
    let name = match op {
        Op::Adda => "ADD",
        Op::Suba => "SUB",
        _ => "CMP",
    };
    // Op mode `011` is the word form and `111` the long one, so bit 8 alone
    // tells them apart (M68000UM ADDA/SUBA/CMPA).
    let size = if opcode & 0x0100 == 0 {
        Size::Word
    } else {
        Size::Long
    };
    let from = ea_text(r, ea, Some(size));
    format!(
        "{name}{} {from},{}",
        size_suffix(Some(size)),
        an(field(opcode, 9))
    )
}

/// `ADDX`/`SUBX`: bit 3 set is `-(Ay),-(Ax)`, clear is `Dy,Dx`.
fn double(opcode: u16, name: &str) -> String {
    let suffix = size_suffix(alu_size(opcode));
    let (left, right) = if opcode & 0b1000 == 0 {
        (dn(field(opcode, 0)), dn(field(opcode, 9)))
    } else {
        (an(field(opcode, 0)), an(field(opcode, 9)))
    };
    format!("{name}{suffix} {left},{right}")
}

/// `ADDI`/`SUBI`/`ANDI`/`ORI`/`EORI`/`CMPI`: the immediate sits in the
/// extension words *before* any of the destination's words, so it is read first
/// even though it prints second-to-last.
fn immediate_alu(r: &mut Reader<'_>, opcode: u16, op: Op, ea: u8) -> String {
    let name = match op {
        Op::Addi => "ADD",
        Op::Subi => "SUB",
        Op::Andi => "AND",
        Op::Ori => "OR",
        Op::Eori => "EOR",
        _ => "CMP",
    };
    let size = alu_size(opcode);
    let imm = immediate(r, size);
    let to = ea_text(r, ea, size);
    format!("{name}{} {imm},{to}", size_suffix(size))
}

/// `ANDI.B #imm,CCR` and the rest of the `$003C` handful, where the destination
/// is a condition register rather than an effective address.
fn logic_imm(r: &mut Reader<'_>, opcode: u16, reg: &str) -> String {
    let name = match field(opcode, 9) {
        0b001 => "ANDI",
        0b101 => "EORI",
        _ => "ORI",
    };
    let size = alu_size(opcode);
    let imm = immediate(r, size);
    format!("{name}{} {imm},{reg}", size_suffix(size))
}

/// `ADDQ`/`SUBQ`: the data field is the immediate, and `000` means 8 rather
/// than nothing. A mode of `001` makes the destination an address register, and
/// it is the only difference from the ALU shape (M68000UM ADDQ).
fn quick(op: Op, opcode: u16) -> String {
    let name = if op == Op::Addq { "ADDQ" } else { "SUBQ" };
    let n = field(opcode, 9);
    let value = num(u32::from(if n == 0 { 8 } else { n }));
    let to = if field(opcode, 3) == 0b001 {
        an(field(opcode, 0))
    } else {
        dn(field(opcode, 0))
    };
    format!("{name}{} {value},{to}", size_suffix(alu_size(opcode)))
}

/// The one-operand ALU handful, plus `CHK` (which reads `Dn,<ea>`).
fn unary(r: &mut Reader<'_>, opcode: u16, op: Op, ea: u8) -> String {
    let (name, size) = match op {
        Op::Neg => ("NEG", alu_size(opcode)),
        Op::Negx => ("NEGX", alu_size(opcode)),
        Op::Clr => ("CLR", alu_size(opcode)),
        Op::Not => ("NOT", alu_size(opcode)),
        Op::Tst => ("TST", alu_size(opcode)),
        // `TAS` and `CHK` carry no size in the opcode: byte and word
        // respectively (M68000UM TAS, CHK).
        Op::Tas => ("TAS", None),
        _ => ("CHK", None),
    };
    let ea_size = match op {
        Op::Tas => Some(Size::Byte),
        Op::Chk => Some(Size::Word),
        _ => size,
    };
    let text = ea_text(r, ea, ea_size);
    if op == Op::Chk {
        format!("CHK.W {text},{}", dn(field(opcode, 9)))
    } else {
        format!("{name}{} {text}", size_suffix(size))
    }
}

/// `tt`, the two-bit selector the bit instructions share, at bits 7..6 of both
/// their forms.
fn bit_kind(opcode: u16) -> usize {
    usize::from((opcode >> 6) & 0b11)
}

/// `L` or `R`, from the direction bit every shift and rotate has at bit 8.
fn direction(opcode: u16) -> &'static str {
    if opcode & 0x0100 == 0 { "R" } else { "L" }
}

/// `Bcc`/`BRA`/`BSR`: the displacement is a byte, except that a zero byte means
/// a displacement word follows and the branch is `.W` (M68000UM Bcc).
fn branch(r: &mut Reader<'_>, opcode: u16, name: &str) -> String {
    let byte = (opcode & 0xFF) as u8;
    let (d, suffix) = if byte == 0 {
        (r.word() as i16, ".W")
    } else {
        (i16::from(byte as i8), ".S")
    };
    let at = r.target(d);
    format!("{name}{suffix} {at}")
}

/// `MOVEM`: the register-list word comes first, then the operand's own words.
///
/// A store through `-(An)` reads its list end for end, which is the one place
/// the printed list is not simply the mask read low to high
/// (`ops::movem::movem_list_ea`).
fn movem(r: &mut Reader<'_>, opcode: u16) -> String {
    let to_memory = opcode & 0x0400 != 0;
    let long = opcode & 0x0040 != 0;
    let list = r.word();
    let reversed = to_memory && field(opcode, 3) == 0b100;
    let regs = reg_list(list, reversed);
    let size = if long { Size::Long } else { Size::Word };
    let at = ea_text(r, (opcode & 0x3F) as u8, Some(size));
    let suffix = size_suffix(Some(size));
    if to_memory {
        format!("MOVEM{suffix} {regs},{at}")
    } else {
        format!("MOVEM{suffix} {at},{regs}")
    }
}

/// A `MOVEM` list word as `D0-D2/A0`: data registers then address registers,
/// consecutive registers collapsed into a run.
fn reg_list(mask: u16, reversed: bool) -> String {
    let mut d = Vec::new();
    let mut a = Vec::new();
    for bit in 0..16 {
        if mask & (1 << bit) == 0 {
            continue;
        }
        // Reversed: bit 0 is `A7` up to bit 7 `A0`, then bit 8 `D7` down to
        // bit 15 `D0`.
        if !reversed && bit < 8 {
            d.push(bit);
        } else if !reversed {
            a.push(bit - 8);
        } else if bit < 8 {
            a.push(7 - bit);
        } else {
            d.push(15 - bit);
        }
    }
    let mut out = String::new();
    if !d.is_empty() {
        runs(&mut out, 'D', &d);
    }
    if !a.is_empty() {
        if !out.is_empty() {
            out.push('/');
        }
        runs(&mut out, 'A', &a);
    }
    out
}

/// One bank's registers, appended as `0-2/5`.
fn runs(out: &mut String, bank: char, regs: &[u8]) {
    let mut sorted = regs.to_vec();
    sorted.sort_unstable();
    let mut start = sorted[0];
    let mut prev = sorted[0];
    for n in sorted[1..].iter().copied().chain(std::iter::once(0xFF)) {
        if n == prev + 1 {
            prev = n;
            continue;
        }
        if !out.is_empty() && !out.ends_with('/') && !out.ends_with(bank) {
            out.push('/');
        }
        if start == prev {
            out.push_str(&prev.to_string());
        } else {
            out.push_str(&format!("{start}-{prev}"));
        }
        start = n;
        prev = n;
    }
}

#[cfg(test)]
mod tests {
    use super::disassemble;
    use crate::decode;

    /// Whether `disassemble` has text for `opcode`, as opposed to printing it as
    /// `DC.W`. Mirrors what the formatter treats as data.
    fn is_real_op(opcode: u16) -> bool {
        !matches!(
            decode::decode(opcode),
            decode::Op::Illegal | decode::Op::Unimplemented | decode::Op::LineA | decode::Op::LineF
        )
    }

    /// Extension words an effective-address field consumes, from
    /// M68000UM Table 8-1: none for the register modes, one for `d16(An)`,
    /// the index mode, `d16(PC)`, `d8(PC,Xn)` and an absolute word, two for an
    /// absolute long, and one or two for `#imm` depending on the operand size.
    /// Modes `111` with `reg` above `100` never decode, so they contribute
    /// nothing.
    fn ea_words(ea: u8, imm_long: bool) -> usize {
        let mode = (ea >> 3) & 0b111;
        let reg = ea & 0b111;
        match mode {
            0b000..=0b100 => 0,
            0b101 | 0b110 => 1,
            0b111 => match reg {
                0b000 | 0b010 | 0b011 => 1,
                0b001 => 2,
                0b100 => usize::from(imm_long) + 1,
                _ => 0,
            },
            _ => 0,
        }
    }

    /// The word count an encoding carries, written out from M68000UM rather
    /// than read back from the formatter, so `word_count_matches_encoding`
    /// checks the disassembler against the instruction set instead of against
    /// itself.
    ///
    /// `Some` for the operand shapes spelled out below, which is every shape
    /// the formatter has; `None` for the rest of the universe, where
    /// `never_panics_on_any_opcode` still bounds the count.
    ///
    /// There is no `decode::size_words` to call: the CPU lengths an instruction
    /// by fetching extension words as it executes (`Cpu::fetch_ext_word`), so
    /// the encoder table here is the only other place the rule is written.
    fn reference_size_words(opcode: u16) -> Option<usize> {
        let ea = (opcode & 0x3F) as u8;
        let dst = ((opcode >> 6) & 0x3F) as u8;
        let hi4 = opcode >> 12;
        // `MOVE`/`MOVEA` number their size field differently from the ALU
        // group (M68000UM, "MOVE — Move to Register/Data").
        let move_long = matches!((opcode >> 12) & 0b11, 0b01);
        let alu_long = matches!((opcode >> 6) & 0b11, 0b10);
        match hi4 {
            // `MOVE <ea>,<ea>` and `MOVEA <ea>,An`, plus the `MOVEP` encodings
            // that live in the otherwise-illegal `0000 ddd0 1p01 1000` corner.
            0b0000 | 0b0001 | 0b0010 => {
                if (opcode & 0x01F8) == 0x0008 {
                    Some(2) // MOVEP: one displacement word
                } else {
                    Some(1 + ea_words(dst, move_long) + ea_words(ea, move_long))
                }
            }
            0b0011 => Some(1 + ea_words(ea, true)),
            // `MOVEQ #imm8,Dn`: the whole instruction is the opcode.
            0b0111 => Some(1),
            0b0101 => {
                if (opcode >> 8) & 0b1 == 0 {
                    Some(1) // ADDQ/SUBQ carry the count in the opcode
                } else if (opcode >> 6) & 0b111 == 0b111 {
                    match (opcode >> 9) & 0b111 {
                        0b1110 => Some(2),                  // DBcc: displacement word
                        0b1111 => None,                     // illegal
                        _ => Some(1 + ea_words(ea, false)), // Scc: one <ea>
                    }
                } else {
                    None
                }
            }
            // `BRA`/`BSR`/`Bcc`: a byte displacement in the opcode, or a
            // displacement word when that byte is zero.
            0b0110 => Some(2 - usize::from(opcode & 0xFF != 0)),
            _ => None,
        }
    }

    /// Text only, for the cases where the word count is asserted elsewhere.
    fn text(words: &[u16]) -> String {
        disassemble(words, 0x1000).0
    }

    // -----------------------------------------------------------------------
    // The ticket's table, one test per format.
    // -----------------------------------------------------------------------

    #[test]
    fn nop_is_plain() {
        assert_eq!(text(&[0x4E71]), "NOP");
    }

    #[test]
    fn moveq_names_immediate_then_destination() {
        assert_eq!(text(&[0x7001]), "MOVEQ #$1,D0");
        assert_eq!(text(&[0x7EFF]), "MOVEQ #$FF,D7");
    }

    #[test]
    fn move_names_size_and_prints_source_before_destination() {
        assert_eq!(text(&[0x1001]), "MOVE.B D1,D0");
        assert_eq!(text(&[0x3001]), "MOVE.W D1,D0");
        assert_eq!(text(&[0x2001]), "MOVE.L D1,D0");
    }

    #[test]
    fn bcc_prints_the_computed_target() {
        // `BNE.S $0001004`: the displacement is relative to the *next*
        // instruction, which for a 1-word branch is `pc + 2`.
        let (t, n) = disassemble(&[0x6602], 0x1000);
        assert_eq!(t, "BNE.S $001004");
        assert_eq!(n, 1);
        // A negative displacement still names a target, and stays `.S`.
        assert_eq!(text(&[0x66FE]), "BNE.S $001000");
        // A zero byte displacement means a displacement word follows.
        let (t, n) = disassemble(&[0x6600, 0x0100], 0x1000);
        assert_eq!(t, "BNE.W $001102");
        assert_eq!(n, 2);
        // `BSR` shares the format.
        assert_eq!(text(&[0x6100, 0xFFFE]), "BSR.W $001000");
    }

    #[test]
    fn movem_store_prints_the_collapsed_register_list() {
        let (t, n) = disassemble(&[0x48E7, 0xC080], 0x1000);
        assert_eq!(t, "MOVEM.L D0-D1/A0,-(A7)");
        assert_eq!(n, 2);
    }

    #[test]
    fn movem_load_prints_the_list_after_the_operand() {
        assert_eq!(text(&[0x4C50, 0x00FF]), "MOVEM.L (A1),D0-D7");
        assert_eq!(text(&[0x4C10, 0x0F00]), "MOVEM.W (A0),A0-A3");
    }

    #[test]
    fn lea_prints_the_index_brief_word() {
        let (t, n) = disassemble(&[0x41F0, 0x1004], 0x1000);
        assert_eq!(t, "LEA $4(A0,D1.W),A0");
        assert_eq!(n, 2);
        assert_eq!(text(&[0x41F0, 0x9003]), "LEA $3(A0,A1.L),A0");
    }

    #[test]
    fn jsr_absolute_long_uses_three_words() {
        let (t, n) = disassemble(&[0x4EB9, 0x0001, 0x2345], 0x1000);
        assert_eq!(t, "JSR $012345");
        assert_eq!(n, 3);
    }

    #[test]
    fn line_f_and_unknown_opcodes_print_as_data() {
        assert_eq!(text(&[0xFFFF]), "DC.W $FFFF");
        assert_eq!(text(&[0xA123]), "DC.W $A123");
        assert_eq!(text(&[0x42C8]), "DC.W $42C8");
    }

    // -----------------------------------------------------------------------
    // Addressing-mode coverage.
    // -----------------------------------------------------------------------

    #[test]
    fn addressing_modes_print_in_motorola_syntax() {
        for (words, want) in [
            (&[0x2010][..], "MOVE.L (A0),D0"),
            (&[0x2018][..], "MOVE.L (A0)+,D0"),
            (&[0x2020][..], "MOVE.L -(A0),D0"),
            (&[0x2028, 0x0010][..], "MOVE.L $10(A0),D0"),
            (&[0x2030, 0x1004][..], "MOVE.L $4(A0,D1.W),D0"),
            (&[0x2038, 0x1234][..], "MOVE.L $1234,D0"),
            (&[0x2039, 0x0001, 0x2345][..], "MOVE.L $012345,D0"),
            (&[0x203A, 0x0010][..], "MOVE.L $10(PC),D0"),
            (&[0x203B, 0x1004][..], "MOVE.L $4(PC,D1.W),D0"),
            (&[0x203C, 0x0000, 0x0010][..], "MOVE.L #$10,D0"),
            (&[0x103C, 0x0012][..], "MOVE.B #$12,D0"),
            (&[0x227C, 0x0000, 0x0010][..], "MOVE.L #$10,(A2)"),
        ] {
            assert_eq!(text(&words), want, "{:04X}", words[0]);
        }
    }

    // -----------------------------------------------------------------------
    // Remaining formats worth pinning down.
    // -----------------------------------------------------------------------

    #[test]
    fn arithmetic_and_logic_print_size_and_both_operands() {
        for (words, want) in [
            (&[0xD041][..], "ADD.W D1,D0"),
            (&[0xD1C8][..], "ADD.L (A0)+,A0"),
            (&[0xD0C0][..], "ADD.L D0,(A0)"),
            (&[0x50C0][..], "ADDQ.L #$8,A0"),
            (&[0x5481][..], "SUBQ.B #$4,D1"),
            (&[0x90F0][..], "SUBA.W (A0),A0"),
            (&[0x91F2][..], "SUBA.L (A2),A0"),
            (&[0xB0C8][..], "CMPA.L (A0),A0"),
            (&[0x51C0][..], "SUBX.L -(A0),-(A0)"),
            (&[0x9EC1][..], "CMPM.L (A1)+,(A4)+"),
            (&[0x067C, 0x0001, 0x0002][..], "CMPI.L #$10002,SR"),
            (&[0x003C, 0x00FF][..], "ORI.B #$FF,CCR"),
            (&[0x027C, 0x1234][..], "ANDI.W #$1234,SR"),
            (&[0x0A3C, 0x0042][..], "EORI.B #$42,CCR"),
            (&[0x4A10][..], "TST.W (A0)"),
            (&[0x4200][..], "CLR.W D0"),
            (&[0x4610][..], "NOT.B (A0)"),
            (&[0x40D8][..], "NEGX.L (A0)+"),
            (&[0x4AC0][..], "TAS D0"),
            (&[0x40C0][..], "MOVE.W SR,D0"),
            (&[0x44C0][..], "MOVE.B (A0),CCR"),
            (&[0x46D0][..], "MOVE.W (A0),SR"),
            (&[0x4E61][..], "MOVE USP,A1"),
            (&[0x4E69][..], "MOVE A1,USP"),
            (&[0xC342][..], "EXG D1,D2"),
            (&[0xC1C8][..], "EXG A0,A1"),
            (&[0xD181][..], "EXG D1,A1"),
            (&[0x4841][..], "SWAP D1"),
            (&[0x48C1][..], "EXT.L D1"),
            (&[0x4CC1][..], "EXT.W D1"),
            (&[0xC06E, 0x0010][..], "MULU.W $10(A2),D0"),
            (&[0x814E][..], "DIVU.W D2,(A6)"),
            (&[0xC501][..], "ABCD D1,D2"),
            (&[0x8501][..], "SBCD D1,D2"),
            (&[0x4810][..], "NBCD (A0)"),
            (&[0x4390][..], "CHK.W (A0),D1"),
        ] {
            assert_eq!(text(&words), want, "{:04X}", words[0]);
        }
    }

    #[test]
    fn bit_instructions_print_the_bit_number_or_register() {
        for (words, want) in [
            (&[0x0838, 0x0003, 0x1234][..], "BTST #$3,$1234"),
            (&[0x0850][..], "BTST (A0),D0"),
            (&[0x0C58][..], "BSET (A0)+,D0"),
            (&[0x0478, 0x0001][..], "BCHG.B #$1,D7"),
        ] {
            assert_eq!(text(&words), want, "{:04X}", words[0]);
        }
    }

    #[test]
    fn shifts_print_count_and_direction() {
        for (words, want) in [
            (&[0xE789][..], "LSL.L #$3,D1"),
            (&[0xE098][..], "ASR.W D0,D4"),
            (&[0xE0D0][..], "ASR.W (A0)"),
            (&[0xE6D1][..], "ROR.W (A1)"),
        ] {
            assert_eq!(text(&words), want, "{:04X}", words[0]);
        }
    }

    #[test]
    fn movep_reads_the_displacement_and_direction() {
        assert_eq!(text(&[0x0108, 0x1234]), "MOVEP.W $1234(A0),D0");
        assert_eq!(text(&[0x0148, 0x1234]), "MOVEP.L $1234(A0),D0");
        assert_eq!(text(&[0x0188, 0x1234]), "MOVEP.W D0,$1234(A0)");
        assert_eq!(text(&[0x01C8, 0x1234]), "MOVEP.L D0,$1234(A0)");
    }

    #[test]
    fn conditionals_other_than_branches() {
        for (words, want) in [
            (&[0x51CA, 0x0004][..], "DBF D2,$001006"),
            (&[0x50C8, 0x0004][..], "DBT D0,$001006"),
            (&[0x50C0][..], "ST (A0)"),
            (&[0x51DF, 0x00FF][..], "SF (A7,D7.W)"),
        ] {
            assert_eq!(text(&words), want, "{:04X}", words[0]);
        }
    }

    #[test]
    fn control_transfer_and_privileged_forms() {
        for (words, want) in [
            (&[0x4E40][..], "TRAP #$0"),
            (&[0x4E4F][..], "TRAP #$F"),
            (&[0x4E72, 0x2700][..], "STOP #$2700"),
            (&[0x4E56, 0xFFF8][..], "LINK A6,#$FFF8"),
            (&[0x4E5D][..], "UNLK A5"),
            (&[0x4EB8, 0x1234][..], "JSR $1234"),
            (&[0x4EF9, 0x0001, 0x2345][..], "JMP $012345"),
            (&[0x4E91][..], "JMP (A1)"),
            (&[0x4848, 0x1234][..], "PEA $1234"),
            (&[0x41F8, 0x1234][..], "PEA.W $1234"),
            (&[0x4E70][..], "RESET"),
            (&[0x4E73][..], "RTE"),
            (&[0x4E75][..], "RTS"),
            (&[0x4E77][..], "RTR"),
            (&[0x4E76][..], "TRAPV"),
        ] {
            assert_eq!(text(&words), want, "{:04X}", words[0]);
        }
    }

    // -----------------------------------------------------------------------
    // Robustness and length.
    // -----------------------------------------------------------------------

    #[test]
    fn never_panics_on_any_opcode() {
        for opcode in 0..=u16::MAX {
            // A slice that stops right after the opcode: most instructions
            // need more, and the caller must still be told to step over it.
            let one = [opcode];
            let (t, n) = disassemble(&one, 0x1000);
            assert!(!t.is_empty(), "${opcode:04X} disassembled to nothing");
            assert!(
                n >= 1 && n <= one.len().max(1),
                "${opcode:04X} claims {n} words from {}",
                one.len()
            );

            // The opcode plus two words of padding.
            let padded = [opcode, 0, 0];
            let (t, n) = disassemble(&padded, 0x1000);
            assert!(!t.is_empty(), "${opcode:04X} disassembled to nothing");
            assert!(
                n >= 1 && n <= padded.len().max(1),
                "${opcode:04X} claims {n} words from {}",
                padded.len()
            );
        }
        // Nothing at all is not a panic either.
        disassemble(&[], 0x1000);
    }

    #[test]
    fn word_count_matches_encoding() {
        // One 16-word slice so a truncated read can never be what makes the
        // count look right: every real opcode must report the words the
        // encoding actually carries, not the words available.
        for opcode in 0..=u16::MAX {
            if !is_real_op(opcode) {
                continue;
            }
            let Some(want) = reference_size_words(opcode) else {
                continue;
            };
            let words = [opcode, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let (_, got) = disassemble(&words, 0x1000);
            assert_eq!(got, want, "${opcode:04X} word count");
        }
    }

    #[test]
    fn jsr_absolute_long_and_wide_operands_word_counts() {
        let cases: [(&[u16], usize); 8] = [
            (&[0x4E71], 1),
            (&[0x7001], 1),
            (&[0x4EB9, 0x0001, 0x2345], 3),
            (&[0x4EF9, 0x0001, 0x2345], 3),
            (&[0x2039, 0x0001, 0x2345, 0x0000, 0x0000], 3),
            (&[0x48E7, 0xC080], 2),
            (&[0x6600, 0x0000], 2),
            (&[0x4E72, 0x2700], 2),
        ];
        for (words, want) in cases {
            let (t, got) = disassemble(words, 0x1000);
            assert!(!t.is_empty());
            assert_eq!(got, want, "{:04X}", words[0]);
        }
    }
}
