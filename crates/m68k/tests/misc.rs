//! The four `0100`-group instructions that have no file of their own to join:
//! `TRAPV`, `TAS` and `RESET` from the pinned 680x0 suite (M1-27, #35).
//!
//! One test per suite file, in the style of `status.rs`, checked with
//! [`harness::Check::StateSkipAddressErrors`]. They live together because none of
//! them belongs to a family another test file already covers — `TAS` is the only
//! member of the `TST` group that stores, and `TRAPV` and `RESET` are single
//! opcodes in the `$4E` system block that `jmp_jsr_returns.rs` has no room for.
//!
//! # What the files cover
//!
//! `TAS` (`$4AC0` + `<ea>`, `0100 1010 11 mmm rrr`) is `TST.B` plus "set bit 7":
//! it reads a byte, sets `N` and `Z` from the *original* value, and writes back
//! that value with bit 7 forced to 1 (M68000UM "TAS — Test and Set"). It is byte
//! only, its operand must be data-alterable, and the group leaves the `$4AFC`
//! immediate slot illegal, so the file pins both the read-modify-write and the
//! decoder's `Op::Illegal` edge. It is also the only instruction here that
//! changes memory, so `-(An)` and `(A0)+` cases check the address moves exactly
//! once.
//!
//! `TRAPV` (`$4E76`) is a conditional software trap: it pushes the address of the
//! word *after* itself and vectors to slot `$10` — vector 4, the `TRAPV` vector —
//! only when `V` is set, and otherwise does nothing at all (M68000UM "TRAPV — Trap
//! on Overflow"). Most of its cases are therefore the `V = 0` no-op, which is what
//! makes the file worth having: a `TRAPV` that always traps looks the same on the
//! half of the runs that trap.
//!
//! `RESET` (`$4E70`) peripherals-reset: on a real 68000 it signals the bus and the
//! MC68881, and takes 330 cycles before the next instruction fetch (M68000UM
//! "RESET — Reset Peripherals", "RESET Instruction Timing"). It touches no
//! architectural state, so the suite can only check that execution continues
//! one word further along, which is exactly the property the file pins. The cycle
//! count is the other thing it would pin if the harness compared cycles; it does
//! not, so the latency stays documented in the handler rather than asserted.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `TAS.B <ea>`: `$4AC0` + `<ea>`, `0100 1010 11 mmm rrr` (M68000UM "TAS — Test
/// and Set").
#[test]
fn tas_file_passes() {
    harness::assert_file_passes("TAS", harness::Check::StateSkipAddressErrors);
}

/// `TRAPV`: `$4E76`, `0100 1110 0111 0110` — trap on overflow, vector 4, and a
/// no-op when `V` is clear (M68000UM "TRAPV — Trap on Overflow").
#[test]
fn trapv_file_passes() {
    harness::assert_file_passes("TRAPV", harness::Check::StateSkipAddressErrors);
}

/// `RESET`: `$4E70`, `0100 1110 0111 0000` — reset peripherals, no architectural
/// effect (M68000UM "RESET — Reset Peripherals").
#[test]
fn reset_file_passes() {
    harness::assert_file_passes("RESET", harness::Check::StateSkipAddressErrors);
}

/// The three file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of three tests that quietly do nothing.
///
/// `STOP` has no file: `STOP.json.gz` is not in the suite, which is worth stating
/// out loud because #35 implements `STOP` and the reader may otherwise look for
/// one. It is a privileged instruction that stops the clock and restores SR from
/// the next word, so it needs a harness that can model a halted CPU; the unit test
/// in `ops::control` covers it until the suite grows the file.
#[test]
fn the_suite_ships_the_misc_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["TAS", "TRAPV", "RESET"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with('T') || n.starts_with("RES"))
                .collect::<Vec<_>>()
        );
    }
    assert!(
        !names.iter().any(|n| n == "STOP"),
        "STOP is not in this suite, and this test is where that fact lives"
    );
}
