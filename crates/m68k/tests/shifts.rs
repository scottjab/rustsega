//! Conformance tests for M1-18 (#26): shifts and rotates, register form
//! (`ASL`, `ASR`, `LSL`, `LSR`, `ROL`, `ROR`, `ROXL`, `ROXR`).
//!
//! One test per suite file, all `Check::StateSkipAddressErrors`: a register-form
//! shift has no operand to fault on, and the flag rules are the point of the
//! exercise. The eight `.w` files are *not* here — they carry the memory-form
//! `ASx.w <ea>` cases, which are M1-19 (#27); see
//! `crates/m68k/tests/shifts_word_memory_form.rs`, which checks that those files
//! fail on memory-form cases and nothing else.
//!
//! Run with `RUSTSEGA_VERBOSE=1` to see every file's line count.

#[path = "harness/mod.rs"]
mod harness;

use harness::Check;

fn file_passes(name: &str) {
    harness::assert_file_passes(name, Check::StateSkipAddressErrors);
}

#[test]
fn asl_b_file_passes() {
    file_passes("ASL.b");
}

#[test]
fn asl_l_file_passes() {
    file_passes("ASL.l");
}

#[test]
fn asr_b_file_passes() {
    file_passes("ASR.b");
}

#[test]
fn asr_l_file_passes() {
    file_passes("ASR.l");
}

#[test]
fn lsl_b_file_passes() {
    file_passes("LSL.b");
}

#[test]
fn lsl_l_file_passes() {
    file_passes("LSL.l");
}

#[test]
fn lsr_b_file_passes() {
    file_passes("LSR.b");
}

#[test]
fn lsr_l_file_passes() {
    file_passes("LSR.l");
}

#[test]
fn rol_b_file_passes() {
    file_passes("ROL.b");
}

#[test]
fn rol_l_file_passes() {
    file_passes("ROL.l");
}

#[test]
fn ror_b_file_passes() {
    file_passes("ROR.b");
}

#[test]
fn ror_l_file_passes() {
    file_passes("ROR.l");
}

#[test]
fn roxl_b_file_passes() {
    file_passes("ROXL.b");
}

#[test]
fn roxl_l_file_passes() {
    file_passes("ROXL.l");
}

#[test]
fn roxr_b_file_passes() {
    file_passes("ROXR.b");
}

#[test]
fn roxr_l_file_passes() {
    file_passes("ROXR.l");
}
