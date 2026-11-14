//! The user guide names every default keyboard binding (M8-08, #163).
//!
//! `docs/USER_GUIDE.md` is the only place a player learns that `A`/`S`/`D` are
//! `B`/`C`/`A` and that Mode is the right-hand Shift. A rebind changes
//! [`Bindings::default`] and nothing else, so this is the half that keeps the page
//! honest: it reads the default table through the same
//! [`web::settings::keycap_label`] the controls page puts on a keycap, and looks
//! for that keycap on the guide's row for the button it drives. A binding that
//! leaves the table, or a keycap whose label changes, fails here rather than
//! teaching somebody the wrong key.
//!
//! Native test, no browser, like the other scans in this directory
//! (`style_tokens.rs`, `icon_button_labels.rs`): `cargo nextest run --workspace`,
//! which is `just test` and so part of `just check`, runs it on the host. Reading
//! a file is the only thing it does. Under `just test-web` it registers no test on
//! wasm32, exactly as the other native tests of this directory do.

use std::path::Path;

use web::input::Bindings;
use web::settings::keycap_label;

/// `docs/USER_GUIDE.md`, the page that has to name the keys.
fn user_guide() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("USER_GUIDE.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// The default keyboard bindings that no line of `guide` names, each as
/// `"<button> (<keycap>)"` for the failure message.
///
/// A binding is documented when one of the guide's table rows carries both the
/// button's own [`web::input::PadButton::label`] and its [`keycap_label`]. Pairing
/// them on one row is the point: a page that mentions `Enter` in a sentence and
/// `Start` in another has not told anybody that Enter is Start. Prose rows do not
/// count, so "press A to jump" cannot document the `A` button.
fn undocumented_bindings(guide: &str) -> Vec<String> {
    let lines: Vec<&str> = guide
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .collect();
    Bindings::default()
        .keyboard
        .iter()
        .filter_map(|(code, _, button)| {
            let keycap = keycap_label(code);
            let label = (*button).label();
            let documented = lines
                .iter()
                .any(|line| line.contains(label) && line.contains(keycap.as_str()));
            (!documented).then(|| format!("{label} ({keycap})"))
        })
        .collect()
}

#[test]
fn every_default_keyboard_binding_is_in_the_user_guide() {
    let missing = undocumented_bindings(&user_guide());
    assert!(
        missing.is_empty(),
        "docs/USER_GUIDE.md does not name these default keyboard bindings, one per line of its controls table: {}",
        missing.join(", ")
    );
}

/// The pairing is really asked for: a page that names no keys at all fails, so a
/// broken scan cannot look like a clean one.
#[test]
fn the_guide_scan_finds_a_missing_binding() {
    let missing = undocumented_bindings("# user guide\n\nMove with the arrows.\n");
    assert!(
        !missing.is_empty(),
        "a guide that documents no keycap must not pass the scan: {missing:?}"
    );
    assert_eq!(missing.len(), Bindings::default().keyboard.len());
}
