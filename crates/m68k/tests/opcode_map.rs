//! Cross-check of the 68000 decode table against the upstream official opcode
//! map (M1-07 table audit, #477).
//!
//! [SingleStepTests/680x0] ships `map/68000.official.json`: one entry for each
//! of the 65 536 16-bit opcode words, naming the officially-defined operation
//! for that word (`"ffff": "None"` for the reserved slots). The
//! instruction-by-instruction suite (`singlestep.rs`) can only exercise
//! opcodes it has cases for; this map is the independent, machine-readable
//! statement of *which opcodes exist*, so walking all 65 536 entries catches a
//! decode-table hole or an over-broad illegal route that no amount of
//! case-based testing would.
//!
//! [SingleStepTests/680x0]: https://github.com/SingleStepTests/680x0
//!
//! # The audit verdict, 2026-11-20, at the map pinned by #477
//!
//! Walked 65 536 entries:
//!
//! * **45 815 defined** — every one decodes to its map token's [`Op`] family.
//!   No defined opcode falls to `Illegal`, `Undefined` or `Unimplemented`.
//! * **19 721 `None`** — every one takes the illegal-instruction exception
//!   ([`Op::Illegal`] 10 144, [`Op::Undefined`] 1 385, [`Op::LineA`] /
//!   [`Op::LineF`] 8 192, the two reserved line groups the map records as
//!   `None`). No pin remains: the 453 over-broad decodes were fixed and
//!   unpinned by #618, #619 and #620, and #621 worked the last 204
//!   still-`Unimplemented` holes into `Undefined`/`Illegal` and removed their
//!   list with them — a `None` slot that decodes to anything but an exception
//!   tag is a plain mismatch.
//!
//! # What is asserted
//!
//! * a `"None"` slot must decode to one of [`NONE_EXCEPTION_TAGS`] — nothing
//!   else. The audit's pin lists are gone: the over-broad decodes came off with
//!   their fixes (#618/#619/#620), and #621 worked the last 204
//!   `Unimplemented` holes into `Undefined`/`Illegal`, so a `None` slot that
//!   decodes to anything but an exception tag is a plain mismatch now;
//! * any other entry must decode to an `Op` in its token's family, per
//!   [`FAMILIES`]. A defined opcode decoding to `Illegal`, `Undefined`,
//!   `Unimplemented` or the wrong family is a mismatch that names the opcode
//!   hex and both readings.
//!
//! # Caveats of the map, from its own README (`map/README.md`)
//!
//! * `MOVE USP`, `MOVE SR` and `MOVE CCR` are specialised operations
//!   (`MOVEfromUSP`, `MOVEtoSR`, `MOVEtoCCR`, …) rather than instances of MOVE;
//! * the conditions for Bcc, Scc and DBcc are not recorded, so those names look
//!   repetitive — the comparison is by family, never by condition;
//! * upstream issue SingleStepTests/680x0#2 flags the file's ordering; the map
//!   is read as a JSON object (unordered) and the coverage test asserts it
//!   holds exactly the 65 536 opcode words, so a pin bump that renames or
//!   loses entries fails loudly.
//!
//! Missing data is a skip, never a failure, like every pinned-data test here:
//! `just check` stays green with or without `just fetch 680x0-map`.

use std::collections::HashMap;

use m68k::{Op, decode};
use testkit::test_rom_dir;

/// Where [`scripts/fetch-test-data.sh`](../../scripts/fetch-test-data.sh) puts
/// the map: the 680x0 repo pinned a second time, keeping only its `map/` dir.
const MAP_PATH: &str = "680x0-map/map/68000.official.json";

/// The map, keyed by opcode word. `None` when the data is not fetched (a skip).
fn official_map() -> Option<HashMap<u16, String>> {
    let path = test_rom_dir().join(MAP_PATH);
    let text = std::fs::read_to_string(&path).ok()?;
    let raw: HashMap<String, String> =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{MAP_PATH}: {e}"));
    Some(
        raw.into_iter()
            .map(|(k, v)| {
                let opcode = u16::from_str_radix(&k, 16)
                    .unwrap_or_else(|e| panic!("{MAP_PATH}: key {k:?} is not a 4-hex opcode: {e}"));
                (opcode, v)
            })
            .collect(),
    )
}

/// The map's leading operation token → the [`Op`] variants that token may
/// decode to. One row per token the pinned map uses (74 of them); a token that
/// appears in the data but not here fails the run, so a pin bump that changes
/// the vocabulary cannot pass silently. The vocabulary is the map's own
/// (`map/README.md`); several rustsega distinctions are invisible to it, each
/// noted on its row:
///
/// * the immediate forms are folded into their base mnemonic: the map writes
///   `ADD.w #, (A0)` for `ADDI` and `ADD.l #4, (A0)` for `ADDQ`, so `ADD`
///   covers [`Op::Add`] plus [`Op::Addi`] and [`Op::Addq`] (same for `SUB`,
///   `AND`, `OR`, `EOR`, `CMP`); the to-CCR and to-SR corners are NOT folded
///   (`ORItoCCR`, `ANDItoSR`, …) and map one-to-one onto [`Op::LogicImmCcr`] /
///   [`Op::LogicImmSr`];
/// * `CMP (Ay)+,(Ax)+` is how the map writes `CMPM`, which its README does not
///   name, so `CMP` covers [`Op::Cmp`] plus [`Op::Cmpi`] and [`Op::Cmpm`];
/// * `MOVE #<data>,Dn` is how the map writes `MOVEQ` (its README lists no
///   MOVEQ), so `MOVE` covers [`Op::Move`] and [`Op::Moveq`]; `MOVEA` is its
///   own token;
/// * the shift/rotate mnemonics cover both the register form ([`Op::Shift`])
///   and the single-operand memory form ([`Op::ShiftMemory`]);
/// * the bit operations cover both the register form ([`Op::BitDynamic`]) and
///   the immediate form ([`Op::BitStatic`]);
/// * `UNLINK` is the map's (and the suite's) spelling of `UNLK`.
const FAMILIES: &[(&str, &[Op])] = &[
    ("ABCD", &[Op::Abcd]),
    ("ADD", &[Op::Add, Op::Addi, Op::Addq]),
    ("ADDA", &[Op::Adda]),
    ("ADDX", &[Op::Addx]),
    ("AND", &[Op::And, Op::Andi]),
    ("ANDItoCCR", &[Op::LogicImmCcr]),
    ("ANDItoSR", &[Op::LogicImmSr]),
    ("ASL", &[Op::Shift, Op::ShiftMemory]),
    ("ASR", &[Op::Shift, Op::ShiftMemory]),
    ("BCHG", &[Op::BitDynamic, Op::BitStatic]),
    ("BCLR", &[Op::BitDynamic, Op::BitStatic]),
    ("BSR", &[Op::Bsr]),
    ("BSET", &[Op::BitDynamic, Op::BitStatic]),
    ("BTST", &[Op::BitDynamic, Op::BitStatic]),
    ("Bcc", &[Op::Bcc]),
    ("CHK", &[Op::Chk]),
    ("CLR", &[Op::Clr]),
    ("CMP", &[Op::Cmp, Op::Cmpi, Op::Cmpm]),
    ("CMPA", &[Op::Cmpa]),
    ("DBcc", &[Op::Dbcc]),
    ("DIVS", &[Op::Divs]),
    ("DIVU", &[Op::Divu]),
    ("EOR", &[Op::Eor, Op::Eori]),
    ("EORItoCCR", &[Op::LogicImmCcr]),
    ("EORItoSR", &[Op::LogicImmSr]),
    ("EXG", &[Op::Exg]),
    ("EXT", &[Op::Ext]),
    ("JMP", &[Op::Jmp]),
    ("JSR", &[Op::Jsr]),
    ("LEA", &[Op::Lea]),
    ("LINK", &[Op::Link]),
    ("LSL", &[Op::Shift, Op::ShiftMemory]),
    ("LSR", &[Op::Shift, Op::ShiftMemory]),
    ("MOVE", &[Op::Move, Op::Moveq]),
    ("MOVEA", &[Op::Movea]),
    ("MOVEM", &[Op::Movem]),
    ("MOVEP", &[Op::Movep]),
    ("MOVEfromSR", &[Op::MoveFromSr]),
    ("MOVEfromUSP", &[Op::MoveUsp]),
    ("MOVEtoCCR", &[Op::MoveToCcr]),
    ("MOVEtoSR", &[Op::MoveToSr]),
    ("MOVEtoUSP", &[Op::MoveUsp]),
    ("MULS", &[Op::Muls]),
    ("MULU", &[Op::Mulu]),
    ("NBCD", &[Op::Nbcd]),
    ("NEG", &[Op::Neg]),
    ("NEGX", &[Op::Negx]),
    ("NOP", &[Op::Nop]),
    ("NOT", &[Op::Not]),
    ("OR", &[Op::Or, Op::Ori]),
    ("ORItoCCR", &[Op::LogicImmCcr]),
    ("ORItoSR", &[Op::LogicImmSr]),
    ("PEA", &[Op::Pea]),
    ("RESET", &[Op::Reset]),
    ("ROL", &[Op::Shift, Op::ShiftMemory]),
    ("ROR", &[Op::Shift, Op::ShiftMemory]),
    ("ROXL", &[Op::Shift, Op::ShiftMemory]),
    ("ROXR", &[Op::Shift, Op::ShiftMemory]),
    ("RTE", &[Op::Rte]),
    ("RTR", &[Op::Rtr]),
    ("RTS", &[Op::Rts]),
    ("SBCD", &[Op::Sbcd]),
    ("STOP", &[Op::Stop]),
    ("SUB", &[Op::Sub, Op::Subi, Op::Subq]),
    ("SUBA", &[Op::Suba]),
    ("SUBX", &[Op::Subx]),
    ("SWAP", &[Op::Swap]),
    ("Scc", &[Op::Scc]),
    ("TAS", &[Op::Tas]),
    ("TRAP", &[Op::Trap]),
    ("TRAPV", &[Op::Trapv]),
    ("TST", &[Op::Tst]),
    ("UNLINK", &[Op::Unlk]),
];

/// The leading operation token of a map value: `MOVEtoSR A0` → `MOVEtoSR`,
/// `None` → `None`, `ADD.w #, (A0)` → `ADD`.
fn token(operation: &str) -> &str {
    let end = operation
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(operation.len());
    &operation[..end]
}

/// Tags a `"None"` slot may decode to without being a finding. All of them run
/// the illegal-instruction handler, which is what the 68000 does with a word
/// no instruction occupies:
///
/// * [`Op::Illegal`] — a format the manual defines whose operand is not
///   allowed, or the documented `$4AFC` `ILLEGAL` opcode itself, which the map
///   records as `"None"` even though the manual prints it;
/// * [`Op::Undefined`] — an encoding outside the ISA entirely (#355 split it
///   from `Illegal` so audits could tell the two complaints apart);
/// * [`Op::LineA`] / [`Op::LineF`] — the two 4 096-word emulator-line groups,
///   `"None"` throughout the map; the 68000 has no line emulator *instructions*,
///   so a word in either group raises the matching line-emulator exception
///   (vector 10 / 11 since #482) rather than executing anything, like any other
///   word the map records as `"None"` (measured in #194).
const NONE_EXCEPTION_TAGS: &[Op] = &[Op::Illegal, Op::Undefined, Op::LineA, Op::LineF];

// The `None` slots that decode to an actual instruction — the over-broad
// routes the audit found — are **bugs, not deliberate choices**: on the
// hardware they take the illegal-instruction exception; rustsega instead
// executes them. The audit pinned them here so the cross-check stayed green
// while their fixes were pending, each with a count that failed when the fix
// changed the set; that bookkeeping is gone now that the last pin is off.
//
// The audit's OR/AND over-broad decodes (448 opcodes, `Or`: 256 / `And`: 192)
// were fixed by #618, and their pin entries removed; the MOVEM
// store-to-PC-relative pins (4 opcodes) followed when #619 restricted
// `is_movem_ea`'s store half to control minus PC-relative, and the static-BTST
// pin (1 opcode) when #620 closed the immediate corner of `classify_bit_op`.
// With the pin list empty the predicate went away too: a `None` slot that
// decodes to an instruction is a mismatch again, named with the opcode hex and
// both readings.
//
// The *dynamic* form `BTST Dn,#` is a different story, recorded so it does not
// get "fixed": the map defines `$013C`/`$0B3C`, the suite measures it (`0b3c
// [BTST D5, #]` cases), and rustsega agrees with map+suite — keep it.

/// Walk all 65 536 map entries through [`decode`] and compare.
#[test]
fn decode_agrees_with_the_official_opcode_map() {
    let Some(map) = official_map() else {
        eprintln!("SKIPPED: {MAP_PATH} not found");
        return;
    };

    assert_eq!(map.len(), 65_536, "the map must hold every 16-bit word");

    // The None-slot census: how many words the map calls undefined decode to
    // each tag. Linear find like `decode`'s own count test — `Op` is not `Hash`.
    let mut none_counts: Vec<(Op, u32)> = Vec::new();
    let mut mismatches: Vec<(u16, &str, Op)> = Vec::new();

    for (&opcode, operation) in &map {
        let op = decode(opcode);
        if operation == "None" {
            match none_counts.iter_mut().find(|(seen, _)| *seen == op) {
                Some((_, n)) => *n += 1,
                None => none_counts.push((op, 1)),
            }
            if NONE_EXCEPTION_TAGS.contains(&op) {
                continue;
            }
            mismatches.push((opcode, operation, op));
        } else {
            let name = token(operation);
            let Some(family) = FAMILIES
                .iter()
                .find(|(map_name, _)| *map_name == name)
                .map(|&(_, variants)| variants)
            else {
                panic!(
                    "{MAP_PATH}: operation token {name:?} (from {operation:?}) is not in FAMILIES"
                );
            };
            if !family.contains(&op) {
                mismatches.push((opcode, operation, op));
            }
        }
    }

    // The report: the census first, then every disagreement naming the opcode
    // hex, the map's reading and rustsega's.
    let mut census = none_counts.clone();
    census.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (op, n) in census {
        eprintln!("None -> {op:?}: {n}");
    }
    for &(opcode, operation, op) in &mismatches {
        eprintln!("${opcode:04X}: map says {operation:?}, rustsega decodes {op:?}");
    }
    assert!(
        mismatches.is_empty(),
        "{} decode disagreements with {MAP_PATH} (see the lines above)",
        mismatches.len()
    );
}

/// The coverage half: the map holds exactly the 65 536 opcode words, each a
/// 4-hex key, so the walk above really is whole and a pin bump that renames or
/// loses entries fails here first with a count instead of deep in the compare.
#[test]
fn official_opcode_map_covers_every_word() {
    let Some(map) = official_map() else {
        eprintln!("SKIPPED: {MAP_PATH} not found");
        return;
    };
    assert_eq!(map.len(), 65_536, "one entry per 16-bit word");
    for opcode in 0..=u16::MAX {
        assert!(
            map.contains_key(&opcode),
            "${opcode:04X} missing from {MAP_PATH}"
        );
    }
}

#[test]
fn probe_bit_static() {
    for w in [0x0C3Cu16, 0x0E3C, 0x0DBC, 0x08BC, 0x08DC] {
        eprintln!("PROBE ${w:04X} -> {:?}", decode(w));
    }
}
