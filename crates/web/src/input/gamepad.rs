//! What a Gamepad API pad means, as data (M7-08, #141).
//!
//! The pad half of the input model, and none of the browser: [`stick_to_dpad`]
//! decides what a stick position is, [`PadSlots`] decides which pad drives which
//! port, [`poll_snapshot`] turns one raw reading of one pad into the Genesis
//! buttons it is pressing, and [`apply_snapshot`] moves [`InputState`] from one
//! such reading to the next. All four are pure so `cargo test -p web` runs the
//! whole of the pad policy on the host; [`crate::gamepad_events`] is the half that
//! owns a `Gamepad`, and it does nothing but read one into plain `Vec<bool>` /
//! `Vec<f64>` values, call these, and say what they answered out loud.
//!
//! The indices and axes below are the W3C standard mapping - "Gamepad API",
//! "the standard gamepad" - which is what `Gamepad.mapping()` reports as
//! [`web_sys::GamepadMappingType::Standard`] in the browser half: buttons 0
//! bottom, 1 right, 2 left, 3 top, 4 LB, 5 RB, 6 LT, 7 RT, 8 back/select, 9
//! start, 10 L3, 11 R3, 12 up, 13 down, 14 left, 15 right, 16 home; axes 0 left
//! X, 1 left Y with down positive, 2 right X, 3 right Y.

use super::Bindings;
use super::InputState;
use super::PadButton;
use super::Source;
use super::keyboard::Hotkey;

/// How far a stick has to be off centre before it is a direction.
///
/// A stick at rest does not read `(0, 0)`: the W3C Gamepad API says an axis
/// value is "the current value of the axis" measured by a potentiometer that
/// drifts, and every browser-facing emulator leaves a margin before it calls a
/// centred stick a direction. A quarter of the stick's travel is wide enough for
/// the drift of every pad this has been tried on and narrow enough that a
/// deliberate press on the edge of the gate still registers.
pub const DEADZONE: f64 = 0.25;

/// The tangent of half a direction's slice of the stick: eight directions, so
/// each one is 45 degrees wide and any stick within 22.5 degrees of it counts.
///
/// Comparing against a ratio rather than an angle keeps this to two
/// multiplications per reading and gets the diagonals right without a
/// `atan2` - and a diagonal is exactly the case that wants both directions:
/// a Genesis d-pad has eight positions and a stick at 45 degrees is between two
/// of them, held towards both at once.
const SECTOR_TAN: f64 = 0.414_213_56;

/// The standard index of the right trigger, the pad's "run faster" button.
const RIGHT_TRIGGER: u8 = 7;
/// The standard index of `back`, which is the Genesis pad's `Mode`/`Select`.
///
/// `pub` because a menu is opened by `select` + `start` too - see
/// [`super::super::nav::pad_actions`], which owns the same combination while a
/// menu is open.
pub const SELECT: u8 = 8;
/// The standard index of `start`. See [`SELECT`].
pub const START: u8 = 9;
/// The standard index of the home button, which has no Genesis equivalent and so
/// drives the menu instead. See [`SELECT`].
pub const HOME: u8 = 16;

/// The four directions of one analog stick, as `(up, down, left, right)`.
///
/// `x` and `y` are one axis pair of the standard mapping - axes 0 and 1, the left
/// stick, with `y` positive downwards - and `deadzone` is how far off centre the
/// stick has to be to count at all (see [`DEADZONE`]).
///
/// The eight positions come from 45 degree sectors around the four cardinal
/// directions, so `(1, 0)` is Right alone while `(0.8, 0.8)` - 45 degrees, the
/// middle of a corner - is Down and Right together. A stick that is not where a
/// d-pad can be is reported as nothing rather than as the nearest direction it
/// happens to be closest to: that is what keeps a stick at rest, and a stick
/// whose browser reports as `NaN` because the device does not have that axis,
/// from becoming a held key.
pub fn stick_to_dpad(x: f64, y: f64, deadzone: f64) -> (bool, bool, bool, bool) {
    if !x.is_finite() || !y.is_finite() {
        return (false, false, false, false);
    }
    // The deadzone is a distance from the centre, so a stick cannot escape it by
    // moving diagonally.
    if x.hypot(y) <= deadzone {
        return (false, false, false, false);
    }
    let (ax, ay) = (x.abs(), y.abs());
    // "Within 22.5 degrees of the horizontal" is `ay <= ax * SECTOR_TAN`, and
    // `ax <= ay * SECTOR_TAN` the same for the vertical. Neither is true exactly
    // for a corner, and that is the case that wants both directions.
    let horizontal_only = ay <= ax * SECTOR_TAN;
    let vertical_only = ax <= ay * SECTOR_TAN;
    let (up, down) = if horizontal_only {
        (false, false)
    } else {
        (y < 0.0, y > 0.0)
    };
    let (left, right) = if vertical_only {
        (false, false)
    } else {
        (x < 0.0, x > 0.0)
    };
    (up, down, left, right)
}

/// Which of the two control ports each connected pad drives.
///
/// The Gamepad API numbers pads by the order the browser plugged them in, which
/// is not the order a player plugged them in and changes when one is unplugged:
/// a pad in port 2 has to keep driving port 2 when port 1's pad goes away, and
/// the pad plugged in next has to land in the port that just came free rather
/// than in a third one. Hence a slot table, held in connection order, of exactly
/// [`super::PORTS`] entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PadSlots {
    /// The gamepad index driving each port, `None` when nothing is plugged into
    /// it. Indexed by port, so port `n` is `slots[n]`.
    slots: Vec<Option<u8>>,
}

impl PadSlots {
    /// Two empty ports.
    pub fn new() -> Self {
        Self {
            slots: vec![None; super::PORTS],
        }
    }

    /// Files pad `index` as connected, and says which port it got.
    ///
    /// `None` means there was no port free for it: a third controller is not
    /// plugged into anything the console has, so it is ignored rather than
    /// stealing a player's pad. A pad already in a slot keeps it, so a browser
    /// that repeats the connect event does not shuffle the players about.
    pub fn connect(&mut self, index: u8) -> Option<usize> {
        if let Some(port) = self.port_of(index) {
            return Some(port);
        }
        let port = self.slots.iter().position(|slot| slot.is_none())?;
        self.slots[port] = Some(index);
        Some(port)
    }

    /// Takes pad `index` out of its slot and says which port that freed.
    ///
    /// `None` for a pad this table never heard of - a third controller, which is
    /// not in a slot and so frees nothing.
    pub fn disconnect(&mut self, index: u8) -> Option<usize> {
        let port = self.port_of(index)?;
        self.slots[port] = None;
        Some(port)
    }

    /// The port pad `index` drives, `None` when it drives nothing.
    pub fn port_of(&self, index: u8) -> Option<usize> {
        self.slots.iter().position(|slot| *slot == Some(index))
    }

    /// Whether every port is empty again.
    ///
    /// What a page with a touch overlay asks when a controller comes and goes: a
    /// player holding a pad does not want the on-screen buttons under their thumbs
    /// (M7-09, #142).
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(|slot| slot.is_none())
    }
}

/// The Genesis buttons one raw reading of one pad is pressing.
///
/// `buttons` is indexed by the standard gamepad button index and `axes` by the
/// standard axis index, both read straight out of a `Gamepad` by
/// [`crate::gamepad_events`]; `bindings` is the table of which standard index is
/// which Genesis button, whose [`Bindings::gamepad`] half is exactly this
/// mapping by default.
///
/// The left stick is folded in on top of the d-pad buttons, which is what lets a
/// player use either: a pad that reports Right on button 15 and a stick pushed
/// right both land on [`PadButton::Right`], and holding both is still one
/// direction rather than two.
///
/// The answer is in [`PadButton::ALL`] order, and always holds each button once,
/// so two readings can be compared by name - see [`apply_snapshot`].
pub fn poll_snapshot(buttons: &[bool], axes: &[f64], bindings: &Bindings) -> Vec<PadButton> {
    let (up, down, left, right) = match (axes.first(), axes.get(1)) {
        (Some(x), Some(y)) => stick_to_dpad(*x, *y, DEADZONE),
        // A pad that reported no axes at all: its d-pad buttons still work.
        _ => (false, false, false, false),
    };
    PadButton::ALL
        .into_iter()
        .filter(|button| {
            let bound = bindings.gamepad.iter().any(|(index, bound)| {
                bound == button && buttons.get(usize::from(*index)) == Some(&true)
            });
            let stick = match button {
                PadButton::Up => up,
                PadButton::Down => down,
                PadButton::Left => left,
                PadButton::Right => right,
                _ => false,
            };
            bound || stick
        })
        .collect()
}

/// The hotkey one raw reading of one pad is asking for, `None` for an ordinary
/// reading.
///
/// Two of the standard buttons are not pad buttons at all: `home` has nothing to
/// be on a Genesis pad, and the right trigger is the natural "run faster" hand.
/// `home` or `select` + `start` together open the menu - the combination because a
/// pad with no `home` button needs a way in, and holding the two buttons no game
/// wants at once is free. `start` alone is the Genesis `Start` and must stay that
/// way, so the combination is the only way in for those pads.
pub fn poll_hotkey(buttons: &[bool]) -> Option<Hotkey> {
    let held = |index: u8| buttons.get(usize::from(index)) == Some(&true);
    if held(HOME) || (held(SELECT) && held(START)) {
        return Some(Hotkey::ToggleMenu);
    }
    held(RIGHT_TRIGGER).then_some(Hotkey::FastForward)
}

/// A hotkey a pad has just started holding, or just let go of (M7-17, #150).
///
/// The release half is here because a poll has no keyup: [`poll_hotkey`] is asked
/// the same question every frame, and the only way to learn that the player has let
/// go of the trigger is to compare two readings and say so. A keyboard does not need
/// the second case - the browser sends one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// This hotkey has just been asked for.
    Pressed(Hotkey),
    /// This hotkey is no longer being asked for.
    Released(Hotkey),
}

/// The difference between two readings of one pad's [`poll_hotkey`], as the event
/// that change is: the arrival of a hotkey, or the release of the one that has gone.
///
/// A pad that has moved straight from one hotkey to another is reported as the
/// arrival, not as a release and a press in the wrong order. That is safe for the
/// one hotkey this shell holds rather than taps: `poll_hotkey`'s other answer opens
/// a menu, and `crate::menu::Menu::enter` lets go of fast forward for it.
pub fn hotkey_change(previous: Option<Hotkey>, current: Option<Hotkey>) -> Option<HotkeyEvent> {
    if current == previous {
        return None;
    }
    match (previous, current) {
        (_, Some(hotkey)) => Some(HotkeyEvent::Pressed(hotkey)),
        (Some(hotkey), None) => Some(HotkeyEvent::Released(hotkey)),
        (None, None) => None,
    }
}

/// Moves one pad's held buttons in `input` from `previous` to `next`.
///
/// Only the difference is reported, because [`InputState::set`] is a press or a
/// release and a press repeats nothing: writing all twelve buttons every frame
/// would be twelve times the work and would make every button of this pad the
/// newest press on its port, which is what [`super::SocdMode::LastWins`] reads.
/// Releases go first, so a pad moving from Right to Left is seen to let go before
/// it is seen to press.
pub fn apply_snapshot(
    input: &mut InputState,
    source: Source,
    port: usize,
    previous: &[PadButton],
    next: &[PadButton],
) {
    for button in previous {
        if !next.contains(button) {
            input.set(source, port, *button, false);
        }
    }
    for button in next {
        if !previous.contains(button) {
            input.set(source, port, *button, true);
        }
    }
}

/// The first button index whose press began this frame: held now, not held on
/// the `previous` reading.
///
/// The controls page (M7-10, #143) captures the next pad button for a binding,
/// and it wants the edge rather than the level: a player who was already holding
/// a button when they clicked the cell must not bind it without pressing
/// anything. `previous` is `None` for the reading that arms a capture, so this
/// answers `None` for it - the baseline frame binds nothing, however many buttons
/// are down.
///
/// `held` and `previous` are one pad's pressed button indices, as
/// [`crate::gamepad_events`] reads them off the `Gamepad`.
pub fn newly_pressed(held: &[u8], previous: Option<&[u8]>) -> Option<u8> {
    held.iter()
        .find(|index| previous.is_some_and(|previous| !previous.contains(index)))
        .copied()
}

/// The toast for a pad that has just been given port `port`.
pub fn connect_toast(port: usize, id: &str) -> String {
    format!("Controller {} connected: {}", port + 1, id)
}

/// The toast for the pad that drove port `port`.
pub fn disconnect_toast(port: usize) -> String {
    format!("Controller {} disconnected", port + 1)
}

/// The toast for a pad whose reports do not follow the standard mapping.
///
/// The indices are still used as they are, because they are the ones the
/// bindings table is written against and a pad that reports itself as
/// non-standard usually only got its layout string wrong - but when it did not,
/// the player is the only one who can tell what button 2 really is, and M7-10
/// (#143) is where they get to say so.
pub const NON_STANDARD_TOAST: &str =
    "This controller isn't using the standard layout, you may need to rebind it";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PORTS;
    use genesis::Buttons;

    /// The four directions `stick_to_dpad` answers for `x`/`y` at the default
    /// deadzone, as the pad buttons they are.
    fn stick(x: f64, y: f64) -> Vec<PadButton> {
        let (up, down, left, right) = stick_to_dpad(x, y, DEADZONE);
        [
            (up, PadButton::Up),
            (down, PadButton::Down),
            (left, PadButton::Left),
            (right, PadButton::Right),
        ]
        .into_iter()
        .filter(|(held, _)| *held)
        .map(|(_, button)| button)
        .collect()
    }

    #[test]
    fn a_centred_stick_is_no_direction() {
        assert_eq!(
            stick_to_dpad(0.0, 0.0, DEADZONE),
            (false, false, false, false)
        );
        // And at zero deadzone too, so a stick resting at (0, 0) cannot be read
        // as the corner it is technically equidistant from.
        assert_eq!(stick_to_dpad(0.0, 0.0, 0.0), (false, false, false, false));
    }

    #[test]
    fn a_stick_inside_the_deadzone_is_no_direction() {
        let deadzone = 0.5;
        assert_eq!(
            stick_to_dpad(0.3, 0.3, deadzone),
            (false, false, false, false)
        );
        assert_eq!(
            stick_to_dpad(0.49, 0.0, deadzone),
            (false, false, false, false)
        );
        // The edge of the deadzone is still inside it.
        assert_eq!(
            stick_to_dpad(0.5, 0.0, deadzone),
            (false, false, false, false)
        );
        // The first reading outside it is a direction.
        assert_eq!(
            stick_to_dpad(0.51, 0.0, deadzone),
            (false, false, false, true)
        );
    }

    #[test]
    fn a_stick_fully_right_is_right_alone() {
        assert_eq!(stick_to_dpad(1.0, 0.0, 0.0), (false, false, false, true));
        assert_eq!(stick(1.0, 0.0), vec![PadButton::Right]);
    }

    #[test]
    fn a_stick_fully_up_is_up_alone() {
        // Axes point down a positive y, so up is negative y.
        assert_eq!(stick_to_dpad(0.0, -1.0, 0.0), (true, false, false, false));
        assert_eq!(stick(0.0, -1.0), vec![PadButton::Up]);
    }

    #[test]
    fn a_stick_on_the_sector_boundary_is_a_corner() {
        // 45 degrees is the middle of a corner sector, so both directions of it.
        assert_eq!(stick_to_dpad(0.8, 0.8, 0.0), (false, true, false, true));
        assert_eq!(stick(0.8, 0.8), vec![PadButton::Down, PadButton::Right]);
        assert_eq!(stick(-0.8, -0.8), vec![PadButton::Up, PadButton::Left]);
    }

    #[test]
    fn a_stick_just_inside_a_cardinal_sector_is_that_direction_alone() {
        // 22.5 degrees is the edge of Right's sector: inside it, Right only.
        assert_eq!(stick(0.9, 0.2), vec![PadButton::Right]);
        // And past it, the corner.
        assert_eq!(stick(0.9, 0.4), vec![PadButton::Down, PadButton::Right]);
    }

    #[test]
    fn a_stick_the_browser_cannot_read_is_no_direction() {
        // Firefox reports an axis a device has not got as NaN, and a NaN
        // comparison is false in every direction at once - which must not read
        // as the corner two false comparisons point at.
        assert_eq!(
            stick_to_dpad(f64::NAN, 0.5, DEADZONE),
            (false, false, false, false)
        );
        assert_eq!(
            stick_to_dpad(0.5, f64::NAN, DEADZONE),
            (false, false, false, false)
        );
    }

    #[test]
    fn pads_get_ports_in_connection_order() {
        let mut slots = PadSlots::new();
        assert_eq!(slots.connect(4), Some(0));
        assert_eq!(slots.connect(0), Some(1));
        assert_eq!(slots.port_of(4), Some(0));
        assert_eq!(slots.port_of(0), Some(1));
        assert_eq!(slots.port_of(9), None);
    }

    #[test]
    fn a_disconnect_frees_its_port_for_the_next_pad() {
        let mut slots = PadSlots::new();
        slots.connect(4);
        slots.connect(0);
        assert_eq!(slots.disconnect(4), Some(0));
        assert_eq!(slots.port_of(4), None);
        // The pad plugged in next takes the port that came free, and the pad
        // still in play keeps its own.
        assert_eq!(slots.connect(1), Some(0));
        assert_eq!(slots.port_of(0), Some(1));
        // Replugging the original pad then finds it nothing to take: port 2 is
        // busy and it is third in line.
        assert_eq!(slots.connect(4), None);
    }

    #[test]
    fn a_third_pad_is_ignored() {
        let mut slots = PadSlots::new();
        assert_eq!(slots.connect(0), Some(0));
        assert_eq!(slots.connect(1), Some(1));
        assert_eq!(slots.connect(2), None);
        assert_eq!(slots.port_of(2), None);
        assert_eq!(slots.disconnect(2), None);
        // The two real pads are untouched by it.
        assert_eq!(slots.port_of(0), Some(0));
        assert_eq!(slots.port_of(1), Some(1));
    }

    #[test]
    fn a_repeated_connect_does_not_move_a_pad_off_its_port() {
        let mut slots = PadSlots::new();
        assert_eq!(slots.connect(3), Some(0));
        assert_eq!(slots.connect(3), Some(0));
        assert_eq!(slots.port_of(3), Some(0));
        assert_eq!(slots.port_of(4), None);
    }

    #[test]
    fn the_slots_are_empty_until_a_pad_arrives_and_empty_again_when_it_goes() {
        let mut slots = PadSlots::new();
        assert!(slots.is_empty());
        slots.connect(1);
        assert!(!slots.is_empty());
        slots.disconnect(1);
        assert!(slots.is_empty());
    }

    #[test]
    fn pad_slots_has_room_for_every_port_the_page_drives() {
        let slots = PadSlots::new();
        assert_eq!(PORTS, 2);
        for index in 0..(PORTS as u8) {
            assert_eq!(slots.port_of(index), None);
        }
    }

    /// The reading of a pad with standard button `index` held, nothing else.
    fn press(index: u8) -> Vec<bool> {
        let mut buttons = vec![false; 17];
        buttons[index as usize] = true;
        buttons
    }

    #[test]
    fn the_default_bindings_read_a_standard_pad_as_a_genesis_pad() {
        let bindings = Bindings::default();
        let axes = vec![0.0, 0.0, 0.0, 0.0];
        let read = |index| poll_snapshot(&press(index), &axes, &bindings);
        assert_eq!(read(0), vec![PadButton::B]);
        assert_eq!(read(1), vec![PadButton::C]);
        assert_eq!(read(2), vec![PadButton::A]);
        assert_eq!(read(3), vec![PadButton::Y]);
        assert_eq!(read(4), vec![PadButton::X]);
        assert_eq!(read(5), vec![PadButton::Z]);
        assert_eq!(read(8), vec![PadButton::Mode]);
        assert_eq!(read(9), vec![PadButton::Start]);
        assert_eq!(read(12), vec![PadButton::Up]);
        assert_eq!(read(13), vec![PadButton::Down]);
        assert_eq!(read(14), vec![PadButton::Left]);
        assert_eq!(read(15), vec![PadButton::Right]);
    }

    #[test]
    fn the_triggers_and_the_thumb_sticks_are_bound_to_nothing() {
        // The default table has twelve entries for twelve Genesis buttons, so
        // LT (6), the thumb sticks (10, 11) and home (16) read as nothing: they
        // are the hotkeys and the unbound buttons, not pad buttons.
        let bindings = Bindings::default();
        let axes = vec![0.0, 0.0, 0.0, 0.0];
        for index in [6, 7, 10, 11, 16] {
            assert_eq!(
                poll_snapshot(&press(index), &axes, &bindings),
                Vec::new(),
                "index {index}"
            );
        }
    }

    #[test]
    fn a_stick_reads_as_the_d_pad() {
        let bindings = Bindings::default();
        let axes = vec![0.8, 0.8, 0.0, 0.0];
        assert_eq!(
            poll_snapshot(&[], &axes, &bindings),
            vec![PadButton::Down, PadButton::Right]
        );
        // A centred stick, and one at rest in an array too short to hold it.
        assert_eq!(poll_snapshot(&[], &[0.0, 0.0], &bindings), Vec::new());
        assert_eq!(poll_snapshot(&[], &[], &bindings), Vec::new());
    }

    #[test]
    fn a_d_pad_button_and_the_stick_are_one_direction() {
        // Both hands on one direction is still that direction, once.
        let bindings = Bindings::default();
        let snapshot = poll_snapshot(&press(15), &[1.0, 0.0], &bindings);
        assert_eq!(snapshot, vec![PadButton::Right]);
    }

    #[test]
    fn a_snapshot_reads_only_the_buttons_the_pad_reported() {
        let bindings = Bindings::default();
        // A pad with fewer buttons than the table, and one with fewer axes, must
        // answer from what is there rather than run off the end of either.
        assert_eq!(
            poll_snapshot(&[true], &[0.0], &Bindings::default()),
            vec![PadButton::B]
        );
        assert!(poll_snapshot(&[], &[], &bindings).is_empty());
    }

    #[test]
    fn a_snapshot_follows_a_rebound_table() {
        let bindings = Bindings {
            gamepad: vec![(6, PadButton::A), (7, PadButton::Start)],
            ..Bindings::default()
        };
        let buttons = press(6);
        assert_eq!(
            poll_snapshot(&buttons, &[0.0, 0.0], &bindings),
            vec![PadButton::A]
        );
        let buttons = press(7);
        assert_eq!(
            poll_snapshot(&buttons, &[0.0, 0.0], &bindings),
            vec![PadButton::Start]
        );
    }

    #[test]
    fn a_snapshot_lists_each_button_once_in_pad_button_order() {
        // The order [`apply_snapshot`] compares two readings by.
        let bindings = Bindings::default();
        let mut buttons = vec![false; 17];
        for index in [9, 0, 13, 2, 15] {
            buttons[index as usize] = true;
        }
        let snapshot = poll_snapshot(&buttons, &[0.0, 1.0], &bindings);
        assert_eq!(
            snapshot,
            vec![
                PadButton::Down,
                PadButton::Right,
                PadButton::A,
                PadButton::B,
                PadButton::Start,
            ]
        );
    }

    #[test]
    fn home_and_select_with_start_are_the_menu() {
        assert_eq!(poll_hotkey(&press(HOME)), Some(Hotkey::ToggleMenu));
        let mut both = vec![false; 17];
        both[SELECT as usize] = true;
        both[START as usize] = true;
        assert_eq!(poll_hotkey(&both), Some(Hotkey::ToggleMenu));
    }

    #[test]
    fn the_right_trigger_is_fast_forward() {
        assert_eq!(
            poll_hotkey(&press(RIGHT_TRIGGER)),
            Some(Hotkey::FastForward)
        );
    }

    #[test]
    fn an_ordinary_reading_asks_for_no_hotkey() {
        assert_eq!(poll_hotkey(&[]), None);
        // Start on its own is the Genesis Start, and select on its own the
        // Genesis Mode: neither opens a menu.
        assert_eq!(poll_hotkey(&press(START)), None);
        assert_eq!(poll_hotkey(&press(SELECT)), None);
        assert_eq!(poll_hotkey(&[true; 5]), None);
    }

    /// Fast forward is held rather than tapped, so the end of the hold is as much
    /// news as its start: a pad that has let go of the trigger has to be heard,
    /// or the console runs on at 4x for the rest of the session.
    #[test]
    fn a_held_hotkey_is_reported_at_both_ends() {
        assert_eq!(
            hotkey_change(None, Some(Hotkey::FastForward)),
            Some(HotkeyEvent::Pressed(Hotkey::FastForward))
        );
        assert_eq!(
            hotkey_change(Some(Hotkey::FastForward), None),
            Some(HotkeyEvent::Released(Hotkey::FastForward))
        );
    }

    /// A trigger held across sixty frames is one press, not sixty: `poll_hotkey`
    /// says the same thing twice and the second time is not a change.
    #[test]
    fn a_hotkey_that_has_not_changed_is_not_reported_again() {
        assert_eq!(
            hotkey_change(Some(Hotkey::FastForward), Some(Hotkey::FastForward)),
            None
        );
        assert_eq!(hotkey_change(None, None), None);
    }

    /// Moving straight from one hotkey to the other is the arrival of the new one.
    /// The departure of the old one is not reported, which is safe because the
    /// only hotkey a pad reports other than fast forward opens a menu, and
    /// `crate::menu::Menu::enter` lets go of fast forward for it.
    #[test]
    fn moving_from_one_hotkey_to_another_reports_the_arrival() {
        assert_eq!(
            hotkey_change(Some(Hotkey::ToggleMenu), Some(Hotkey::FastForward)),
            Some(HotkeyEvent::Pressed(Hotkey::FastForward))
        );
        assert_eq!(
            hotkey_change(Some(Hotkey::FastForward), Some(Hotkey::ToggleMenu)),
            Some(HotkeyEvent::Pressed(Hotkey::ToggleMenu))
        );
    }

    #[test]
    fn applying_a_snapshot_presses_what_is_new_and_releases_what_is_not() {
        let mut input = InputState::new(PORTS);
        let source = Source::Gamepad(3);
        apply_snapshot(
            &mut input,
            source,
            0,
            &[],
            &[PadButton::Right, PadButton::B],
        );
        assert_eq!(input.buttons(0), Buttons::RIGHT | Buttons::B);

        // The stick comes back to centre and B is still held: one release.
        apply_snapshot(
            &mut input,
            source,
            0,
            &[PadButton::Right, PadButton::B],
            &[PadButton::B],
        );
        assert_eq!(input.buttons(0), Buttons::B);

        // Nothing changed, so nothing is reported - and the pad keeps its own
        // press order rather than becoming the newest thing on the port again.
        apply_snapshot(&mut input, source, 0, &[PadButton::B], &[PadButton::B]);
        assert_eq!(input.buttons(0), Buttons::B);
    }

    #[test]
    fn applying_a_snapshot_leaves_another_source_alone() {
        let mut input = InputState::new(PORTS);
        input.set(Source::Keyboard, 0, PadButton::B, true);
        let source = Source::Gamepad(0);
        apply_snapshot(&mut input, source, 0, &[PadButton::B], &[]);
        // The pad let go of B; the keyboard is still holding it.
        assert_eq!(input.buttons(0), Buttons::B);
    }

    #[test]
    fn a_replugged_pad_leaves_no_button_held() {
        // The third acceptance criterion, in the only form it can take without a
        // pad: the slots and the state going through a whole connect / play /
        // unplug / replug cycle.
        let mut input = InputState::new(PORTS);
        let mut slots = PadSlots::new();
        let bindings = Bindings::default();

        assert_eq!(slots.connect(7), Some(0));
        let held = poll_snapshot(&press(0), &[1.0, 0.0], &bindings);
        apply_snapshot(&mut input, Source::Gamepad(7), 0, &[], &held);
        assert_eq!(input.buttons(0), Buttons::B | Buttons::RIGHT);

        // Unplugged: the slot frees and the source is cleared, which is what the
        // browser half does on a `gamepaddisconnected`.
        assert_eq!(slots.disconnect(7), Some(0));
        input.clear_source(Source::Gamepad(7));
        assert_eq!(input.buttons(0), Buttons::empty());

        // A different pad in a different index takes port 1 and reads clean.
        assert_eq!(slots.connect(2), Some(0));
        let previous = poll_snapshot(&[false; 17], &[0.0, 0.0], &bindings);
        assert!(previous.is_empty());
        assert_eq!(input.buttons(0), Buttons::empty());
        let held = poll_snapshot(&press(9), &[0.0, 0.0], &bindings);
        apply_snapshot(&mut input, Source::Gamepad(2), 0, &previous, &held);
        assert_eq!(input.buttons(0), Buttons::START);
    }

    #[test]
    fn the_toasts_name_the_port_and_the_controller() {
        assert_eq!(
            connect_toast(0, "Microsoft X-Arcade (platform-usb-0x1234)"),
            "Controller 1 connected: Microsoft X-Arcade (platform-usb-0x1234)"
        );
        assert_eq!(disconnect_toast(1), "Controller 2 disconnected");
        assert_eq!(
            NON_STANDARD_TOAST,
            "This controller isn't using the standard layout, you may need to rebind it"
        );
    }

    #[test]
    fn newly_pressed_finds_the_button_that_began_being_held() {
        assert_eq!(newly_pressed(&[0, 4], Some(&[0])), Some(4));
    }

    #[test]
    fn newly_pressed_is_nothing_on_the_baseline_reading() {
        // A pad already covered in buttons arms a capture with its first frame;
        // that frame is the baseline, and it must bind nothing.
        assert_eq!(newly_pressed(&[0, 1, 2], None), None);
    }

    #[test]
    fn newly_pressed_is_nothing_while_the_same_buttons_are_held() {
        assert_eq!(newly_pressed(&[0, 1], Some(&[0, 1])), None);
        assert_eq!(newly_pressed(&[], Some(&[])), None);
    }
}
