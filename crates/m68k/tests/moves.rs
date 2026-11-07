//! The move class in the pinned 680x0 suite: MOVE.b/w/l, MOVEA.w/l and MOVEQ
//! (M1-09, #17).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateSkipAddressErrors`]: these are the first files the
//! core runs real memory traffic through the EA layer on, and the cases that
//! end in the suite's exception handler are left to M1-29 (#37) — see
//! [`harness::is_address_error_case`] and #185 for what that predicate really
//! skips.
//!
//! MOVEQ's file is `MOVE.q`, the suite's own spelling: its cases are labelled
//! like `7cb5 [MOVE.q Q, D6] 1`, so reading the mnemonic off the file name
//! would call the instruction `MOVE.q`. The opcode in that first case is
//! `$7CB5`, which is MOVEQ with bit 8 clear as the 68000 requires.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `MOVE.B`: `$0001`-`$1FFF`, byte size, destination mode `001` illegal.
#[test]
fn move_b_file_passes() {
    harness::assert_file_passes("MOVE.b", harness::Check::StateSkipAddressErrors);
}

/// `MOVE.W`: `$3000`-`$3FFF` minus the MOVEA encodings.
#[test]
fn move_w_file_passes() {
    harness::assert_file_passes("MOVE.w", harness::Check::StateSkipAddressErrors);
}

/// `MOVE.L`: `$2000`-`$2FFF` minus the MOVEA encodings.
#[test]
fn move_l_file_passes() {
    harness::assert_file_passes("MOVE.l", harness::Check::StateSkipAddressErrors);
}

/// `MOVEA.W`: destination mode `001` in the word group, word only.
#[test]
fn movea_w_file_passes() {
    harness::assert_file_passes("MOVEA.w", harness::Check::StateSkipAddressErrors);
}

/// `MOVEA.L`: destination mode `001` in the long group.
#[test]
fn movea_l_file_passes() {
    harness::assert_file_passes("MOVEA.l", harness::Check::StateSkipAddressErrors);
}

/// `MOVEQ`: `$7000`-`$7FFF`, with bit 8 set an illegal instruction on the 68000.
#[test]
fn moveq_file_passes() {
    harness::assert_file_passes("MOVE.q", harness::Check::StateSkipAddressErrors);
}

/// The six file names above are the ones the suite actually ships, so a rename
/// or a mis-spelling (`MOVEQ.q` vs `MOVE.q`) fails here with the directory
/// listing instead of silently skipping a missing file.
#[test]
fn the_suite_ships_all_six_move_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in [
        "MOVE.b",
        "MOVE.w",
        "MOVE.l",
        "MOVEA.w",
        "MOVEA.l",
        "MOVE.q",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("MOVE"))
                .collect::<Vec<_>>()
        );
    }
}
