//! The library's row height is one number written in two files (M8-04, #158).
//!
//! `style.css` gives a row its height with `--library-row`; `web::library` places the
//! virtualized rows at multiples of `ROW_HEIGHT_PX` and decides how many of them a
//! viewport holds. Only the first is what a player sees, and only the second is what
//! gets scrolled, so a change to one without the other is a list whose rows overlap by
//! a few pixels or - worse, at a distance - leave gaps of blank background where the
//! rows should be.
//!
//! A test rather than a comment, because the two files are in two languages and the
//! one thing that keeps numbers like this in step in this repository is a check that
//! reads both.

mod support;

use support::style_css;

#[test]
fn the_row_the_stylesheet_draws_is_the_row_the_list_positions_by() {
    let css = style_css();
    let want = format!("--library-row: {}px", web::library::ROW_HEIGHT_PX);
    assert!(
        css.contains(&want),
        "style.css does not set {want}: the library list positions its rows by \
         web::library::ROW_HEIGHT_PX = {} and must draw them at the same height",
        web::library::ROW_HEIGHT_PX
    );
}

/// The library view reaches an entry only by `web::library::entry_at`.
///
/// A virtualized row number is a row of the *filtered* list, and turning one into an
/// entry is the one step in the view that has an easy wrong answer: index the
/// collection instead and the list draws the first games in the directory whatever
/// was searched for, and launching a row starts a different game from the one the row
/// names. `entry_at` is where that step lives and what its own tests cover, so the
/// rule for the view is that it never indexes the listing by hand.
#[test]
fn the_view_reaches_an_entry_only_by_entry_at() {
    let source = include_str!("../src/ui/library_view.rs");
    let uses_entry_at = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| line.contains("entry_at("))
        .count();
    assert!(
        uses_entry_at >= 2,
        "the library view draws its rows and its Enter-to-launch through entry_at; \
         found {uses_entry_at} use(s)"
    );
    let by_hand = source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| line.contains("all.get(") || line.contains("all["))
        .map(|(line, text)| format!("{}: {}", line + 1, text.trim()))
        .collect::<Vec<_>>();
    assert!(
        by_hand.is_empty(),
        "library_view.rs indexes the listing itself, which is the row/filter confusion \
         waiting to happen:\n{}",
        by_hand.join("\n")
    );
}
