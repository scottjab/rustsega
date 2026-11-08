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

use crate::ea::{Size, is_control, is_data, is_data_alterable};
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
    /// `CMP <ea>,Dn`: the `1011` group's operation modes `000`/`001`/`010`
    /// (M68000UM "CMP — Compare", #20). `Dn - <ea>` is computed for its flags
    /// and thrown away, so unlike [`crate::ops::arith::op_sub`] it writes
    /// nothing and takes no copy of the borrow into `X`.
    Cmp,
    /// `CMPA <ea>,An`: the `1011` group's operation modes `011` (word, source
    /// sign-extended to 32 bits) and `111` (long). Destination is the address
    /// register in bits 11..9; `X` is unaffected (M68000UM "CMPA — Compare
    /// Address", #20).
    Cmpa,
    /// `CMPM (Ay)+,(Ax)+`: `1011 xxx 1 ss 001 yyy` (M68000UM "CMPM — Compare
    /// Memory", #20). It lives inside EOR's `1011 RRR 1ss eeeeee` half of the
    /// group and is told apart by its forced mode `001`, which EOR never uses;
    /// [`crate::ops::arith::op_cmpm`] re-reads both registers from bits 11..9
    /// and 2..0, as [`Op::Subx`] does.
    Cmpm,
    /// `CMPI #imm,<ea>`, `$0C00 + ss<<6 + <ea>` (M68000UM "CMPI — Compare
    /// Immediate", #20). The immediate is the operand encoded in the EA field as
    /// mode `111` register `100` and is fetched before the destination's own
    /// extension words, exactly as in [`Op::Subi`].
    Cmpi,
    /// `TST <ea>`, `$4A00 + ss<<6 + <ea>` with `ss` != `11` (M68000UM "TST —
    /// Test", #20). Sets N and Z from the operand, clears V and C, leaves `X`
    /// alone, and writes nothing back.
    Tst,
    /// `AND <ea>,Dn` and `AND Dn,<ea>`: the `1100` group's operation modes
    /// `000`/`001`/`010` and `100`/`101`/`110` (M68000UM AND, #21). Re-reads the
    /// size, direction, register and EA from the opcode bits exactly as
    /// [`Op::Add`] does; see [`crate::ops::logic`].
    And,
    /// `OR <ea>,Dn` and `OR Dn,<ea>`: the `1000` group's operation modes
    /// `000`/`001`/`010` and `100`/`101`/`110` (M68000UM OR, #21). [`Op::And`]
    /// in every respect but the operation.
    Or,
    /// `EOR Dn,<ea>`: `1011 RRR 1ss eeeeee`, the half of the `1011` group that
    /// is not [`Op::Cmpm`] (M68000UM EOR, #21). EOR is read-modify-write into
    /// the EA and has no `EOR <ea>,Dn` form, which is why the group splits on
    /// bit 8 rather than on a destination register.
    Eor,
    /// `NOT <ea>`, `$4600 + ss<<6 + <ea>` with `ss` != `11` (M68000UM NOT, #21).
    /// It reads, inverts and writes back one operand; `ss` = `11` is MOVE to
    /// CCR/SR's half of `$46xx` (#34).
    Not,
    /// `ANDI #imm,<ea>`, `$0200 + ss<<6 + <ea>` (M68000UM ANDI, #21). The
    /// immediate is the operand encoded in the EA field as mode `111` register
    /// `100` and is fetched before the destination's own extension words, as in
    /// [`Op::Addi`]. `ss` = `11` and mode `111` register `100` are the to-SR and
    /// to-CCR forms (#34).
    Andi,
    /// `ORI #imm,<ea>`, `$0000 + ss<<6 + <ea>` (M68000UM ORI, #21). [`Op::Andi`]
    /// with a different operation and base opcode; `ddd` != `000` in this range
    /// is nothing at all, because `BTST D0,<ea>` has no encoding.
    Ori,
    /// `EORI #imm,<ea>`, `$0A00 + ss<<6 + <ea>` (M68000UM EORI, #21).
    /// [`Op::Andi`] with a different operation and base opcode.
    Eori,
    /// `NEG <ea>`, `$4400 + ss<<6 + <ea>` with `ss` != `11` (M68000UM NEG,
    /// #22). `0 - <ea>` written back to the same data-alterable operand, so it
    /// is [`Op::Not`]'s shape with [`crate::ops::arith`]'s subtract flags.
    Neg,
    /// `NEGX <ea>`, `$4000 + ss<<6 + <ea>` with `ss` != `11` (M68000UM NEGX,
    /// #22). `0 - <ea> - X`, so it takes `SUBX`'s sticky `Z` as well
    /// ([`Op::Addx`]'s add-side twin).
    Negx,
    /// `CLR <ea>`, `$4200 + ss<<6 + <ea>` with `ss` != `11` (M68000UM CLR,
    /// #22). Zero to a data-alterable operand; on the 68000 the write to memory
    /// is preceded by a read of the same location, which is not an optimisation
    /// to skip.
    Clr,
    /// `EXT.W Dn` / `EXT.L Dn`, `$4880 + ss<<6 + r` (M68000UM EXT, #22). Bit 6
    /// is the size — clear for the word, set for the long — and `r` is the only
    /// operand, which is why no EA predicate applies and why mode `000` is all
    /// of this opcode's EA field.
    Ext,
    /// `SWAP Dn`, `$4840 + r` (M68000UM SWAP, #22). The other instruction that
    /// bit 6 of the `$48??` window leaves free, and the one place a `Dn`-only
    /// opcode lives next to `PEA` and `MOVEM` — see [`classify_swap_ext_pea`].
    Swap,
    /// `EXG Rx,Ry`: `1100 xxx 1 ooooo yyy` (M68000UM EXG, #22). The slot
    /// #21 reserved inside AND's group: operation `101` at mode `000` (`Dx,Dy`)
    /// or `001` (`Ax,Ay`), and operation `110` at mode `001` (`Dx,Ay`). Both
    /// operands are registers, so there is no EA to validate, and no flag is
    /// affected.
    Exg,
    /// `LEA <ea>,An`, `$4000 + ddd<<9 + 0b111<<6 + <ea>` — the `0100` group's
    /// opcodes with bits 8..6 all set (M68000UM "LEA — Address Register Indirect
    /// and Program", M1-15, #23). The destination is always an address register
    /// and the source is the *address* the EA field computes, so only the control
    /// modes of [`crate::ea::is_control`] are legal and no flag is affected.
    Lea,
    /// `PEA <ea>`: PEA's half of the `$48??` window, `$4840 + <ea>` at a control
    /// mode (M68000UM "PEA — Push Effective Address", M1-15, #23). [`Op::Lea`]'s
    /// operand rule with the result pushed onto the stack instead of written to a
    /// register; see [`classify_swap_ext_pea`] for how the window splits.
    Pea,
    /// `LINK An,d16`, `$4E50 + r` (M68000UM "LINK — Link Address Register",
    /// M1-15, #23). Push `An`, make `An` point at what it pushed, then move the
    /// stack pointer over `d16` of local space: one extension word, no EA field.
    Link,
    /// `UNLK An` — `UNLINK` in the manual and in the suite's file name —
    /// `$4E58 + r` (M68000UM "UNLINK — Unlink Address Register", M1-15, #23).
    /// The one-word teardown of [`Op::Link`].
    Unlk,
    /// `Bcc d8`/`Bcc d16`, and `BRA` as its `cccc` = `0000` case: the whole
    /// `0110` group except condition code `0001`, which is [`Op::Bsr`]
    /// (M68000UM Format VI, "Bcc — Branch Conditional", M1-16, #24). The
    /// condition code and the displacement are both re-read from the opcode,
    /// and a zero byte displacement means a displacement word follows.
    Bcc,
    /// `BSR d8`/`BSR d16`: `0110 0001 dddddddd`, `Bcc` with condition code
    /// `0001` and a long push of the return address first (M68000UM "BSR —
    /// Branch to Subroutine", M1-16, #24). Its `RTS` arrived with #25.
    Bsr,
    /// `Scc <ea>` — `0101 cccc 11 eeeeee` at every EA mode but `001`, which is
    /// [`Op::Dbcc`] (M68000UM Format V, "Scc — Set Conditional", M1-16, #24).
    /// Writes `$FF` when [`crate::sr::test_cc`] says the condition holds and
    /// `$00` when it does not; byte only.
    Scc,
    /// `DBcc Dn,d16` — `0101 cccc 11 001 rrr`, the mode-`001` sixth of
    /// [`Op::Scc`]'s opcode (M68000UM "DBcc — Decrement and Branch", M1-16,
    /// #24). The `rrr` is the loop counter, and the `eeeeee` that would be the
    /// EA is the first bits of the displacement word instead.
    Dbcc,
    /// `JMP <ea>` — `0100 1110 11 mmm rrr` (M68000UM "JMP — Jump", M1-17, #25).
    /// `PC` becomes the address the operand field *computes*, so as with
    /// [`Op::Lea`] and [`Op::Pea`] the handler reads it through
    /// [`crate::M68k::ea_address`] and the target keeps the bits the bus would
    /// drop (#193). Only the [control addresses](crate::ea::is_control) are
    /// encodable, and `decode` has already rejected the other 36 operand fields.
    Jmp,
    /// `JSR <ea>` — `0100 1110 10 mmm rrr` (M68000UM "JSR — Jump to Subroutine",
    /// M1-17, #25). [`Op::Jmp`]'s operand, and the operand is legal or illegal in
    /// exactly the same cases, plus a long push of the address of the
    /// instruction after it.
    Jsr,
    /// `RTS` — `$4E75` (M68000UM "RTS — Return from Subroutine", M1-17, #25).
    /// Pop a long into `PC`; no flag moves.
    Rts,
    /// `RTR` — `$4E77` (M68000UM "RTR — Return and Restore Condition Code",
    /// M1-17, #25). Pop a word into the CCR — five bits, the upper byte of `SR`
    /// stays as it was — and then a long into `PC`.
    Rtr,
    /// `RTE` — `$4E73` (M68000UM "RTE — Return from Exception", M1-17, #25).
    /// Privileged: user mode raises [`crate::exceptions::EXC_PRIVILEGE`]. The word
    /// it pops goes into the whole of `SR`, so this is the only instruction so far
    /// that can move the active stack pointer; then a long into `PC`.
    Rte,
    /// `NOP` — `$4E71` (M68000UM "NOP — No Operation", M1-17, #25). Advances
    /// past the opcode and nothing else. `$4E71` was the
    /// [`Op::Unimplemented`](Op::Unimplemented) anchor of
    /// `decode::tests::unknown_is_unimplemented_for_now` until this variant
    /// landed, which moved that anchor to `RESET` at `$4E70`.
    Nop,
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
        // `0100 1110 0101 0nnn` and `0100 1110 0101 1nnn`: LINK and UNLK sit next
        // to each other above the traps, and `r` is their only operand
        // (M68000UM LINK, UNLINK). `$4E60..=$4E6F` between them and RESET is
        // unassigned, so the two ranges stop where they do.
        0x4E50..=0x4E57 => Op::Link,
        0x4E58..=0x4E5F => Op::Unlk,
        // `0100 1110 11 mmm rrr` and `0100 1110 10 mmm rrr`: JMP and JSR take
        // the whole `$4E80..=$4EFF` window between them and differ in one bit of
        // the operand field, so the window is entered whole and split by the
        // addressing mode — see [`classify_control_jump`] (M68000UM JMP, JSR;
        // M1-17, #25).
        0x4E80..=0x4EFF => classify_control_jump(opcode),
        // The four one-word members of the `$4E` group. Between them sit
        // `RESET` `$4E70` (M1-27), `STOP` `$4E72` (M1-26), TRAPV `$4E76` (#20)
        // and the reserved `$4E78`/`$4E79`, none of which may be pulled in here;
        // listing the four opcodes rather than a range is what keeps it that way
        // (M68000UM NOP, RTE, RTS, RTR; M1-17, #25).
        0x4E71 => Op::Nop,
        0x4E73 => Op::Rte,
        0x4E75 => Op::Rts,
        0x4E77 => Op::Rtr,
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
        // `0000 1100 ss eeeeee`: `CMPI` is the third instruction of that shape,
        // after SUBI at `$0400` and ADDI at `$0600`.
        0x0C00..=0x0CFF => classify_cmpi(opcode),
        // `0101 ddd 0 ss eeeeee` is ADDQ and `0101 ddd 1 ss eeeeee` is SUBQ —
        // unless `ss` = `11`, which is not a size: `ss` = `11` is Format V,
        // `Scc`/`DBcc`, and it takes both halves of bit 8 because there the bit
        // is the low bit of the condition code (M68000UM Format V; M1-16, #24).
        0x5000..=0x5FFF => {
            if opcode & SCC_DBCC_BITS == SCC_DBCC_BITS {
                classify_scc_dbcc(opcode)
            } else if opcode & QUICK_BIT8 == 0 {
                classify_addq(opcode)
            } else {
                classify_subq(opcode)
            }
        }
        // `0110 cccc dddddddd`: the whole branch group. The low byte is a
        // displacement and not an effective address, so no opcode of the group is
        // an illegal operand combination — see [`classify_bcc`] (M68000UM Format
        // VI; M1-16, #24).
        0x6000..=0x6FFF => classify_bcc(opcode),
        // `1101 ddd ooo eeeeee`: ADD, ADDA and ADDX.
        0xD000..=0xDFFF => classify_add(opcode),
        // `1001 ddd ooo eeeeee`: SUB, SUBA and SUBX.
        0x9000..=0x9FFF => classify_sub(opcode),
        // `1011 ddd ooo eeeeee`: CMP, CMPA, CMPM and EOR.
        0xB000..=0xBFFF => classify_cmp(opcode),
        // `0100 1010 ss eeeeee`: TST. `ss` = `11` is the top sixth of `$4Axx`,
        // `$4AC0..=$4AFF`, which is TAS's (M1-27) and `ILLEGAL` at `$4AFC`, so
        // the range stops dead at `$4ABF` rather than running to `$4AFF`.
        0x4A00..=0x4ABF => classify_tst(opcode),
        // `0100 0110 ss eeeeee`: NOT. `ss` = `11` — `$46C0..=$46FF` — is MOVE to
        // CCR and MOVE to SR (#34), so the range stops dead at `$46BF`, exactly
        // as TST's stops before TAS.
        0x4600..=0x46BF => classify_not(opcode),
        // `0100 0000`, `0100 0010` and `0100 0100`, each `+ ss<<6 + <ea>`:
        // NEGX, CLR and NEG, the three single-operand read-modify-writes of the
        // `0100` group that take NOT's operand rule (M68000UM NEG, NEGX, CLR;
        // M1-14, #22). As in NOT and TST, `ss` = `11` is left out of the range:
        // `$40C0..=$40FF` is MOVE from SR's sixth (#34) and `$42C0..=$42FF` and
        // `$44C0..=$44FF` are unassigned on the 68000.
        0x4000..=0x40BF => classify_single_operand(opcode, Op::Negx),
        0x4200..=0x42BF => classify_single_operand(opcode, Op::Clr),
        0x4400..=0x44BF => classify_single_operand(opcode, Op::Neg),
        // `0100 ddd 111 eeeeee`: LEA. It is the reason TST, NOT, NEGX, CLR and
        // NEG above stop at `??BF` instead of taking their whole `$xx00` block —
        // bits 8..6 all set is LEA's, and nothing else in the `0100` group uses
        // that triple (M68000UM LEA, and the instruction-set summary that lists
        // `CHK` as `00`, `JMP` as `100` and `JSR` as `110` there). The eighth of
        // the group, `$4800..=$48FF`, is bit 8 clear throughout, so LEA cannot
        // reach SWAP, EXT, PEA or MOVEM.
        0x4000..=0x4FFF if lea_bits(opcode) => classify_lea(opcode),
        // `0100 1000 0100 0 rrr` is SWAP, `0100 1000 1s 000 rrr` is EXT and
        // `0100 1000 01 mmm rrr` (`mmm` != `000`) is PEA; the rest of the
        // `$4800..=$48FF` window is MOVEM (M1-24, #32) and unassigned space, so
        // this window is the one place a group is entered whole and then split by
        // EA field — see [`classify_swap_ext_pea`], #22 and #23.
        0x4800..=0x48FF => classify_swap_ext_pea(opcode),
        // `0000 0000 ss eeeeee`, `0000 0010 ss eeeeee` and `0000 1010 ss eeeeee`:
        // ORI, ANDI and EORI, the three `#imm,<ea>` logic operations. Bit 8 is
        // clear throughout all three ranges, which is what keeps them apart from
        // the bit operations (`0000 ddd 1oo eeeeee` with `ddd` != `000`).
        0x0000..=0x00FF => classify_logic_imm(opcode, Op::Ori),
        0x0200..=0x02FF => classify_logic_imm(opcode, Op::Andi),
        0x0A00..=0x0AFF => classify_logic_imm(opcode, Op::Eori),
        // `1000 ddd ooo eeeeee`: OR, and DIV/SBCD's space.
        0x8000..=0x8FFF => classify_or(opcode),
        // `1100 ddd ooo eeeeee`: AND, and MUL/ABCD/EXG's space.
        0xC000..=0xCFFF => classify_and(opcode),
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

/// `0101 cccc 11 eeeeee`: bits 7..6 of Format V set, which is how the `0101`
/// group's `ss` = `11` sixth is recognised before `ADDQ`/`SUBQ` get to read it as
/// a size (M68000UM Format V; M1-16, #24).
const SCC_DBCC_BITS: u16 = 0x00C0;

/// The `Scc` EA mode that is not an effective address: `DBcc`'s register field
/// (`0101 cccc 11 001 rrr`, M68000UM "DBcc"). `ADDQ`'s `AN_DIRECT_MODE` happens to
/// be the same number and means something completely different there.
const DBCC_MODE: u8 = 0b001;

/// The condition code `0001` — false — which in the branch group is the opcode
/// that pushes: `BSR` (M68000UM "BSR").
const BSR_CC: u16 = 0b0001;

/// One opcode of Format V, `0101 cccc 11 eeeeee`: [`Op::Dbcc`] where the EA mode
/// is `001` and [`Op::Scc`] everywhere else.
///
/// The split is the whole of the distinction between the two instructions: for
/// `DBcc` the `eee` register field is the loop counter and the remaining `eee`
/// bits are the top of a 16-bit displacement, so no `DBcc` opcode carries an
/// effective address and none can be illegal (M68000UM "DBcc", "Scc"). `Scc`
/// does write memory, so it takes the usual destination rule — it is a byte
/// write, exactly `CLR`'s (#22).
fn classify_scc_dbcc(opcode: u16) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if mode == DBCC_MODE {
        return Op::Dbcc;
    }
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Scc
}

/// One opcode of Format VI, `0110 cccc dddddddd`: [`Op::Bsr`] for condition code
/// `0001` and [`Op::Bcc`] for the other fifteen, `0000` (`BRA`) included —
/// `BRA` is not a separate instruction on the 68000, which is why `Bcc.json.gz`
/// contains it 512 times (M68000UM "BRA — Branch", "Bcc — Branch Conditional";
/// M1-16, #24).
fn classify_bcc(opcode: u16) -> Op {
    if (opcode >> 8) & 0b1111 == BSR_CC {
        Op::Bsr
    } else {
        Op::Bcc
    }
}

/// One opcode of the `0101` group with bit 8 set: [`Op::Subq`].
///
/// `ss` = `11` never reaches it: [`SCC_DBCC_BITS`] takes those to
/// [`classify_scc_dbcc`] first.
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

/// One opcode of the `0101` group with bit 8 clear: [`Op::Addq`].
///
/// `ss` = `11` no longer reaches it — #24 took that sixth of the group to
/// [`classify_scc_dbcc`] — so the [`Op::Unimplemented`] arm below, which used to be
/// how those opcodes were reported, is unreachable in practice. It stays because
/// [`Size::from_bits`] is total over the two bits.
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

/// One opcode of the `1011` group: [`Op::Cmp`], [`Op::Cmpa`], [`Op::Cmpm`] or
/// [`Op::Eor`].
///
/// The group has three shapes sharing one operation-mode field in bits 8..6
/// (M68000UM, "Instruction Formats", Format III and the operation-code-map
/// appendix):
///
/// ```text
/// CMP    1011 RRR 0ss eeeeee    operation 000 / 001 / 010
/// CMPA   1011 RRR 011 eeeeee    and 1011 RRR 111 eeeeee
/// CMPM   1011 xxx 1ss 001 yyy   forced mode 001 inside EOR's half
/// EOR    1011 RRR 1ss eeeeee    operation 100 / 101 / 110
/// ```
///
/// CMPA is taken first because its long form has bit 8 set: testing EOR's half
/// as a whole would claim `$B1C0` (`CMPA.L D0,A0`) for EOR. CMPM is then the
/// `mode == 001` case of what is left, and only that case — EOR uses every other
/// mode, and mode `001` is not one of EOR's (M68000PM "EOR", destination "data
/// alterable"), which is what makes the two sets disjoint rather than merely
/// ordered.
fn classify_cmp(opcode: u16) -> Op {
    let op_mode = ((opcode >> 6) & 0b111) as u8;
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;

    // `011` and `111` are CMPA, whose destination is the address register in
    // bits 11..9 and whose source may be any data operand: `CMPA.W A0,A1` is
    // legal (M68000UM "CMPA", source "data").
    if op_mode == 0b011 || op_mode == 0b111 {
        if !is_data(mode, reg) {
            return Op::Illegal;
        }
        return Op::Cmpa;
    }
    if op_mode < 0b100 {
        // `CMP <ea>,Dn`: the destination is a data register by construction, so
        // only the source can be illegal, and an address register read a byte is
        // the same rejection `MOVE`, `ADD` and `SUB` make for the identical
        // reason. Unlike `SUB`, this side never writes, so `is_data` is enough.
        let Some(size) = Size::from_bits(u16::from(op_mode & 0b11)) else {
            // `011` was the only undefined width below `100` and it left above.
            return Op::Illegal;
        };
        if !is_data(mode, reg) || (size == Size::Byte && mode == AN_DIRECT_MODE) {
            return Op::Illegal;
        }
        return Op::Cmp;
    }
    // EOR's half, `1ss`. CMPM is `1011 xxx 1 ss 001 yyy`: both operands are
    // `(An)+`, the source's register is in bits 2..0 and the destination's in
    // bits 11..9, so no EA bits are left to read and every mode-`001` opcode
    // here is a CMPM whatever its size.
    if mode == AN_DIRECT_MODE {
        return Op::Cmpm;
    }
    // What is left is EOR (`1011 RRR 1ss eeeeee`): `Dn ^ <ea> → <ea>`, so the EA
    // names a read-modify-write destination and has to be data-alterable — the
    // one property that made mode `001` free for CMPM above (M68000UM EOR,
    // destination "data alterable", #21).
    if is_data_alterable(mode, reg) {
        Op::Eor
    } else {
        Op::Illegal
    }
}

/// One opcode of the `1100` group: [`Op::And`], [`Op::Exg`], or
/// [`Op::Unimplemented`] for the three corners of the group that MULU, MULS and
/// ABCD own.
///
/// AND is `1100 ddd ooo eeeeee` with `ooo` = `000`/`001`/`010` for
/// `AND <ea>,Dn` and `100`/`101`/`110` for `AND Dn,<ea>` (M68000UM AND, #21).
/// Everything else in the group is claimed:
///
/// ```text
/// MULU   1100 ddd 011 eeeeee    M1-14
/// MULS   1100 ddd 111 eeeeee    M1-14
/// ABCD   1100 ddd 100 00 m yyy  M1-16
/// EXG    1100 xxx 101 000 yyy   #22, and 101 001 and 110 001
/// ```
///
/// So the operation-mode test comes first, then the corners of the `1ss` half.
/// EXG's three forms, read off `EXG.json.gz` — its opcodes are exactly
/// `$Cx40`..`$xC4F` and `$Cx88`..`$C8F` for every destination — occupy operation
/// `101` at modes `000` and `001` **and** operation `110` at mode `001`.
/// Everything else in the `1ss` half is AND, including operation `110` mode
/// `000`, which is `AND.L Dn,Dn` and is not EXG.
fn classify_and(opcode: u16) -> Op {
    let op_mode = ((opcode >> 6) & 0b111) as u8;
    let mode = ((opcode >> 3) & 0b111) as u8;
    let mul = matches!(op_mode, 0b011 | 0b111);
    let abcd = op_mode == 0b100 && mode <= AN_DIRECT_MODE;
    if mul || abcd {
        return Op::Unimplemented;
    }
    // EXG, the slot #21 reserved for this ticket: `101`/`000` is `EXG Dx,Dy`
    // (`c140`), `101`/`001` is `EXG Ax,Ay` (`c94a`) and `110`/`001` is
    // `EXG Dx,Ay` (`c18f`). `is_exg` is the same test [`crate::ops::logic`] uses
    // to pick the two register files apart, so the two cannot drift.
    if is_exg(op_mode, mode) {
        return Op::Exg;
    }
    classify_and_or(opcode, op_mode, Op::And)
}

/// The `1100` group's three EXG operand forms: operation `101` at mode `000`
/// (`Dx,Dy`) or `001` (`Ax,Ay`), and operation `110` at mode `001` (`Dx,Ay`)
/// (M68000UM EXG; #22, measured against `EXG.json.gz`).
///
/// Shared between [`classify_and`] and [`crate::ops::logic::M68k::op_exg`],
/// which has to know which of the two registers names an address register so the
/// stored half of `A7` moves with it.
pub(crate) const fn is_exg(op_mode: u8, mode: u8) -> bool {
    (op_mode == 0b101 && mode <= 0b001) || (op_mode == 0b110 && mode == 0b001)
}

/// One opcode of the `1000` group: [`Op::Or`], or [`Op::Unimplemented`] for
/// DIVU, DIVS and SBCD's corner of it.
///
/// ```text
/// OR     1000 ddd ooo eeeeee    operation 000 / 001 / 010 and 100 / 101 / 110
/// DIVU   1000 ddd 011 eeeeee    M1-15
/// DIVS   1000 ddd 111 eeeeee    M1-15
/// SBCD   1000 ddd 100 00 m yyy  M1-16
/// ```
///
/// Unlike [`classify_and`], only operation `100` loses its mode-`000` case: OR
/// has no EXG in its group, so `OR.W D0,D1` — `1000 ddd 101 000 yyy` — is a
/// plain read-modify-write into a data register (M68000UM OR, destination "data
/// alterable", #21).
fn classify_or(opcode: u16) -> Op {
    let op_mode = ((opcode >> 6) & 0b111) as u8;
    let mode = ((opcode >> 3) & 0b111) as u8;
    let div = matches!(op_mode, 0b011 | 0b111);
    let sbcd = op_mode == 0b100 && mode <= AN_DIRECT_MODE;
    if div || sbcd {
        return Op::Unimplemented;
    }
    classify_and_or(opcode, op_mode, Op::Or)
}

/// The operand rules `AND` and `OR` share (M68000UM AND, OR, #21): the `0ss`
/// half reads an EA into a data register, the `1ss` half writes the result back
/// to a data-alterable EA.
fn classify_and_or(opcode: u16, op_mode: u8, op: Op) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    let Some(size) = Size::from_bits(u16::from(op_mode & 0b11)) else {
        // `011` and `111` — MUL, DIV — were taken by the caller.
        return Op::Illegal;
    };
    if op_mode < 0b100 {
        // `AND <ea>,Dn`/`OR <ea>,Dn`: the destination is a data register by
        // construction, so only the source can be illegal, and a byte read of an
        // address register is the rejection `MOVE`, `ADD` and `SUB` make.
        if !is_data(mode, reg) || (size == Size::Byte && mode == AN_DIRECT_MODE) {
            return Op::Illegal;
        }
    } else if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    op
}

/// `$4600 + ss<<6 + <ea>`: `NOT <ea>` (M68000UM NOT, #21).
///
/// `classify` only routes `$4600..=$46BF` here, so `ss` is never `11` and MOVE to
/// CCR/SR is out of reach. NOT's operand is read and written back, so it is
/// [`is_data_alterable`]'s set, exactly as `classify_tst` found for the
/// read-only case: `NOT.b/w/l` use Dn, the four indirect modes and the two
/// absolutes and nothing else.
fn classify_not(opcode: u16) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Not
}

/// `$4000`/`$4200`/`$4400` `+ ss<<6 + <ea>`: `NEGX`, `CLR` and `NEG`, which are
/// [`classify_not`]'s instruction shape with a different operation (M68000UM
/// NEG, NEGX, CLR; #22). All three read one operand and write it back, so the
/// operand set is `is_data_alterable`'s again: `Dn`, the four indirect modes and
/// the two absolutes. That is not a guess — `NEG.b`, `NEGX.w` and `CLR.l` each
/// measure exactly those eight modes over their 8065 cases and no `An`,
/// PC-relative or immediate operand.
///
/// `op` says which of the three the caller's range is; nothing else differs.
/// `classify` routes only `ss` != `11` here, so MOVE from SR (`$40C0`) and the
/// two unassigned sixths cannot reach the handlers.
fn classify_single_operand(opcode: u16, op: Op) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    op
}

/// Bits 8..6 of a `0100`-group opcode: set all three and it is
/// `LEA <ea>,An`, `0100 ddd 111 eeeeee` (M68000UM "LEA — Address Register
/// Indirect and Program"). Every other member of the group — NEGX, CLR, NEG,
/// NOT, TST, SWAP, EXT, PEA, TAS, CHK, MOVEM, MOVE, JSR, JMP and the traps —
/// leaves at least one of the three clear, which is what lets the single-operand
/// ranges in [`classify`] stop at `??BF`; `LEA.json.gz` is the only suite file
/// with an opcode of the shape.
const fn lea_bits(opcode: u16) -> bool {
    (opcode >> 6) & 0b111 == 0b111
}

/// One opcode of the `LEA` set: [`Op::Lea`] or [`Op::Illegal`] (M68000UM LEA,
/// M1-15, #23).
///
/// `LEA` takes only the *control* addressing modes — `(An)`, `d16(An)`,
/// `d8(An,Xn)`, `abs.W`, `abs.L`, `d16(PC)`, `d8(PC,Xn)`. `Dn`, `An`, `(An)+`,
/// `-(An)` and `#imm` are not encodings of it, so `$41C8` (`LEA A0,A0`) and
/// `$41D8` (`LEA (A0)+,A0`) are illegal instructions, and the 68010's extension
/// of `LEA` to postincrement is not this core's business. `LEA.json.gz` runs
/// exactly the 224 opcodes this admits (8 destinations x 28 control modes) and
/// `PEA.json.gz` exactly the 28 that [`classify_swap_ext_pea`] takes.
fn classify_lea(opcode: u16) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if is_control(mode, reg) {
        Op::Lea
    } else {
        Op::Illegal
    }
}

/// Bit 6 of a `$4E??` opcode: the low bit of the two-bit `op` field that
/// separates `JSR` (`0100 1110 10 mmm rrr`, `$4E80`, bit clear) from `JMP`
/// (`0100 1110 11 mmm rrr`, `$4EC0`, bit set). The two are otherwise the same
/// instruction, and the rest of the `$4E` group leaves bit 6 alone: `LINK`
/// `$4E50`, `UNLK` `$4E58`, `NOP` `$4E71` and friends are all below `$4E80`.
const CONTROL_JUMP_JMP_BIT: u16 = 0x0040;

/// One opcode of `$4E80..=$4EFF`: [`Op::Jsr`], [`Op::Jmp`] or [`Op::Illegal`]
/// (M68000UM JMP and JSR, "Addressing Modes"; M1-17, #25).
///
/// [`CONTROL_JUMP_JMP_BIT`] picks the instruction, and both take exactly the
/// *control* modes `LEA` and `PEA` take — `(An)`, `d16(An)`, `d8(An,Xn)`,
/// `abs.W`, `abs.L`, `d16(PC)`, `d8(PC,Xn)` — 28 operand fields apiece, which is
/// the opcode set `JMP.json.gz` and `JSR.json.gz` run over.
///
/// The other 36 of each are illegal, including the ones that look most like real
/// operands: `JMP (A0)+` and `JMP -(A0)` name a stack address, `JMP A0` names a
/// code address and `JMP #imm` is a number, but the 68000 gives none of them to
/// a jump (compare [`classify_lea`], which refuses the same fields).
fn classify_control_jump(opcode: u16) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if !is_control(mode, reg) {
        return Op::Illegal;
    }
    if opcode & CONTROL_JUMP_JMP_BIT == 0 {
        Op::Jsr
    } else {
        Op::Jmp
    }
}

/// One opcode of the `$4800..=$48FF` window: [`Op::Swap`], [`Op::Ext`],
/// [`Op::Pea`], or [`Op::Unimplemented`] for MOVEM (M68000UM SWAP, EXT, PEA,
/// MOVEM; #22, and PEA for M1-15, #23).
///
/// The window holds four instructions:
///
/// ```text
/// SWAP    0100 1000 0100 0 rrr      this window, mode `000` with bit 6 set
/// EXT     0100 1000 1s 00 0 rrr     this window, mode `000` with bit 7 set
/// PEA     0100 1000 01 mmm rrr     M1-15 (#23), mode != `000`
/// MOVEM   0100 1000 1mmm mmm rrr    M1-24 (#32),   mode != `000`
/// ```
///
/// So the EA mode field is what separates them, and it cannot be ignored: bit 7
/// splits the window into PEA's half and MOVEM's half, and in each half the mode
/// bits are the instruction's own operand field only in PEA's half. Reading the
/// suite's own opcodes makes that concrete — `PEA.json.gz` uses `$4850`..`$4857`,
/// `$4868`..`$486F` and `$4870`..`$487B`, exactly the control modes of `$4840` +
/// EA; `MOVEM.w`/`MOVEM.l` use `$4890`..`$48F9`, where those same bits are a
/// register list; and SWAP and EXT are exactly `$4840`..`$4847`,
/// `$4880`..`$4887` and `$48C0`..`$48C7`. Claiming the window whole would have
/// made `$48D0` (`MOVEM.L`) execute as a PEA.
///
/// PEA takes the same control modes `LEA` does (see [`classify_lea`]), so
/// `$4848` (`PEA A0`), `$4858` (`PEA (A0)+`) and `$487C` (`PEA #imm`) are illegal
/// instructions rather than unimplemented opcodes, exactly as an illegal mode in
/// any other instruction's EA field is.
///
/// `$4800`..`$483F` — bit 7 clear, bit 6 clear — is unassigned on the 68000: PEA
/// needs bit 6 set, and all 28 of its opcodes in `PEA.json.gz` have it. Those
/// stay unimplemented rather than called illegal, as `$4AFC` — the one documented
/// `ILLEGAL` — is handled separately.
fn classify_swap_ext_pea(opcode: u16) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if opcode & SWAP_EXT_BIT7 != 0 {
        // MOVEM's half: only EXT lives here today, and only at mode `000`.
        if mode == 0 {
            Op::Ext
        } else {
            Op::Unimplemented
        }
    } else if opcode & SWAP_BIT6 == 0 {
        Op::Unimplemented
    } else if mode == 0 {
        Op::Swap
    } else if is_control(mode, reg) {
        Op::Pea
    } else {
        Op::Illegal
    }
}

/// Bit 7 of a `$48??` opcode: set for EXT (and MOVEM), clear for SWAP (and PEA).
const SWAP_EXT_BIT7: u16 = 0x0080;
/// Bit 6 of a `$48??` opcode: SWAP's discriminator inside PEA's half, and EXT's
/// size (`0` word, `1` long) inside MOVEM's half.
const SWAP_BIT6: u16 = 0x0040;

/// One opcode of the `$0000`/`$0200`/`$0A00` groups: [`Op::Ori`], [`Op::Andi`] or
/// [`Op::Eori`] — the three `#imm,<ea>` logic operations, which are `ORI`'s
/// pattern `0000 ddd 0 ss eeeeee` with `ddd` = `000`, `001` and `101` and differ
/// from each other in nothing but the base opcode (M68000UM ORI, ANDI, EORI,
/// #21).
///
/// `op` says which of the three the caller's range is. The rules are
/// [`classify_subi`]'s — the EA field names the destination, which must be
/// data-alterable, and the immediate itself lives in the extension words — plus
/// the two forms #34 owns: mode `111` register `100` is `ORI #imm,CCR`/`ANDI
/// #imm,SRC`/`EORI #imm,SR`-style (`$003C`, `$023C`, `$0A3C`), and `ss` = `11`
/// with that EA is MOVE to SR (`$00FC`, `$02FC`, `$0AFC`).
fn classify_logic_imm(opcode: u16, op: Op) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if mode == 0b111 && reg == 0b100 {
        return Op::Unimplemented;
    }
    if Size::from_bits((opcode >> 6) & 0b11).is_none() {
        return Op::Unimplemented;
    }
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    op
}

/// `$0C00 + ss<<6 + <ea>`: `CMPI #imm,<ea>` (M68000UM "CMPI — Compare
/// Immediate").
///
/// `classify_subi`'s rules: the EA field names the *destination*, which must be
/// data-alterable, and the immediate lives in the instruction's extension words,
/// so [`crate::ops::arith::op_cmpi`] resolves it as mode `111` register `100`.
/// Measured over `CMP.b/w/l`, whose `$0Cxx` cases use Dn, the indirect modes and
/// the two absolutes and nothing else — which is `is_data_alterable` exactly,
/// with PC-relative correctly absent (M68000PM/AD 2.2.12: the 68000 adds it to
/// CMPI later).
fn classify_cmpi(opcode: u16) -> Op {
    // `ss` = `11` is not one of the three widths `CMPI` defines, and no ticket
    // has claimed it, so it stays unimplemented rather than guessed at.
    if Size::from_bits((opcode >> 6) & 0b11).is_none() {
        return Op::Unimplemented;
    }
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Cmpi
}

/// `$4A00 + ss<<6 + <ea>`: `TST <ea>` (M68000UM "TST — Test").
///
/// `classify` only routes `$4A00..=$4ABF` here, so `ss` is never `11` and TAS is
/// out of reach. A TST operand is read, never written, so the rule looks like
/// `is_data`'s — but An direct, PC-relative and an immediate are all readable
/// and are still not TST operands on the 68000: M68000PM section 8 "TST" gives
/// the modes as `010, 011, 100, 101, 110, 111(000, 001)`, and `TST.b/w/l` use
/// exactly those plus `Dn`, with no An, PC-relative or immediate case anywhere
/// in 24195 cases. That set is [`is_data_alterable`]'s: the same predicate the
/// write-side instructions use, applied here because the 68000 happened to
/// define TST's operand set as "the alterable data operands".
fn classify_tst(opcode: u16) -> Op {
    let mode = ((opcode >> 3) & 0b111) as u8;
    let reg = (opcode & 0b111) as u8;
    if !is_data_alterable(mode, reg) {
        return Op::Illegal;
    }
    Op::Tst
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
        // until #19 implemented SUB/SUBA/SUBX, so it was `1000` and `1011`.
        // #20 moved `1011`'s anchor to `$B100` (`EOR.B D0,D0`), the part of the
        // group CMP could not take, and #21 implemented EOR itself — so `1011`
        // has no unimplemented opcode left and this anchor moves to `1110`'s
        // shifts (M1-30, #38), the other group next door that is still loud.
        // `0x8FFF` is `DIVS`-shaped (`op_mode 111`), which #21 deliberately left
        // out of the `1000` group; `0xC0C0` is `MULU`, likewise.
        assert_eq!(decode(0x8FFF), Op::Unimplemented, "DIVS");
        assert_eq!(decode(0xC0C0), Op::Unimplemented, "MULU");
        assert_eq!(decode(0xE0C0), Op::Unimplemented, "ASR.W D0");
    }

    /// The EA modes `LEA` and `PEA` address, written out from M68000UM's mode
    /// list for the two instructions — `(An)`, `d16(An)`, `d8(An,Xn)`, `abs.W`,
    /// `abs.L`, `d16(PC)`, `d8(PC,Xn)` — rather than by calling the production
    /// [`is_control`], so these tests check that rule instead of restating it.
    fn addresses(mode: u8, reg: u8) -> bool {
        matches!(mode, 0b010 | 0b101 | 0b110) || (mode == 0b111 && reg <= 0b011)
    }

    /// `LINK An,d16` and `UNLK An` (`$4E50 + r` and `$4E58 + r`, M68000UM LINK,
    /// UNLINK; M1-15, #23): the two slots above the traps, and nothing else.
    #[test]
    fn link_and_unlk_decode() {
        assert_range_decodes_to(0x4E50..=0x4E57, Op::Link);
        assert_range_decodes_to(0x4E58..=0x4E5F, Op::Unlk);
        assert_eq!(decode(0x4E4F), Op::Trap, "TRAP #15, just below");
        assert_eq!(decode(0x4E60), Op::Unimplemented, "unassigned, just above");
    }

    /// `LEA <ea>,An` is exactly the 224 opcodes `0100 ddd 111 mmm rrr` at a mode
    /// it can address (M68000UM LEA; M1-15, #23). 224 = eight destinations times
    /// the 28 control modes, which is also every opcode `LEA.json.gz` runs.
    ///
    /// Stated over the whole `$4000..=$4FFF` group, because that is where the
    /// risk is: `LEA` shares the group with NEGX, CLR, NEG, NOT, TST, SWAP, EXT,
    /// PEA, MOVEM, CHK, TAS, JSR and the `ILLEGAL` opcode, and the only thing
    /// telling them apart is bits 8..6 plus the mode field. The `!= Op::Lea`
    /// half of the assertion is what keeps #19, #32 and #36's opcodes out of it.
    #[test]
    fn lea_is_exactly_its_own_opcodes() {
        let mut lea = 0;
        for opcode in 0x4000u16..=0x4FFF {
            let mode = ((opcode >> 3) & 0b111) as u8;
            let reg = (opcode & 0b111) as u8;
            let shape = opcode & 0x1C0 == 0x1C0;
            let want = shape && addresses(mode, reg);
            assert_eq!(decode(opcode) == Op::Lea, want, "LEA? ${opcode:04X}");
            if shape && !addresses(mode, reg) {
                assert_eq!(
                    decode(opcode),
                    Op::Illegal,
                    "mode {mode:b} {reg:b} is no address to take, and bits 8..6 say LEA"
                );
            }
            lea += u8::from(want);
        }
        assert_eq!(lea, 224, "eight destinations x 28 addressable modes");
    }

    /// `PEA <ea>` is exactly the 28 opcodes `0100 1000 01 mmm rrr` at a mode it
    /// can address — bit 6 set, bit 7 clear — and so neither SWAP/EXT (mode
    /// `000`) nor MOVEM (bit 7) nor the unassigned `$4800`..`$483F` (M68000UM
    /// PEA; M1-15, #23). See [`swap_ext_and_pea_decode_and_leave_movem_alone`].
    #[test]
    fn pea_is_exactly_its_own_opcodes() {
        let mut pea = 0;
        for opcode in 0x4800u16..=0x48FF {
            let mode = ((opcode >> 3) & 0b111) as u8;
            let reg = (opcode & 0b111) as u8;
            let shape = opcode & 0x1C0 == 0x040;
            let want = shape && addresses(mode, reg);
            assert_eq!(decode(opcode) == Op::Pea, want, "PEA? ${opcode:04X}");
            pea += u8::from(want);
        }
        assert_eq!(pea, 28, "the opcode count PEA.json.gz covers");
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
    /// loudly, not silently. `$4E70` is `RESET` (M1-27).
    ///
    /// The `$4E4x` traps are the exception to this in the same group: TRAP is
    /// not an arithmetic instruction but an exception route, and #16 owns it.
    ///
    /// This test is expected to change as instructions land: when `NOP` is
    /// implemented it decodes to its own variant, and the ticket that does it
    /// moves this assertion to an opcode still unimplemented. That licence is
    /// this test alone (ticket #15). `NOP` itself landed in #25, so the anchor is
    /// now `RESET`, the next unclaimed opcode up in the `$4E` group.
    #[test]
    fn unknown_is_unimplemented_for_now() {
        assert_eq!(decode(0x4E70), Op::Unimplemented, "RESET");
        // ... except the traps, which #16 wired up, and which sit right next to
        // NOP in the `$4E` group.
        assert_range_decodes_to(0x4E40..=0x4E4F, Op::Trap);
        assert_eq!(decode(0x4E3F), Op::Unimplemented, "just below the traps");
        // LINK (`$4E50`..`$4E57`) and UNLK (`$4E58`..`$4E5F`) are above the traps
        // now (#23), so the anchor moves to the first unassigned opcode after
        // them — `$4E60`..`$4E6F`, between UNLK and RESET.
        assert_eq!(decode(0x4E60), Op::Unimplemented, "just above UNLK");
        // `$0000` was ORI until #21 implemented it (see
        // `ori_andi_eori_decode_and_leave_ccr_sr_alone`); `$0100` is BTST,
        // the bit-test half of the same `$0000` group that M1-14 (#22) owns.
        assert_eq!(decode(0x0100), Op::Unimplemented, "BTST.B D0,D0");
        // The move groups — bits 15..12 = `0001`, `0010`, `0011` (`MOVE`,
        // `MOVEA`) and `0111` (`MOVEQ`) — left this list with #17, see
        // `move_family_is_not_unimplemented`, so the anchor is a
        // move-family opcode #17 deliberately left out: MOVEM is group `$48xx`
        // and belongs to M1-10 (#19).
        assert_eq!(decode(0x48E7), Op::Unimplemented, "MOVEM.L #(imm),(A7)-");
    }

    /// `JMP $4EC0 | <ea>` and `JSR $4E80 | <ea>` over all 64 values of the
    /// operand field: the manual gives both instructions the control addresses
    /// and nothing else, so `(An)`, `d16(An)`, `d8(An,Xn)`, `abs.W`, `abs.L`,
    /// `d16(PC)` and `d8(PC,Xn)` — 28 apiece, the count `JMP.json.gz` and
    /// `JSR.json.gz` cover — decode, and the other 36 are illegal. `JSR (A0)+`
    /// is the one worth naming: it is an address in every other instruction that
    /// takes one, and the 68000 still refuses it here (M68000UM JMP, JSR,
    /// "Addressing Modes"; M1-17, #25).
    #[test]
    fn jmp_and_jsr_take_only_the_control_addresses() {
        let mut control = 0;
        for mode in 0..8u8 {
            for reg in 0..8u8 {
                let ea = (u16::from(mode) << 3) | u16::from(reg);
                let want = addresses(mode, reg);
                control += u8::from(want);
                assert_eq!(
                    decode(0x4EC0 | ea),
                    if want { Op::Jmp } else { Op::Illegal },
                    "JMP mode {mode:03b} reg {reg:03b}"
                );
                assert_eq!(
                    decode(0x4E80 | ea),
                    if want { Op::Jsr } else { Op::Illegal },
                    "JSR mode {mode:03b} reg {reg:03b}"
                );
            }
        }
        assert_eq!(control, 28, "seven addressing modes x eight registers");
    }

    /// The four no-operand members of the `$4E` group are single opcodes, and
    /// counting the whole `$4E00`..`$4EFF` window is the only way to see that
    /// they took exactly those four: `NOP` `$4E71`, `RTE` `$4E73`, `RTS` `$4E75`
    /// and `RTR` `$4E77`, and that TRAPV `$4E76` in the middle of them is still
    /// unimplemented (M1-12, #20 — `RTR` sits directly above it, so this ticket
    /// cannot take `$4E76` by accident).
    ///
    /// The rest of the count is the point: `RESET` `$4E70`, `STOP` `$4E72`, MOVE
    /// to USP `$4E60`..`$4E67` and the `$4E78`/`$4E79` holes are still
    /// unimplemented, and `JSR`/`JMP` plus their 72 illegal operand fields are
    /// the whole of `$4E80`..`$4EFF` (M68000UM section 8, "Instruction Set
    /// Summary"; M1-17, #25).
    #[test]
    fn the_returns_and_no_operand_misc_are_single_opcodes() {
        assert_eq!(decode(0x4E71), Op::Nop, "NOP");
        assert_eq!(decode(0x4E73), Op::Rte, "RTE");
        assert_eq!(decode(0x4E75), Op::Rts, "RTS");
        assert_eq!(decode(0x4E77), Op::Rtr, "RTR");
        assert_eq!(decode(0x4E76), Op::Unimplemented, "TRAPV is #20's");

        let mut counts = [
            (Op::Nop, 0),
            (Op::Rte, 0),
            (Op::Rts, 0),
            (Op::Rtr, 0),
            (Op::Trap, 0),
            (Op::Link, 0),
            (Op::Unlk, 0),
            (Op::Jmp, 0),
            (Op::Jsr, 0),
            (Op::Illegal, 0),
            (Op::Unimplemented, 0),
        ];
        for opcode in 0x4E00u16..=0x4EFF {
            let op = decode(opcode);
            let slot = counts
                .iter_mut()
                .find(|(want, _)| *want == op)
                .unwrap_or_else(|| panic!("{op:?} should not appear in the $4E group"));
            slot.1 += 1;
        }
        for (op, count) in counts {
            let want = match op {
                Op::Nop | Op::Rte | Op::Rts | Op::Rtr => 1,
                Op::Trap => 16,
                Op::Link | Op::Unlk => 8,
                Op::Jmp | Op::Jsr => 28,
                Op::Illegal => 72,
                Op::Unimplemented => 92,
                _ => unreachable!("only the variants above are named"),
            };
            assert_eq!(count, want, "{op:?} in $4E00..=$4EFF");
        }
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
        // see `subi_decodes_and_validates_its_destination`. `$00` and `$02` were
        // neighbours too until #21 claimed them as ORI and ANDI, so the list is
        // now the bit tests and CMPI's own size-`11` hole.
        for (opcode, name) in [
            (0x0100u16, "BTST.B #imm,D0"),
            (0x0500, "BCHG.B #imm,D0"),
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
        // `ss` = `11` is not a size: it is Scc/DBcc, which the two `$51C8`-style
        // entries here used to defer to the branch ticket (#24). Note that both
        // of them are mode `001` and therefore `DBcc`, not the `Scc -(A0)` the
        // old comment claimed.
        assert_eq!(
            decode(0x51C8),
            Op::Dbcc,
            "DBcc.F D0 — mode 001, not Scc -(A0)"
        );
        assert_eq!(decode(0x51CC), Op::Dbcc, "DBcc.F D4 — mode 001");
        assert_eq!(decode(0x50C0), Op::Scc, "Scc.T D0 — mode 000");
    }

    /// `0101 cccc 11 eeeeee` is M68000UM Format V, the conditional group, and its
    /// two instructions are told apart by one field of the EA word: mode `001` is
    /// `DBcc`, whose "register" is the loop counter and whose remaining EA bits
    /// are the 16-bit displacement, and every other mode is `Scc`. Bit 8 is part
    /// of the condition code here, not a `SUBQ` marker, which is why `$50C8` and
    /// `$51C8` are the same instruction (M68000UM Format V, "DBcc", "Scc";
    /// M1-16, #24).
    #[test]
    fn scc_and_dbcc_split_the_0101_group_on_ea_mode_001() {
        for (opcode, name) in [
            (0x50C8u16, "DBcc.T D0 — bit 8 clear, mode 001"),
            (0x51C8, "DBcc.F D0 — cc 0001, still mode 001"),
            (0x5DC8, "DBcc.LT D0"),
            (0x5FCF, "DBcc.LE D7 — the last encoding of the family"),
        ] {
            assert_eq!(decode(opcode), Op::Dbcc, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x50C0u16, "Scc.T D0 — one bit of the EA below DBcc"),
            (0x51C0, "Scc.F D0 — bit 8 set is still Scc"),
            (0x57D0, "Scc.EQ (A0)"),
            (0x5FF9, "Scc.LE ($0000).L — mode 111, reg 001"),
            (0x5FE1, "Scc.LE -(A1) — mode 100"),
        ] {
            assert_eq!(decode(opcode), Op::Scc, "{name} (${opcode:04X})");
        }
        // `Scc` writes a byte, so the modes that cannot be written are illegal
        // exactly as they are for `CLR` (#22) — and mode `001` never reaches the
        // rule, because `DBcc` took it first.
        for (opcode, name) in [
            (0x57FAu16, "Scc.EQ d16(PC): not a destination"),
            (0x57FC, "Scc.EQ #imm: immediates are source-only"),
            (
                0x57FF,
                "Scc.EQ with mode 111, reg 111: no such address mode",
            ),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
    }

    /// The whole `0110` group is branches — `0110 cccc dddddddd` — and the low
    /// byte is a displacement, not an effective address, so no encoding of it is
    /// illegal: `$6000` is `BRA` with a zero displacement (a displacement word
    /// follows) and `$60FF` is `BRA #-1`, not a long branch (M68000UM Format VI,
    /// "BRA", "BSR", "Bcc"; M1-16, #24).
    #[test]
    fn bcc_bra_bsr_take_the_whole_6000_group() {
        for opcode in 0x6000u16..=0x6FFF {
            let want = if (opcode >> 8) & 0b1111 == 0b0001 {
                Op::Bsr
            } else {
                Op::Bcc
            };
            assert_eq!(decode(opcode), want, "${opcode:04X}");
        }
        assert_eq!(decode(0x60FE), Op::Bcc, "BRA.B #-2, the self loop");
        assert_eq!(decode(0x67FC), Op::Bcc, "Bcc.EQ.B #-4");
        assert_eq!(decode(0x6100), Op::Bsr, "BSR with a zero byte displacement");
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
    /// bits 11..9 and data `000` meaning 8. `ss` = `11` is not a size at all —
    /// `Scc`/`DBcc` take the whole of it, both bit-8 halves — and is pinned by
    /// `scc_and_dbcc_split_the_0101_group_on_ea_mode_001` (M68000UM Format V,
    /// "SUBQ", and the operation-code-map appendix; M1-11, #19, M1-16, #24).
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
        assert_eq!(decode(0x51C8), Op::Dbcc, "DBcc.F D0 — mode 001");
        assert_eq!(decode(0x51CC), Op::Dbcc, "DBcc.F D4 — mode 001");
        assert_eq!(decode(0x50C0), Op::Scc, "Scc.T D0 — mode 000");
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
            (
                0xB03C,
                "CMP.B #imm,D0: the 68000 runs it, measured in CMP.b",
            ),
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
    /// is `EOR.B D1,D0`, which M1-13 (#21) owns. Claiming all of `1ss` for the
    /// compare would hand EOR's whole range to `op_cmpm` and nothing would notice
    /// until M1-13 landed.
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
        // Bit 8 set with any other EA mode is EOR, which #21 implemented, and
        // mode `001` still has to lose to CMPM: `$B109` and `$B101` differ by one
        // bit and one is a compare that steps two pointers while the other is a
        // write to D0.
        for (opcode, name) in [
            (0xB101u16, "EOR.B D1,D0"),
            (0xB110, "EOR.B D0,(A0)"),
            (0xB132, "EOR.B D0,(d8,A2,Xn) — measured in EOR.b"),
            (0xB36C, "EOR.W D1,(d16,A4) — measured in EOR.w"),
            (0xB782, "EOR.L D3,D2 — measured in EOR.l"),
        ] {
            assert_eq!(
                decode(opcode),
                Op::Eor,
                "{name} (${opcode:04X}) must stay EOR's, not CMPM's"
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
                "CMPI.W #imm,d16(PC): the 68000 does not allow PC-relative here",
            ),
            (
                0x0C7C,
                "CMPI.W #imm,#imm: not an operand to compare against",
            ),
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

    /// `AND` is `1100 ddd ooo mmm rrr` and `OR` is `1000 ddd ooo mmm rrr`: `ooo`
    /// `000`..`010` is `<ea> op Dd → Dd`, whose source must be a data EA — a
    /// *byte* address register is not one, exactly as in ADD, while a word or long
    /// one is — and `100`..`110` is `Dd op <ea> → <ea>`, whose destination must be
    /// data-alterable, so neither an address register nor an immediate
    /// (M68000UM AND, OR; M1-13, #21). Every opcode in the legal list was read
    /// out of `AND.b/w/l` and `OR.b/w/l`, which together use all of modes
    /// `010`..`111` in the read-modify-write half and `000` and `010`..`111` in
    /// the load half; mode `001` appears in neither, which is the same fact
    /// `decode_does_not_steal_abcd_mulu_exg` relies on.
    #[test]
    fn and_or_decode_and_validate_their_eas() {
        for (opcode, name) in [
            (0xC001u16, "AND.B D1,D0: operation 000, source D1"),
            (0xC041, "AND.W D1,D0"),
            (0xC081, "AND.L D1,D0"),
            (0xC018, "AND.B (A0)+,D0 — measured in AND.b"),
            (0xC221, "AND.B -(A1),D1 — measured in AND.b"),
            (0xC0A9, "AND.L d16(A1),D0 — measured in AND.l"),
            (0xC038, "AND.B ($0000).W,D0 — measured in AND.b"),
            (
                0xC07C,
                "AND.W #imm,D0: an immediate *is* a legal source — `c07c [AND.w #, D0]` in AND.w",
            ),
            (0xC048, "AND.W A0,D0: a word An *is* a data source"),
            (0xC398, "AND.L D1,(A0)+ — measured in AND.l"),
            (0xC730, "AND.B D3,d8(A0,D0): the destination is alterable"),
            (0x8001, "OR.B D1,D0"),
            (0x8041, "OR.W D1,D0"),
            (0x8081, "OR.L D1,D0"),
            (0x8318, "OR.B D1,(A0)+ — measured in OR.b"),
            (0x8FB9, "OR.L D7,($0000).L — measured in OR.l"),
        ] {
            let want = if opcode >> 12 == 0xC { Op::And } else { Op::Or };
            assert_eq!(decode(opcode), want, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0xC008u16, "AND.B (A0),D0: a byte An is not a data source"),
            (0x8008, "OR.B (A0),D0: the same rule, one group away"),
            (0xC13C, "AND.B D0,#imm: an immediate is not a destination"),
            (0x8148, "OR.W D0,A0: an address register is not alterable"),
            (0x8FBC, "OR.L D7,#imm"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
    }

    /// `EOR` is `1011 ddd 1ss mmm rrr`, `Dd ^ <ea> → <ea>`, so the EA names a
    /// read-modify-write destination and has to be data-alterable — the one
    /// property that made mode `001` free for CMPM. Operation mode `111` stays
    /// CMPA's, which is why `$BF9F` (EOR.L D7,(A7)+) is EOR but `$BF8F`
    /// (CMPM.L (A7)+,(A7)+, measured in CMP.l) is not, even though the two differ
    /// only in the size bits (M68000UM EOR, CMPM, CMPA; M1-13, #21).
    #[test]
    fn eor_decodes_to_eor_with_a_data_alterable_destination() {
        for (opcode, name) in [
            (
                0xB100u16,
                "EOR.B D0,D0 — the CMP half's shape, taken by bit 8",
            ),
            (0xB140, "EOR.W D0,D0"),
            (0xB198, "EOR.L D0,(A0)+ — measured-shaped"),
            (0xBF9F, "EOR.L D7,(A7)+ — measured in EOR.l"),
            (0xB36C, "EOR.W D1,d16(A4) — measured in EOR.w"),
            (0xB132, "EOR.B D0,d8(A2,Xn) — measured in EOR.b"),
            (0xBFA7, "EOR.L D7,-(A7)"),
        ] {
            assert_eq!(decode(opcode), Op::Eor, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (
                0xB13Cu16,
                "EOR.B D0,#imm: an immediate is not a destination",
            ),
            (0xBFBC, "EOR.L D7,#imm"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        // Mode `001`, one bit from `$B101`, is CMPM's; operation mode `111` is
        // CMPA's. Both are #20's and neither is EOR.
        assert_eq!(decode(0xB108), Op::Cmpm, "CMPM.B (A0)+,(A0)+");
        assert_eq!(decode(0xBF8F), Op::Cmpm, "CMPM.L (A7)+,(A7)+");
        assert_eq!(decode(0xB1C8), Op::Cmpa, "CMPA.L A0,A0");
    }

    /// `ORI` is `$0000 + ss<<6 + <ea>`, `ANDI` `$0200 + …` and `EORI` `$0A00 + …`,
    /// the three `#imm,<ea>` operations of the `0000` group next to `SUBI`, `ADDI`
    /// and `CMPI`. The EA names the destination, which must be data-alterable,
    /// and the two forms that are not destinations at all — mode `111` register
    /// `100`, the CCR, and `ss` = `11` — stay [`Op::Unimplemented`] for M1-26
    /// (#34) rather than being called illegal from here (M68000UM ORI, ANDI,
    /// EORI; M1-13, #21).
    #[test]
    fn ori_andi_eori_decode_and_leave_ccr_sr_alone() {
        for (opcode, want, name) in [
            (0x0000u16, Op::Ori, "ORI.B #imm,D0"),
            (0x0040, Op::Ori, "ORI.W #imm,D0"),
            (0x0080, Op::Ori, "ORI.L #imm,D0"),
            (0x0010, Op::Ori, "ORI.B #imm,(A0)"),
            (0x0098, Op::Ori, "ORI.L #imm,(A0)+"),
            (0x00B8, Op::Ori, "ORI.L #imm,($0000).W"),
            (0x0200, Op::Andi, "ANDI.B #imm,D0"),
            (0x0268, Op::Andi, "ANDI.W #imm,d16(A0) — measured in AND.w"),
            // `(xxx).L` is mode `111` register `001`; AND.b measures the same EA at
            // byte size as `0239 [AND.b #, (xxx).l]`, so the mode is a destination.
            (0x02B9, Op::Andi, "ANDI.L #imm,($0000).L — mode 111 reg 001"),
            (0x0A00, Op::Eori, "EORI.B #imm,D0"),
            (0x0A50, Op::Eori, "EORI.W #imm,(A0)"),
            (0x0AB8, Op::Eori, "EORI.L #imm,($0000).W"),
        ] {
            assert_eq!(decode(opcode), want, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (0x003Cu16, "ORI.B #imm,CCR"),
            (0x007C, "ORI.W #imm,CCR"),
            (0x023C, "ANDI.B #imm,CCR"),
            (0x0A3C, "EORI.B #imm,CCR"),
            (0x00C0, "ORI with ss = 11: MOVE to SR's space"),
            (0x02C0, "ANDI with ss = 11"),
            (0x0AC0, "EORI with ss = 11"),
            (0x02FC, "ANDI with ss = 11 at the CCR EA"),
        ] {
            assert_eq!(decode(opcode), Op::Unimplemented, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (
                0x0048u16,
                "ORI.W #imm,A0: an address register is not alterable",
            ),
            (
                0x02BA,
                "ANDI.L #imm,d16(PC): mode 111 reg 010 is PC-relative, not a destination",
            ),
            (
                0x027D,
                "ANDI.W #imm,<mode 111 reg 101>: not an EA on the 68000",
            ),
            (0x0ABD, "EORI.L #imm,<mode 111 reg 101>"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
    }

    /// `NOT` is `0100 0110 ss mmm rrr`, a one-operand read-modify-write, so its EA
    /// must be data-alterable (M68000UM NOT; M1-13, #21). `classify` only routes
    /// `$4600..=$46BF` to it, so `ss` = `11` — MOVE to SR (`$46C0`, `$46FC`) and
    /// MOVE to CCR's space, which M1-26 (#34) owns — never reaches it. Every mode
    /// in the legal list but `001` is measured in `NOT.b/w/l`, and mode `111`
    /// register `100` (`#immediate`, `$463C`/`$46BC`) is in no file at all.
    #[test]
    fn not_decodes_and_validates_its_destination() {
        for (opcode, name) in [
            (0x4600u16, "NOT.B D0"),
            (0x4641, "NOT.W D1"),
            (0x4682, "NOT.L D2"),
            // Mode `010` is `(Am)`, a memory operand: `4610 [NOT.b (A0)]` and
            // `4617 [NOT.b (A7)]` are both measured cases in NOT.b. Only mode
            // `001`, the address register itself, is not a destination.
            (0x4610, "NOT.B (A0) — measured in NOT.b"),
            (0x4617, "NOT.B (A7) — measured in NOT.b"),
            (0x4690, "NOT.L (A0) — measured in NOT.l"),
            (0x4698, "NOT.L (A0)+ — measured in NOT.l"),
            (0x46A7, "NOT.L -(A7) — measured in NOT.l"),
            (0x4669, "NOT.W d16(A1) — measured in NOT.w"),
            (0x4674, "NOT.W d8(A4,D0) — measured in NOT.w"),
            (0x4638, "NOT.B ($0000).W — measured in NOT.b"),
            (0x46B9, "NOT.L ($0000).L — measured in NOT.l"),
        ] {
            assert_eq!(decode(opcode), Op::Not, "{name} (${opcode:04X})");
        }
        for (opcode, name) in [
            (
                0x4608u16,
                "NOT.B A0: mode 001, an address register, is not a destination",
            ),
            (0x468F, "NOT.L A7: not a destination either"),
            (0x463C, "NOT.B #imm: not a destination"),
            (0x46BC, "NOT.L #imm: mode 111 reg 100, whatever the size"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }
        assert_eq!(
            decode(0x46C0),
            Op::Unimplemented,
            "NOT is not encoded at ss = 11"
        );
        assert_eq!(decode(0x46FC), Op::Unimplemented, "MOVE.L #imm,SR");
    }

    /// The `1100` and `1000` groups are not AND's and OR's alone. The corners
    /// below were read out of the pinned suite's own opcode fields, which is how
    /// the exclusions in [`classify_and`] and [`classify_or`] are known rather
    /// than guessed:
    ///
    /// ```text
    /// ABCD   1100 ddd 100 0m yyy   operation 100, modes 000 and 001
    /// EXG    1100 ddd 101 0m yyy   and 1100 ddd 110 001 yyy
    /// MULU   1100 ddd 011 eeeeee   MULS 1100 ddd 111 eeeeee
    /// SBCD   1000 ddd 100 0m yyy   DIVU 1000 ddd 011 eeeeee   DIVS 1000 ddd 111 eeeeee
    /// ```
    ///
    /// The direction bit `100` and a byte size make ABCD look like a plain
    /// `Dn op <ea>` at a glance, and EXG's third form is one bit away from
    /// `AND.L Dn,(An)+`, so the group split is asserted here opcode by opcode
    /// (M68000UM ABCD, SBCD, MULU, MULS, DIVU, DIVS, EXG; M1-13, #21).
    ///
    /// #22 took the EXG lines of this test to [`Op::Exg`] — they are now in
    /// [`exg_decodes_its_three_legal_pairs`] — and replaced their coverage of
    /// "a `1100` corner is not AND" with the MULS opcode `$C1C8` below, which no
    /// other test names. ABCD, MULU, MULS, SBCD, DIVU and DIVS are unchanged.
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn decode_does_not_steal_abcd_mulu() {
        // The three the ticket names, then the rest of both groups.
        assert_ne!(decode(0xC100), Op::And, "ABCD.B (A0)+,(A1)+");
        assert_ne!(decode(0xC0C0), Op::And, "MULU D0,D0");
        assert_ne!(decode(0xC141), Op::And, "EXG D0,D1");
        assert_eq!(decode(0xC100), Op::Unimplemented, "ABCD.B (A0)+,(A1)+");
        assert_eq!(decode(0xC108), Op::Unimplemented, "ABCD.B -(A0),-(A1)");
        assert_eq!(decode(0xC0C0), Op::Unimplemented, "MULU D0,D0");
        assert_eq!(decode(0xC0EE), Op::Unimplemented, "MULU d16(A0),D6");
        assert_eq!(decode(0xCFC8), Op::Unimplemented, "MULS D0,(A0)");
        assert_eq!(decode(0xC1C8), Op::Unimplemented, "MULS.W (A0),D0");
        assert_eq!(decode(0x8100), Op::Unimplemented, "SBCD.B (A0)+,(A1)+");
        assert_eq!(decode(0x8108), Op::Unimplemented, "SBCD.B -(A0),-(A1)");
        assert_eq!(decode(0x80C0), Op::Unimplemented, "DIVU D0,D0");
        assert_eq!(decode(0x8FC8), Op::Unimplemented, "DIVS D0,(A0)");
        // The other side of the same line: the `1000` group has no EXG, so
        // operation `101` into a data register is an ordinary OR — and `1100`'s
        // operation `110` with mode `010`, one bit from EXG's `001`, is an
        // ordinary AND.
        assert_eq!(decode(0x8141), Op::Or, "OR.W D0,D1");
        assert_eq!(decode(0xC198), Op::And, "AND.L D0,(A0)+");
        // EXG's other two homes are in groups AND never touches.
        assert_ne!(decode(0x48C0), Op::And, "EXG-shaped, the $48 group");
        assert_ne!(decode(0x4CC0), Op::And, "EXG-shaped, the $4C group");
    }

    /// `NEG` `$4400`, `NEGX` `$4000` and `CLR` `$4200`, each `+ ss<<6 + <ea>`,
    /// all three under `is_data_alterable` (M68000UM NEG, NEGX, CLR; M1-14, #22).
    ///
    /// The legal list is the suite's own: each of `NEG.b`, `NEGX.w` and `CLR.l`
    /// runs the same eight modes over its size — `Dn` (`$4400`), `(An)` (`$4410`),
    /// `(An)+` (`$441F`), `-(An)` (`$4420`), `d16(An)` (`$442A`), `d8(An,Xn)`
    /// including `d8(A7,Xn)` (`$4437`), `abs.W` (`$4438`) and `abs.L` (`$4439`) —
    /// and never an `An`, a PC-relative operand or an immediate, so the two lists
    /// below are the whole of the rule rather than a sample of it.
    #[test]
    fn neg_negx_clr_decode_and_validate_their_operands() {
        for (opcode, want, name) in [
            (0x4400u16, Op::Neg, "NEG.B D0"),
            (0x4410, Op::Neg, "NEG.B (A0)"),
            (0x441F, Op::Neg, "NEG.B (A7)+"),
            (0x4420, Op::Neg, "NEG.B -(A0)"),
            (0x442A, Op::Neg, "NEG.B d16(A2)"),
            (0x4437, Op::Neg, "NEG.B d8(A7,Xn)"),
            (0x4438, Op::Neg, "NEG.B (xxx).W"),
            (0x44B9, Op::Neg, "NEG.L (xxx).L"),
            (0x4481, Op::Neg, "NEG.L D1"),
            (0x4001, Op::Negx, "NEGX.B D1"),
            (0x4052, Op::Negx, "NEGX.W (A2)"),
            (0x40B9, Op::Negx, "NEGX.L (xxx).L"),
            (0x4200, Op::Clr, "CLR.B D0"),
            (0x4237, Op::Clr, "CLR.B d8(A7,Xn)"),
            (0x42A9, Op::Clr, "CLR.L d16(A1)"),
            (0x42B9, Op::Clr, "CLR.L (xxx).L"),
        ] {
            assert_eq!(decode(opcode), want, "{name} (${opcode:04X})");
        }

        // The operands none of the three may take: an `An` (mode `001`), which
        // NEG/NEGX/CLR do not sign-extend or otherwise write as an address, and
        // the mode `111` sub-modes that are not alterable — `d16(PC)`, `#imm` and
        // `d8(PC,Xn)` (M68000UM NEG: "Address register direct is illegal", and the
        // destination-operand rules of the section 8 descriptions).
        for (opcode, name) in [
            (
                0x4409u16,
                "NEG.B A1: an address register is not a data destination",
            ),
            (0x4489, "NEG.L A1"),
            (0x4049, "NEGX.W A1"),
            (0x4289, "CLR.L A1"),
            (
                0x42BA,
                "CLR.B d16(PC): an operand inside the instruction stream",
            ),
            (0x44BB, "NEG.L #imm"),
            (0x44BC, "NEG.L d8(PC,Xn)"),
        ] {
            assert_eq!(decode(opcode), Op::Illegal, "{name} (${opcode:04X})");
        }

        // `ss` = `11` is outside all three ranges: `$40C0` is MOVE from SR, which
        // `MOVEfromSR.json.gz` measures at `$40C0`..`$40F9` (M1-26, #34), and
        // `$42C0`/`$44C0` are unassigned on the 68000.
        for (opcode, name) in [
            (
                0x40C0u16,
                "MOVE from SR D0: the first opcode of MOVEfromSR.json.gz",
            ),
            (0x40F9, "MOVE from SR (xxx).L"),
            (0x42C0, "unassigned sixth of CLR’s group"),
            (0x44C0, "unassigned sixth of NEG’s group"),
            (0x44FF, "the top of NEG’s group"),
        ] {
            assert_eq!(decode(opcode), Op::Unimplemented, "{name} (${opcode:04X})");
        }
    }

    /// The `$48??` window is SWAP, EXT, PEA and MOVEM (M68000UM SWAP, EXT, PEA,
    /// MOVEM; M1-14 #22 and M1-15 #23), and what separates them is the EA mode
    /// field: mode `000` is SWAP (bit 7 clear) or EXT (bit 7 set, bit 6 the
    /// size), a control mode with bit 7 clear is PEA, and anything else is MOVEM
    /// and stays [`Op::Unimplemented`].
    ///
    /// The PEA and MOVEM opcodes below are read off the pinned suite, not
    /// invented: `PEA.json.gz` runs `$4850`..`$4857`, `$4868`..`$486F` and
    /// `$4870`..`$487B`, `MOVEM.w.json.gz` runs `$4890`.. and `MOVEM.l.json.gz`
    /// runs `$48D0`.., so this test is what stops SWAP, EXT and now PEA from
    /// swallowing the first opcode of M1-24 (#32) along the way.
    #[test]
    fn swap_ext_and_pea_decode_and_leave_movem_alone() {
        for (opcode, want, name) in [
            (0x4840u16, Op::Swap, "SWAP D0"),
            (0x4847, Op::Swap, "SWAP D7"),
            (0x4880, Op::Ext, "EXT.W D0"),
            (0x4887, Op::Ext, "EXT.W D7"),
            (0x48C0, Op::Ext, "EXT.L D0"),
            (0x48C7, Op::Ext, "EXT.L D7"),
            // PEA's half of the window at a control mode (#23): `$4840` + the EA
            // field. These are the opcodes `PEA.json.gz` runs.
            (0x4850, Op::Pea, "PEA (A0): measured in PEA.json.gz"),
            (0x4868, Op::Pea, "PEA d16(A0): measured in PEA.json.gz"),
            (0x487A, Op::Pea, "PEA d16(PC): measured in PEA.json.gz"),
        ] {
            assert_eq!(decode(opcode), want, "{name} (${opcode:04X})");
        }

        // A mode `LEA`/`PEA` cannot address is an illegal instruction, not an
        // unimplemented opcode: `$4848` is `PEA A0`.
        assert_eq!(decode(0x4848), Op::Illegal, "mode 001 in PEA's half");

        for (opcode, name) in [
            (0x4890, "MOVEM.W #reglist,(A0): measured in MOVEM.w.json.gz"),
            (
                0x48A0,
                "MOVEM.W #reglist,-(A0): measured in MOVEM.w.json.gz",
            ),
            (0x48D0, "MOVEM.L #reglist,(A0): measured in MOVEM.l.json.gz"),
            (0x4CC0, "MOVEM.L #reglist,-(A0) in the $4C half"),
            // Bit 6 clear, bit 7 clear: unassigned on the 68000. PEA is
            // `0100 1000 01 mmm rrr`, so `$4800` is not `PEA` with a missing
            // destination, and it stays loudly unimplemented rather than
            // illegal.
            (0x4800, "unassigned: bit 6 and bit 7 clear"),
            (0x4807, "the same, at r=7"),
        ] {
            assert_eq!(decode(opcode), Op::Unimplemented, "{name} (${opcode:04X})");
        }
    }

    /// `EXG` is operation `101` at mode `000` (`Dx,Dy`) or `001` (`Ax,Ay`) and
    /// operation `110` at mode `001` (`Dx,Ay`) — 192 opcodes, which is exactly the
    /// set `EXG.json.gz` runs (`$C140`..`$C14F`, `$C188`..`$C18F` and the same for
    /// every `xxx`) — and nothing else in the `1100` group (M68000UM EXG; M1-14,
    /// #22).
    ///
    /// The neighbours are the point. `$C180` is operation `110` at mode `000`, one
    /// bit from `Dx,Ay`, and is `AND.L D0,D0`; `$C198` is operation `110` at mode
    /// `011` and is `AND.L D0,(A0)+`; `$C100` is ABCD and `$C1C0` is MULS, both
    /// still unimplemented. An off-by-one in [`is_exg`] fails in one direction or
    /// the other.
    #[test]
    fn exg_decodes_its_three_forms_and_nothing_else() {
        for (opcode, name) in [
            (0xC140u16, "EXG D0,D0"),
            (0xC147, "EXG D0,D7"),
            (0xC94A, "EXG A4,A2"),
            (0xC14F, "EXG A0,A7"),
            (0xC188, "EXG D0,A0"),
            (0xC18F, "EXG D0,A7: the case that moves SSP"),
            (0xCF47, "EXG D7,D7"),
            (0xCF8F, "EXG D7,A7"),
        ] {
            assert_eq!(decode(opcode), Op::Exg, "{name} (${opcode:04X})");
        }

        assert_eq!(
            decode(0xC180),
            Op::And,
            "AND.L D0,D0: operation 110 mode 000, one bit from EXG Dx,Ay"
        );
        assert_eq!(
            decode(0xC198),
            Op::And,
            "AND.L D0,(A0)+: operation 110 mode 011"
        );
        assert_eq!(
            decode(0x8198),
            Op::Or,
            "OR.L D0,(A0)+: operation 110 in the 1000 group, which has no EXG at all"
        );
    }
}
