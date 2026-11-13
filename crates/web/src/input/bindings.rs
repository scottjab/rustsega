//! Rebindable bindings, as data (M7-06, #139).
//!
//! The shape of a binding table, its defaults, and its `serde` form - nothing
//! else. Editing these is the settings UI of M7-10 (#143), and the code that
//! reads them is the event listeners of M7-07 (#140) and M7-08 (#141): a
//! keyboard listener looks a `KeyboardEvent.code` up in [`Bindings::keyboard`],
//! a gamepad listener looks a standard-mapping index up in
//! [`Bindings::gamepad`], and each hands [`super::InputState`] the result.
//!
//! `serde` because docs/ARCHITECTURE.md says all bindings are rebindable and
//! stored in `localStorage`; the JSON this writes is what that storage holds.

use serde::Deserialize;
use serde::Serialize;

use super::button::PadButton;

/// What presses what, for one player's setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    /// Keyboard bindings as `(KeyboardEvent.code, port, button)`.
    ///
    /// `code`, not `key`: it names the physical key and so survives a different
    /// keyboard layout, which is what a movement map needs.
    pub keyboard: Vec<(String, usize, PadButton)>,
    /// Gamepad bindings as `(standard button index, button)`, the W3C standard
    /// mapping of a "gamepad"-typed device ("Gamepad API", "button and axis
    /// mapping").
    ///
    /// There is no port here because the pad's own index is the port: the
    /// gamepad listener of M7-08 (#141) reports pad `n` as
    /// [`super::Source::Gamepad(n)`] driving port `n`, so one entry per Genesis
    /// button is enough for every pad in the house.
    pub gamepad: Vec<(u8, PadButton)>,
}

impl Default for Bindings {
    /// The pad-one default layout: arrows and `KeyA`/`KeyS`/`KeyD` for the
    /// three buttons of the original pad, the row above them for the three the
    /// 6-button pad adds, `Enter` for Start and the right-hand `Shift` for Mode;
    /// and a standard gamepad laid out as a Genesis pad sits in your hands.
    ///
    /// Port 1 - the second controller - is deliberately unbound: the common case
    /// is one player, and binding a second player's keys is a settings choice
    /// rather than a default.
    fn default() -> Self {
        Self {
            keyboard: DEFAULT_KEYBOARD
                .iter()
                .map(|(code, port, button)| ((*code).to_string(), *port, *button))
                .collect(),
            gamepad: DEFAULT_GAMEPAD.to_vec(),
        }
    }
}

/// The default keyboard bindings, in the order the ticket lists them: d-pad,
/// then the `A`/`B`/`C` row, then the `X`/`Y`/`Z` row above it, then Start and
/// Mode. Player 2 unbound.
const DEFAULT_KEYBOARD: [(&str, usize, PadButton); 12] = [
    ("ArrowUp", 0, PadButton::Up),
    ("ArrowDown", 0, PadButton::Down),
    ("ArrowLeft", 0, PadButton::Left),
    ("ArrowRight", 0, PadButton::Right),
    ("KeyA", 0, PadButton::A),
    ("KeyS", 0, PadButton::B),
    ("KeyD", 0, PadButton::C),
    ("KeyQ", 0, PadButton::X),
    ("KeyW", 0, PadButton::Y),
    ("KeyE", 0, PadButton::Z),
    ("Enter", 0, PadButton::Start),
    ("ShiftRight", 0, PadButton::Mode),
];

/// The default gamepad bindings: the cross and the right-hand stick of a modern
/// pad do nothing, because on a Genesis pad the cross is the d-pad and the three
/// face buttons are the ones on the right.
const DEFAULT_GAMEPAD: [(u8, PadButton); 12] = [
    (12, PadButton::Up),
    (13, PadButton::Down),
    (14, PadButton::Left),
    (15, PadButton::Right),
    (2, PadButton::A),
    (0, PadButton::B),
    (1, PadButton::C),
    (4, PadButton::X),
    (3, PadButton::Y),
    (5, PadButton::Z),
    (9, PadButton::Start),
    (8, PadButton::Mode),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The keyboard entry bound to `code`, if any.
    fn keyboard(binding: &Bindings, code: &str) -> Option<(usize, PadButton)> {
        binding
            .keyboard
            .iter()
            .find(|(bound, _, _)| bound == code)
            .map(|(_, port, button)| (*port, *button))
    }

    /// The Genesis button the standard index `index` is bound to, if any.
    fn gamepad(binding: &Bindings, index: u8) -> Option<PadButton> {
        binding
            .gamepad
            .iter()
            .find(|(bound, _)| *bound == index)
            .map(|(_, button)| *button)
    }

    #[test]
    fn bindings_survive_a_serde_round_trip() {
        let bindings = Bindings {
            keyboard: vec![
                ("ArrowUp".to_string(), 0, PadButton::Up),
                ("Space".to_string(), 1, PadButton::Start),
            ],
            gamepad: vec![(0, PadButton::B), (9, PadButton::Start)],
        };
        let json = serde_json::to_string(&bindings).expect("bindings serialize");
        let back: Bindings = serde_json::from_str(&json).expect("bindings deserialize");
        assert_eq!(back, bindings);
    }

    #[test]
    fn the_default_bindings_can_be_read_back_from_json() {
        // What `localStorage` will actually hold, so a default written by one
        // build is readable by the next - and `PadButton`'s snake_case names are
        // part of that file, not an implementation detail.
        let json = serde_json::to_string(&Bindings::default()).expect("defaults serialize");
        let back: Bindings = serde_json::from_str(&json).expect("defaults deserialize");
        assert_eq!(back, Bindings::default());
    }

    #[test]
    fn the_default_keyboard_binds_player_one() {
        let bindings = Bindings::default();
        assert_eq!(keyboard(&bindings, "ArrowUp"), Some((0, PadButton::Up)));
        assert_eq!(keyboard(&bindings, "ArrowDown"), Some((0, PadButton::Down)));
        assert_eq!(keyboard(&bindings, "ArrowLeft"), Some((0, PadButton::Left)));
        assert_eq!(
            keyboard(&bindings, "ArrowRight"),
            Some((0, PadButton::Right))
        );
        assert_eq!(keyboard(&bindings, "KeyA"), Some((0, PadButton::A)));
        assert_eq!(keyboard(&bindings, "KeyS"), Some((0, PadButton::B)));
        assert_eq!(keyboard(&bindings, "KeyD"), Some((0, PadButton::C)));
        assert_eq!(keyboard(&bindings, "KeyQ"), Some((0, PadButton::X)));
        assert_eq!(keyboard(&bindings, "KeyW"), Some((0, PadButton::Y)));
        assert_eq!(keyboard(&bindings, "KeyE"), Some((0, PadButton::Z)));
        assert_eq!(keyboard(&bindings, "Enter"), Some((0, PadButton::Start)));
        assert_eq!(
            keyboard(&bindings, "ShiftRight"),
            Some((0, PadButton::Mode))
        );
    }

    #[test]
    fn the_default_keyboard_leaves_the_second_port_unbound() {
        let bindings = Bindings::default();
        assert!(
            !bindings.keyboard.iter().any(|(_, port, _)| *port != 0),
            "default keyboard bindings reached a port other than 0: {:?}",
            bindings.keyboard
        );
    }

    #[test]
    fn the_default_gamepad_follows_the_standard_mapping() {
        let bindings = Bindings::default();
        assert_eq!(gamepad(&bindings, 12), Some(PadButton::Up));
        assert_eq!(gamepad(&bindings, 13), Some(PadButton::Down));
        assert_eq!(gamepad(&bindings, 14), Some(PadButton::Left));
        assert_eq!(gamepad(&bindings, 15), Some(PadButton::Right));
        assert_eq!(gamepad(&bindings, 2), Some(PadButton::A));
        assert_eq!(gamepad(&bindings, 0), Some(PadButton::B));
        assert_eq!(gamepad(&bindings, 1), Some(PadButton::C));
        assert_eq!(gamepad(&bindings, 4), Some(PadButton::X));
        assert_eq!(gamepad(&bindings, 3), Some(PadButton::Y));
        assert_eq!(gamepad(&bindings, 5), Some(PadButton::Z));
        assert_eq!(gamepad(&bindings, 9), Some(PadButton::Start));
        assert_eq!(gamepad(&bindings, 8), Some(PadButton::Mode));
    }
}
