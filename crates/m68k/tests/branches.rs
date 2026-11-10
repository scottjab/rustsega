//! The branch/condition files of the pinned 680x0 suite (M1-16, #24): `Bcc`
//! (which contains `BRA`), `BSR`, `DBcc` and `Scc`.
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateAndCyclesSkipAddressErrors`]: state *and* the clock
//! count, as in the add, logic and move files. M1-31c (#41) turned the cycle
//! half on for these four files; the cases that end in the suite's exception
//! handler are still left to M1-29 (#37) — see [`harness::is_address_error_case`]
//! and #185. Of the 8065 cases in each file the cycle check now covers 5865
//! (`Bcc`), 4085 (`BSR`), 6101 (`DBcc`) and all 8065 (`Scc`).
//!
//! The suite has no `BRA.json.gz`: `BRA` is `Bcc` with condition code `0000`,
//! so `$6000`..`$60FF` of `Bcc.json.gz` is `BRA.B` and `$6000` with a zero low
//! byte is `BRA.W`. `BSR` is its own file because it is a different
//! instruction (`$61`, condition code `0001` of the same group), and it is the
//! only one of the four that writes the stack.
//!
//! `DBcc.json.gz` and `Scc.json.gz` both run over condition codes `$50`..`$5F`
//! and are separated by the EA mode field alone — mode `001` is `DBcc`, every
//! other mode `Scc` — so the two files together are what pins
//! `decode::classify_scc_dbcc`.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `Bcc` and `BRA`: `$6000`..`$6FFF` except condition code `0001`, branching to
/// `pc + 2 + d8` (or `+ d16` when `d8` is zero), taking no flags.
#[test]
fn bcc_file_passes() {
    harness::assert_file_passes("Bcc", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `BSR`: `$6100`..`$61FF`, the same displacement plus a long push of the
/// address of the following instruction.
#[test]
fn bsr_file_passes() {
    harness::assert_file_passes("BSR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `DBcc`: `$50C8`..`$5FCF`, decrement-and-branch on a word counter.
#[test]
fn dbcc_file_passes() {
    harness::assert_file_passes("DBcc", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `Scc`: `$50C0`..`$5FC7` at every mode but `001`, writing `$FF` or `$00`.
#[test]
fn scc_file_passes() {
    harness::assert_file_passes("Scc", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// The four file names above are what the suite ships for this ticket, so a
/// rename fails here with a directory listing instead of a test that quietly
/// does nothing. `BRA` in particular is not a file of its own.
#[test]
fn the_suite_ships_all_four_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["Bcc", "BSR", "DBcc", "Scc"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; its branch-file entries are {:?}",
            names
                .iter()
                .filter(|n| {
                    let head = n.split('.').next().unwrap_or(n);
                    matches!(head, "Bcc" | "BSR" | "DBcc" | "Scc" | "BRA")
                })
                .collect::<Vec<_>>()
        );
    }
}
