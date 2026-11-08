//! `CHK` in the pinned 680x0 suite (M1-27, #35).
//!
//! One test for `CHK.json.gz`, in the style of `status.rs`, checked with
//! [`harness::Check::StateSkipAddressErrors`]. The file runs `CHK.W Dn,<ea>` for
//! every `Dn` and every addressing mode, so a large share of its cases end in the
//! suite's exception handler on an odd address and the skip is doing real work
//! here rather than hiding anything.
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
//! The `CHK.L` block below each register's `CHK.W` (`0100 ddd 100 mmm rrr`) is a
//! 68020 instruction; the 68000 has no case for it and it stays in the
//! illegal-instruction path.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `CHK.W Dn,<ea>`: `$4180` + `<ea>` for `D0`, walked for all eight `Dn` and all
/// eighteen addressing modes (M68000UM "CHK — Check Data Register Against Upper
/// Bound").
#[test]
fn chk_file_passes() {
    harness::assert_file_passes("CHK", harness::Check::StateSkipAddressErrors);
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
