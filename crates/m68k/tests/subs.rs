//! The subtract class in the pinned 680x0 suite: `SUB.b/w/l`, `SUBA.w/l`,
//! `SUBX.b/w/l` (M1-11, #19).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateAndCyclesSkipAddressErrors`], the same one the add class uses:
//! the cases that end in the suite's exception handler are left to M1-29 (#37) —
//! see [`harness::is_address_error_case`] and #185.
//!
//! The suite ships no `SUBI.*` or `SUBQ.*` file. Those instructions are not
//! untested by this binary: their cases live in `SUB.b`, `SUB.w` and `SUB.l`,
//! which is 897 cases with a `$04xx` opcode (`0405 [SUB.b #, D5]` — 318/300/279
//! over the three sizes) and 8264 with a `$5xxx` one (`5310 [SUB.b Q, (A0)]`).
//! `src/ops/arith.rs` pins the rules those cases do not cover as units: `SUBQ`'s
//! `000`-means-8 data field, the always-long, flagless `SUBQ` to `An`, and the
//! fetch order of `SUBI`'s extension words.
//!
//! The clock half of the check is #225's and is the add class's rule, which
//! `SUB`/`SUBA`/`SUBQ` follow instruction for instruction: see the module docs
//! of `tests/adds.rs` and `arith::alu_op_clocks`. `SUBX`'s memory form is the
//! one extra fix — its destination resolves as a write, so `-(Ax)` costs no
//! address calculation.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `SUB.B`: `$9000`-`$903F` / `$9100`-`$913F` with the `SUBX` and `SUBA`
/// encodings taken out.
#[test]
fn sub_b_file_passes() {
    harness::assert_file_passes("SUB.b", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `SUB.W`: `$9040`-`$907F` / `$9140`-`$917F`.
#[test]
fn sub_w_file_passes() {
    harness::assert_file_passes("SUB.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `SUB.L`: `$9080`-`$90BF` / `$9180`-`$91BF`.
#[test]
fn sub_l_file_passes() {
    harness::assert_file_passes("SUB.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `SUBA.W`: operation `011`, word only.
#[test]
fn suba_w_file_passes() {
    harness::assert_file_passes("SUBA.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `SUBA.L`: operation `111`.
#[test]
fn suba_l_file_passes() {
    harness::assert_file_passes("SUBA.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `SUBX.B`: `$9100`-`$9107` style, `ss = 00`, which overlaps `SUB`'s
/// `$9100`-`$913F` byte group and must win there.
#[test]
fn subx_b_file_passes() {
    harness::assert_file_passes("SUBX.b", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `SUBX.W`: `ss = 01` inside the `SUB.W` `$9140`-`$917F` range.
#[test]
fn subx_w_file_passes() {
    harness::assert_file_passes("SUBX.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `SUBX.L`: `ss = 10` inside the `SUB.L` `$9180`-`$91BF` range.
#[test]
fn subx_l_file_passes() {
    harness::assert_file_passes("SUBX.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// The eight file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_all_eight_sub_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in [
        "SUB.b", "SUB.w", "SUB.l", "SUBA.w", "SUBA.l", "SUBX.b", "SUBX.w", "SUBX.l",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("SUB"))
                .collect::<Vec<_>>()
        );
    }
}
