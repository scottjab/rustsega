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

/// Which action a rebind took a key off.
///
/// Returned by [`Bindings::rebind_key`] so the controls page can say what it
/// just did - "`A` is now unbound" - rather than letting a key disappear from one
/// row with no explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Displaced {
    /// The player whose action lost the key.
    pub port: usize,
    /// The button that lost it.
    pub button: PadButton,
}

impl Bindings {
    /// The key bound to `button` for `port`, as the `KeyboardEvent.code` that
    /// [`super::keyboard::map_key`] looks up - the keycap the controls page shows.
    pub fn key_of(&self, port: usize, button: PadButton) -> Option<&str> {
        self.keyboard
            .iter()
            .find(|(_, bound_port, bound_button)| *bound_port == port && *bound_button == button)
            .map(|(code, _, _)| code.as_str())
    }

    /// The standard gamepad button index bound to `button`, for the Controller
    /// column of the controls page.
    pub fn gamepad_of(&self, button: PadButton) -> Option<u8> {
        self.gamepad
            .iter()
            .find(|(_, bound)| *bound == button)
            .map(|(index, _)| *index)
    }

    /// Binds `code` to `button` for `port`, keeping one key on one action.
    ///
    /// A code that was bound to anything else is taken off it - that action is
    /// left unbound and comes back as [`Some(Displaced)`] - and `button`'s own
    /// previous key goes with it, because a row of the table shows one keycap.
    /// Binding a button to the key it already has is a no-op that reports
    /// nothing, so the controls page can call this from every capture without
    /// having to know whether anything moved.
    pub fn rebind_key(&mut self, port: usize, button: PadButton, code: &str) -> Option<Displaced> {
        let displaced = self
            .keyboard
            .iter()
            .find(|(bound, _, _)| bound == code)
            .filter(|(_, bound_port, bound_button)| *bound_port != port || *bound_button != button)
            .map(|(_, bound_port, bound_button)| Displaced {
                port: *bound_port,
                button: *bound_button,
            });
        // Where this button's own entry was, so the new one takes its place and
        // the table does not shuffle every time a key is rebound.
        let previous = self
            .keyboard
            .iter()
            .position(|(_, bound_port, bound_button)| {
                *bound_port == port && *bound_button == button
            });
        self.keyboard.retain(|(bound, bound_port, bound_button)| {
            bound != code && !(*bound_port == port && *bound_button == button)
        });
        let entry = (code.to_string(), port, button);
        match previous {
            Some(index) => self.keyboard.insert(index, entry),
            None => self.keyboard.push(entry),
        }
        displaced
    }

    /// Binds the standard gamepad button `index` to `button`, the same rule as
    /// [`Bindings::rebind_key`] on the other side of the table: whoever else had
    /// `index` loses it and comes back as the button that did.
    pub fn rebind_gamepad(&mut self, button: PadButton, index: u8) -> Option<PadButton> {
        let displaced = self
            .gamepad
            .iter()
            .find(|(bound, _)| *bound == index)
            .filter(|(_, bound_button)| *bound_button != button)
            .map(|(_, bound_button)| *bound_button);
        let previous = self
            .gamepad
            .iter()
            .position(|(_, bound_button)| *bound_button == button);
        self.gamepad
            .retain(|(bound, bound_button)| *bound != index && *bound_button != button);
        let entry = (index, button);
        match previous {
            Some(position) => self.gamepad.insert(position, entry),
            None => self.gamepad.push(entry),
        }
        displaced
    }

    /// Restores what one player's table on the controls page shows: that port's
    /// keyboard entries, and the controller map.
    ///
    /// The gamepad map is not per port - a pad's own index is its port, so there
    /// is one map for every pad (see [`Bindings::gamepad`]) - so it is restored
    /// with the rest of the table rather than being left alone.
    pub fn reset_port(&mut self, port: usize) {
        let defaults = Self::default();
        self.keyboard
            .retain(|(_, bound_port, _)| *bound_port != port);
        self.keyboard.extend(
            defaults
                .keyboard
                .into_iter()
                .filter(|(_, bound_port, _)| *bound_port == port),
        );
        self.gamepad = defaults.gamepad;
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

    /// No code is bound twice: "one key, one action" is the rule `rebind_key`
    /// exists to keep, so every test that changes a binding checks it.
    fn no_duplicate_codes(bindings: &Bindings) -> bool {
        let mut codes: Vec<&str> = bindings
            .keyboard
            .iter()
            .map(|(code, _, _)| code.as_str())
            .collect();
        codes.sort_unstable();
        codes.dedup();
        codes.len() == bindings.keyboard.len()
    }

    /// And no button is bound twice on one port, which is what lets the controls
    /// page show one keycap per button.
    fn no_duplicate_buttons(bindings: &Bindings) -> bool {
        // `PadButton` is a binding key rather than an ordered thing, so the sort
        // key is its discriminant.
        let mut pairs: Vec<(usize, u8)> = bindings
            .keyboard
            .iter()
            .map(|(_, port, button)| (*port, *button as u8))
            .collect();
        pairs.sort_unstable();
        pairs.dedup();
        pairs.len() == bindings.keyboard.len()
    }

    #[test]
    fn rebinding_a_free_key_reports_no_displacement() {
        let mut bindings = Bindings::default();
        let displaced = bindings.rebind_key(0, PadButton::A, "KeyK");
        assert_eq!(displaced, None);
        assert_eq!(keyboard(&bindings, "KeyK"), Some((0, PadButton::A)));
        assert_eq!(bindings.key_of(0, PadButton::A), Some("KeyK"));
        assert!(
            keyboard(&bindings, "KeyA").is_none(),
            "A moved off its default key"
        );
        assert!(no_duplicate_codes(&bindings));
        assert!(no_duplicate_buttons(&bindings));
    }

    #[test]
    fn rebinding_a_key_displaces_whichever_action_was_using_it() {
        // The swap case the ticket asks for: `KeyA` is `A` by default, and
        // binding `B` to it takes `KeyA` off `A` rather than binding two actions
        // to one key.
        let mut bindings = Bindings::default();
        let displaced = bindings.rebind_key(0, PadButton::B, "KeyA");
        assert_eq!(
            displaced,
            Some(Displaced {
                port: 0,
                button: PadButton::A
            })
        );
        assert_eq!(keyboard(&bindings, "KeyA"), Some((0, PadButton::B)));
        assert_eq!(bindings.key_of(0, PadButton::B), Some("KeyA"));
        // `B`'s own default key is gone with it, and `A` is left unbound.
        assert!(keyboard(&bindings, "KeyS").is_none());
        assert_eq!(bindings.key_of(0, PadButton::A), None);
        assert!(no_duplicate_codes(&bindings));
        assert!(no_duplicate_buttons(&bindings));
    }

    #[test]
    fn rebinding_a_button_to_the_key_it_already_has_changes_nothing() {
        let mut bindings = Bindings::default();
        let before = bindings.clone();
        let displaced = bindings.rebind_key(0, PadButton::A, "KeyA");
        assert_eq!(displaced, None);
        assert_eq!(bindings, before, "and in the same order");
    }

    #[test]
    fn rebinding_a_key_on_the_second_port_displaces_the_first() {
        let mut bindings = Bindings::default();
        let displaced = bindings.rebind_key(1, PadButton::Start, "ArrowUp");
        assert_eq!(
            displaced,
            Some(Displaced {
                port: 0,
                button: PadButton::Up
            })
        );
        assert_eq!(keyboard(&bindings, "ArrowUp"), Some((1, PadButton::Start)));
        assert_eq!(bindings.key_of(0, PadButton::Up), None);
        assert_eq!(bindings.key_of(1, PadButton::Start), Some("ArrowUp"));
        assert!(no_duplicate_codes(&bindings));
        assert!(no_duplicate_buttons(&bindings));
    }

    #[test]
    fn rebinding_a_gamepad_button_displaces_its_previous_owner() {
        let mut bindings = Bindings::default();
        // Index 2 is `A` by default; binding `C` to it must not leave `A` there.
        let displaced = bindings.rebind_gamepad(PadButton::C, 2);
        assert_eq!(displaced, Some(PadButton::A));
        assert_eq!(gamepad(&bindings, 2), Some(PadButton::C));
        assert_eq!(bindings.gamepad_of(PadButton::C), Some(2));
        // `C`'s own index is free, and `A` has no button at all.
        assert!(gamepad(&bindings, 1).is_none());
        assert_eq!(bindings.gamepad_of(PadButton::A), None);
    }

    #[test]
    fn rebinding_a_gamepad_button_to_its_own_index_changes_nothing() {
        let mut bindings = Bindings::default();
        let before = bindings.clone();
        assert_eq!(bindings.rebind_gamepad(PadButton::B, 0), None);
        assert_eq!(bindings, before);
    }

    #[test]
    fn resetting_a_player_restores_its_default_keys() {
        let mut bindings = Bindings::default();
        bindings.rebind_key(0, PadButton::A, "Space");
        bindings.rebind_key(1, PadButton::A, "Numpad1");
        bindings.reset_port(0);
        let port_one: Vec<(String, usize, PadButton)> = bindings
            .keyboard
            .iter()
            .filter(|(_, port, _)| *port == 0)
            .cloned()
            .collect();
        assert_eq!(port_one, Bindings::default().keyboard);
        assert_eq!(
            keyboard(&bindings, "Numpad1"),
            Some((1, PadButton::A)),
            "player two's keys are not player one to reset"
        );
    }

    #[test]
    fn resetting_a_player_restores_the_shared_controller_map_too() {
        // The Controller column of a player's table shows the shared gamepad map,
        // so the reset button beside it has to restore it (see
        // `Bindings::gamepad` for why there is one map rather than two).
        let mut bindings = Bindings::default();
        bindings.rebind_gamepad(PadButton::A, 6);
        bindings.reset_port(0);
        assert_eq!(bindings.gamepad, Bindings::default().gamepad);
    }

    #[test]
    fn a_binding_can_be_read_back_by_port_and_button() {
        let bindings = Bindings::default();
        assert_eq!(bindings.key_of(0, PadButton::Up), Some("ArrowUp"));
        assert_eq!(bindings.key_of(1, PadButton::Up), None);
        assert_eq!(bindings.gamepad_of(PadButton::Mode), Some(8));
    }
}
