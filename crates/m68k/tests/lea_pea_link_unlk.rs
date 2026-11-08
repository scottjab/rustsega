//! The `LEA`/`PEA`/`LINK`/`UNLINK` files of the pinned 680x0 suite (M1-15, #23).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateSkipAddressErrors`], the same one `NEG`/`CLR`/`EXG`
//! use: the cases that end in the suite's exception handler are left to M1-29
//! (#37) — see [`harness::is_address_error_case`] and #185.
//!
//! `LEA` and `PEA` are each 8065 cases over the seven control addressing modes
//! — `(An)`, `d16(An)`, `d8(An,Xn)`, `abs.W`, `abs.L`, `d16(PC)`, `d8(PC,Xn)` —
//! never `(An)+`, `-(An)`, `Dn`, `An` or `#imm`, which is what `decode`'s
//! `is_control` check for `$4xC0`-style `LEA` opcodes and for PEA's half of the
//! `$48??` window has to agree with. Both files keep the *whole* 32-bit
//! computed address: 7162 of the `LEA` cases and 7153 of the `PEA` cases end
//! with an address above the 24-bit bus mask, so an implementation that
//! resolves the operand through [`harness`]'s bus-masked memory operand
//! (`crate::ea::Ea::Mem`) fails 88% of both files — #193 is the report, and
//! `ops::address::tests::lea_d16_an_keeps_the_whole_32_bit_sum` the pin.
//!
//! `LINK` is `$4E50`+`r` and `UNLINK` — `UNLK`'s other mnemonic, which is why
//! the suite file is named that — is `$4E58`+`r`. Both cover `A7`:
//! `4e57 [LINK A7, #] 2` pushes the *decremented* stack pointer rather than the
//! old `A7` (the write cycle reads a register file that has already moved), and
//! `4e5f [UNLINK A7] 5` leaves `A7` holding the long it popped rather than that
//! long plus four.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `LEA <ea>,An`: `$41C0`..`$4FFF` with bits 8..6 all set, no flag affected.
#[test]
fn lea_file_passes() {
    harness::assert_file_passes("LEA", harness::Check::StateSkipAddressErrors);
}

/// `PEA <ea>`: PEA's half of the `$48??` window (`$4840`+EA at any mode but
/// `000`, which is `SWAP`), pushing the computed address rather than its contents.
#[test]
fn pea_file_passes() {
    harness::assert_file_passes("PEA", harness::Check::StateSkipAddressErrors);
}

/// `LINK An,#d16`: `$4E50`+`r`, the frame-pointer prologue.
#[test]
fn link_file_passes() {
    harness::assert_file_passes("LINK", harness::Check::StateSkipAddressErrors);
}

/// `UNLINK An` — the suite's `UNLINK.json.gz`, which is `$4E58`+`r`: the frame
/// teardown that reverses `LINK`.
#[test]
fn unlk_file_passes() {
    harness::assert_file_passes("UNLINK", harness::Check::StateSkipAddressErrors);
}

/// The four file names above are what the suite ships for this ticket, so a
/// rename fails here with a directory listing instead of a test that quietly
/// does nothing. `UNLK` in particular ships as `UNLINK`.
#[test]
fn the_suite_ships_all_four_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["LEA", "PEA", "LINK", "UNLINK"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; its LEA/PEA/LINK/UNLINK entries are {:?}",
            names
                .iter()
                .filter(|n| {
                    let head = n.split('.').next().unwrap_or(n);
                    matches!(head, "LEA" | "PEA" | "LINK" | "UNLINK")
                })
                .collect::<Vec<_>>()
        );
    }
}
