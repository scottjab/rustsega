//! The add class in the pinned 680x0 suite: `ADD.b/w/l`, `ADDA.w/l`,
//! `ADDX.b/w/l` (M1-10, #18).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateAndCyclesSkipAddressErrors`], the same one the move class
//! uses: the cases that end in the suite's exception handler are left to
//! M1-29 (#37) — see [`harness::is_address_error_case`] and #185.
//!
//! The clock half of that check is #225's. It holds on one rule, in
//! `arith::alu_op_clocks`: a long ALU operation is two passes of 2 clocks and
//! every memory access the operand performs covers one, so `.b`/`.w` are exact
//! everywhere, a long `<ea>,Dn` is 2 over for a memory source and 4 for a
//! register, `An` or `#imm` one, and a long read-modify-write — `ADD D0,(A0)`,
//! `ADDI`, `ADDQ` — is exact again. `ADDA.W` pays its sign-extension pass in
//! every mode, `ADDQ`/`SUBQ` to `An` follow their own measured pair, and
//! `ADDX`/`SUBX` in the memory form resolve their destination as a write so its
//! predecrement is free (`ea::EaUse`).
//!
//! The suite ships no `ADDI.*` or `ADDQ.*` file. Those instructions are not
//! untested by this binary: their cases live in `ADD.b`, `ADD.w` and `ADD.l`,
//! which is 910 cases at `$06xx` (`0600 [ADD.b #, D0]`) and 8265 at `$5xxx`
//! (`5010 [ADD.b Q, (A0)]`). `src/ops/arith.rs` pins the rules those cases do
//! not cover as units: `ADDQ`'s `000`-means-8 data field, the always-long,
//! flagless `ADDQ` to `An`, and the fetch order of `ADDI`'s extension words.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `ADD.B`: `$D000`-`$D03F` / `$D100`-`$D13F` with the `ADDX` and `ADDA`
/// encodings taken out.
#[test]
fn add_b_file_passes() {
    harness::assert_file_passes("ADD.b", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ADD.W`: `$D040`-`$D07F` / `$D140`-`$D17F`.
#[test]
fn add_w_file_passes() {
    harness::assert_file_passes("ADD.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ADD.L`: `$D080`-`$D0BF` / `$D180`-`$D1BF`.
#[test]
fn add_l_file_passes() {
    harness::assert_file_passes("ADD.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ADDA.W`: operation `011`, word only.
#[test]
fn adda_w_file_passes() {
    harness::assert_file_passes("ADDA.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ADDA.L`: operation `111`.
#[test]
fn adda_l_file_passes() {
    harness::assert_file_passes("ADDA.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ADDX.B`: `$D100`-`$D107` style, `ss = 00`, which overlaps `ADD`'s
/// `$D100`-`$D13F` byte group and must win there.
#[test]
fn addx_b_file_passes() {
    harness::assert_file_passes("ADDX.b", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ADDX.W`: `ss = 01` inside the `ADD.W` `$D140`-`$D17F` range.
#[test]
fn addx_w_file_passes() {
    harness::assert_file_passes("ADDX.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ADDX.L`: `ss = 10` inside the `ADD.L` `$D180`-`$D1BF` range.
#[test]
fn addx_l_file_passes() {
    harness::assert_file_passes("ADDX.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// The eight file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_all_eight_add_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in [
        "ADD.b", "ADD.w", "ADD.l", "ADDA.w", "ADDA.l", "ADDX.b", "ADDX.w", "ADDX.l",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("ADD"))
                .collect::<Vec<_>>()
        );
    }
}
