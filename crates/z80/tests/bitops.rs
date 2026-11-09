//! The bit test/set/reset slice of the Z80 SingleStepTests suite (M2-09, #52).
//!
//! Same shape as the rotate ticket: 1000 hardware-captured cases per opcode,
//! checking every visible and hidden register plus the RAM the instruction
//! wrote. Two things make this slice its own ticket rather than a continuation
//! of the rotates.
//!
//! First, `BIT` is the only group in the `CB` space that reads memory without
//! writing it back, and the only one that sets flags at all: `RES` and `SET`
//! leave `F` exactly as they found it and leave `q` at zero, so a single test
//! binary that mixed them would not say which of the two rules broke.
//!
//! Second, `DD CB d op` / `FD CB d op` is the indexed version of all three
//! groups, and for `RES`, `SET` and the rotates it has an undocumented second
//! destination: the result lands in the memory at `IX + d` *and* in register
//! `z`. `z = 6` is the memory slot itself and copies nowhere. Those 512 files
//! are where `wz = IX + d`, the 20 T-state `BIT` and the 23 T-state
//! read-modify-write all come from.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `cb 40` through `cb ff` (`BIT`, `RES`, `SET` over the eight operand slots,
/// `z = 6` being the memory at `HL`), and all 256 `dd cb __ __` and
/// `fd cb __ __` files, where `__` is the displacement: the suite groups every
/// displacement of one opcode into a single file.
#[test]
fn sst_bitops() {
    let mut names: Vec<String> = (0x40u8..=0xFF).map(|op| format!("cb {op:02x}")).collect();
    for prefix in ["dd", "fd"] {
        names.extend((0x00u8..=0xFF).map(|op| format!("{prefix} cb __ {op:02x}")));
    }

    let files: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(files.len(), 192 + 512, "192 CB opcodes + 2 x 256 indexed");
    harness::run_files(&files);
}
