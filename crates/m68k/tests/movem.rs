//! `MOVEM` in the pinned 680x0 suite: `MOVEM.w` and `MOVEM.l` (M1-24, #32).
//!
//! One test per suite file, in the style of `bcd.rs`, checked with
//! [`harness::Check::StateAndCyclesSkipAddressErrors`]. These two files are the heaviest
//! users of that skip in the whole suite: 3784 of `MOVEM.w`'s and 3842 of
//! `MOVEM.l`'s 8065 cases end in the suite's exception handler because their
//! operand address is odd, and a word or long access there is an address error
//! (M1-29, #37). The remaining 4281 and 4223 cases all run.
//!
//! # What the files cover
//!
//! Both files run every operand MOVEM can encode: registers to memory over
//! `(An)`, `-(An)`, `d16(An)`, `d8(An,Xn)`, `abs.W` and `abs.L`, and memory to
//! registers over `(An)`, `(An)+`, `d16(An)`, `d8(An,Xn)`, `abs.W` and `abs.L`.
//! Mode `000` is absent from both directions — it is `EXT` in the store half and
//! no operand at all in the load half — and mode `001` (`An`) is absent too.
//!
//! So the three quirks are all in the data, not just in the manual: the reversed
//! register-list order of a store through `-(An)` (921 cases name the mode's own
//! `An` in the list, which is what pins the "initial value is stored" rule), the
//! `(An)+` update that lands past the last read, and sign extension of every word
//! loaded back — into the data registers as well as the address registers.
//!
//! One thing the data cannot cover: `MOVEM` memory to registers issues one extra
//! word read after the last register (M68000UM MOVEM, and the ticket's quirk
//! list). A read changes no state, and the suite compares state only, so no case
//! can pin it; see [`crate::ops::movem`] for what the core does about it.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `MOVEM.w`: `$4890`/`$48B0` and `$4C90`/`$4CBB` — `0100 1d00 0s eeeeee` with
/// `d` picking the direction (M68000UM MOVEM).
#[test]
fn movem_w_file_passes() {
    harness::assert_file_passes("MOVEM.w", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// `MOVEM.l`: the same with bit 6 set, `$48D0`..`$48F9` and `$4CD0`..`$4CFB`.
#[test]
fn movem_l_file_passes() {
    harness::assert_file_passes("MOVEM.l", harness::Check::StateAndCyclesSkipAddressErrors);
}

/// The two file names above are what the suite ships, so a rename fails here
/// with a directory listing instead of a test that quietly does nothing.
#[test]
fn the_suite_ships_the_two_movem_files() {
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    for name in ["MOVEM.w", "MOVEM.l"] {
        assert!(
            names.iter().any(|n| n == name),
            "{name} should be in the suite; it lists {:?}",
            names
                .iter()
                .filter(|n| n.starts_with("MOVEM"))
                .collect::<Vec<_>>()
        );
    }
}
