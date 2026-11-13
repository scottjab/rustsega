//! The gamepad's browser half: the two hotplug listeners and the reading of
//! every pad into [`crate::input`] once per animation frame (M7-08, #141).
//!
//! The policy is elsewhere and pure - [`crate::input::gamepad`] decides which pad
//! drives which port, what a raw reading means, and what the difference between
//! two readings is. This file is the part that cannot be pure: it holds
//! `Gamepad`s, and it is the only place in the crate that calls
//! `Navigator::get_gamepads`.
//!
//! It is split in two because the Gamepad API reports in two ways.
//! `gamepadconnected`/`gamepaddisconnected` say that a pad arrived or went, which
//! is what [`PadSlots`](crate::input::gamepad::PadSlots) keeps and what the
//! toasts report; what a pad is *doing* only comes from polling
//! `get_gamepads()`, so [`poll_gamepads`] runs once per animation frame.
//!
//! The poll runs outside the running console's borrow - it is the animation
//! frame's first move, before [`crate::emulator`] borrows the machine - because a
//! pad has hotkeys, and a hotkey can ask the console for something. Doing that
//! while the console is borrowed is a second `borrow_mut` inside the first,
//! which is the #387 way to end a page.

use std::cell::RefCell;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::Gamepad;
use web_sys::GamepadButton;
use web_sys::GamepadEvent;
use web_sys::GamepadMappingType;

use crate::emulator::Controls;
use crate::input::Bindings;
use crate::input::PadButton;
use crate::input::Source;
use crate::input::gamepad;
use crate::input::gamepad::PadSlots;
use crate::input::keyboard::Hotkey;
use crate::keyboard_events::hotkey_do;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// One port's pad, as the last animation frame read it.
#[derive(Default)]
struct PadSeen {
    /// The Genesis buttons it was holding, in [`PadButton::ALL`] order.
    buttons: Vec<PadButton>,
    /// The hotkey it was asking for, so that a held trigger is one hotkey
    /// rather than one per frame.
    hotkey: Option<Hotkey>,
}

/// The page's pads: which port each drives, what each was last read as holding,
/// and the shell a pad can reach through a hotkey.
struct Watcher {
    slots: PadSlots,
    /// One table, read on every reading and written by nobody until the controls
    /// page (M7-10, #143) - the same arrangement as the keyboard's in
    /// [`crate::keyboard_events`].
    bindings: Bindings,
    /// Indexed by port, and reset whenever a pad takes a slot, so a pad cannot
    /// inherit the held buttons of the pad that left it.
    seen: Vec<PadSeen>,
    console: Controls,
    drawer_open: RwSignal<bool>,
    toasts: Toasts,
    /// Whether any pad is plugged in at all, published for the touch overlay -
    /// which hides itself while a real controller is about (M7-09, #142).
    controller_connected: RwSignal<bool>,
}

impl Watcher {
    /// A pad has arrived: give it a port and say so.
    fn connected(&mut self, pad: &Gamepad) {
        let index = pad_index(pad);
        // Before the port is handed out, because a pad that found no free port is
        // still a pad in the player's hands.
        self.controller_connected.set(true);
        let Some(port) = self.slots.connect(index) else {
            // A third controller. The console has two ports, so this one joins
            // the game as nothing at all - and saying so would only be noise.
            return;
        };
        self.seen[port] = PadSeen::default();
        self.toasts
            .push(ToastKind::Ok, gamepad::connect_toast(port, &pad.id()));
        if pad.mapping() != GamepadMappingType::Standard {
            self.toasts
                .push(ToastKind::Warn, gamepad::NON_STANDARD_TOAST);
        }
    }

    /// A pad has gone: let go of everything it was holding, and say so.
    ///
    /// No `buttonup` ever arrives for a pad that has been pulled out, so this is
    /// the only thing standing between an unplug and a game that walks into a
    /// wall for the rest of the session.
    fn disconnected(&mut self, pad: &Gamepad) {
        let index = pad_index(pad);
        // By gamepad index rather than by port: a pad that never got a slot has
        // nothing to report, but it must not keep a button either.
        crate::input::with_state(|input| input.clear_source(Source::Gamepad(index)));
        let port = self.slots.disconnect(index);
        self.controller_connected.set(self.slots.is_empty());
        let Some(port) = port else { return };
        self.seen[port] = PadSeen::default();
        self.toasts
            .push(ToastKind::Info, gamepad::disconnect_toast(port));
    }

    /// One pad's reading: move its port in [`crate::input`] to what it reports
    /// now, and return the hotkey it has just started asking for.
    fn poll_pad(&mut self, pad: &Reading) -> Option<Hotkey> {
        let port = self.slots.port_of(pad.index)?;
        let buttons = gamepad::poll_snapshot(&pad.buttons, &pad.axes, &self.bindings);
        let hotkey = gamepad::poll_hotkey(&pad.buttons);
        let seen = &mut self.seen[port];
        let previous = std::mem::take(&mut seen.buttons);
        crate::input::with_state(|input| {
            gamepad::apply_snapshot(input, Source::Gamepad(pad.index), port, &previous, &buttons);
        });
        seen.buttons = buttons;
        // Only the start of a press is reported, so a player holding the trigger
        // gets one hotkey rather than sixty a second.
        let last = std::mem::replace(&mut seen.hotkey, hotkey);
        if hotkey != last { hotkey } else { None }
    }
}

thread_local! {
    /// The page's pads, while a shell is up to be told about them.
    static WATCHER: RefCell<Option<Watcher>> = const { RefCell::new(None) };
}

/// Runs `f` against the page's pads, doing nothing at all when there are none to
/// speak of - which is the case until [`use_gamepad`] has mounted, and again once
/// it has been cleaned up.
fn with_watcher<T>(f: impl FnOnce(&mut Watcher) -> T) -> Option<T> {
    WATCHER.with(|watcher| watcher.borrow_mut().as_mut().map(f))
}

/// Listens for gamepads for as long as the component that calls this is mounted.
///
/// On the `window`, because a pad is not a document resource: it belongs to the
/// browser, and its events arrive with no interesting target.
///
/// `console`, `drawer_open` and `toasts` are the shell's, so that the two hotkeys
/// a pad asks for - `home` and the right trigger - reach the same places the
/// keyboard's hotkeys do (see [`crate::input::gamepad::poll_hotkey`]).
///
/// `controller_connected` says whether any pad is plugged in, which is the fact the
/// touch overlay hides itself behind (M7-09, #142). The browser only reports a pad
/// after the player has touched it, so an overlay that appears before that is not a
/// bug: it is the player's first chance to press anything.
pub fn use_gamepad(
    console: Controls,
    drawer_open: RwSignal<bool>,
    toasts: Toasts,
    controller_connected: RwSignal<bool>,
) {
    WATCHER.with(|watcher| {
        *watcher.borrow_mut() = Some(Watcher {
            slots: PadSlots::new(),
            bindings: Bindings::default(),
            seen: (0..crate::input::PORTS)
                .map(|_| PadSeen::default())
                .collect(),
            console,
            drawer_open,
            toasts,
            controller_connected,
        });
    });

    let connected = window_event_listener_untyped("gamepadconnected", move |event| {
        let Some(pad) = pad_of(&event) else { return };
        with_watcher(|watcher| watcher.connected(&pad));
    });

    let disconnected = window_event_listener_untyped("gamepaddisconnected", move |event| {
        let Some(pad) = pad_of(&event) else { return };
        with_watcher(|watcher| watcher.disconnected(&pad));
    });

    on_cleanup(move || {
        connected.remove();
        disconnected.remove();
        // The watcher holds the shell's signals, and this is the one chance to
        // let them go. With nothing installed [`poll_gamepads`] asks the browser
        // for nothing, which is the right answer for a page with no shell.
        WATCHER.with(|watcher| *watcher.borrow_mut() = None);
    });
}

/// Reads every pad the browser can see, once, into the page's input state, and
/// runs the hotkey a pad started asking for.
///
/// The Gamepad API is poll based: `get_gamepads()` is the only way to learn what
/// a pad is doing, and a pad is not reported at all until the player has pressed
/// something on the page ("Gamepad API", "Acquiring and processing events").
/// This is the animation frame's first move, before the running console is
/// borrowed and before it reads its ports (see [`crate::emulator`]).
pub fn poll_gamepads() {
    let asked = with_watcher(|watcher| {
        let mut asked = Vec::new();
        for reading in readings() {
            if let Some(hotkey) = watcher.poll_pad(&reading) {
                asked.push(hotkey);
            }
        }
        // The shell travels out with the hotkeys, so that they are run once the
        // watcher is put back: a hotkey writes signals, and a signal write is a
        // render.
        (asked, watcher.console, watcher.drawer_open, watcher.toasts)
    });
    let Some((asked, console, drawer_open, toasts)) = asked else {
        return;
    };
    for hotkey in asked {
        hotkey_do(hotkey, console, drawer_open, toasts);
    }
}

/// One pad, read once, with nothing browser-shaped left in it.
struct Reading {
    index: u8,
    buttons: Vec<bool>,
    axes: Vec<f64>,
}

/// Every connected pad, as plain data for [`gamepad`] to interpret.
///
/// `get_gamepads()` answers with a hole where a pad used to be, and with a pad
/// that says `connected() == false` where one has just gone, so this is the one
/// place that has to look: a pad that is not connected has no reading, and its
/// buttons were released by the `gamepaddisconnected` handler.
fn readings() -> Vec<Reading> {
    let Some(pads) = web_sys::window()
        .map(|window| window.navigator().get_gamepads())
        .and_then(|pads| pads.ok())
    else {
        return Vec::new();
    };
    pads.iter()
        .filter_map(|pad| pad.dyn_into::<Gamepad>().ok())
        .filter(|pad| pad.connected())
        .filter_map(|pad| {
            Some(Reading {
                index: u8::try_from(pad.index()).ok()?,
                // A hole in the button list is a button that is not held: `false`
                // is both what the hole is and what it means here.
                buttons: pad
                    .buttons()
                    .iter()
                    .map(|button| {
                        button
                            .dyn_into::<GamepadButton>()
                            .ok()
                            .is_some_and(|button| button.pressed())
                    })
                    .collect(),
                // And a hole in the axes is a stick that nobody is pushing, which
                // is 0.0 rather than whatever its neighbour says.
                axes: pad
                    .axes()
                    .iter()
                    .map(|axis| axis.as_f64().unwrap_or(0.0))
                    .collect(),
            })
        })
        .collect()
}

/// The pad a `gamepadconnected`/`gamepaddisconnected` is about.
fn pad_of(event: &web_sys::Event) -> Option<Gamepad> {
    event
        .dyn_ref::<GamepadEvent>()
        .and_then(|event| event.gamepad())
}

/// The Gamepad API index of `pad`, as the `u8` of
/// [`Source::Gamepad`](crate::input::Source::Gamepad).
///
/// The browser numbers pads from 0 and has never been observed past 3; a pad
/// numbered past a `u8` is not a pad this page can name, and the largest index is
/// one no browser hands out - so it is reported as a pad of nobody else's, which
/// releases nothing and claims nothing.
fn pad_index(pad: &Gamepad) -> u8 {
    u8::try_from(pad.index()).unwrap_or(u8::MAX)
}
