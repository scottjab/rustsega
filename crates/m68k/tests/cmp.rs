//! The compare class in the pinned 680x0 suite: `CMP.b/w/l` and `CMPA.w/l`
//! (M1-12, #20).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateAndCyclesSkipAddressErrors`] — state *and* `step`'s
//! clock total (M1-31a, #39); the cases that end in the suite's exception
//! handler are left to M1-29 (#37) — see [`harness::is_address_error_case`] and
//! #185.
//!
//! The suite ships no `CMPI.*` or `CMPM.*` file. Neither instruction is
//! untested by this binary: their cases live in `CMP.b`, `CMP.w` and `CMP.l`,
//! which are 8065 cases each of which 796 / 614 / 640 carry a `$0Cxx` opcode
//! (`0c00 [CMP.b #, D0]` — 2050 in all) and 911 / 889 / 877 a CMPM one
//! (`b109 [CMP.b (A1)+, (A0)+]` — 2677 in all, the `$B1xx`/`$B3xx`/… half of
//! EOR's opcode space). `src/ops/arith.rs` pins the rules those cases do not
//! cover as units: `CMPA.W`'s sign extension to a 32-bit compare, CMPM reading
//! `(Ay)` first and stepping each pointer exactly once, and the fetch order of
//! `CMPI`'s extension words.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `CMP.B`: `$B000`-`$B03F` plus the `$0C00`-`$0C3F` `CMPI.B` and
/// `$B108`-`$B10F`-style `CMPM.B` cases.
#[test]
fn cmp_b_file_passes() {
    harness::assert_file_passes("CMP.b", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `CMP.W`: `$B040`-`$B07F`, `$0C40`-`$0C7F`, `$B148`-`$B14F`-style.
#[test]
fn cmp_w_file_passes() {
    harness::assert_file_passes("CMP.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `CMP.L`: `$B080`-`$B0BF`, `$0C80`-`$0CBF`, `$B188`-`$B18F`-style.
///
/// Cycle-checked since #230, which found the file's only wrong charge to be
/// `CMPI.L #,Dn` — the one long compare whose destination register covers
/// nothing and whose immediate operand was already popped by the fetch.
#[test]
fn cmp_l_file_passes() {
    harness::assert_file_passes("CMP.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `CMPA.W`: operation `011`, so high bytes `$B0`, `$B2`, … `$BE`. Word sources
/// are sign-extended before the 32-bit compare.
#[test]
fn cmpa_w_file_passes() {
    harness::assert_file_passes("CMPA.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `CMPA.L`: operation `111`, the bit-8-set half of the group CMPM also lives in.
#[test]
fn cmpa_l_file_passes() {
    harness::assert_file_passes("CMPA.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// The five file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_all_five_cmp_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["CMP.b", "CMP.w", "CMP.l", "CMPA.w", "CMPA.l"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("CMP"))
                .collect::<Vec<_>>()
        );
    }
}
