//! Hardware-captured conformance for the misc group (M2-13, #56).
//!
//! These files are the reference for the group's undocumented behaviour: `DAA`'s
//! half-carry rule and `P/V` as the parity of the adjusted result, the
//! `((q ^ F) | A) & 0x28` rule behind `SCF`/`CCF`, `X`/`Y` following the value
//! in `LD A,I`/`LD A,R`, `wz = HL + 1` for `RRD`/`RLD`, and `P/V` mirroring
//! `IFF2` rather than anything about the byte that was loaded.

mod harness;

/// `DAA`, `CPL`, `SCF`, `CCF`.
#[test]
fn sst_misc_adjust() {
    harness::run_files(&["27", "2f", "37", "3f"]);
}

/// `NEG` and its seven mirrors: `ED 44` plus `4C`, `54`, `5C`, `64`, `6C`,
/// `74`, `7C`.
#[test]
fn sst_misc_neg() {
    harness::run_files(&[
        "ed 44", "ed 4c", "ed 54", "ed 5c", "ed 64", "ed 6c", "ed 74", "ed 7c",
    ]);
}
