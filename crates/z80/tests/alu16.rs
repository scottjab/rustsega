//! The 16-bit arithmetic slice of the Z80 SingleStepTests suite (M2-07, #50).
//!
//! Same deal as the sibling ALU ticket: 1000 hardware-captured cases per
//! opcode, with random register state, and the suite checks every visible and
//! hidden register (including `wz` and the undocumented `q`), plus the T-state
//! count. For this group the hidden state is most of the point: `ADD HL,rr`
//! leaves `S`, `Z` and `P/V` alone, so the suite is what proves `q` still ends
//! up equal to the resulting `F`, and `INC`/`DEC rr` touch no flags at all yet
//! must still leave `q` at 0 rather than at the stale flag byte.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `ADD HL,rr` (`09 19 29 39`), `INC rr` (`03 13 23 33`) and `DEC rr`
/// (`0B 1B 2B 3B`); `ADC HL,rr` and `SBC HL,rr` (`ED 4A/5A/6A/7A` and
/// `ED 42/52/62/72`); and the `DD`/`FD` forms of the three that mention `HL`,
/// where the pair field `2` is the index register itself, so `DD 29` is
/// `ADD IX,IX`.
///
/// There are no indexed `ADC`/`SBC`: they live in the `ED` space, which a
/// `DD`/`FD` cannot reach (the prefix rots there, and `ed` is already covered).
#[test]
fn sst_alu16() {
    let mut names: Vec<String> = [
        "09", "19", "29", "39", "03", "13", "23", "33", "0b", "1b", "2b", "3b",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    names.extend(
        [
            "ed 42", "ed 4a", "ed 52", "ed 5a", "ed 62", "ed 6a", "ed 72", "ed 7a",
        ]
        .into_iter()
        .map(String::from),
    );

    for prefix in ["dd", "fd"] {
        for op in ["09", "19", "29", "39", "23", "2b"] {
            names.push(format!("{prefix} {op}"));
        }
    }

    let files: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(files.len(), 32, "12 + 8 + 12");
    harness::run_files(&files);
}
