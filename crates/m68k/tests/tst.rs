//! `TST` in the pinned 680x0 suite: `TST.b/w/l` (M1-12, #20).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateSkipAddressErrors`], the same one the move, add and
//! subtract classes use: the cases that end in the suite's exception handler are
//! left to M1-29 (#37) — see [`harness::is_address_error_case`] and #185.
//!
//! Each of the three files is 8065 cases, all of them `$4A00`-`$4ABF`-shaped,
//! spread over exactly the eight operand forms the 68000 allows: `Dn`, `(An)`,
//! `(An)+`, `-(An)`, `d16(An)`, `d8(An,Xn)` and the two absolutes. No `An`, no
//! PC-relative and no immediate appears, which is what
//! `decode::tests::tst_decodes_and_leaves_tas_alone` pins as `Op::Illegal`,
//! together with the fact that `$4AC0`-`$4AFF` is TAS's and is not ours.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `TST.B`: `$4A00`-`$4A3F`.
#[test]
fn tst_b_file_passes() {
    harness::assert_file_passes("TST.b", harness::Check::StateSkipAddressErrors);
}

/// `TST.W`: `$4A40`-`$4A7F`.
#[test]
fn tst_w_file_passes() {
    harness::assert_file_passes("TST.w", harness::Check::StateSkipAddressErrors);
}

/// `TST.L`: `$4A80`-`$4ABF`. The next sixth of the group, `$4AC0`-`$4AFF`, is
/// TAS (M1-27, #35) and ILLEGAL at `$4AFC`.
#[test]
fn tst_l_file_passes() {
    harness::assert_file_passes("TST.l", harness::Check::StateSkipAddressErrors);
}

/// The three file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_all_three_tst_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["TST.b", "TST.w", "TST.l"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("TST"))
                .collect::<Vec<_>>()
        );
    }
}
