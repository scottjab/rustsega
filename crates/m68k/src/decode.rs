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
//! in `0100`, and the one documented illegal opcode. Every other group decodes
//! to [`Op::Unimplemented`] and panics loudly until its ticket lands (M1-08, #16
//! onward).
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
        // The groups either side of them are not: `1001` is SUB/SUBA/SUBX and
        // `0000` is the bit-operation group, so both stay unimplemented here.
        assert_eq!(decode(0x9FFF), Op::Unimplemented);
        assert_eq!(decode(0xB000), Op::Unimplemented);
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
