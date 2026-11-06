//! The 16-bit-load slice of the Z80 SingleStepTests suite (M2-05, #48).
//!
//! Same deal as the 8-bit loads: 1000 hardware-captured cases per opcode, with
//! random register and memory state, and the suite checks every visible and
//! hidden register (including `wz`), the RAM bytes the instruction wrote, and
//! the T-state count.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `LD rr,nn`, `LD IX/IY,nn`, the `(nn)` word forms, `LD SP,HL`, `PUSH`/`POP`,
/// `EX AF,AF'`, `EXX`, `EX DE,HL` and `EX (SP),HL`, plus the `ED` word loads and
/// the whole `DD`/`FD` slice of this group.
///
/// The `dd`/`fd` pairs `eb` and `d9` are beyond the ticket's list, but they are
/// implemented here (the prefix cannot reach either instruction, so it rots) and
/// the suite has files for them, so they get checked too.
#[test]
fn sst_ld16() {
    let files = [
        // LD rr,nn / LD (nn),HL / LD HL,(nn) / LD SP,HL
        "01", "11", "21", "31", "22", "2a", "f9", //
        // POP qq / PUSH qq
        "c1", "d1", "e1", "f1", "c5", "d5", "e5", "f5", //
        // the exchanges
        "08", "d9", "eb", "e3", //
        // ED LD (nn),rr and ED LD rr,(nn)
        "ed 43", "ed 4b", "ed 53", "ed 5b", "ed 63", "ed 6b", "ed 73", "ed 7b",
    ];

    let mut names: Vec<String> = files.iter().map(|s| (*s).to_string()).collect();
    for prefix in ["dd", "fd"] {
        for op in ["21", "22", "2a", "e1", "e3", "e5", "f9", "eb", "d9"] {
            names.push(format!("{prefix} {op}"));
        }
    }

    let files: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(files.len(), 45, "19 + 8 + 18");
    harness::run_files(&files);
}
