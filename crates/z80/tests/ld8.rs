//! The 8-bit-load slice of the Z80 SingleStepTests suite (M2-04, #47).
//!
//! Every opcode listed here gets 1000 hardware-captured cases: random register
//! and memory state, and the suite compares all visible and hidden registers
//! (including `wz`), the RAM bytes it wrote, and the T-state count.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// Every 8-bit load: `LD r,r'` (`40`-`7F` bar `76`, which is HALT and belongs
/// to M2-13), `LD r,n`, `LD (HL),n`, the `BC`/`DE`/`(nn)` accumulator forms, and
/// the whole `(IX+d)` / `(IY+d)` set for both prefixes.
///
/// The list stops short of `DD 76` / `FD 76`. The decode tables say those are
/// `LD (IX+d),(HL)`, and that is how I wrote them, but the suite disagrees: both
/// files are 8 T-states with `PC` up by 2 and no displacement fetched, which is
/// a rotted prefix followed by a HALT. They belong with HALT (M2-13, #56), and
/// `dd_76_is_halt_with_a_wasted_prefix` in the crate pins that shape.
#[test]
fn sst_ld8() {
    let mut names: Vec<String> = (0x40u16..=0x7F)
        .filter(|op| *op != 0x76)
        .map(|op| format!("{op:02x}"))
        .collect();

    names.extend(
        [
            "06", "0e", "16", "1e", "26", "2e", "36", "3e", // LD r,n and LD (HL),n
            "02", "0a", "12", "1a", // (BC) / (DE) with the accumulator
            "32", "3a", // (nn) with the accumulator
        ]
        .into_iter()
        .map(String::from),
    );

    for prefix in ["dd", "fd"] {
        for op in 0x36u16..=0x7F {
            let indexed_read = matches!(op, 0x46 | 0x4E | 0x56 | 0x5E | 0x66 | 0x6E | 0x7E);
            let indexed_write = (0x70..=0x75).contains(&op) || op == 0x77;
            if op == 0x36 || indexed_read || indexed_write {
                names.push(format!("{prefix} {op:02x}"));
            }
        }
    }

    let files: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(files.len(), 107, "63 + 8 + 6 + 16 + 16");
    harness::run_files(&files);
}
