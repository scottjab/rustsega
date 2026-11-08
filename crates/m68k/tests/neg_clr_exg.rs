//! The `NEG`/`NEGX`/`CLR`/`EXT`/`SWAP`/`EXG` files of the pinned 680x0 suite
//! (M1-14, #22).
//!
//! One test per suite file, so a regression names the instruction that broke
//! rather than the group. The check is
//! [`harness::Check::StateSkipAddressErrors`], the same one the add, subtract
//! and compare classes use: the cases that end in the suite's exception handler
//! are left to M1-29 (#37) — see [`harness::is_address_error_case`] and #185.
//!
//! The nine `NEG`/`NEGX`/`CLR` files are each 8065 cases over the eight
//! data-alterable operand forms — `Dn`, `(An)`, `(An)+`, `-(An)`, `d16(An)`,
//! `d8(An,Xn)`, `abs.W`, `abs.L` — and never an `An`, PC-relative or immediate
//! operand, which is what `decode`'s `is_data_alterable` check for the
//! `$4000`/`$4200`/`$4400` groups has to agree with.
//!
//! `EXT.w`, `EXT.l` and `SWAP` are Dn-only: 8065 cases each over `$4880`+`r`,
//! `$48C0`+`r` and `$4840`+`r`. Those three base opcodes share the `$48??`
//! window with `PEA` (`$4840`-`$487F` at any other EA mode, M1-15) and `MOVEM`
//! (`$4880`-`$48FF` at any other EA mode, M1-24); `decode` gives those to their
//! own tickets, and `decode::tests::swap_and_ext_decode_and_leave_pea_and_movem_alone`
//! pins the split from the `PEA` and `MOVEM` opcodes this suite ships.
//!
//! `EXG` is the slot #21 reserved inside AND's group, and its cases reach `A7`:
//! `c18f [EXG D0, A7] 1` swaps D0 with *SSP* and reports the new SSP in the
//! final state, so a core that wrote `a[7]` without moving the stored stack
//! pointer fails this file.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `NEG.B`: `$4400`-`$44BF`, `0 - <ea>` → `<ea>`.
#[test]
fn neg_b_file_passes() {
    harness::assert_file_passes("NEG.b", harness::Check::StateSkipAddressErrors);
}

/// `NEG.W`: the same group at `ss` = `01`.
#[test]
fn neg_w_file_passes() {
    harness::assert_file_passes("NEG.w", harness::Check::StateSkipAddressErrors);
}

/// `NEG.L`: the same group at `ss` = `10`. `ss` = `11` is nothing at all in
/// `$44xx`, which is why the group stops dead at `$44BF`.
#[test]
fn neg_l_file_passes() {
    harness::assert_file_passes("NEG.l", harness::Check::StateSkipAddressErrors);
}

/// `NEGX.B`: `$4000`-`$40BF`, `0 - <ea> - X` → `<ea>` with `SUBX`'s sticky `Z`.
#[test]
fn negx_b_file_passes() {
    harness::assert_file_passes("NEGX.b", harness::Check::StateSkipAddressErrors);
}

/// `NEGX.W`.
#[test]
fn negx_w_file_passes() {
    harness::assert_file_passes("NEGX.w", harness::Check::StateSkipAddressErrors);
}

/// `NEGX.L`. The next sixth, `$40C0`-`$40FF`, is MOVE from SR (M1-26, #34) and
/// stays unimplemented.
#[test]
fn negx_l_file_passes() {
    harness::assert_file_passes("NEGX.l", harness::Check::StateSkipAddressErrors);
}

/// `CLR.B`: `$4200`-`$42BF`. A memory destination is read before it is written
/// (M68000PM "CLR"), which this check cannot see — it compares final state — so
/// the read itself is pinned by `ops::arith::tests::clr_to_memory_reads_then_writes`.
#[test]
fn clr_b_file_passes() {
    harness::assert_file_passes("CLR.b", harness::Check::StateSkipAddressErrors);
}

/// `CLR.W`.
#[test]
fn clr_w_file_passes() {
    harness::assert_file_passes("CLR.w", harness::Check::StateSkipAddressErrors);
}

/// `CLR.L`.
#[test]
fn clr_l_file_passes() {
    harness::assert_file_passes("CLR.l", harness::Check::StateSkipAddressErrors);
}

/// `EXT.W`: `$4880`+`r`, the low byte of `Dn` sign extended to a word.
#[test]
fn ext_w_file_passes() {
    harness::assert_file_passes("EXT.w", harness::Check::StateSkipAddressErrors);
}

/// `EXT.L`: `$48C0`+`r`, the low word of `Dn` sign extended to a long.
#[test]
fn ext_l_file_passes() {
    harness::assert_file_passes("EXT.l", harness::Check::StateSkipAddressErrors);
}

/// `SWAP`: `$4840`+`r`, the halves of `Dn` exchanged.
#[test]
fn swap_file_passes() {
    harness::assert_file_passes("SWAP", harness::Check::StateSkipAddressErrors);
}

/// `EXG`: opmode `01000` (`Dx,Dy`), `01001` (`Ax,Ay`) and `10001` (`Dx,Ay`),
/// none of which sets a flag.
#[test]
fn exg_file_passes() {
    harness::assert_file_passes("EXG", harness::Check::StateSkipAddressErrors);
}

/// The thirteen file names above are what the suite ships for this ticket, so a
/// rename fails here with a directory listing instead of a test that quietly
/// does nothing.
#[test]
fn the_suite_ships_all_thirteen_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in [
        "NEG.b", "NEG.w", "NEG.l", "NEGX.b", "NEGX.w", "NEGX.l", "CLR.b", "CLR.w", "CLR.l",
        "EXT.w", "EXT.l", "SWAP", "EXG",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; its NEG/NEGX/CLR/EXT/SWAP/EXG entries are {:?}",
            names
                .iter()
                .filter(|n| {
                    let head = n.split('.').next().unwrap_or(n);
                    matches!(head, "NEG" | "NEGX" | "CLR" | "EXT" | "SWAP" | "EXG")
                })
                .collect::<Vec<_>>()
        );
    }
}
