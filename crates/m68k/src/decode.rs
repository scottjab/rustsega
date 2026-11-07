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
//! | `1010` | line A (`1010` emulator) — the 68000 has no line emulator, so this is a reserved format: [`crate::ops::EXC_ILLEGAL`] |
//! | `1011` | `CMP`, `CMPA`, `CMPM`, `EOR` |
//! | `1100` | `AND`, `MUL`, `ABCD`, `EXG` |
//! | `1101` | `ADD`, `ADDA`, `ADDX` |
//! | `1110` | shifts and rotates |
//! | `1111` | line F (`1111` emulator) — reserved on the 68000 like `1010`: [`crate::ops::EXC_ILLEGAL`] |
//!
//! The whole map is documented here because every instruction ticket reads it
//! off this table, but only the two groups the core routes today (`1010`,
//! `1111`) plus the one documented illegal opcode are acted on; every other
//! group decodes to [`Op::Unimplemented`] and panics loudly until its ticket
//! lands (M1-08, #16 onward).
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
    /// exception (vector [`crate::ops::EXC_ILLEGAL`]).
    Illegal,
    /// An opcode in the `1010` group (M68000UM Format I, "Line 1010 emulator
    /// instruction"). The 68000 has no line emulator, so it takes
    /// [`crate::ops::EXC_ILLEGAL`] like any other reserved opcode; the variant
    /// is separate from [`Op::Illegal`] because the 68010 (M1-12, #20) does
    /// have a line-A vector and will route the two differently.
    LineA,
    /// An opcode in the `1111` group (M68000UM Format J, "Line 1111 emulator
    /// instruction"). [`Op::LineA`] in every respect on the 68000 — illegal
    /// instruction — and separate for the same 68010 reason.
    LineF,
}

/// The one opcode the manual spells out as illegal: `ILLEGAL`, `$4AFC`
/// (M68000PM/AD section 8, "ILLEGAL — Illegal Instruction"; it exists so
/// software and simulators can force the exception). A reserved or undefined
/// encoding that is not this one still decodes to [`Op::Unimplemented`] until
/// the ticket that owns its group says otherwise.
const ILLEGAL_OPCODE: u16 = 0x4AFC;

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
        // Every other group is a real instruction whose handler has not been
        // written yet; the tickets that write them split these groups up.
        _ => Op::Unimplemented,
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
    /// This test is expected to change as instructions land: when `NOP` is
    /// implemented it decodes to its own variant, and the ticket that does it
    /// moves this assertion to an opcode still unimplemented. That licence is
    /// this test alone (ticket #15).
    #[test]
    fn unknown_is_unimplemented_for_now() {
        assert_eq!(decode(0x4E71), Op::Unimplemented, "NOP");
        assert_eq!(decode(0x0000), Op::Unimplemented, "ORI.b #imm,D0");
        assert_eq!(decode(0x3280), Op::Unimplemented, "MOVE.l (A0),D1");
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
