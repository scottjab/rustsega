//! Conformance tests for M1-18 (#26): shifts and rotates, register form
//! (`ASL`, `ASR`, `LSL`, `LSR`, `ROL`, `ROR`, `ROXL`, `ROXR`).
//!
//! One test per suite file, all `Check::StateAndCyclesSkipAddressErrors`: a register-form
//! shift has no operand to fault on, and the flag rules are the point of the
//! exercise. The eight `.w` files are *not* here — they carry the memory-form
//! `ASx.w <ea>` cases, which are M1-19 (#27); see
//! `crates/m68k/tests/shifts_word_memory_form.rs`, which checks that those files
//! fail on memory-form cases and nothing else.
//!
//! One file is not run whole: `ASL.b` carries two cases whose recorded final
//! state cannot come from their own recorded initial state, so
//! `asl_b_file_passes` drops exactly those two and says so. See
//! [`ASL_B_BROKEN_CASES`].
//!
//! Run with `RUSTSEGA_VERBOSE=1` to see every file's line count.

#[path = "harness/mod.rs"]
mod harness;

use harness::Check;

fn file_passes(name: &str) {
    harness::assert_file_passes(name, Check::StateAndCyclesSkipAddressErrors);
}

/// Two cases of `ASL.b` whose recorded final state cannot come from their own
/// recorded initial state, on an 68000 or any other machine: #209.
///
/// Both are `$E502` = `ASL.B #2,D2` (`i` = 0, so `ccc` = 2 is an immediate
/// count; `ss` = `00` is byte), and each runs exactly one instruction, so a byte
/// shift writes one byte: the upper three bytes of `D2` have to survive it.
///
/// | case | `D2` in | `D2` asked for | byte shift by 2 gives |
/// |---|---|---|---|
/// | `1583` | `$CDFB7FBE` | `$2E5E4304` | `$CDFB7FF8` |
/// | `1761` | `$417C7E7D` | `$6461D390` | `$417C7EF4` |
///
/// Each requested value is not a shift of the value it starts from, and each
/// recorded final *is* a correct `ASL.B #2` — data and all five condition codes
/// — of some other register: `$2E5E4341 << 2` is `$2E5E4304` with `N=0 Z=0 V=1
/// C=X=1`, and `$6461D364 << 2` is `$6461D390` with `N=1 Z=0 V=1 C=X=1`, exactly
/// the `final.sr` of the two cases. So the snapshots are individually plausible
/// but do not belong to each other, which no emulator behaviour can fix.
///
/// They are the only two cases in the 177,314 register-form cases of the 24
/// shift and rotate files (193,560 cases in all) that ask a byte or word
/// instruction to change the destination's bits above its own width, and the
/// same file runs `ASL.B #2,D2` on both `initial.d2` values two cases earlier
/// (`... 1341`, `... 1722`) recording the results we compute.
///
/// So `asl_b_file_passes` runs `ASL.b` with these two cases dropped. That is a
/// hole in the coverage of one file, not a weakening of a rule: every other case
/// of `ASL.b`, including every other `$E502` case, is checked, and the
/// whole-suite gate (`singlestep.rs`'s `all_instructions`) still counts the two
/// as failures — it lets the file fail only because `KNOWN_FAILURES` carries
/// #209, and it stops letting it the moment they pass.
const ASL_B_BROKEN_CASES: [&str; 2] = ["e502 [ASL.b Q, D2] 1583", "e502 [ASL.b Q, D2] 1761"];

/// Every case of `ASL.b` except [`ASL_B_BROKEN_CASES`].
#[test]
fn asl_b_file_passes() {
    let Some(cases) = harness::load_file("ASL.b") else {
        eprintln!("{}", harness::skip_line("ASL.b"));
        return;
    };
    let broken = |name: &str| ASL_B_BROKEN_CASES.contains(&name);
    let total = cases.len();
    let kept: Vec<_> = cases
        .into_iter()
        .filter(|case| !broken(&case.name))
        .collect();
    assert_eq!(
        kept.len() + ASL_B_BROKEN_CASES.len(),
        total,
        "ASL.b: both broken case names must still be in the file, or this filter is stale"
    );

    let report = harness::run_cases(&kept, Check::StateAndCyclesSkipAddressErrors);
    assert!(
        report.failures.is_empty(),
        "ASL.b: {}/{} cases passed, {} skipped; first failing cases:\n{}",
        report.passed,
        report.total,
        report.skipped,
        report.failures.join("\n"),
    );
    println!(
        "ASL.b: {}/{} cases passed ({} dropped as broken suite data: {})",
        report.passed,
        report.total,
        ASL_B_BROKEN_CASES.len(),
        ASL_B_BROKEN_CASES.join(", "),
    );
}

#[test]
fn asl_l_file_passes() {
    file_passes("ASL.l");
}

#[test]
fn asr_b_file_passes() {
    file_passes("ASR.b");
}

#[test]
fn asr_l_file_passes() {
    file_passes("ASR.l");
}

#[test]
fn lsl_b_file_passes() {
    file_passes("LSL.b");
}

#[test]
fn lsl_l_file_passes() {
    file_passes("LSL.l");
}

#[test]
fn lsr_b_file_passes() {
    file_passes("LSR.b");
}

#[test]
fn lsr_l_file_passes() {
    file_passes("LSR.l");
}

#[test]
fn rol_b_file_passes() {
    file_passes("ROL.b");
}

#[test]
fn rol_l_file_passes() {
    file_passes("ROL.l");
}

#[test]
fn ror_b_file_passes() {
    file_passes("ROR.b");
}

#[test]
fn ror_l_file_passes() {
    file_passes("ROR.l");
}

#[test]
fn roxl_b_file_passes() {
    file_passes("ROXL.b");
}

#[test]
fn roxl_l_file_passes() {
    file_passes("ROXL.l");
}

#[test]
fn roxr_b_file_passes() {
    file_passes("ROXR.b");
}

#[test]
fn roxr_l_file_passes() {
    file_passes("ROXR.l");
}
