//! The packed decimal class in the pinned 680x0 suite: `ABCD`, `SBCD` and
//! `NBCD` (M1-23, #31).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateSkipAddressErrors`], the same one the add, subtract
//! and multiply classes use. It is a formality for these three files — none of
//! their 3 x 8065 cases reaches the suite's exception handler, because no BCD
//! operand address in the suite is odd (M1-29, #37) — but it is the check every
//! ALU ticket uses and it keeps that promise auditable.
//!
//! # What the files cover
//!
//! `ABCD` and `SBCD` are each half register form and half memory form: their
//! 8065 cases split 4032 / 4033 between `Dy,Dx` and `-(Ay),-(Ax)` over every
//! register pair, and both include the `Ax == Ay` case, which is what pins the
//! double predecrement that [`abcd_sbcd_memory_forms_move_both_pointers`] and
//! its `SBCD` twin check. `NBCD` has no register form at all: its operand is
//! the data-alterable EA field, so its cases run over `Dn`, the four indirect
//! modes, the indexed mode and the two absolutes.
//!
//! Invalid packed decimal is in the data, not just valid BCD: `ABCD $FF,$FF`
//! with `X` clear appears, and the suite's answer is `$66` with `C` and `X` set
//! — the binary add followed by the same two `+6` corrections a valid digit
//! gets. That is why `ops::bcd` implements the correction rules rather than a
//! digit-validating decimal adder, and why [`abcd_invalid_nibbles_follow_the_correction_rule`]
//! pins an invalid case rather than only valid ones.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `ABCD`: `$C100`-`$C107`/`$C108`-`$C10F` and the same for every destination
/// (`1100 ddd 100 0m yyy`).
#[test]
fn abcd_file_passes() {
    harness::assert_file_passes("ABCD", harness::Check::StateSkipAddressErrors);
}

/// `SBCD`: `$8100`-`$8107`/`$8108`-`$810F` and the same for every destination
/// (`1000 ddd 100 0m yyy`).
#[test]
fn sbcd_file_passes() {
    harness::assert_file_passes("SBCD", harness::Check::StateSkipAddressErrors);
}

/// `NBCD`: `$4800`-`$483F` (`0100 1000 00 mmm rrr`).
#[test]
fn nbcd_file_passes() {
    harness::assert_file_passes("NBCD", harness::Check::StateSkipAddressErrors);
}

/// The three file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_the_three_bcd_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["ABCD", "SBCD", "NBCD"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("ABC") || n.starts_with("SBC") || n.starts_with("NBC"))
                .collect::<Vec<_>>()
        );
    }
}
