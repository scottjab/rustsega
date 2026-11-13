//! What a key means, as data (M7-07, #140).
//!
//! The mapping half of the keyboard, and none of the browser: [`map_key`] turns a
//! `KeyboardEvent.code` into a [`KeyAction`] by reading a [`Bindings`] table and
//! the fixed hotkey table, and the two rules beside it say when a key belongs to
//! something else and when the browser may be told to forget it. All three are
//! pure so `cargo test -p web` runs the whole of the keyboard policy on the host;
//! `crate::keyboard_events` is the half that owns a `KeyboardEvent`.
//!
//! `code`, never `key`: `code` names the physical key, so the default arrow
//! layout still works on a French keyboard where the same physical keys are
//! `AZERTY` (`KeyboardEvent`, "the code attribute").
//!
//! [`capture_key`] is the same question asked while the controls page (M7-10,
//! #143) is taking a binding, which has different answers - see it for why.

use super::Bindings;
use super::PadButton;

/// What a key does to the running console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Drive this button of this port: the key is a pad button.
    Pad { port: usize, button: PadButton },
    /// An emulator control rather than a pad button.
    Hotkey(Hotkey),
    /// Nothing is bound to the key.
    None,
}

/// The emulator's own keys, fixed for now (M7-07, #140).
///
/// They are not in [`Bindings`]: rebinding `Escape` or `Tab` away would leave no
/// way back to the menu, and the settings page (M7-10, #143) binds pad buttons,
/// not these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// `Escape`: open or close the settings drawer.
    ToggleMenu,
    /// `F1`: write a save state (M7-15, #148).
    SaveState,
    /// `F4`: read a save state (M7-15, #148).
    LoadState,
    /// `F11`: the screen fills the window (M7-12, #145).
    Fullscreen,
    /// `Tab`, held: run the console faster (M7-17, #150).
    FastForward,
    /// `KeyP`: hold the console or let it go.
    Pause,
}

/// The fixed hotkeys, as `(KeyboardEvent.code, hotkey)`.
///
/// Looked up before the pad bindings, so a hotkey wins a key that a binding
/// table also claims - see [`map_key`].
const HOTKEYS: [(&str, Hotkey); 6] = [
    ("Escape", Hotkey::ToggleMenu),
    ("F1", Hotkey::SaveState),
    ("F4", Hotkey::LoadState),
    ("F11", Hotkey::Fullscreen),
    ("Tab", Hotkey::FastForward),
    ("KeyP", Hotkey::Pause),
];

/// What `code` is bound to: a pad button, a hotkey, or nothing.
///
/// Hotkeys are asked first, so [`Hotkey::ToggleMenu`] cannot be shadowed by a
/// binding - the table a settings page writes is player input, and a player
/// who binds `Escape` to Jump must still be able to get out of the game.
pub fn map_key(code: &str, bindings: &Bindings) -> KeyAction {
    if let Some((_, hotkey)) = HOTKEYS.iter().find(|(bound, _)| *bound == code) {
        return KeyAction::Hotkey(*hotkey);
    }
    bindings
        .keyboard
        .iter()
        .find(|(bound, _, _)| bound == code)
        .map_or(KeyAction::None, |(_, port, button)| KeyAction::Pad {
            port: *port,
            button: *button,
        })
}

/// What a keydown means while a settings cell is capturing the next key (M7-10,
/// #143).
///
/// The capture owns the key absolutely, which is the whole reason this is a rule
/// of its own rather than a case in the listener: `Escape` is the shell's
/// `ToggleMenu` hotkey and `Tab` is its fast forward, so a listener that ran
/// [`map_key`] first would shut the drawer on a player trying to cancel a rebind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureKey {
    /// `Escape`: abandon the capture and leave the binding as it was.
    Cancel,
    /// Bind this `code` to the button whose cell is capturing.
    Bind {
        /// The `KeyboardEvent.code` to bind.
        code: String,
    },
    /// One of the shell's fixed hotkeys other than `Escape`. [`map_key`] gives
    /// those to the shell whatever a bindings table says, so binding one here
    /// would write an entry that can never fire - and a player who is told beats
    /// a player who is silently ignored.
    Refused,
}

/// Decides a keydown that arrived while a cell was capturing: cancel, bind, or
/// refuse. `code` is the `KeyboardEvent.code`, never the `key`.
pub fn capture_key(code: &str) -> CaptureKey {
    if code == "Escape" {
        return CaptureKey::Cancel;
    }
    if HOTKEYS.iter().any(|(bound, _)| *bound == code) {
        return CaptureKey::Refused;
    }
    CaptureKey::Bind {
        code: code.to_string(),
    }
}

/// The `input` types that are text entries: they take typed text, so their
/// letters and arrows belong to them.
///
/// `number` is one of them because it takes typed digits; `range` is not, nor is
/// any of the button-ish types, whose keys the browser spends on a value or an
/// activation that the player is better off reaching with the mouse.
const TEXT_INPUT_TYPES: [&str; 7] = [
    "text", "search", "url", "tel", "email", "password", "number",
];

/// Whether a key aimed at an element named `tag` belongs to that element rather
/// than to the game.
///
/// The two kinds that need it are a text entry and a `select`: the first takes
/// typed text and the second takes the arrow keys to move its own selection, so
/// typing a name in the settings drawer must not walk Sonic into a wall. Both
/// answers are the browser's own business, so the caller also leaves the event
/// alone (see [`should_prevent_default`]).
///
/// `input_type` is `Some` for an `input` element - the value of its `type`, which
/// is `text` when the attribute is absent - and `None` for anything else.
/// Deliberately *not* every focusable element: the top bar's buttons take focus
/// when clicked, and a player who clicks Pause and then reaches for the arrows
/// is still playing, not filling in a form.
pub fn element_takes_key(tag: &str, input_type: Option<&str>) -> bool {
    if tag.eq_ignore_ascii_case("SELECT") {
        return true;
    }
    tag.eq_ignore_ascii_case("INPUT")
        && input_type.is_none_or(|input_type| {
            TEXT_INPUT_TYPES
                .iter()
                .any(|text_type| input_type.eq_ignore_ascii_case(text_type))
        })
}

/// Whether the browser's own action for this key is cancelled: the arrow keys
/// stop scrolling the page and `Tab` stops moving focus while a game is running.
///
/// Only while a game is running, because with the empty screen up the page is
/// just a page and its controls are tabbable; and not while the settings drawer
/// is open, because the drawer is a form and the arrow keys are how its `select`
/// and its slider are used (M7-07, #140).
pub fn should_prevent_default(action: &KeyAction, playing: bool, drawer_open: bool) -> bool {
    !matches!(action, KeyAction::None) && playing && !drawer_open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_bindings_map_arrow_up_to_port_zero_up() {
        assert_eq!(
            map_key("ArrowUp", &Bindings::default()),
            KeyAction::Pad {
                port: 0,
                button: PadButton::Up
            }
        );
    }

    #[test]
    fn an_unbound_key_is_no_action() {
        let bindings = Bindings::default();
        // Not a pad binding, and not one of the hotkeys either.
        assert_eq!(map_key("KeyZ", &bindings), KeyAction::None);
        assert_eq!(map_key("Backquote", &bindings), KeyAction::None);
    }

    #[test]
    fn map_key_reads_the_bindings_it_is_handed() {
        // Not a table of its own: the bindings of M7-10 (#143) have to be
        // readable without touching this file, including on port 1.
        let bindings = Bindings {
            keyboard: vec![("KeyK".to_string(), 1, PadButton::Mode)],
            gamepad: Vec::new(),
        };
        assert_eq!(
            map_key("KeyK", &bindings),
            KeyAction::Pad {
                port: 1,
                button: PadButton::Mode
            }
        );
        // And the defaults are off: `ArrowUp` is bound by `Bindings::default()`,
        // not by `map_key`.
        assert_eq!(map_key("ArrowUp", &bindings), KeyAction::None);
    }

    #[test]
    fn escape_toggles_the_menu() {
        assert_eq!(
            map_key("Escape", &Bindings::default()),
            KeyAction::Hotkey(Hotkey::ToggleMenu)
        );
    }

    #[test]
    fn f1_saves_state() {
        assert_eq!(
            map_key("F1", &Bindings::default()),
            KeyAction::Hotkey(Hotkey::SaveState)
        );
    }

    #[test]
    fn f4_loads_state() {
        assert_eq!(
            map_key("F4", &Bindings::default()),
            KeyAction::Hotkey(Hotkey::LoadState)
        );
    }

    #[test]
    fn f11_toggles_fullscreen() {
        assert_eq!(
            map_key("F11", &Bindings::default()),
            KeyAction::Hotkey(Hotkey::Fullscreen)
        );
    }

    #[test]
    fn tab_holds_fast_forward() {
        assert_eq!(
            map_key("Tab", &Bindings::default()),
            KeyAction::Hotkey(Hotkey::FastForward)
        );
    }

    #[test]
    fn key_p_pauses() {
        assert_eq!(
            map_key("KeyP", &Bindings::default()),
            KeyAction::Hotkey(Hotkey::Pause)
        );
    }

    #[test]
    fn a_hotkey_beats_a_pad_binding_on_the_same_key() {
        // Every hotkey key, bound to a pad button by a settings table: none of
        // them may stop being a hotkey. `Tab` and `Enter`-adjacent keys are the
        // ones a player is most likely to bind by accident.
        let bindings = Bindings {
            keyboard: ["Escape", "F1", "F4", "F11", "Tab", "KeyP", "ArrowUp"]
                .iter()
                .map(|code| (code.to_string(), 0, PadButton::A))
                .collect(),
            gamepad: Vec::new(),
        };
        assert_eq!(
            map_key("Escape", &bindings),
            KeyAction::Hotkey(Hotkey::ToggleMenu)
        );
        assert_eq!(
            map_key("F1", &bindings),
            KeyAction::Hotkey(Hotkey::SaveState)
        );
        assert_eq!(
            map_key("F4", &bindings),
            KeyAction::Hotkey(Hotkey::LoadState)
        );
        assert_eq!(
            map_key("F11", &bindings),
            KeyAction::Hotkey(Hotkey::Fullscreen)
        );
        assert_eq!(
            map_key("Tab", &bindings),
            KeyAction::Hotkey(Hotkey::FastForward)
        );
        assert_eq!(map_key("KeyP", &bindings), KeyAction::Hotkey(Hotkey::Pause));
    }

    #[test]
    fn a_text_entry_takes_its_own_keys() {
        // `tag_name` is upper case in HTML, and an `input` with no `type`
        // attribute is a text entry.
        assert!(element_takes_key("INPUT", None));
        assert!(element_takes_key("input", Some("text")));
        for input_type in ["search", "url", "tel", "email", "password", "number"] {
            assert!(
                element_takes_key("INPUT", Some(input_type)),
                "{input_type} should take its own keys"
            );
        }
    }

    #[test]
    fn a_select_takes_its_own_keys() {
        // The theme list in the drawer: its arrow keys change the selection.
        assert!(element_takes_key("SELECT", None));
    }

    #[test]
    fn anything_else_leaves_the_keys_to_the_game() {
        assert!(!element_takes_key("BODY", None));
        assert!(!element_takes_key("CANVAS", None));
        // A focused top bar button must not swallow the d-pad: clicking Pause
        // leaves the button focused, and the player keeps playing with arrows.
        assert!(!element_takes_key("BUTTON", None));
        for input_type in ["button", "submit", "checkbox", "range", "file"] {
            assert!(
                !element_takes_key("INPUT", Some(input_type)),
                "{input_type} should leave the keys to the game"
            );
        }
    }

    #[test]
    fn a_bound_key_is_swallowed_while_a_game_runs() {
        let pad = KeyAction::Pad {
            port: 0,
            button: PadButton::Down,
        };
        assert!(should_prevent_default(&pad, true, false));
        assert!(should_prevent_default(
            &KeyAction::Hotkey(Hotkey::FastForward),
            true,
            false
        ));
    }

    #[test]
    fn a_bound_key_is_left_alone_with_no_game_loaded() {
        // With the empty screen up the page is just a page, and its controls are
        // there to be tabbed to.
        assert!(!should_prevent_default(
            &KeyAction::Pad {
                port: 0,
                button: PadButton::Down
            },
            false,
            false
        ));
    }

    #[test]
    fn a_bound_key_is_left_alone_while_the_drawer_is_open() {
        // The drawer's `select` and slider are moved with the arrow keys.
        assert!(!should_prevent_default(
            &KeyAction::Pad {
                port: 0,
                button: PadButton::Down
            },
            true,
            true
        ));
    }

    #[test]
    fn an_unbound_key_is_never_swallowed() {
        assert!(!should_prevent_default(&KeyAction::None, true, false));
    }

    #[test]
    fn a_capturing_cell_owns_escape_and_cancels_itself() {
        // `Escape` is the shell's `ToggleMenu` hotkey. A player cancelling a
        // rebind must not find the drawer sliding shut behind them, so the
        // capture takes the key before `map_key` is ever asked.
        assert_eq!(capture_key("Escape"), CaptureKey::Cancel);
    }

    #[test]
    fn a_capturing_cell_binds_an_ordinary_key() {
        assert_eq!(
            capture_key("KeyK"),
            CaptureKey::Bind {
                code: "KeyK".to_string()
            }
        );
        // Including a key the pad table does not know yet, and one that is
        // already bound elsewhere - `rebind_key` sorts out the displacement.
        assert_eq!(
            capture_key("ArrowUp"),
            CaptureKey::Bind {
                code: "ArrowUp".to_string()
            }
        );
    }

    #[test]
    fn a_capturing_cell_refuses_a_key_the_shell_keeps() {
        // `map_key` always wins a hotkey key for the shell (see
        // `a_hotkey_beats_a_pad_binding_on_the_same_key`), so binding one here
        // would write a table entry that can never fire. Saying so is kinder.
        for code in ["Tab", "F1", "F4", "F11", "KeyP"] {
            assert_eq!(capture_key(code), CaptureKey::Refused, "{code}");
        }
    }
}
