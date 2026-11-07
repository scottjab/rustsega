//! Opcode decode: the group map and the 65536-entry lookup table (M1-07, #15).
//!
//! Every 16-bit opcode maps to one [`Op`]. [`crate::M68k::step`] takes the
//! opcode out of the front of the prefetch queue, looks it up here, and calls
//! the handler in `crate::ops` that owns the variant. Later tickets add
//! instructions by adding patterns to [`classify`] plus a handler there;
//! `step()` and this table's shape do not change.
//!
//! # Why a table and not function pointers
//!
//! A handler is a method on `M68k` generic over the bus (`impl Bus`), so it
//! cannot be stored as a `fn` pointer. The table therefore stores the small
//! [`Op`] tag and the handlers re-read their operand fields from the opcode
//! bits, which keeps `Op` `Copy` and the table 64 KB of one-byte tags.
//!
//! # The group map
//!
//! The top four bits of the opcode pick the group (M68000PM/AD section 8
//! "Instruction Set" and the "Operation code map" appendix at the back of the
//! manual):
//!
//! | Bits 15–12 | Group |
//! |---|---|
//! | `0000` | bit operations (BTST, BCLR, BSET, BCHG), `MOVEP`, and the immediate operations `ORI`, `ANDI`, `SUBI`, `ADDI`, `EORI`, `CMPI` |
//! | `0001` | `MOVE.b` |
//! | `0010` | `MOVE.l` |
//! | `0011` | `MOVE.w`, and `MOVEA` |
//! | `0100` | miscellaneous: `LEA`, `CHK`, `CLR`, `NEG`, `NOT`, `TST`, `MOVEM`, `JMP`, `JSR`, `TRAP`, `RTS`, `RESET`, `NOP`, … |
//! | `0101` | `ADDQ`, `SUBQ`, `Scc`, `DBcc` |
//! | `0110` | `Bcc`, `BRA`, `BSR` |
//! | `0111` | `MOVEQ` |
//! | `1000` | `OR`, `DIV`, `SBCD` |
//! | `1001` | `SUB`, `SUBA`, `SUBX` |
//! | `1010` | line A (`1010` emulator) — the 68000 has no line emulator, so this is a reserved format: [`crate::exceptions::EXC_ILLEGAL`] |
//! | `1011` | `CMP`, `CMPA`, `CMPM`, `EOR` |
//! | `1100` | `AND`, `MUL`, `ABCD`, `EXG` |
//! | `1101` | `ADD`, `ADDA`, `ADDX` |
//! | `1110` | shifts and rotates |
//! | `1111` | line F (`1111` emulator) — reserved on the 68000 like `1010`: [`crate::exceptions::EXC_ILLEGAL`] |
//!
//! The whole map is documented here because every instruction ticket reads it
//! off this table, but only the groups the core routes today are acted on:
//! `1010` and `1111` (line A / line F), the `0111` group (`MOVEQ`, #17), the
//! three move groups `0001`/`0010`/`0011` (`MOVE` and `MOVEA`, #17), the traps
//! in `0100`, the one documented illegal opcode, the `1101` group (`ADD`,
//! `ADDA`, `ADDX`, #18), the `1001` group (`SUB`, `SUBA`, `SUBX`, #19), the
//! `0101` group's `ADDQ` and `SUBQ` halves (#18, #19), the `$0600` `ADDI`
//! pattern inside `0000` (#18) and the `$0400` `SUBI` pattern inside `0000`
//! (#19). Everything else in those groups still
//! decodes to [`Op::Unimplemented`] and panics loudly until its ticket lands
//! (M1-08, #16 onward).
//!
//! # EA validity is a decode-time rule
//!
//! An instruction whose opcode carries an effective-address field may not
//! accept every mode: an `ADD.B` cannot land in an address register, and no
//! instruction may write to `d16(PC)` or to an immediate. Those combinations
//! are illegal instructions (M68000PM/AD section 2.2.12 "Effective Addressing
//! Mode Summary", which lists per mode which operand classes may use it). The
//! check belongs in decode rather than in the handlers, so a handler can assume
//! its operands are legal and never re-litigate the mode: the classification
//! predicates in [`crate::ea`] (`is_data`, `is_memory`, `is_control`,
//! `is_alterable`, `is_data_alterable`, `is_memory_alterable`) are what it
//! tests, and each instruction ticket applies them to its own opcode pattern
//! as it lands — the pattern has to say which field of the opcode is the EA
//! field, which is only knowable per instruction.

use crate::ea::{Size, is_data, is_data_alterable};
use std::sync::OnceLock;

/// What a decoded opcode means: which handler runs, or why none can.
///
/// Deliberately tiny and `Copy` (see the module docs): everything the handler
/// needs beyond this tag — registers, size, mode fields, displacement kind —
/// it re-reads from the opcode bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// A real instruction the core has not implemented yet. [`crate::ops`]
    /// panics on this rather than doing nothing, so a missing instruction can
    /// never be mistaken for a working one.
    Unimplemented,
    /// Not a valid instruction on the 68000: takes the illegal-instruction
    /// exception (vector [`crate::exceptions::EXC_ILLEGAL`]).
    Illegal,
    /// An opcode in the `1010` group (M68000UM Format I, "Line 1010 emulator
    /// instruction"). The 68000 has no line emulator, so it takes
    /// [`crate::exceptions::EXC_ILLEGAL`] like any other reserved opcode; the variant
    /// is separate from [`Op::Illegal`] because the 68010 (M1-12, #20) does
    /// have a line-A vector and will route the two differently.
    LineA,
    /// An opcode in the `1111` group (M68000UM Format J, "Line 1111 emulator
    /// instruction"). [`Op::LineA`] in every respect on the 68000 — illegal
    /// instruction — and separate for the same 68010 reason.
    LineF,
    /// `TRAP #n`, `$4E40 + n` (M68000PM section 8, "TRAP — Trap"): the opcode's
    /// low nibble is the trap number and the whole instruction is one word, so
    /// the tag has no payload — [`crate::ops`] re-reads the nibble. Takes vector
    /// [`crate::exceptions::trap_vector`], which is measured in `TRAP.json.gz`.
    Trap,
    /// `MOVE <ea>,Dn|(An)|<ea>` — the `0001`/`0010`/`0011` groups with
    /// destination mode other than `001` (M68000PM section 8, "MOVE — Move").
    /// [`crate::ops`] re-reads the size, the swapped destination fields and the
    /// source EA from the opcode bits.
    Move,
    /// `MOVEA <ea>,An`: a `MOVE`-shaped opcode whose destination mode is `001`.
    /// Word and long only, so no `$0001`-group opcode is a `MOVEA` — see
    /// [`Op::Illegal`]. [`crate::ops::move_`] for the semantics.
    Movea,
    /// `MOVEQ #imm8,Dn`, `$7000 + d8` with the destination register in bits
    /// 11..9 and bit 8 clear (M68000UM, "Instruction Formats", Format IV).
    /// Bit 8 set is [`Op::Illegal`] on the 68000.
    Moveq,
    /// `ADD <ea>,Dn` and `ADD Dn,<ea>`, the `1101` group's operation modes
    /// `000`/`001`/`010` and `100`/`101`/`110` (M68000UM "ADD — Add", #18).
    /// [`crate::ops::arith`] re-reads the size, direction, register and source
    /// or destination EA from the opcode bits.
    Add,
    /// `ADDA <ea>,An`: the `1101` group's operation modes `011` (word) and
    /// `111` (long). The destination is the address register in bits 11..9
    /// (M68000UM "ADDA — Add Address", #18).
    Adda,
    /// `ADDX` — extend-form add, `1101 xxx 1 ss 00 m yyy` (M68000UM "ADDX — Add
    /// with Extend", #18). Both operands are predecrement address-register
    /// indirect when bit 3 is set, `Dy`/`Dx` when it is clear; bits 11..9 pick
    /// the destination and bits 2..0 the source, which is the reverse of every
    /// other two-operand instruction here.
    Addx,
    /// `ADDI #imm,<ea>`, `$0600 + ss<<6 + <ea>` (M68000UM "ADDI — Add Integer",
    /// #18). The immediate is the operand encoded in the EA field as mode `111`
    /// register `100`, and it is fetched before the destination's own extension
    /// words.
    Addi,
    /// `ADDQ #quick,<ea>`, `0101 ddd 0 ss mmm rrr` with `ss` != `11` (M68000UM
    /// "ADDQ — Add Quick", #18). Data `000` means 8, and a destination of mode
    /// `001` is an address register added to in full 32 bits without touching
    /// the flags.
    Addq,
    /// `SUB <ea>,Dn` and `SUB Dn,<ea>`, the `1001` group's operation modes
    /// `000`/`001`/`010` and `100`/`101`/`110` (M68000UM "SUB — Subtract",
    /// #19). The two directions differ in which operand is the minuend: the
    /// first is `Dn - <ea> → Dn`, the second `<ea> - Dn → <ea>`.
    /// [`crate::ops::arith`] re-reads the size, direction, register and EA from
    /// the opcode bits.
    Sub,
    /// `SUBA <ea>,An`: the `1001` group's operation modes `011` (word, source
    /// sign-extended) and `111` (long). Destination is the address register in
    /// bits 11..9, and no condition code changes (M68000UM "SUBA — Subtract
    /// Address", #19).
    Suba,
    /// `SUBX` — extend-form subtract, `1001 xxx 1 ss 00 m yyy` (M68000UM "SUBX
    /// — Subtract with Extend", #19). `Dy`/`Dx` when bit 3 is clear, `-(Ay)`/
    /// `-(Ax)` when set; bits 11..9 pick the destination and bits 2..0 the
    /// source, exactly as in [`Op::Addx`]. Subtract with entry borrow `X`, and
    /// `Z` is left alone on a zero result.
    Subx,
    /// `SUBI #imm,<ea>`, `$0400 + ss<<6 + <ea>` (M68000UM "SUBI — Subtract
    /// Integer", #19). The immediate is the operand encoded in the EA field as
    /// mode `111` register `100` and is fetched before the destination's own
    /// extension words.
    Subi,
    /// `SUBQ #quick,<ea>`, `0101 ddd 1 ss mmm rrr` with `ss` != `11` (M68000UM
    /// "SUBQ — Subtract Quick", #19). Data `000` means 8, and a destination of
    /// mode `001` is an address register subtracted from in full 32 bits without
    /// touching the flags.
    Subq,
}

/// The one opcode the manual spells out as illegal: `ILLEGAL`, `$4AFC`
/// (M68000PM/AD section 8, "ILLEGAL — Illegal Instruction"; it exists so
/// software and simulators can force the exception). A reserved or undefined
/// encoding that is not this one still decodes to [`Op::Unimplemented`] until
/// the ticket that owns its group says otherwise.
const ILLEGAL_OPCODE: u16 = 0x4AFC;

/// Address register direct, the destination mode that turns a move-group opcode
/// into `MOVEA` instead of `MOVE` (M68000UM, "Instruction Formats", Format III).
const AN_DIRECT_MODE: u8 = 0b001;

/// Bit 8 of a `MOVEQ` opcode: `0111 RRR 0 dddddddd`, so a set bit 8 is not a
/// `MOVEQ` on the 68000 (M68000UM, "Instruction Formats", Format IV). Shared
/// with [`crate::ops::move_`], which re-reads the opcode, so the bit is spelled
/// once.
pub(crate) const MOVEQ_BIT8: u16 = 0x0100;

/// Bit 8 of an `0101`-group opcode: clear for `ADDQ` (`0101 ddd 0 ss mmm rrr`),
/// set for `SUBQ` (`0101 ddd 1 ss mmm rrr`) and for `Scc`/`DBcc`
/// (`0101 ccc 1 11 mmm rrr`) — M68000UM "Instruction Formats", Format V, and
/// the operation-code-map appendix.
const QUICK_BIT8: u16 = 0x0100;

/// Every opcode's [`Op`], indexed by the opcode itself, so a decode is one load.
///
/// Built once, on first use, from [`classify`]: `M68k::new` must not allocate
/// or fill 64 KB per CPU, because tests construct CPUs by the thousand and the
/// suite constructs one per case. A [`OnceLock`] static is the wasm-safe way to
/// say "once, lazily, no `std::once` thread fiddling" (the core crate has to
/// build for `wasm32-unknown-unknown`, per `docs/ARCHITECTURE.md`).
static DECODE_TABLE: OnceLock<[Op; TABLE_ENTRIES]> = OnceLock::new();

/// One entry per 16-bit opcode.
const TABLE_ENTRIES: usize = 1 << 16;

/// The decode table itself: `TABLE_ENTRIES` entries, one per opcode.
pub(crate) fn decode_table() -> &'static [Op; TABLE_ENTRIES] {
    DECODE_TABLE.get_or_init(build_table)
}

/// Fill the table by asking [`classify`] about every opcode in turn.
fn build_table() -> [Op; TABLE_ENTRIES] {
    let mut table = [Op::Unimplemented; TABLE_ENTRIES];
    for (opcode, entry) in table.iter_mut().enumerate() {
        *entry = classify(opcode as u16);
    }
    table
}

/// The [`Op`] for `opcode`.
pub fn decode(opcode: u16) -> Op {
    decode_table()[usize::from(opcode)]
}

/// The opcode pattern behind [`decode`], which is what the table is built from.
///
/// Public through the table, not on its own: the table is the thing `step()`
/// uses, and going through it is how a new opcode gets the same answer every
/// time.
fn classify(opcode: u16) -> Op {
    match opcode {
        ILLEGAL_OPCODE => Op::Illegal,
        // `1010` and `1111` are whole groups: no operand field of the opcode
        // has to be read to know what they are.
        0xA000..=0xAFFF => Op::LineA,
        0xF000..=0xFFFF => Op::LineF,
        // `4E40 + n`: sixteen traps, all of them one word. `$4E40..=$4E4F` is
        // the whole group, so no operand field has to be read to know it.
        0x4E40..=0x4E4F => Op::Trap,
        // The three move groups: `00ss` with `ss` = `01` byte, `10` long,
        // `11` word. `$0000..=$0FFF` is *not* a move — `ss` = `00` is the
        // ORI/BTST/MOVEP group — so the range starts at `$1000`.
        0x1000..=0x3FFF => classify_move(opcode),
        // `0111 RRR 0 dddddddd`: the whole `0111` group is MOVEQ, and the
        // encodings with bit 8 set are illegal on the 68000.
        0x7000..=0x7FFF => classify_moveq(opcode),
        // `0000 0100 ss eeeeee`: SUBI is the only instruction with that fixed
        // pattern, so the whole `$0400..=$04FF` range is its group (the same
        // shape `ADDI` uses at `$0600`).
        0x0400..=0x04FF => classify_subi(opcode),
        // `0000 0110 ss eeeeee`: `ADDI` is the only instruction with that fixed
        // pattern, so the whole `$0600..=$06FF` range is its group. The rest of
        // the `0000` group (ORI, the bit operations, MOVEP, the other
        // immediates) is untouched here.
        0x0600..=0x06FF => classify_addi(opcode),
        // `0101 ddd 0 ss eeeeee` is ADDQ and `0101 ddd 1 ss eeeeee` is SUBQ;
        // bit 8 set with `ss` = `11` is Scc/DBcc, which stays unimplemented for
        // the branch ticket.
        0x5000..=0x5FFF => {
            if opcode & QUICK_BIT8 == 0 {
                classify_addq(opcode)
            } else {
                classify_subq(opcode)
            }
        }
        // `1101 ddd ooo eeeeee`: ADD, ADDA and ADDX.
        0xD000..=0xDFFF => classify_add(opcode),
        // `1001 ddd ooo eeeeee`: SUB, SUBA and SUBX.
        0x9000..=0x9FFF => classify_sub(opcode),
        // Every other group is a real instruction whose handler has not been
        // written yet; the tickets that write them split these groups up.
        _ => Op::Unimplemented,
    }
}

/// One opcode of the `0001`/`0010`/`0011` move groups: [`Op::Move`],
/// [`Op::Movea`] or [`Op::Illegal`], with the EA validity rules applied here so
/// the handler can assume its operands are legal (see the module docs).
///
/// This is the one pattern that reads the destination fields in the *swapped*
/// order — register in bits 11..9, mode in bits 8..6, the reverse of the source
/// EA field in bits 5..0 (M68000UM, "Instruction Formats", Format III, and
/// [`crate::ops::move_`], which re-reads them the same way).
fn classify_move(opcode: u16) -> Op {
    let Some(size) = Size::from_move_bits(opcode >> 12) else {
        // `00` in bits 13..12 is the `$0000` group, which `classify` routes
        // elsewhere; naming it keeps this function total.
        return Op::Illegal;
    };
    let dest_reg = ((opcode >> 9) & 0b111) as u8;
    let dest_mode = ((opcode >> 6) & 0b111) as u8;
    let src_mode = ((opcode >> 3) & 0b111) as u8;
    let src_reg = (opcode & 0b111) as u8;

    // Destination mode `001` is MOVEA: it writes the address register itself,
    // it only exists for word and long, and its source may be an An.
    if dest_mode == AN_DIRECT_MODE {
        // A byte MOVEA would be a byte access to an address register, which no
        // 68000 instruction encodes (M68000UM section 1 "Address Registers",
        // which gives An a word or long operand width).
        if size == Size::Byte || !is_data(src_mode, src_reg) {
            return Op::Illegal;
        }
        return Op::Movea;
    }

    if !is_data_alterable(dest_mode, dest_reg) || !is_data(src_mode, src_reg) {
        return Op::Illegal;
    }
    // The other byte-to-An case: `MOVE.B A1,D0` reads an address register a
    // byte at a time, which the chip does not do either.
    if size == Size::Byte && src_mode == AN_DIRECT_MODE {
        return Op::Illegal;
    }
    Op::Move
}

/// `MOVEQ #imm8,Dn`: the whole `0111` group, except that bit 8 set is not a
/// `MOVEQ` on the 68000 at all and takes the illegal-instruction exception
/// (M68000UM, "Instruction Formats", Format IV, and M68000PM section 8
/// "MOVEQ", which lists only the `$7000 + d8` form).
fn classify_moveq(opcode: u16) -> Op {
    if opcode & MOVEQ_BIT8 != 0 {
        Op::Illegal
    } else {
        Op::Moveq
    }
}

/// One opcode of the `1101` group: `ADD`, `ADDA` or `ADDX`.
///
/// The operation-mode field is bits 8..6, so the format is
/// `1101 ddd ooo mmm rrr` with the EA in the low six bits: `000`/`001`/`010`
/// are `ADD <ea>,Dn` at byte/word/long, `100`/`101`/`110` are the same three
/// sizes with the direction reversed (`ADD Dn,<ea>`), and `011`/`111` are
/// `ADDA.W`/`ADDA.L` (M68000UM "ADD", "ADDA").
///
/// # Why `ADDX` has to be recognised first
///
/// `ADDX` is `1101 xxx 1 ss 00 m yyy` (M68000UM "ADDX"), so its operation-mode
/// field is `1ss` — one of `ADD`'s three `Dn,<ea>` modes — and its `00` at
/// bits 5..4 forces the EA mode to `000` or `001`, i.e. a register-operand EA.
/// Those are exactly the opcodes an `ADD Dn,<ea>` with a data-register or
/// address-register destination would use, and on the 68000 no `ADD` may
/// destination an address register at all, so the whole overlap belongs to
/// `ADDX`. Testing `mode <= 001` after `ADD` would silently misroute every
/// `ADDX` to `op_add`: `$D100` (`ADDX.B D0,D0`) would add `D0` to itself
/// without the extend carry and write flags `ADD` does not use. See
/// [`crate::ops::arith::op_addx`] for the semantics the two then share.
fn classify_add(opcode: u16) -> Op {
    let op_mode = ((opcode >> 6) & 0b111) as u8;
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;

    if (0b100..=0b110).contains(&op_mode) && mode <= AN_DIRECT_MODE {
        return Op::Addx;
    }
    // `011` and `111` are ADDA, whose destination is an address register, so
    // neither the data-register width nor the data-operand rules below apply.
    if op_mode == 0b011 || op_mode == 0b111 {
        // ADDA's source may be any data operand, address registers included:
        // `ADDA.W A0,A1` is legal (M68000UM "ADDA", source "data" with An
        // allowed by the operand-type column of M68000PM/AD section 2.2.12).
        if !is_data(mode, reg) {
            return Op::Illegal;
        }
        return Op::Adda;
    }
    let Some(size) = Size::from_bits(u16::from(op_mode & 0b11)) else {
        // `0b011`/`0b111` were the two undefined widths and they left above, so
        // this is unreachable; naming the route keeps the function total.
        return Op::Illegal;
    };
    if op_mode < 0b100 {
        // `ADD <ea>,Dn`: the destination is a data register by construction, so
        // only the source can be illegal. An address register read a byte is
        // the same rejection `MOVE` makes for the identical reason.
        if !is_data(mode, reg) || (size == Size::Byte && mode == AN_DIRECT_MODE) {
            return Op::Illegal;
        }
        Op::Add
    } else {
        // `ADD Dn,<ea>`: the destination is written, so it must be
        // data-alterable — which rules out `An` direct (no `ADD` may write an
        // address register; that is `ADDA`'s job), `d16(PC)` and an immediate.
        // An opcode with `An` direct here never reaches this line, because
        // `ADDX` claimed it above.
        if !is_data_alterable(mode, reg) {
            return Op::Illegal;
        }
        Op::Add
    }
}

/// `$0600 + ss<<6 + <ea>`: `ADDI #imm,<ea>` (M68000UM "ADDI — Add Integer").
///
/// The EA field names the *destination*, which must be alterable, and the
/// immediate itself lives in the instruction's extension words, so
/// [`crate::ops::arith::op_addi`] resolves it as mode `111` register `100`.
fn classify_addi(opcode: u16) -> Op {
    // `ss` = `11` is not one of the three widths `ADDI` defines. It is not
    // `SUBI` (which has its own opcode pattern) and no ticket has claimed it,
    // so it stays unimplemented rather than guessed at.
    if Size::from_bits((opcode >> 6) & 0b11).is_none() {
        return Op::Unimplemented;
    }
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Addi
}

/// One opcode of the `1001` group: `SUB`, `SUBA` or `SUBX`.
///
/// The operation-mode field is bits 8..6, so the format is
/// `1001 ddd ooo mmm rrr` with the EA in the low six bits:
/// `000`/`001`/`010` are `SUB <ea>,Dn` (`Dn - <ea> → Dn`) at byte/word/long,
/// `100`/`101`/`110` are the same three sizes reversed (`SUB Dn,<ea>`, which is
/// `<ea> - Dn → <ea>`), and `011`/`111` are `SUBA.W`/`SUBA.L`
/// (M68000UM "SUB", "SUBA").
///
/// # Why `SUBX` has to be recognised first
///
/// Exactly as for [`classify_add`]: `SUBX` is `1001 xxx 1 ss 00 m yyy`
/// (M68000UM "SUBX"), so its operation-mode field is `1ss` — one of `SUB`'s
/// three `Dn,<ea>` modes — and its forced `00` at bits 5..4 makes the EA mode
/// `000` or `001`, a register-operand EA. Testing `mode <= 001` after `SUB`
/// would misroute every `SUBX` to `op_sub`, which would subtract without the
/// entry borrow and set `Z` unconditionally, breaking the multi-precision chains
/// `SUBX` exists for. See [`crate::ops::arith::op_subx`].
fn classify_sub(opcode: u16) -> Op {
    let op_mode = ((opcode >> 6) & 0b111) as u8;
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;

    if (0b100..=0b110).contains(&op_mode) && mode <= AN_DIRECT_MODE {
        return Op::Subx;
    }
    // `011` and `111` are SUBA, whose destination is an address register, so
    // neither the data-register width nor the data-operand rules below apply.
    if op_mode == 0b011 || op_mode == 0b111 {
        // SUBA's source may be any data operand, address registers included:
        // `SUBA.W A0,A1` is legal (M68000UM "SUBA", source "data"; M68000PM/AD
        // section 2.2.12 allows An).
        if !is_data(mode, reg) {
            return Op::Illegal;
        }
        return Op::Suba;
    }
    let Some(size) = Size::from_bits(u16::from(op_mode & 0b11)) else {
        // `0b011`/`0b111` were the two undefined widths and they left above, so
        // this is unreachable; naming the route keeps the function total.
        return Op::Illegal;
    };
    if op_mode < 0b100 {
        // `SUB <ea>,Dn`: the destination is a data register by construction, so
        // only the source can be illegal, and an address register read a byte is
        // the same rejection `MOVE` and `ADD` make for the identical reason.
        if !is_data(mode, reg) || (size == Size::Byte && mode == AN_DIRECT_MODE) {
            return Op::Illegal;
        }
        Op::Sub
    } else {
        // `SUB Dn,<ea>`: the destination is written, so it must be
        // data-alterable — which rules out `An` direct (no `SUB` may write an
        // address register; that is `SUBA`'s job), `d16(PC)` and an immediate.
        // An opcode with `An` direct here never reaches this line, because
        // `SUBX` claimed it above.
        if !is_data_alterable(mode, reg) {
            return Op::Illegal;
        }
        Op::Sub
    }
}

/// `$0400 + ss<<6 + <ea>`: `SUBI #imm,<ea>` (M68000UM "SUBI — Subtract
/// Integer").
///
/// The EA field names the *destination*, which must be alterable, and the
/// immediate itself lives in the instruction's extension words, so
/// [`crate::ops::arith::op_subi`] resolves it as mode `111` register `100`.
fn classify_subi(opcode: u16) -> Op {
    // `ss` = `11` is not one of the three widths `SUBI` defines, and no ticket
    // has claimed it, so it stays unimplemented rather than guessed at.
    if Size::from_bits((opcode >> 6) & 0b11).is_none() {
        return Op::Unimplemented;
    }
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Subi
}

/// One opcode of the `0101` group with bit 8 set: [`Op::Subq`], or
/// [`Op::Unimplemented`] for the `Scc`/`DBcc` encodings (bit 8 set with
/// `ss` = `11`, M68000UM Format V), which the branch ticket owns.
///
/// The rules are `classify_addq`'s: `SUBQ` to an address register always
/// subtracts 32 bits, so the byte form is not encodable (M68000UM "SUBQ").
fn classify_subq(opcode: u16) -> Op {
    let Some(size) = Size::from_bits((opcode >> 6) & 0b11) else {
        return Op::Unimplemented;
    };
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if mode == AN_DIRECT_MODE {
        if size == Size::Byte {
            return Op::Illegal;
        }
        return Op::Subq;
    }
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Subq
}

/// One opcode of the `0101` group with bit 8 clear: [`Op::Addq`], or
/// [`Op::Unimplemented`] for the `ss` = `11` encodings, which are neither ADDQ
/// nor SUBQ and are not claimed by any ticket yet (M68000UM operation-code-map
/// appendix).
fn classify_addq(opcode: u16) -> Op {
    let Some(size) = Size::from_bits((opcode >> 6) & 0b11) else {
        return Op::Unimplemented;
    };
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if mode == AN_DIRECT_MODE {
        // `ADDQ` to an address register always adds 32 bits, so the byte form
        // is not encodable (M68000UM "ADDQ": "... the byte size is not allowed
        // for address register destination").
        if size == Size::Byte {
            return Op::Illegal;
        }
        return Op::Addq;
    }
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Addq
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Op` every opcode in a range must decode to, with the range named in
    /// the assertion so a failure says which group went wrong.
    fn assert_range_decodes_to(range: core::ops::RangeInclusive<u16>, want: Op) {
        for opcode in range {
            assert_eq!(
                decode(opcode),
                want,
                "${opcode:04X} should decode to {want:?}"
            );
        }
    }

    /// Line A is the whole `1010` group and line F the whole `1111` group
    /// (M68000PM/AD section 8: the `1010` and `1111` emulator groups).
    #[test]
    fn line_a_and_line_f() {
        assert_eq!(decode(0xA000), Op::LineA);
        assert_eq!(decode(0xF123), Op::LineF);
        assert_range_decodes_to(0xA000..=0xAFFF, Op::LineA);
        assert_range_decodes_to(0xF000..=0xFFFF, Op::LineF);
        // The groups either side of them are not. `1001` was the anchor here
        // until #19 implemented SUB/SUBA/SUBX, so it is now `1000` — OR, DIV and
        // SBCD, still unimplemented — and `1011`. `1011`'s anchor moved to
        // `$B100` with #20, which implemented CMP/CMPA/CMPM there; `$B100` is
        // `EOR.B D0,D0`, the part of the group M1-13 owns, so the assertion kept
        // its meaning (see `cmpm_decodes_before_eor`).
        assert_eq!(decode(0x8FFF), Op::Unimplemented);
        assert_eq!(decode(0xB100), Op::Unimplemented, "EOR.B D0,D0");
    }

    /// `$4AFC` is `ILLEGAL` (M68000PM/AD section 8).
    #[test]
    fn illegal_opcode() {
        assert_eq!(decode(0x4AFC), Op::Illegal);
        // The opcodes either side of it are not the illegal one: `$4AFB` is
        // `MOVEM.l (A0)+,D1`-shaped and `$4AFD` is `MOVEM`-shaped too.
        assert_eq!(decode(0x4AFB), Op::Unimplemented);
        assert_eq!(decode(0x4AFD), Op::Unimplemented);
    }

    /// Until an instruction's ticket lands, its opcodes are unimplemented —
    /// loudly, not silently. `$4E71` is `NOP`.
    ///
    /// The `$4E4x` traps are the exception to this in the same group: TRAP is
    /// not an arithmetic instruction but an exception route, and #16 owns it.
    ///
    /// This test is expected to change as instructions land: when `NOP` is
    /// implemented it decodes to its own variant, and the ticket that does it
    /// moves this assertion to an opcode still unimplemented. That licence is
    /// this test alone (ticket #15).
    #[test]
    fn unknown_is_unimplemented_for_now() {
        assert_eq!(decode(0x4E71), Op::Unimplemented, "NOP");
        // ... except the traps, which #16 wired up, and which sit right next to
        // NOP in the `$4E` group.
        assert_range_decodes_to(0x4E40..=0x4E4F, Op::Trap);
        assert_eq!(decode(0x4E3F), Op::Unimplemented, "just below the traps");
        assert_eq!(decode(0x4E50), Op::Unimplemented, "just above the traps");
        assert_eq!(decode(0x0000), Op::Unimplemented, "ORI.b #imm,D0");
        // The move groups — bits 15..12 = `0001`, `0010`, `0011` (`MOVE`,
        // `MOVEA`) and `0111` (`MOVEQ`) — left this list with #17, see
        // `move_family_is_not_unimplemented`, so the anchor is a
        // move-family opcode #17 deliberately left out: MOVEM is group `$48xx`
        // and belongs to M1-10 (#19).
        assert_eq!(decode(0x48E7), Op::Unimplemented, "MOVEM.L #(imm),(A7)-");
    }

    /// MOVE, MOVEA and MOVEQ are their own variants, not `Unimplemented`
    /// (M1-09, #17). The opcodes spell out the three things that make MOVE
    /// unlike its neighbours: the size field is `01` byte / `11` word / `10`
    /// long (M68000UM Format III), the *destination* is reg in bits 11..9 and
    /// mode in bits 8..6 (swapped relative to a normal EA field), and
    /// destination mode `001` is MOVEA rather than MOVE.
    #[test]
    fn move_family_is_not_unimplemented() {
        for (opcode, name) in [
            (0x1210u16, "MOVE.b (A0),D1"),
            (0x3210, "MOVE.w (A0),D1"),
            (0x2210, "MOVE.l (A0),D1"),
            (0x1038, "MOVE.b $2000.W,D0"),
            (0x123C, "MOVE.b #imm,D1"),
            (0x12D8, "MOVE.b (A0)+,(A1)+"),
            (0x3001, "MOVE.w D1,D0"),
            (0x3510, "MOVE.w (A0),-(A2)"),
            (0x20D8, "MOVE.l (A0)+,(A0)+"),
            (0x3040, "MOVEA.w D0,A0"),
            (0x2258, "MOVEA.l (A0)+,A1"),
            (0x3440, "MOVEA.w D0,A2"),
            (0x70FF, "MOVEQ #-1,D0"),
            (0x7EFF, "MOVEQ #-1,D7"),
        ] {
            assert_ne!(
                decode(opcode),
                Op::Unimplemented,
                "{name} (${opcode:04X}) must decode to its own variant"
            );
        }
    }

    /// The EA combinations the move class may not do are illegal instructions
    /// (vector [`crate::exceptions::EXC_ILLEGAL`]) decided at decode, so the
    /// handlers never have to re-check them (M68000PM/AD 2.2.12, and M68000UM
    /// Format III/IV; #17).
    #[test]
    fn move_illegal_ea_combinations_decode_as_illegal() {
        for (opcode, name) in [
            (0x1040u16, "MOVE.B D0,A0: byte MOVEA does not exist"),
            (0x1258, "MOVE.B (A0)+,A1: byte MOVEA does not exist"),
            (0x1009, "MOVE.B A1,D0: byte access to An"),
            (0x39C0, "MOVE.W D0,#imm: an immediate is not a destination"),
            (
                0x35C0,
                "MOVE.W D0,d16(PC): PC-relative is not a destination",
            ),
            (0x3DC0, "MOVE.W D0,(d8,PC,Xn)68020: not a 68000 mode"),
            (0x303E, "MOVE.W (d8,PC,Xn)68020,D0: not a 68000 mode"),
            (0x227E, "MOVEA.l (d8,PC,Xn)68020,A1: not a 68000 mode"),
            (0x71FF, "MOVEQ with bit 8 set: illegal on the 68000"),
            (0x7FFF, "MOVEQ with bit 8 set, D7"),
        ] {
            assert_eq!(
                decode(opcode),
                Op::Illegal,
                "{name} (${opcode:04X}) must take the illegal-instruction route"
            );
        }
    }

    /// The `1101` group splits on the operation-mode field in bits 8..6:
    /// `000`/`001`/`010` are `ADD <ea>,Dn` at byte/word/long, `100`/`101`/`110`
    /// the same three sizes as `ADD Dn,<ea>`, and `011`/`111` are `ADDA.W` and
    /// `ADDA.L` (M68000UM "ADD", "ADDA"; M1-10, #18). `$D089` is the reminder
    /// that a word or long `ADD` may *read* an address register.
    #[test]
    fn add_group_sizes_directions_and_adda() {
        for (opcode, name) in [
            (0xD001u16, "ADD.B D1,D0"),
            (0xD041, "ADD.W D1,D0"),
            (0xD081, "ADD.L D1,D0"),
            (0xD090, "ADD.L (A0),D0"),
            (0xD089, "ADD.L A1,D0"),
            (0xD03C, "ADD.B #imm,D0"),
            (0xD110, "ADD.B D0,(A0)"),
            (0xD150, "ADD.W D0,(A0)"),
            (0xD1A0, "ADD.L D0,-(A0)"),
            (0xD1B8, "ADD.L D0,$0000.W"),
            (0xD2C0, "ADDA.W D0,A1"),
            (0xD2C8, "ADDA.W A0,A1"),
            (0xD1C0, "ADDA.L D0,A0"),
            (0xD3C8, "ADDA.L A0,A1"),
        ] {
            assert_ne!(
                decode(opcode),
                Op::Unimplemented,
                "{name} (${opcode:04X}) must decode to its own variant"
            );
        }
        assert_eq!(decode(0xD001), Op::Add, "ADD.B D1,D0");
        assert_eq!(decode(0xD110), Op::Add, "ADD.B D0,(A0)");
        assert_eq!(decode(0xD2C8), Op::Adda, "ADDA.W A0,A1");
        assert_eq!(decode(0xD3C8), Op::Adda, "ADDA.L A0,A1");
    }

    /// `ADDX` is `1101 xxx 1 ss 00 m yyy`, so it lands inside `ADD`'s
    /// `100`..`110` operation modes and can only be told apart by its EA mode
    /// (`000`/`001`, from the forced `00` at bits 5..4). These two opcodes
    /// differ in bit 4 alone, and the classification order is what makes them
    /// different instructions: `$D100` is `ADDX.B D0,D0`, adding with the old
    /// `X` and leaving `Z` alone on a zero result, while `$D110` is
    /// `ADD.B D0,(A0)`, adding without the carry-in and setting `Z` normally.
    /// Classify `ADD` first and `$D100` silently becomes an `ADD` — same width,
    /// same registers, wrong arithmetic (M68000UM "ADDX"; M1-10, #18).
    #[test]
    fn addx_decodes_before_the_add_it_overlaps() {
        for (opcode, name) in [
            (0xD100u16, "ADDX.B D0,D0"),
            (0xD140, "ADDX.W D0,D0"),
            (0xD180, "ADDX.L D0,D0"),
            (0xD108, "ADDX.B -(A0),-(A0)"),
            (0xD34F, "ADDX.W -(A7),-(A1)"),
            (0xD909, "ADDX.B -(A1),-(A4) — measured in ADDX.b.json.gz"),
            (0xDD80, "ADDX.L D0,D6 — measured in ADDX.l.json.gz"),
        ] {
            assert_eq!(decode(opcode), Op::Addx, "{name} (${opcode:04X})");
        }
        // The near-misses: an EA mode outside `000`/`001` is a plain `ADD`.
        assert_eq!(decode(0xD110), Op::Add, "ADD.B D0,(A0)");
        assert_eq!(decode(0xD150), Op::Add, "ADD.W D0,(A0)");
        // ... and `ss` = `11` would make the operation-mode field `111`, which
        // is ADDA.L, so ADDX has no undefined size to reject.
        assert_eq!(decode(0xD1C0), Op::Adda, "ADDA.L D0,A0");
    }

    /// The EA combinations the add class may not do are illegal instructions
    /// decided at decode, so `crate::ops::arith` never re-checks them
    /// (M68000PM/AD 2.2.12; M1-10, #18).
    #[test]
    fn add_illegal_ea_combinations_decode_as_illegal() {
        for (opcode, name) in [
            (0xD008u16, "ADD.B A0,D0: An read a byte"),
            (0xD03D, "ADD.B (d8,PC,Xn)68020,D0: not a 68000 mode"),
            (0xD13D, "ADD.B D0,(d8,PC,Xn)68020: not a 68000 mode"),
            (0xD17C, "ADD.W D0,#imm: an immediate is not a destination"),
            (0xD1BA, "ADD.L D0,d16(PC): PC-relative is not a destination"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
    }

    /// `ADDI` is `$0600 + ss<<6 + <ea>` and its EA field names the destination,
    /// so it must be alterable. The rest of the `0000` group is other people's
    /// tickets and still decodes as unimplemented (M68000UM "ADDI"; #18).
    #[test]
    fn addi_decodes_and_validates_its_destination() {
        for (opcode, name) in [
            (0x0600u16, "ADDI.B #imm,D0"),
            (0x0640, "ADDI.W #imm,D0"),
            (0x0680, "ADDI.L #imm,D0"),
            (0x0610, "ADDI.B #imm,(A0)"),
            (0x0698, "ADDI.L #imm,(A0)+"),
            (0x06B8, "ADDI.L #imm,($0000).W"),
        ] {
            assert_eq!(decode(opcode), Op::Addi, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x0648u16, "ADDI.W #imm,A0: ADDI may not write An"),
            (0x067A, "ADDI.W #imm,d16(PC): not a destination"),
            (0x067C, "ADDI.W #imm,#imm: not a destination"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        // `ss` = `11` is not a size `ADDI` defines and no ticket has claimed
        // it, so it stays loud rather than guessed at.
        assert_eq!(decode(0x06C0), Op::Unimplemented, "ADDI with ss = 11");
        // Its neighbours in the `0000` group are untouched by this ticket. The
        // `$04` pattern was one of them until #19 claimed it: `$0400` is SUBI,
        // see `subi_decodes_and_validates_its_destination`.
        for (opcode, name) in [
            (0x0000u16, "ORI.B #imm,D0"),
            (0x0200, "ANDI.B #imm,D0"),
            (0x0700, "the pattern one size above ADDI"),
            (0x0800, "BTST #imm,D0"),
        ] {
            assert_eq!(decode(opcode), Op::Unimplemented, "{name} (${opcode:04X})");
        }
    }

    /// `ADDQ` is `0101 ddd 0 ss eeeeee` with `ss` != `11`, its quick data in
    /// bits 11..9 and data `000` meaning 8; `Scc`/`DBcc` (bit 8 set with `ss` =
    /// `11`) and the undefined `bit 8 clear, ss = 11` encodings stay
    /// unimplemented for their own tickets. `SUBQ`, the bit-8-set half of the
    /// group, is #19's and is covered by
    /// `subq_decodes_and_validates_its_destination` (M68000UM Format V, "ADDQ",
    /// and the operation-code-map appendix; M1-10, #18).
    #[test]
    fn addq_decodes_and_validates_its_destination() {
        for (opcode, name) in [
            (0x5080u16, "ADDQ.L #8,D0: data 000 means 8"),
            (0x5240, "ADDQ.W #1,D0"),
            (0x5618, "ADDQ.B #3,(A0)+"),
            (0x5092, "ADDQ.L #8,(A2): data 000 means 8 here too"),
            (0x52A0, "ADDQ.L #1,-(A0)"),
            (0x5E4A, "ADDQ.W #7,A2 — measured in ADD.w.json.gz"),
            (0x5248, "ADDQ.W #1,A0"),
            (0x5448, "ADDQ.W #2,A0"),
        ] {
            assert_eq!(decode(opcode), Op::Addq, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x5208u16, "ADDQ.B #1,A0: ADDQ to An is word or long only"),
            (0x527A, "ADDQ.W #1,d16(PC): not a destination"),
            (0x50BC, "ADDQ.L #8,#imm: not a destination"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x51C8u16, "Scc.EQ -(A0) — the branch ticket"),
            (0x51CC, "DBRA D1,d16 — the branch ticket"),
            (0x50C0, "bit 8 clear with ss = 11: defined by nobody yet"),
        ] {
            assert_eq!(decode(opcode), Op::Unimplemented, "{name} (${opcode:04X})");
        }
    }

    /// The `1001` group splits the same way as `1101`: operation modes
    /// `000`/`001`/`010` are `SUB <ea>,Dn` (`Dn - <ea> → Dn`), `100`/`101`/`110`
    /// are `SUB Dn,<ea>` (`<ea> - Dn → <ea>`), and `011`/`111` are `SUBA.W` and
    /// `SUBA.L` (M68000UM "SUB", "SUBA"; M1-11, #19). `$9089` is the reminder
    /// that a word or long `SUB` may *read* an address register.
    #[test]
    fn sub_group_sizes_directions_and_suba() {
        for (opcode, name) in [
            (0x9001u16, "SUB.B D1,D0"),
            (0x9041, "SUB.W D1,D0"),
            (0x9081, "SUB.L D1,D0"),
            (0x9090, "SUB.L (A0),D0"),
            (0x9089, "SUB.L A1,D0"),
            (0x903C, "SUB.B #imm,D0"),
            (0x9110, "SUB.B D0,(A0)"),
            (0x9150, "SUB.W D0,(A0)"),
            (0x91A0, "SUB.L D0,-(A0)"),
            (0x91B8, "SUB.L D0,$0000.W"),
            (0x92C0, "SUBA.W D0,A1"),
            (0x92C8, "SUBA.W A0,A1"),
            (0x91C0, "SUBA.L D0,A0"),
            (0x93C8, "SUBA.L A0,A1"),
        ] {
            assert_ne!(
                decode(opcode),
                Op::Unimplemented,
                "{name} (${opcode:04X}) must decode to its own variant"
            );
        }
        assert_eq!(decode(0x9001), Op::Sub, "SUB.B D1,D0");
        assert_eq!(decode(0x9110), Op::Sub, "SUB.B D0,(A0)");
        assert_eq!(decode(0x92C8), Op::Suba, "SUBA.W A0,A1");
        assert_eq!(decode(0x93C8), Op::Suba, "SUBA.L A0,A1");
    }

    /// `SUBX` is `1001 xxx 1 ss 00 m yyy`, so it lands inside `SUB`'s
    /// `100`..`110` operation modes exactly where `ADDX` lands inside `ADD`'s,
    /// and only the forced `00` at bits 5..4 tells them apart. `$9100` is
    /// `SUBX.B D0,D0` — subtract with the incoming `X` and a sticky `Z` — while
    /// `$9110` differs in bit 4 alone and is `SUB.B D0,(A0)`, a plain memory
    /// subtract (M68000UM "SUBX"; M1-11, #19).
    #[test]
    fn subx_decodes_before_the_sub_it_overlaps() {
        for (opcode, name) in [
            (0x9100u16, "SUBX.B D0,D0"),
            (0x9140, "SUBX.W D0,D0"),
            (0x9180, "SUBX.L D0,D0"),
            (0x9108, "SUBX.B -(A0),-(A0)"),
            (0x934F, "SUBX.W -(A7),-(A1)"),
            (0x9B0F, "SUBX.B -(A7),-(A5) — measured in SUBX.b.json.gz"),
            (0x9D83, "SUBX.L D3,D6 — measured in SUBX.l.json.gz"),
        ] {
            assert_eq!(decode(opcode), Op::Subx, "{name} (${opcode:04X})");
        }
        // The near-misses: an EA mode outside `000`/`001` is a plain `SUB`.
        assert_eq!(decode(0x9110), Op::Sub, "SUB.B D0,(A0)");
        assert_eq!(decode(0x9150), Op::Sub, "SUB.W D0,(A0)");
        // ... and `ss` = `11` would make the operation-mode field `111`, which
        // is SUBA.L, so SUBX has no undefined size to reject.
        assert_eq!(decode(0x91C0), Op::Suba, "SUBA.L D0,A0");
    }

    /// The EA combinations the subtract class may not do are illegal instructions
    /// decided at decode, so `crate::ops::arith` never re-checks them
    /// (M68000PM/AD 2.2.12; M1-11, #19).
    #[test]
    fn sub_illegal_ea_combinations_decode_as_illegal() {
        for (opcode, name) in [
            (0x9008u16, "SUB.B A0,D0: An read a byte"),
            (0x903D, "SUB.B (d8,PC,Xn)68020,D0: not a 68000 mode"),
            (0x913D, "SUB.B D0,(d8,PC,Xn)68020: not a 68000 mode"),
            (0x917C, "SUB.W D0,#imm: an immediate is not a destination"),
            (0x91BA, "SUB.L D0,d16(PC): PC-relative is not a destination"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
    }

    /// `SUBI` is `$0400 + ss<<6 + <ea>` and its EA field names the destination,
    /// so it must be alterable. The rest of the `0000` group is other people's
    /// tickets and still decodes as unimplemented (M68000UM "SUBI"; #19).
    #[test]
    fn subi_decodes_and_validates_its_destination() {
        for (opcode, name) in [
            (0x0400u16, "SUBI.B #imm,D0"),
            (0x0440, "SUBI.W #imm,D0"),
            (0x0480, "SUBI.L #imm,D0"),
            (0x0410, "SUBI.B #imm,(A0)"),
            (0x0498, "SUBI.L #imm,(A0)+"),
            (0x04B8, "SUBI.L #imm,($0000).W"),
        ] {
            assert_eq!(decode(opcode), Op::Subi, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x0448u16, "SUBI.W #imm,A0: SUBI may not write An"),
            (0x047A, "SUBI.W #imm,d16(PC): not a destination"),
            (0x047C, "SUBI.W #imm,#imm: not a destination"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        // `ss` = `11` is not a size `SUBI` defines and no ticket has claimed it.
        assert_eq!(decode(0x04C0), Op::Unimplemented, "SUBI with ss = 11");
    }

    /// `SUBQ` is `0101 ddd 1 ss eeeeee` with `ss` != `11`, its quick data in
    /// bits 11..9 and data `000` meaning 8. `Scc`/`DBcc` (bit 8 set with
    /// `ss` = `11`) stay unimplemented for the branch ticket, as does the
    /// `bit 8 clear, ss = 11` encoding `ADDQ` left alone too (M68000UM Format V,
    /// "SUBQ", and the operation-code-map appendix; M1-11, #19).
    #[test]
    fn subq_decodes_and_validates_its_destination() {
        for (opcode, name) in [
            (0x5180u16, "SUBQ.L #8,D0: data 000 means 8"),
            (0x5140, "SUBQ.W #1,D0"),
            (0x5318, "SUBQ.B #1,(A0)+"),
            (0x5192, "SUBQ.L #8,(A2): data 000 means 8 here too"),
            (0x53A0, "SUBQ.L #1,-(A0)"),
            (0x5F4A, "SUBQ.W #7,A2"),
            (0x5348, "SUBQ.W #1,A0"),
            (0x5548, "SUBQ.W #2,A0"),
        ] {
            assert_eq!(decode(opcode), Op::Subq, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x5308u16, "SUBQ.B #1,A0: SUBQ to An is word or long only"),
            (0x537A, "SUBQ.W #1,d16(PC): not a destination"),
            (0x51BC, "SUBQ.L #8,#imm: not a destination"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x51C8u16, "Scc.EQ -(A0) — the branch ticket"),
            (0x51CC, "DBRA D1,d16 — the branch ticket"),
            (0x50C0, "bit 8 clear with ss = 11: defined by nobody yet"),
        ] {
            assert_eq!(decode(opcode), Op::Unimplemented, "{name} (${opcode:04X})");
        }
    }

    /// The `1011` group splits on the operation-mode field in bits 8..6:
    /// `000`/`001`/`010` are `CMP <ea>,Dn` (`Dn - <ea>`, flags only), `011` and
    /// `111` are `CMPA.W`/`CMPA.L`, and `100`/`101`/`110` are EOR's
    /// (M68000UM "CMP", "CMPA"; M1-12, #20). `$B049` is the reminder that a word
    /// or long `CMP` may *read* an address register, and `$B009` — the same
    /// operand one byte down — is not an instruction at all.
    #[test]
    fn cmp_group_sizes_and_cmpa() {
        for (opcode, name) in [
            (0xB001u16, "CMP.B D1,D0"),
            (0xB041, "CMP.W D1,D0"),
            (0xB081, "CMP.L D1,D0"),
            (0xB090, "CMP.L (A0),D0"),
            (0xB049, "CMP.W A1,D0"),
            (0xB03C, "CMP.B #imm,D0: the 68000 runs it, measured in CMP.b"),
            (0xB0BA, "CMP.W d16(PC),D0"),
            (0xB2C0, "CMPA.W D0,A1"),
            (0xB2C8, "CMPA.W A0,A1"),
            (0xB1C0, "CMPA.L D0,A0"),
            (0xBFC8, "CMPA.L A0,A7"),
        ] {
            assert_ne!(
                decode(opcode),
                Op::Unimplemented,
                "{name} (${opcode:04X}) must decode to its own variant"
            );
        }
        assert_eq!(decode(0xB001), Op::Cmp, "CMP.B D1,D0");
        assert_eq!(decode(0xB2C8), Op::Cmpa, "CMPA.W A0,A1");
        assert_eq!(decode(0xB1C0), Op::Cmpa, "CMPA.L D0,A0");
        for (opcode, name) in [
            (0xB009u16, "CMP.B A1,D0: An read a byte"),
            (0xB03D, "CMP.B (d8,PC,Xn)68020,D0: not a 68000 mode"),
            (0xB07D, "CMP.W (d8,PC,Xn)68020,D0: not a 68000 mode"),
            (0xB0BD, "CMP.L (d8,PC,Xn)68020,D0: not a 68000 mode"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
    }

    /// `CMPM` is `1011 xxx 1 ss 001 yyy` — the address-register-direct case of
    /// EOR's half of the group, `1011 RRR 1ss eeeeee` (M68000UM "CMPM", "EOR").
    /// Neither bit 8 nor the size tells the two apart, so the EA mode has to be
    /// read before an opcode in `1ss` can be routed: `$B109` is
    /// `CMPM.B (A1)+,(A0)+`, comparing and stepping two pointers, while `$B101`
    /// is `EOR.B D1,D0`, which M1-13 owns and which must stay
    /// [`Op::Unimplemented`]. Claiming all of `1ss` for the compare would hand
    /// EOR's whole range to `op_cmpm` and nothing would notice until M1-13
    /// landed.
    ///
    /// Every CMPM opcode below is measured in the pinned suite (`CMP.b/w/l`,
    /// whose case names spell the mode out as `[CMP.b (A1)+, (A0)+]`); none of
    /// the EOR ones is — EOR never uses mode `001`, which is what makes the two
    /// sets disjoint.
    #[test]
    fn cmpm_decodes_before_eor() {
        for (opcode, name) in [
            (0xB108u16, "CMPM.B (A0)+,(A0)+"),
            (0xB109, "CMPM.B (A1)+,(A0)+"),
            (0xB10A, "CMPM.B (A2)+,(A0)+"),
            (0xB709, "CMPM.B (A1)+,(A3)+ — measured in CMP.b"),
            (0xB149, "CMPM.W (A1)+,(A0)+ — measured in CMP.w"),
            (0xB74C, "CMPM.W (A4)+,(A3)+"),
            (0xB188, "CMPM.L (A0)+,(A0)+"),
            (0xBF8F, "CMPM.L (A7)+,(A7)+ — measured in CMP.l"),
        ] {
            assert_eq!(decode(opcode), Op::Cmpm, "{name} (${opcode:04X})");
        }
        // Bit 8 set with any other EA mode is EOR, still unimplemented (M1-13),
        // including the data-register form CMPM's `001` is one bit away from.
        for (opcode, name) in [
            (0xB101u16, "EOR.B D1,D0"),
            (0xB110, "EOR.B D0,(A0)"),
            (0xB132, "EOR.B D0,(d8,A2,Xn) — measured in EOR.b"),
            (0xB36C, "EOR.W D1,(d16,A4) — measured in EOR.w"),
            (0xB782, "EOR.L D3,D2 — measured in EOR.l"),
        ] {
            assert_eq!(
                decode(opcode),
                Op::Unimplemented,
                "{name} (${opcode:04X}) must stay EOR's until M1-13"
            );
        }
        // And operation mode `111` is CMPA.L, not a fourth CMPM size: CMPA has to
        // be claimed before the `1ss` half is split at all, or `$B1C8` —
        // `ss` = `11`, mode `001` — would read as a compare-memory with an
        // undefined width.
        assert_eq!(decode(0xB1C8), Op::Cmpa, "CMPA.L A0,A0, not CMPM");
    }

    /// `CMPI` is `$0C00 + ss<<6 + <ea>`, the third immediate-operation pattern in
    /// the `0000` group after `SUBI` (`$0400`) and `ADDI` (`$0600`). Its EA field
    /// names the destination, which must be data-alterable; the 68000 does not
    /// allow PC-relative there either (M68000UM "CMPI", and M68000PM/AD 2.2.12).
    /// Measured in `CMP.b/w/l`, whose `$0Cxx` cases use Dn, the indirect modes
    /// and the two absolute forms and nothing else (M1-12, #20).
    #[test]
    fn cmpi_decodes_and_validates_its_destination() {
        for (opcode, name) in [
            (0x0C00u16, "CMPI.B #imm,D0"),
            (0x0C40, "CMPI.W #imm,D0"),
            (0x0C80, "CMPI.L #imm,D0"),
            (0x0C10, "CMPI.B #imm,(A0)"),
            (0x0C98, "CMPI.L #imm,(A0)+"),
            (0x0CB8, "CMPI.L #imm,($0000).W"),
        ] {
            assert_eq!(decode(opcode), Op::Cmpi, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x0C48u16, "CMPI.W #imm,A0: CMPI may not name An"),
            (
                0x0C7A,
                "CMPI.W #imm,d16(PC): the 68000 does not allow PC-relative here"
            ),
            (0x0C7C, "CMPI.W #imm,#imm: not an operand to compare against"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        // `ss` = `11` is not a size CMPI defines and no ticket has claimed it.
        assert_eq!(decode(0x0CC0), Op::Unimplemented, "CMPI with ss = 11");
    }

    /// `TST` is `0100 1010 ss eeeeee` with `ss` != `11`, so it occupies
    /// `$4A00`..`$4ABF` — and **`$4AC0`..`$4AFF` is `TAS`**, `ILLEGAL` included
    /// at `$4AFC`. That boundary in [`classify`] is what keeps this ticket out of
    /// M1-27's opcode space. A `TST` operand may not be An, PC-relative or an
    /// immediate on the 68000 (M68000PM section 8 "TST"; measured over
    /// `TST.b/w/l`, which use Dn, the five address-register-indirect modes and
    /// the two absolute forms and nothing else; M1-12, #20).
    #[test]
    fn tst_decodes_and_leaves_tas_alone() {
        for (opcode, name) in [
            (0x4A00u16, "TST.B D0"),
            (0x4A40, "TST.W D0"),
            (0x4A80, "TST.L D0"),
            (0x4A10, "TST.B (A0)"),
            (0x4A58, "TST.W (A0)+"),
            (0x4AA1, "TST.L -(A1)"),
            (0x4AB8, "TST.L ($0000).W"),
        ] {
            assert_eq!(decode(opcode), Op::Tst, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x4A08u16, "TST.B A0: An is not a TST operand on the 68000"),
            (0x4A3A, "TST.B d16(PC): neither is PC-relative here"),
            (0x4A3C, "TST.B #imm: nor an immediate"),
            (0x4A3D, "TST.B (d8,PC,Xn)68020: not a 68000 mode"),
            (0x4A49, "TST.W A0"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        // The `ss` = `11` sixth of `$4Axx` is TAS's and unclaimed; it stays loud
        // rather than becoming a TST with an undefined width.
        assert_range_decodes_to(0x4AC0..=0x4AFB, Op::Unimplemented);
        assert_eq!(decode(0x4AFC), Op::Illegal, "ILLEGAL, inside the TAS range");
        assert_range_decodes_to(0x4AFD..=0x4AFF, Op::Unimplemented);
    }

    /// Every opcode has an entry, and the entry is what [`classify`] says it
    /// should be — the table is the pattern, applied to all 65536 of them, not
    /// a list that can silently miss one.
    #[test]
    fn table_has_65536_entries() {
        let table = decode_table();
        assert_eq!(table.len(), 65536);
        assert_eq!(
            TABLE_ENTRIES,
            usize::from(u16::MAX) + 1,
            "one entry per opcode"
        );
        for opcode in 0..=u16::MAX {
            assert_eq!(
                table[usize::from(opcode)],
                classify(opcode),
                "entry ${opcode:04X} disagrees with the pattern"
            );
            assert_eq!(decode(opcode), classify(opcode));
        }
    }

    /// The table is built once, not per lookup and definitely not per CPU: two
    /// calls hand back the same static.
    #[test]
    fn table_is_built_once() {
        assert!(std::ptr::eq(decode_table(), decode_table()));
    }
}
