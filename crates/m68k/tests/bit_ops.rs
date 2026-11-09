//! The single-bit operations in the pinned 680x0 suite: `BTST`, `BCHG`, `BCLR`
//! and `BSET` (M1-20, #28).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateAndCyclesSkipAddressErrors`], the same one the logic and shift
//! classes use: the cases that end in the suite's exception handler are left to
//! M1-29 (#37) — see [`harness::is_address_error_case`] and #185.
//!
//! Each of the four files is 8065 cases, mostly the dynamic form (`0000 ddd 1 tt
//! eeeeee`, bit number in a `Dn`) with a static tail (`0000 1000 tt eeeeee`, bit
//! number in the extension word) of 880, 892, 899 and 966 cases for `BTST`,
//! `BCHG`, `BCLR` and `BSET`. Measured over the four files, the operand modes
//! that appear are `0`, `2`, `3`, `4`, `5`, `6` and `7` — mode `1`, `An` direct,
//! never does, because that mode of the `$0000` group is `MOVEP`, which M1-25
//! (#33) owns. So no case of these four files is excluded by
//! [`harness::is_address_error_case`].

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `BTST`: `BTST.json.gz`, 8065 cases.
#[test]
fn btst_file_passes() {
    harness::assert_file_passes("BTST", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `BCHG`: `BCHG.json.gz`, 8065 cases.
#[test]
fn bchg_file_passes() {
    harness::assert_file_passes("BCHG", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `BCLR`: `BCLR.json.gz`, 8065 cases.
#[test]
fn bclr_file_passes() {
    harness::assert_file_passes("BCLR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `BSET`: `BSET.json.gz`, 8065 cases.
#[test]
fn bset_file_passes() {
    harness::assert_file_passes("BSET", harness::Check::StateAndCyclesSkipAddressErrors);
}
