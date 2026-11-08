//! The `.w` half of the shift and rotate sweep: the eight `.w` files, run whole.
//!
//! Until M1-19 (#27) these files could not pass, and this test said exactly why.
//! A `.w` name says nothing about the *data* size: each file carries both the
//! `ss` = `00`/`01`/`10` register-form cases of its mnemonic at all three sizes
//! and the `ss` = `11` memory-form `ASx.w <ea>` cases, and #26 implemented only
//! the first kind. So this file asserted the split rather than a result — every
//! register-form case passed and everything that failed was a memory-form case.
//!
//! With memory-form `ASx.w <ea>` implemented the premise is gone, so the test is
//! what its successor always was meant to be: each `.w` file passes completely,
//! register-form and memory-form cases alike. That is why the memory-form cases
//! are not listed or filtered here; the point is that nothing is left over.
//!
//! Run with `RUSTSEGA_VERBOSE=1` to see each file's line count.

#[path = "harness/mod.rs"]
mod harness;

use harness::Check;

fn file_passes(name: &str) {
    harness::assert_file_passes(name, Check::StateSkipAddressErrors);
}

#[test]
fn asl_w_file_passes() {
    file_passes("ASL.w");
}

#[test]
fn asr_w_file_passes() {
    file_passes("ASR.w");
}

#[test]
fn lsl_w_file_passes() {
    file_passes("LSL.w");
}

#[test]
fn lsr_w_file_passes() {
    file_passes("LSR.w");
}

#[test]
fn rol_w_file_passes() {
    file_passes("ROL.w");
}

#[test]
fn ror_w_file_passes() {
    file_passes("ROR.w");
}

#[test]
fn roxl_w_file_passes() {
    file_passes("ROXL.w");
}

#[test]
fn roxr_w_file_passes() {
    file_passes("ROXR.w");
}
