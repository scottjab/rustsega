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
