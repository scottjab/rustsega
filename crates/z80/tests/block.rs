//! Hardware-captured conformance for the block transfer and search group
//! (M2-11, #54).
//!
//! These files are the reference for the whole of the undocumented behaviour of
//! the group: `S`/`Z`/`C` surviving a transfer, `P/V` following `BC - 1`, `X`/`Y`
//! coming from `byte + A` (or `A - (HL) - H`) rather than from any result, and
//! the repeat case where both of those bits come from the instruction's own
//! address instead. They also pin the two timings, 16 T-states for one pass and
//! 21 for a pass that repeats with `PC` back on the instruction.

mod harness;

#[test]
fn sst_block() {
    // LDI, CPI, LDD, CPD, then the four repeating forms. Every case of the four
    // `B_` files repeats - `BC` is 16 bits and a thousand random starts do not
    // often hit the last byte - except three `CPIR` cases that find a match.
    harness::run_files(&[
        "ed a0", "ed a1", "ed a8", "ed a9", "ed b0", "ed b1", "ed b8", "ed b9",
    ]);
}
