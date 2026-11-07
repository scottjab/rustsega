//! The logic class in the pinned 680x0 suite: `AND.b/w/l`, `OR.b/w/l`,
//! `EOR.b/w/l` and `NOT.b/w/l` (M1-13, #21).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateSkipAddressErrors`], the same one the add, subtract
//! and compare classes use: the cases that end in the suite's exception handler
//! are left to M1-29 (#37) — see [`harness::is_address_error_case`] and #185.
//!
//! The suite ships no `ANDI.*`, `ORI.*` or `EORI.*` file for the `#imm,<ea>`
//! forms; only `ANDItoCCR`, `ANDItoSR`, `ORItoCCR`, `ORItoSR`, `EORItoCCR` and
//! `EORItoSR` carry those mnemonics, and those six are the to-CCR/to-SR forms
//! M1-26 (#34) owns. The immediate forms themselves are covered here: they live
//! in the file of the operation they perform, and their opcodes are the
//! `$02xx`/`$00xx`/`$0Axx` slice of that file. Measured over the pinned suite,
//! by opcode prefix:
//!
//! | File | `$02xx` ANDI | `$00xx` ORI | `$0Axx` EORI |
//! |---|---|---|---|
//! | `AND.b` / `AND.w` / `AND.l` | 520 / 475 / 486 | — | — |
//! | `OR.b` / `OR.w` / `OR.l` | — | 454 / 505 / 502 | — |
//! | `EOR.b` / `EOR.w` / `EOR.l` | — | — | 911 / 908 / 914 |
//!
//! So 1481 `ANDI`, 1461 `ORI` and 2733 `EORI` cases run under these twelve
//! tests. `OR.b/w/l` is also where `ORI`'s overlap with `BTST` is pinned: the
//! suite's `$00xx` cases all carry `ddd` = `000` in bits 11..9, because
//! `BTST D0,<ea>` has no encoding (`decode`'s `classify_logic_imm` relies on it,
//! and `addi_decodes_and_validates_its_destination` pins `$0100`/`$0500`/`$0800`
//! — the bit operations of the same group — as still unimplemented).
//!
//! `EOR.b/w/l` carry the other half of the story: the `$B1xx`-style CMPM cases
//! that #20 implemented live in `CMP.b/w/l`, and no EOR case uses EA mode `001`,
//! which is what keeps the two sets disjoint (`decode`'s
//! `cmpm_decodes_before_eor` test pins it from the other side).

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `AND.B`: `$C000`-`$C03F`/`$C100`-`$C13F`-style `1100 ddd 000|100 eeeeee`,
/// plus the `$0200`-`$023F` `ANDI.B` cases.
#[test]
fn and_b_file_passes() {
    harness::assert_file_passes("AND.b", harness::Check::StateSkipAddressErrors);
}

/// `AND.W`: operation `001` (`AND <ea>,Dn`) and `101` (`AND Dn,<ea>`), plus
/// `$0240`-`$027F` `ANDI.W`.
#[test]
fn and_w_file_passes() {
    harness::assert_file_passes("AND.w", harness::Check::StateSkipAddressErrors);
}

/// `AND.L`: operation `010` and `110`, plus `$0280`-`$02BF` `ANDI.L`.
#[test]
fn and_l_file_passes() {
    harness::assert_file_passes("AND.l", harness::Check::StateSkipAddressErrors);
}

/// `OR.B`: `$8000`-`$803F`/`$8100`-`$813F`-style, plus the `$0000`-`$003F`
/// `ORI.B` cases.
#[test]
fn or_b_file_passes() {
    harness::assert_file_passes("OR.b", harness::Check::StateSkipAddressErrors);
}

/// `OR.W`: operation `001` and `101`, plus `$0040`-`$007F` `ORI.W`.
#[test]
fn or_w_file_passes() {
    harness::assert_file_passes("OR.w", harness::Check::StateSkipAddressErrors);
}

/// `OR.L`: operation `010` and `110`, plus `$0080`-`$00BF` `ORI.L`.
#[test]
fn or_l_file_passes() {
    harness::assert_file_passes("OR.l", harness::Check::StateSkipAddressErrors);
}

/// `EOR.B`: `$B100`-`$B13F`-style `1011 ddd 100 eeeeee` with every mode but
/// `001`, plus the `$0A00`-`$0A3F` `EORI.B` cases.
#[test]
fn eor_b_file_passes() {
    harness::assert_file_passes("EOR.b", harness::Check::StateSkipAddressErrors);
}

/// `EOR.W`: operation `101`, plus `$0A40`-`$0A7F` `EORI.W`.
#[test]
fn eor_w_file_passes() {
    harness::assert_file_passes("EOR.w", harness::Check::StateSkipAddressErrors);
}

/// `EOR.L`: operation `110`, plus `$0A80`-`$0ABF` `EORI.L`.
#[test]
fn eor_l_file_passes() {
    harness::assert_file_passes("EOR.l", harness::Check::StateSkipAddressErrors);
}

/// `NOT.B`: `$4600`-`$463F`. The three `NOT` files hold no immediate case at
/// all, and `$46C0`..`$46FF` — `ss` = `11` — is MOVE to SR (#34), so it appears
/// nowhere here either.
#[test]
fn not_b_file_passes() {
    harness::assert_file_passes("NOT.b", harness::Check::StateSkipAddressErrors);
}

/// `NOT.W`: `$4640`-`$467F`.
#[test]
fn not_w_file_passes() {
    harness::assert_file_passes("NOT.w", harness::Check::StateSkipAddressErrors);
}

/// `NOT.L`: `$4680`-`$46BF`.
#[test]
fn not_l_file_passes() {
    harness::assert_file_passes("NOT.l", harness::Check::StateSkipAddressErrors);
}

/// The twelve file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_all_twelve_logic_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in [
        "AND.b", "AND.w", "AND.l", "OR.b", "OR.w", "OR.l", "EOR.b", "EOR.w", "EOR.l", "NOT.b",
        "NOT.w", "NOT.l",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("AND") || n.starts_with("OR") || n.starts_with("EOR"))
                .collect::<Vec<_>>()
        );
    }
}
