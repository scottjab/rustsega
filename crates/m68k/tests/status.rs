//! The status-register transfers and the immediate masks in the pinned 680x0
//! suite: `MOVEfromSR`, `MOVEtoSR`, `MOVEtoCCR`, `MOVEfromUSP`, `MOVEtoUSP`, and
//! the six `ANDI`/`ORI`/`EORI` to CCR/SR files (M1-26, #34).
//!
//! One test per suite file, in the style of `movem.rs`, checked with
//! [`harness::Check::StateAndCyclesSkipAddressErrors`] (clocks since #41). The
//! skip matters more here than in
//! most files: `MOVEfromSR`, `MOVEtoSR` and `MOVEtoCCR` run their word operand
//! through every addressing mode, so a couple of thousand cases per file end in
//! the suite's exception handler on an odd address (M1-29, #37). The USP and
//! `#imm` files have almost no address errors at all — the USP transfers are
//! register-only and the immediate masks write a register, not memory.
//!
//! # What the files cover
//!
//! `MOVEfromSR` (`$40C0`..`$40F9`) writes SR to `Dn` and to every memory mode;
//! `MOVEtoSR` (`$46C0`..`$46FC`) and `MOVEtoCCR` (`$44C0`..`$44FC`) take the
//! mirrored read set — `Dn`, the indirect modes, the two absolutes, the two
//! PC-relative modes and `#imm`, but never `An` direct (M68000UM "MOVE from SR",
//! "MOVE to SR", "MOVE to CCR"). Those three are where the S and T bits and the
//! interrupt mask move, so they pin the privilege check (a `MOVE <ea>,SR` in user
//! mode is a privilege-violation exception, vector 8) and the fact that the
//! program may not silently lose the mask it just wrote.
//!
//! `MOVEfromUSP` (`$4E60` + n) and `MOVEtoUSP` (`$4E68` + n) are the two halves of
//! `MOVE USP,An`/`MOVE An,USP` (M68000UM "MOVE to/from USP"). They are supervisor
//! instructions and they are the only way the user stack pointer is reachable at
//! all, which is what makes the S bit's effect on which `A7` is `A7` testable: a
//! file can set SR, read the other stack pointer out, and check it was stored
//! unmoved.
//!
//! The six immediate files (`ANDItoCCR`, `ANDItoSR`, `ORItoCCR`, `ORItoSR`,
//! `EORItoCCR`, `EORItoSR`) are `$003C`/`$007C`, `$023C`/`$027C` and
//! `$0A3C`/`$0A7C` — the mode `111` register `100` rows of `ANDI`, `ORI` and
//! `EORI`, byte-wide into the CCR and word-wide into SR (M68000UM "ANDI/ORI/EORI
//! to CCR", "… to SR"). The CCR forms may only touch the low byte, so the two SR
//! files are what prove the upper byte of the CCR write stays zero and the two
//! CCR files what prove a word `ANDI`/`ORI`/`EORI` cannot drag the S bit around by
//! accident.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `MOVEfromSR`: `$40C0` + `<ea>`, `0100 0000 11 mmm rrr` (M68000UM "MOVE from
/// SR").
#[test]
fn move_from_sr_file_passes() {
    harness::assert_file_passes(
        "MOVEfromSR",
        harness::Check::StateAndCyclesSkipAddressErrors,
    );
}

/// `MOVEtoSR`: `$46C0` + `<ea>`, `0100 0110 11 mmm rrr`, including `$46FC`
/// (`MOVE #imm,SR`) (M68000UM "MOVE to SR").
#[test]
fn move_to_sr_file_passes() {
    harness::assert_file_passes("MOVEtoSR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `MOVEtoCCR`: `$44C0` + `<ea>`, `0100 0100 11 mmm rrr` — the 68000 spelling,
/// not the 68020 byte form (M68000UM "MOVE to CCR").
#[test]
fn move_to_ccr_file_passes() {
    harness::assert_file_passes("MOVEtoCCR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `MOVEfromUSP`: `$4E60` + n, `0100 1110 011 nnn nnn` with `d` = 1 —
/// `MOVE USP,An` (M68000UM "MOVE to/from USP").
#[test]
fn move_from_usp_file_passes() {
    harness::assert_file_passes(
        "MOVEfromUSP",
        harness::Check::StateAndCyclesSkipAddressErrors,
    );
}

/// `MOVEtoUSP`: `$4E68` + n — `MOVE An,USP`, the other half of the same
/// 16-opcode block (M68000UM "MOVE to/from USP").
#[test]
fn move_to_usp_file_passes() {
    harness::assert_file_passes("MOVEtoUSP", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ANDI.B #imm,CCR` (`$023C`): the byte AND that may only clear flag bits
/// (M68000UM "ANDI/ORI/EORI to CCR").
#[test]
fn andi_to_ccr_file_passes() {
    harness::assert_file_passes("ANDItoCCR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ANDI.W #imm,SR` (`$027C`): the word AND over the whole status register,
/// mask and S bit included (M68000UM "ANDI/ORI/EORI to SR").
#[test]
fn andi_to_sr_file_passes() {
    harness::assert_file_passes("ANDItoSR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ORI.B #imm,CCR` (`$003C`) (M68000UM "ANDI/ORI/EORI to CCR").
#[test]
fn ori_to_ccr_file_passes() {
    harness::assert_file_passes("ORItoCCR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `ORI.W #imm,SR` (`$007C`) (M68000UM "ANDI/ORI/EORI to SR").
#[test]
fn ori_to_sr_file_passes() {
    harness::assert_file_passes("ORItoSR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `EORI.B #imm,CCR` (`$0A3C`) (M68000UM "ANDI/ORI/EORI to CCR").
#[test]
fn eori_to_ccr_file_passes() {
    harness::assert_file_passes("EORItoCCR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `EORI.W #imm,SR` (`$0A7C`) (M68000UM "ANDI/ORI/EORI to SR").
#[test]
fn eori_to_sr_file_passes() {
    harness::assert_file_passes("EORItoSR", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// The eleven file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of eleven tests that quietly do nothing.
#[test]
fn the_suite_ships_the_status_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in [
        "MOVEfromSR",
        "MOVEtoSR",
        "MOVEtoCCR",
        "MOVEfromUSP",
        "MOVEtoUSP",
        "ANDItoCCR",
        "ANDItoSR",
        "ORItoCCR",
        "ORItoSR",
        "EORItoCCR",
        "EORItoSR",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.contains("MOVE") || n.ends_with("CCR") || n.ends_with("SR"))
                .collect::<Vec<_>>()
        );
    }
}
