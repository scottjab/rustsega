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

/// `LD I,A`, `LD R,A`, `LD A,I`, `LD A,R`.
#[test]
fn sst_misc_ir() {
    harness::run_files(&["ed 47", "ed 4f", "ed 57", "ed 5f"]);
}

/// `RRD` and `RLD`.
#[test]
fn sst_misc_rxd() {
    harness::run_files(&["ed 67", "ed 6f"]);
}

/// `HALT`, `DI`, `EI`, including the `DD`/`FD` forms of `HALT`.
#[test]
fn sst_misc_flow() {
    harness::run_files(&["76", "dd 76", "fd 76", "f3", "fb"]);
}

/// `IM 0`, `IM 1`, `IM 2` and their mirrors: `46`/`4E`/`66`/`6E`,
/// `56`/`76`, `5E`/`7E`.
#[test]
fn sst_misc_im() {
    harness::run_files(&[
        "ed 46", "ed 4e", "ed 66", "ed 6e", "ed 56", "ed 76", "ed 5e", "ed 7e",
    ]);
}

/// `NEG` and its seven mirrors: `ED 44` plus `4C`, `54`, `5C`, `64`, `6C`,
/// `74`, `7C`.
#[test]
fn sst_misc_neg() {
    harness::run_files(&[
        "ed 44", "ed 4c", "ed 54", "ed 5c", "ed 64", "ed 6c", "ed 74", "ed 7c",
    ]);
}
