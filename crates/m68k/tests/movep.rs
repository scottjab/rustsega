//! `MOVEP` in the pinned 680x0 suite: `MOVEP.w` and `MOVEP.l` (M1-25, #33).
//!
//! One test per suite file, in the style of `movem.rs`, checked with
//! [`harness::Check::StateAndCyclesSkipAddressErrors`]. `MOVEP` only ever does byte
//! accesses, so an odd operand address is legal here and the skip is nearly
//! empty — it stays because the ticket asks for the same check as the other
//! instruction files, and an address error can still be what a case reports
//! (M1-29, #37).
//!
//! # What the files cover
//!
//! `MOVEP.w` runs all 128 encodings of `$0108`..`$0FFF` whose `oo` field is `00`
//! (word, memory to `Dn`) or `10` (word, `Dn` to memory), over every `Dn` and
//! every `An`; `MOVEP.l` the same 128 with `oo` = `01` and `11` (M68000UM MOVEP).
//! So both directions of both sizes are covered for all eight data registers and
//! all eight address registers, which is the whole instruction — `d16(An)` is the
//! only mode MOVEP has.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `MOVEP.w`: `$0108`/`$0148`-style encodings with `oo` = `00` and `10`,
/// `0000 ddd 1 oo 001 aaa` (M68000UM MOVEP).
#[test]
fn movep_w_file_passes() {
    harness::assert_file_passes("MOVEP.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `MOVEP.l`: the same with `oo` = `01` (memory to `Dn`) and `11` (`Dn` to
/// memory), e.g. `$0148` and `$01C8`.
#[test]
fn movep_l_file_passes() {
    harness::assert_file_passes("MOVEP.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// The two file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_the_two_movep_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["MOVEP.w", "MOVEP.l"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("MOVEP"))
                .collect::<Vec<_>>()
        );
    }
}
