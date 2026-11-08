//! The control-transfer files of the pinned 680x0 suite (M1-17, #25): `JMP`,
//! `JSR`, `RTS`, `RTR`, `RTE` and `NOP`.
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateSkipAddressErrors`], as in `BRANCHES`, `LEA`/`PEA` and
//! `NEG`/`CLR`/`EXG`: the cases that end in the suite's own exception handler are
//! left to M1-29 (#37) — see [`harness::is_address_error_case`] and #185. For
//! these six files that is a large share of the file, and it is the instruction,
//! not the harness, that causes it: `JMP`/`JSR` jump to whatever the operand
//! computes and `RTS`/`RTR`/`RTE` jump to whatever the stack holds, so most of
//! the suite's random operands point at an odd address and the return faults
//! before it can report state. Of the 8065 cases in each file, the ones actually
//! compared here are 4259 (`JMP`), 4183 (`JSR`), 4008 (`RTS`), 4038 (`RTR`),
//! 4011 (`RTE`) and all 8065 (`NOP`, which has no operand to go wrong).
//!
//! `JMP` and `JSR` are the two instructions whose operand is a *control* address
//! (`(An)`, `d16(An)`, `d8(An,Xn)`, `abs.W`, `abs.L`, `d16(PC)`, `d8(PC,Xn)`),
//! which is why `decode` rejects `JSR (A0)+` even though `MOVE (A0)+,D0` is fine,
//! and why they go through `ea_address` rather than `resolve_ea`: the target is
//! the address the field computes, unmasked, not what is stored at it (#193).

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `JMP`: `$4EC0 | <ea>` at a control address, `PC` = that address, no flags and
/// nothing pushed.
#[test]
fn jmp_file_passes() {
    harness::assert_file_passes("JMP", harness::Check::StateSkipAddressErrors);
}

/// `JSR`: `$4E80 | <ea>`, the same operand as `JMP` plus a long push of the
/// address of the following instruction.
#[test]
fn jsr_file_passes() {
    harness::assert_file_passes("JSR", harness::Check::StateSkipAddressErrors);
}

/// `RTS` `$4E75`: pop a long into `PC`.
#[test]
fn rts_file_passes() {
    harness::assert_file_passes("RTS", harness::Check::StateSkipAddressErrors);
}

/// `RTR` `$4E77`: pop a word into the CCR, then a long into `PC`.
#[test]
fn rtr_file_passes() {
    harness::assert_file_passes("RTR", harness::Check::StateSkipAddressErrors);
}

/// `RTE` `$4E73`: pop a word into the whole of `SR` — including `S`, which moves
/// the active stack pointer — then a long into `PC`.
#[test]
fn rte_file_passes() {
    harness::assert_file_passes("RTE", harness::Check::StateSkipAddressErrors);
}

/// `NOP` `$4E71`: advance the PC and nothing else. Every case in the file is
/// compared, because none of them can fault.
#[test]
fn nop_file_passes() {
    harness::assert_file_passes("NOP", harness::Check::StateSkipAddressErrors);
}

/// The six file names above are what the suite ships for this ticket, so a rename
/// fails here with a directory listing instead of six tests that quietly do
/// nothing. `NOP` in particular is a real file, not a fixture of the others: it
/// is the only one of the six whose operand list is empty.
#[test]
fn the_suite_ships_all_six_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["JMP", "JSR", "RTS", "RTR", "RTE", "NOP"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; the file names that start with the \
             same letter are {:?}",
            names
                .iter()
                .filter(|n| n.starts_with(&name[..1]))
                .collect::<Vec<_>>()
        );
    }
}
