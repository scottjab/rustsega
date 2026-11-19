//! `CHK` in the pinned 680x0 suite (M1-27, #35).
//!
//! One test for `CHK.json.gz`, in the style of `status.rs`, checked with
//! [`harness::Check::StateAndCyclesSkipExceptions`] since #241: the trap clock
//! law below made the file's 4948 ran cases deterministic, so clocks are now
//! compared along with state. It is still not [`harness::Check::StateAndCycles`]
//! because a large share of its cases end in the suite's exception handler on an
//! odd address and the skip is doing real work here rather than hiding anything
//! (the file runs all eighteen addressing modes).
//!
//! # What the file covers
//!
//! `CHK.W` is `$4000 | (n << 9) | $180 | <ea>` — `0100 ddd 110 mmm rrr`
//! (M68000UM "CHK — Check Data Register Against Upper Bound"). It is a signed
//! 16-bit bounds check that writes nothing: `Dn` is read, compared against the
//! operand, and left alone. Two things make it more interesting than a `CMP`:
//!
//! * it raises vector 6 (slot `$18`) itself, so the file exercises
//!   [`harness`]'s exception path from an instruction that is not a divide and is
//!   not restarted — the frame carries the address of the *next* instruction, and
//!   the file's trapping cases check that the run continues at the handler rather
//!   than re-executing the CHK;
//! * its operand is read-only, so `#immediate` and the PC-relative modes are legal
//!   operands and only `An` direct is not — the file runs all of them, and it is
//!   the reason `CHK` classifies through `is_data` rather than the
//!   `is_data_alterable` the `NEG`/`CLR`/`TAS` group next door uses;
//! * its condition codes are mostly the "undefined" the manual mentions, and the
//!   file pins what the parts do: `Z`, `V` and `C` cleared on every path, `N` left
//!   alone unless the trap fired. `ops::arith::M68k::op_chk` works through the two
//!   cases that separate that from `CMP` semantics and from touching nothing.
//!
//! # The clock law (`length` over all 4948 cases the suite dates)
//!
//! Let `ea` be the bound operand's access cost and the compare be the signed
//! 16-bit one the manual describes (`Dn < 0 || Dn > <ea>`). Which of the two
//! conditions fires the trap is what picks the trap length, and the instruction
//! pays for whichever ran before the exception entry:
//!
//! | `Dn.w` vs bound (signed) | outcome | suite `length` | per case |
//! |---|---|---|---|
//! | `Dn > bound` | trap, high | `38 + ea` | 2409 |
//! | `Dn < 0`, `Dn <= bound` | trap, sign only | `40 + ea` | 1863 |
//! | `0 <= Dn <= bound` | in range | `10 + ea` | 676 |
//!
//! The 38 row covers the "both hold" cases too: a negative `Dn` that the bound
//! compare would also have flagged traps at 38, not 40. Measured against every
//! case in the file the rule above has zero mismatches, and it is not a fit to
//! the data — it is Motorola's own bus timing, two rows in the per-instruction
//! tables this repo cites as yacht.txt: `trap : Dn > Src` at `38(5/3) + ea` and
//! `trap : Dn < 0` at `40(5/3) + ea` (no-trap `10(1/0) + ea`). The manual's
//! exception-summary table rounds both trap rows to a single worst-case
//! `40(4/3) + ea`, which is where the "one number" readings of CHK timing come
//! from. The core models this in `ops::arith::M68k::op_chk`.
//!
//! The `CHK.L` block below each register's `CHK.W` (`0100 ddd 100 mmm rrr`) is a
//! 68020 instruction; the 68000 has no case for it and it stays in the
//! illegal-instruction path.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `CHK.W Dn,<ea>`: `$4180` + `<ea>` for `D0`, walked for all eight `Dn` and all
/// eighteen addressing modes (M68000UM "CHK — Check Data Register Against Upper
/// Bound"). Cycles are compared too: the trap clock law of #241 covers every
/// case the suite dates, so the trap paths are pinned like the rest.
#[test]
fn chk_file_passes() {
    harness::assert_file_passes("CHK", harness::Check::StateAndCyclesSkipExceptions);
}

/// The file name above is what the suite ships, so a rename fails here with a
/// directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_the_chk_file() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    assert!(
        names.iter().any(|n| n == "CHK"),
        "CHK should be in the suite; it lists {:?}",
        names
            .iter()
            .filter(|n| n.starts_with('C'))
            .collect::<Vec<_>>()
    );
}
