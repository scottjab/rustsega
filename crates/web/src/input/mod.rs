//! The input model: keyboard, Gamepad API and touch merged into [`genesis::Buttons`]
//! per port (M7-06, #139).
//!
//! docs/ARCHITECTURE.md, "Input: one `InputState` that merges keyboard, Gamepad
//! API, and touch overlay into `Buttons` per port". This is the whole of that
//! model and none of its browser plumbing: nothing here listens to a browser
//! event, so the module imports no browser bindings at all and [`InputState`]
//! runs under a plain `cargo test -p web`. The listeners - keyboard (M7-07, #140),
//! Gamepad API (M7-08, #141), touch overlay (M7-09, #142) - are the callers that
//! will translate real events into [`InputState::set`] calls, and the settings UI
//! (M7-10, #143) is the caller that edits [`Bindings`].
//!
//! The one piece of state the page shares is here: [`with_state`] is how a
//! listener reports a press and how the emulation loop reads a port. It is a
//! `thread_local` rather than an `InputState` owned by
//! [`crate::emulator::Emulator`] because the held-button table is not the
//! console's - it is there before a cartridge is loaded, it survives loading
//! another one, and every source writes the same table whether or not a console
//! happens to be running. A `Rc<RefCell<InputState>>` in a Leptos context is not
//! an alternative: `provide_context` wants `Send + Sync` (M7-07, #140).
//!
//! The model is a table of held buttons rather than a `Buttons` per port, and
//! that is the reason it exists. A `genesis::Buttons` is a set with no way to
//! take a button back out of it, so "the keyboard and the gamepad both say `A`,
//! the keyboard let go" cannot be answered by OR-ing sets: the state has to
//! remember which source is holding what. The two rules that come out of that
//! are what the tests below pin - a release only lands on its own source, and
//! cross-source presses still count for SOCD cleaning.

pub mod bindings;
pub mod button;
pub mod keyboard;

use std::cell::RefCell;

use genesis::Buttons;

pub use bindings::Bindings;
pub use button::PadButton;

/// The control ports the page drives: the console's two pad ports.
///
/// [`genesis::Genesis::set_pad`] takes a third port for the peripherals no pad
/// sits in, and [`InputState`] ignores a port it was not built with, so a
/// binding that reaches for player two has somewhere to land and a loop cannot
/// ask the console for a port it does not have.
pub const PORTS: usize = 2;

thread_local! {
    /// The page's held buttons, one table for every source.
    static PADS: RefCell<InputState> = RefCell::new(InputState::new(PORTS));
}

/// Runs `f` against the page's [`InputState`], which [`with_state`] owns.
///
/// The emulation loop reads it once per frame (see
/// [`crate::emulator`]); the keyboard listener writes it (M7-07, #140), as do
/// the gamepad (M7-08, #141) and the touch overlay (M7-09, #142).
pub fn with_state<T>(f: impl FnOnce(&mut InputState) -> T) -> T {
    PADS.with(|pads| f(&mut pads.borrow_mut()))
}

/// Who is reporting a button.
///
/// A press is remembered per source, so the same Genesis button held by two
/// sources takes two releases to clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// A `KeyboardEvent`, keyed by its `code` (layout independent).
    Keyboard,
    /// The Gamepad API pad at index `n`, whose buttons are the W3C standard
    /// mapping indices.
    Gamepad(u8),
    /// The on-screen touch overlay.
    Touch,
}

/// How a port resolves two directions on one axis held at the same time.
///
/// A physical d-pad cannot press both of a pair, and games that see both do
/// unpredictable things with it - which is why [`SocdMode::Neutral`] is the
/// default rather than "report both and hope".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SocdMode {
    /// Opposing directions cancel: holding Left and Right reports neither.
    #[default]
    Neutral,
    /// The direction pressed most recently wins, even across sources.
    LastWins,
}

/// One button held down by one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    source: Source,
    button: PadButton,
    /// The press counter of the `set(.., true)` that put this here, so
    /// [`SocdMode::LastWins`] can tell which of two directions came later. Bit
    /// order in a `Buttons` says nothing about press order, which is why this is
    /// a counter and not a bit position.
    seq: u64,
}

/// One port's held buttons.
#[derive(Debug, Clone, Default)]
struct PortState {
    /// Every held button on this port, in the order its press began.
    ///
    /// A `Vec` rather than a set because the same button can be held by several
    /// sources at once and each of them has to be able to let go on its own.
    /// Twelve buttons, a handful of sources, so a linear scan is the cheap
    /// choice and it keeps press order for free.
    presses: Vec<Held>,
}

/// The merged input state of every port.
#[derive(Debug, Clone)]
pub struct InputState {
    ports: Vec<PortState>,
    /// Press counter, incremented for every press that is new to its source.
    /// `u64` so it never wraps in a browser session.
    next_seq: u64,
    socd_mode: SocdMode,
}

impl InputState {
    /// A state with `ports` ports - 1 for a single-player session, 2 when both
    /// control slots are live.
    pub fn new(ports: usize) -> Self {
        Self {
            ports: vec![PortState::default(); ports],
            next_seq: 0,
            socd_mode: SocdMode::default(),
        }
    }

    /// The SOCD rule this state applies to the two d-pad axes.
    pub fn set_socd_mode(&mut self, mode: SocdMode) {
        self.socd_mode = mode;
    }

    /// Reports `button` on `port` as held or released by `source`.
    ///
    /// A press is new unless that same source already holds that same button, so
    /// a key repeat from the browser does not move it to the back of the press
    /// order; a release only ever removes `source`'s own hold, which is why a
    /// gamepad letting go cannot drop a key the keyboard is still holding down.
    ///
    /// A `port` this state does not have is ignored rather than fatal: a binding
    /// pointing at a second port nobody plugged in is a settings mistake, and
    /// one should not take the front end down.
    pub fn set(&mut self, source: Source, port: usize, button: PadButton, pressed: bool) {
        let Some(port) = self.ports.get_mut(port) else {
            return;
        };
        let already_held = port
            .presses
            .iter()
            .any(|held| held.source == source && held.button == button);
        match (pressed, already_held) {
            (true, false) => {
                let seq = self.next_seq;
                self.next_seq += 1;
                port.presses.push(Held {
                    source,
                    button,
                    seq,
                });
            }
            // A repeat of a press already held is not a new press.
            (true, true) => {}
            (false, _) => port
                .presses
                .retain(|held| !(held.source == source && held.button == button)),
        }
    }

    /// Releases every button `source` holds, on every port.
    ///
    /// For a gamepad disconnected mid-game and a window that has lost focus: in
    /// both cases no `keyup`/`buttonup` ever arrives, so the only way back to a
    /// pad that is not held down is to say so here.
    pub fn clear_source(&mut self, source: Source) {
        for port in &mut self.ports {
            port.presses.retain(|held| held.source != source);
        }
    }

    /// The [`Buttons`] to drive port `port` with: every source merged into one
    /// set, with the two d-pad axes cleaned of opposing directions.
    ///
    /// The result is rebuilt from the held table on every read rather than
    /// cached, because a `Buttons` cannot have a button taken out of it and the
    /// table is the only place a release can be accounted for.
    pub fn buttons(&self, port: usize) -> Buttons {
        let Some(port) = self.ports.get(port) else {
            return Buttons::empty();
        };
        // The newest press of each button held on this port. Newest because
        // `SocdMode::LastWins` compares presses, and a button held by two
        // sources has two of them.
        let mut held: Vec<(PadButton, u64)> = Vec::new();
        for press in &port.presses {
            match held.iter_mut().find(|(button, _)| *button == press.button) {
                Some(entry) => entry.1 = entry.1.max(press.seq),
                None => held.push((press.button, press.seq)),
            }
        }
        for (left, right) in [
            (PadButton::Left, PadButton::Right),
            (PadButton::Up, PadButton::Down),
        ] {
            let left_seq = newest(&held, left);
            let right_seq = newest(&held, right);
            let (Some(left_seq), Some(right_seq)) = (left_seq, right_seq) else {
                continue;
            };
            match self.socd_mode {
                SocdMode::Neutral => {
                    release(&mut held, left);
                    release(&mut held, right);
                }
                SocdMode::LastWins => {
                    // Two presses always get different `seq` values, so exactly
                    // one of the two is the earlier one.
                    let loser = if left_seq < right_seq { left } else { right };
                    release(&mut held, loser);
                }
            }
        }
        held.into_iter()
            .fold(Buttons::empty(), |buttons, (button, _)| {
                buttons | button.buttons()
            })
    }
}

/// The newest press of `button`, or `None` when nothing holds it.
fn newest(held: &[(PadButton, u64)], button: PadButton) -> Option<u64> {
    held.iter()
        .find(|(held, _)| *held == button)
        .map(|(_, seq)| *seq)
}

/// Takes `button` out of the held set for one axis of [`InputState::buttons`].
fn release(held: &mut Vec<(PadButton, u64)>, button: PadButton) {
    held.retain(|(held, _)| *held != button);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_are_the_or_of_every_source_on_that_port() {
        let mut input = InputState::new(2);
        input.set(Source::Keyboard, 0, PadButton::A, true);
        input.set(Source::Gamepad(0), 0, PadButton::B, true);
        input.set(Source::Touch, 0, PadButton::Left, true);
        assert_eq!(input.buttons(0), Buttons::A | Buttons::B | Buttons::LEFT);
    }

    #[test]
    fn ports_only_merge_their_own_presses() {
        let mut input = InputState::new(2);
        input.set(Source::Keyboard, 1, PadButton::Start, true);
        assert!(input.buttons(1).contains(Buttons::START));
        assert_eq!(input.buttons(0), Buttons::empty());
    }

    #[test]
    fn releasing_on_one_source_keeps_the_button_another_source_holds() {
        let mut input = InputState::new(2);
        input.set(Source::Keyboard, 0, PadButton::A, true);
        input.set(Source::Gamepad(0), 0, PadButton::A, true);

        // The keyboard lets go; the gamepad is still holding A, so the port does
        // not see a release at all.
        input.set(Source::Keyboard, 0, PadButton::A, false);
        assert!(input.buttons(0).contains(Buttons::A));

        input.set(Source::Gamepad(0), 0, PadButton::A, false);
        assert!(!input.buttons(0).contains(Buttons::A));
    }

    #[test]
    fn socd_defaults_to_neutral_and_drops_both_directions() {
        let mut input = InputState::new(2);
        // Held across sources, which is how SOCD happens on a web front end:
        // an arrow key and a gamepad d-pad are two different inputs.
        input.set(Source::Keyboard, 0, PadButton::Left, true);
        input.set(Source::Gamepad(0), 0, PadButton::Right, true);
        input.set(Source::Keyboard, 0, PadButton::Up, true);
        input.set(Source::Touch, 0, PadButton::Down, true);

        let held = input.buttons(0);
        assert!(!held.contains(Buttons::LEFT));
        assert!(!held.contains(Buttons::RIGHT));
        assert!(!held.contains(Buttons::UP));
        assert!(!held.contains(Buttons::DOWN));

        // Releasing one direction of an axis brings the other one back.
        input.set(Source::Keyboard, 0, PadButton::Left, false);
        let held = input.buttons(0);
        assert!(held.contains(Buttons::RIGHT));
        assert!(!held.contains(Buttons::LEFT));
    }

    #[test]
    fn socd_last_wins_keeps_the_later_press() {
        let mut input = InputState::new(2);
        input.set_socd_mode(SocdMode::LastWins);

        // Right first, then Left: Left is the one the player asked for last, and
        // a `Right` bit higher than `Left` in `Buttons` must not override that.
        input.set(Source::Keyboard, 0, PadButton::Right, true);
        input.set(Source::Gamepad(0), 0, PadButton::Left, true);
        let held = input.buttons(0);
        assert!(held.contains(Buttons::LEFT));
        assert!(!held.contains(Buttons::RIGHT));

        // Releasing the winner leaves the direction still held down.
        input.set(Source::Gamepad(0), 0, PadButton::Left, false);
        let held = input.buttons(0);
        assert!(held.contains(Buttons::RIGHT));
        assert!(!held.contains(Buttons::LEFT));

        // The other way round on both axes, so the rule cannot pass by
        // comparing bit positions in either direction.
        let mut input = InputState::new(2);
        input.set_socd_mode(SocdMode::LastWins);
        input.set(Source::Keyboard, 0, PadButton::Left, true);
        input.set(Source::Touch, 0, PadButton::Right, true);
        assert!(input.buttons(0).contains(Buttons::RIGHT));

        let mut input = InputState::new(2);
        input.set_socd_mode(SocdMode::LastWins);
        input.set(Source::Keyboard, 0, PadButton::Down, true);
        input.set(Source::Gamepad(0), 0, PadButton::Up, true);
        let held = input.buttons(0);
        assert!(held.contains(Buttons::UP));
        assert!(!held.contains(Buttons::DOWN));
    }

    #[test]
    fn a_repeated_press_is_not_a_new_press() {
        let mut input = InputState::new(2);
        input.set_socd_mode(SocdMode::LastWins);
        input.set(Source::Keyboard, 0, PadButton::Right, true);
        input.set(Source::Keyboard, 0, PadButton::Left, true);
        // Browser key repeat for Right arrives again. It must not make Right the
        // newest press of the horizontal axis.
        input.set(Source::Keyboard, 0, PadButton::Right, true);
        let held = input.buttons(0);
        assert!(held.contains(Buttons::LEFT));
        assert!(!held.contains(Buttons::RIGHT));
    }

    #[test]
    fn the_page_state_has_room_for_both_control_ports() {
        with_state(|input| {
            input.set(Source::Keyboard, 1, PadButton::Right, true);
            assert!(input.buttons(1).contains(Buttons::RIGHT));
            assert_eq!(input.buttons(0), Buttons::empty());
            // Back to an empty table for any other test that reads it.
            input.clear_source(Source::Keyboard);
            assert_eq!(input.buttons(1), Buttons::empty());
        });
    }

    #[test]
    fn clear_source_releases_everything_that_source_holds() {
        let mut input = InputState::new(2);
        let pad0 = Source::Gamepad(0);
        let pad1 = Source::Gamepad(1);
        input.set(pad0, 0, PadButton::A, true);
        input.set(pad0, 0, PadButton::Start, true);
        input.set(pad1, 1, PadButton::A, true);
        input.set(Source::Keyboard, 0, PadButton::B, true);
        // And one that is held by two sources on one port: clearing the pad
        // leaves the keyboard's hold standing.
        input.set(Source::Keyboard, 0, PadButton::B, true);

        input.clear_source(pad0);

        let port0 = input.buttons(0);
        assert!(!port0.contains(Buttons::A));
        assert!(!port0.contains(Buttons::START));
        assert!(port0.contains(Buttons::B));
        assert!(input.buttons(1).contains(Buttons::A));
    }
}
