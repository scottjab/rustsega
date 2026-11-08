//! The multiply class in the pinned 680x0 suite: `MULU` and `MULS`
//! (M1-21, #29).
//!
//! Both files hold 8065 cases each, and both are dominated by cases the core
//! cannot finish yet: 3073 of `MULU` and 3122 of `MULS` end in the suite's
//! exception handler (`harness::is_address_error_case`), because an odd
//! operand address has to raise address error — M1-29 (#37). Those are left out
//! by [`harness::Check::StateSkipAddressErrors`], exactly as the add, logic and
//! shift classes do, and the remaining 4992 / 4943 cases have to pass.
//!
//! # Why the cycle tests are separate
//!
//! [`harness::Check::StateAndCycles`] compares `step`'s return against the
//! case's `length` but does *not* skip the exception cases, so it cannot go
//! green for these two files until M1-29 (#37) lands: the state of an exception
//! case is wrong by construction, and its cycles with it. The multiply cost is
//! data-dependent (`38 + 2n`, M68000UM section 8), so it is worth pinning now,
//! and [`run_state_and_cycles_off_exceptions`] does it by filtering with
//! [`harness::is_address_error_case`] itself and then asking for
//! `StateAndCycles` — the same filter the skipped check applies, one step
//! earlier. That is the whole of `MULU, MULS with exact timing`: 4992 + 4943
//! cases whose `length` matches `step`'s return exactly.
//!
//! # What the effective address costs on top
//!
//! `length` for a multiply is `38 + 2n` plus the source operand's own
//! calculation, and `ops::muldiv` charges the second part from a table of its
//! own, because the two instructions have no size field to share with the rest
//! of the group. `length - (38 + 2n)` is one constant per source mode over
//! every non-exception case of both files — no scatter. The allowance and the
//! number of cases per mode (`MULU` / `MULS`) are:
//!
//! | Source EA | allowance | MULU cases | MULS cases |
//! |---|---|---|---|
//! | `Dn` | 0 | 1221 | 1178 |
//! | `(An)` | 4 | 645 | 709 |
//! | `(An)+` | 4 | 683 | 695 |
//! | `-(An)` | 6 | 696 | 708 |
//! | `(d16,An)` | 8 | 634 | 591 |
//! | `(d8,An,Xn)` | 10 | 620 | 631 |
//! | `(xxx).W` | 8 | 81 | 60 |
//! | `(xxx).L` | 12 | 87 | 73 |
//! | `(d16,PC)` | 8 | 85 | 83 |
//! | `(d8,PC,Xn)` | 10 | 75 | 75 |
//! | `#imm` | 4 | 165 | 140 |
//!
//! i.e. 4 clocks per bus access, 4 per extension word and 2 for an index
//! calculation: what the EA layer already charges elsewhere in this core, and
//! the addressing-mode costs M68000UM tabulates. The two cycle tests above are
//! what check it, case by case, rather than this table. No mode `001` (`An`)
//! appears in either file, which is what lets `decode` reject it: the ticket
//! says the source is a data EA, "no An".

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `MULU`: `$C0C0`–`$C0FF` (`1100 ddd 011 eeeeee`), 8065 cases of which 3073
/// take the suite's exception handler.
#[test]
fn mulu_file_passes() {
    harness::assert_file_passes("MULU", harness::Check::StateSkipAddressErrors);
}

/// `MULS`: `$C1C0`–`$C1FF` (`1100 ddd 111 eeeeee`), 8065 cases of which 3122
/// take the suite's exception handler.
#[test]
fn muls_file_passes() {
    harness::assert_file_passes("MULS", harness::Check::StateSkipAddressErrors);
}

/// `MULU`'s cycle count: `step` returns the case's `length` for every case that
/// does not end in the exception handler.
#[test]
fn mulu_cycles_match() {
    run_state_and_cycles_off_exceptions("MULU");
}

/// `MULS`' cycle count, same check (M68000UM section 8: `38 + 2n` where `n` is
/// the transition count of the source, so it is a different `n` to `MULU`).
#[test]
fn muls_cycles_match() {
    run_state_and_cycles_off_exceptions("MULS");
}

/// The two multiply files are both in the suite, so the tests above cannot
/// quietly stop running.
#[test]
fn the_suite_ships_both_multiply_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["MULU", "MULS"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("MUL") || n.starts_with("DIV"))
                .collect::<Vec<_>>()
        );
    }
}

/// Run `name` with [`harness::Check::StateAndCycles`] over the cases that do
/// not end in the suite's exception handler, and fail with the kept failures if
/// any of them differs — in state or in cycles.
fn run_state_and_cycles_off_exceptions(name: &str) {
    let Some(cases) = harness::load_file(name) else {
        eprintln!("{}", harness::skip_line(name));
        return;
    };
    let total = cases.len();
    let runnable: Vec<_> = cases
        .into_iter()
        .filter(|case| !harness::is_address_error_case(case))
        .collect();
    let report = harness::run_cases(&runnable, harness::Check::StateAndCycles);
    assert!(
        report.failures.is_empty(),
        "{name}: {}/{} cases passed ({}/{} skipped as exceptions by hand, and \
         `StateAndCycles` does not skip them itself); first {} failing cases:\n{}",
        report.passed,
        report.total,
        total - report.total,
        total,
        report.failures.len(),
        report.failures.join("\n"),
    );
}
