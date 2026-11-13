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

use crate::controls::CaptureDevice;
use crate::controls::CaptureTarget;
use crate::controls::bind_pad;
use crate::emulator::Slots;
use crate::input::PadButton;
use crate::input::Source;
use crate::input::gamepad;
use crate::input::gamepad::HotkeyEvent;
use crate::input::gamepad::PadSlots;
use crate::input::keyboard::Hotkey;
use crate::keyboard_events::hotkey_do;
use crate::menu::Menu;
use crate::nav::LastDevice;
use crate::nav::RepeatTimer;
use crate::nav::UiAction;
use crate::nav::UiMode;
use crate::nav::fresh_actions;
use crate::nav::pad_actions;
use crate::screen::Screen;
use crate::settings::Settings;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// The baseline a Controller-cell capture is measured against: the capture it
/// belongs to, and every pad's held button indices as of the last frame.
type CaptureBaseline = (CaptureTarget, Vec<(u8, Vec<u8>)>);

/// One port's pad, as the last animation frame read it.
#[derive(Default)]
struct PadSeen {
    /// The Genesis buttons it was holding, in [`PadButton::ALL`] order.
    buttons: Vec<PadButton>,
    /// The hotkey it was asking for, so that a held trigger is one press and one
    /// release rather than one of either per frame (M7-17, #150).
    hotkey: Option<Hotkey>,
}

/// The page's pads: which port each drives, what each was last read as holding,
/// and the shell a pad can reach through a hotkey.
struct Watcher {
    slots: PadSlots,
    /// The store the controls page writes, read on every reading so a rebind
    /// lands on the next frame - the same arrangement as the keyboard's in
    /// [`crate::keyboard_events`].
    settings: RwSignal<Settings>,
    /// The cell of the controls page waiting for a pad button, if there is one.
    capture: RwSignal<Option<CaptureTarget>>,
    /// Fullscreen, for `F11`: the same owner the top bar's button uses, so the
    /// pad cannot disagree with the keyboard about what the shell is doing (#145).
    screen: Screen,
    /// The save state slots, for `F1` and friends - the same handle the keyboard
    /// is given, because a hotkey has one owner whoever pressed it (M7-15, #148).
    state_slots: Slots,
    /// What the pads were asking the menu for as of the last frame, while a menu
    /// was open: the same difference trick as `PadSeen::buttons`, so that a press
    /// is reported once (M7-11, #144).
    ///
    /// Held once for the page rather than once per port, because the focus the walk
    /// moves is one thing for the page: a second pad driving it shares its timing
    /// rather than queueing a second move behind the first.
    menu_seen: Vec<UiAction>,
    /// How a direction held on a d-pad or a stick walks the focus, which needs the
    /// frame clock because a polled pad has no key repeat.
    menu_repeat: RepeatTimer,
    /// Each pad's held button indices as of the last frame, kept only while a
    /// Controller cell is capturing, and paired with the capture they belong to
    /// so that arming a new cell throws the old baseline away. See
    /// [`gamepad::newly_pressed`] for why the capture wants the edge.
    capture_baseline: Option<CaptureBaseline>,
    /// Indexed by port, and reset whenever a pad takes a slot, so a pad cannot
    /// inherit the held buttons of the pad that left it.
    seen: Vec<PadSeen>,
    /// The shell's menu mode, read once per frame to decide which half of the pad
    /// this is: a pad drives the game, or a pad drives the menu, and never both
    /// (M7-11, #144).
    menu: Menu,
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

    /// One frame of one pad spent on the menu instead of on the game (M7-11, #144).
    ///
    /// The pad's Genesis buttons are read and recorded but never sent: `InputState`
    /// belongs to the game, and while a menu is open the game is not supposed to
    /// hear the pad at all. Recording them anyway is what stops a direction held
    /// into the menu from being a press on the way out - the poll reports
    /// differences, and as far as this frame is concerned nothing changed.
    ///
    /// A rebind cell keeps the pad to itself while it is waiting, exactly as the
    /// keyboard's does in [`crate::keyboard_events`]: the button a player is
    /// binding is the button the menu must not spend on moving its focus.
    fn poll_menu(&mut self, pad: &Reading, now_ms: f64) -> Vec<UiAction> {
        // Before the port is looked up, because a pad that never got one is still a
        // pad in the player's hands and still finishes a rebind.
        self.capture_press(pad);
        if let Some(port) = self.slots.port_of(pad.index) {
            let held = self.settings.with_untracked(|settings| {
                gamepad::poll_snapshot(&pad.buttons, &pad.axes, &settings.bindings)
            });
            self.seen[port].buttons = held;
        }
        if self.capture.get().is_some() {
            self.menu_repeat.stop();
            self.menu_seen = Vec::new();
            return Vec::new();
        }
        let current = pad_actions(&pad.buttons, &pad.axes);
        let mut actions = fresh_actions(&self.menu_seen, &current);
        self.menu_seen = current.clone();
        // A direction the player is holding walks the focus at its own pace: the
        // first move is the fresh one above and the rest come from the timer, which
        // a key does not need because the browser repeats the keydown for it.
        let held_dir = current.iter().rev().find_map(|action| match action {
            UiAction::Move(dir) => Some(*dir),
            _ => None,
        });
        let moved = actions
            .iter()
            .any(|action| matches!(action, UiAction::Move(_)));
        match held_dir {
            Some(_) if moved => self.menu_repeat.start(now_ms),
            Some(dir) => {
                if self.menu_repeat.poll(now_ms) {
                    actions.push(UiAction::Move(dir));
                }
            }
            None => self.menu_repeat.stop(),
        }
        actions
    }

    /// One pad's reading in play: move its port in [`crate::input`] to what it
    /// reports now, and return the hotkey event this reading is - the start of a
    /// hold, or the end of one.
    ///
    /// The menu is not asked about here: while a game is running this is the only
    /// thing that reads the pads, and [`gamepad::poll_hotkey`] is the owner of
    /// `home` and `select` + `start` - see [`poll_gamepads`] for how the two halves
    /// avoid a double toggle (M7-11, #144).
    fn poll_pad(&mut self, pad: &Reading) -> Option<HotkeyEvent> {
        // Before the port is looked up: the shared gamepad map is keyed by button
        // index rather than by pad, so whichever pad just pressed a button is the
        // one that finishes the capture (M7-10, #143).
        self.capture_press(pad);
        let port = self.slots.port_of(pad.index)?;
        let buttons = self.settings.with_untracked(|settings| {
            gamepad::poll_snapshot(&pad.buttons, &pad.axes, &settings.bindings)
        });
        let hotkey = gamepad::poll_hotkey(&pad.buttons);
        let seen = &mut self.seen[port];
        let previous = std::mem::take(&mut seen.buttons);
        crate::input::with_state(|input| {
            gamepad::apply_snapshot(input, Source::Gamepad(pad.index), port, &previous, &buttons);
        });
        seen.buttons = buttons;
        // Only a change is reported: a player holding the trigger gets one press
        // rather than sixty a second, and the frame that let go of it gets the
        // release - which is the only way a pad's fast forward ever ends, a poll
        // having no keyup of its own (M7-17, #150).
        let last = std::mem::replace(&mut seen.hotkey, hotkey);
        gamepad::hotkey_change(last, hotkey)
    }

    /// One frame of one pad, spent on the controls page's capture: bind the first
    /// button this pad newly pressed and stop waiting.
    ///
    /// Does nothing at all - and allocates nothing - while no cell is waiting,
    /// which is every frame of a normal game.
    fn capture_press(&mut self, pad: &Reading) {
        let Some(target) = self.capture_target() else {
            self.capture_baseline = None;
            return;
        };
        let held = held_indices(&pad.buttons);
        let previous = self
            .capture_baseline
            .as_ref()
            .filter(|(captured, _)| *captured == target)
            .and_then(|(_, pads)| pads.iter().find(|(index, _)| *index == pad.index))
            .map(|(_, held)| held.clone());
        self.set_capture_baseline(target, pad.index, held.clone());
        let Some(index) = gamepad::newly_pressed(&held, previous.as_deref()) else {
            return;
        };
        bind_pad(target, index, self.settings, self.toasts);
        self.capture.set(None);
    }

    /// The pad button the controls page is waiting for, `None` when it is waiting
    /// for a key or for nothing.
    fn capture_target(&self) -> Option<CaptureTarget> {
        self.capture
            .get()
            .filter(|target| target.device == CaptureDevice::Gamepad)
    }

    /// Records this pad's held buttons as the baseline of `target` for the next
    /// frame, keeping the other pads' readings and starting a fresh table when
    /// the capture itself changed.
    fn set_capture_baseline(&mut self, target: CaptureTarget, index: u8, held: Vec<u8>) {
        let mut pads = self
            .capture_baseline
            .take()
            .filter(|(captured, _)| *captured == target)
            .map(|(_, pads)| pads)
            .unwrap_or_default();
        match pads.iter_mut().find(|(seen, _)| *seen == index) {
            Some(entry) => entry.1 = held,
            None => pads.push((index, held)),
        }
        self.capture_baseline = Some((target, pads));
    }
}

/// The pressed button indices of one pad, for [`gamepad::newly_pressed`].
fn held_indices(buttons: &[bool]) -> Vec<u8> {
    buttons
        .iter()
        .enumerate()
        .filter(|(_, pressed)| **pressed)
        .map(|(index, _)| u8::try_from(index).unwrap_or(u8::MAX))
        .collect()
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
/// `settings` is the store the controls page writes, read on every reading;
/// `capture` is the cell of that page waiting for a pad button, if there is one.
///
/// `menu` and `toasts` are the shell's, so that the two hotkeys a pad asks for -
/// `home` and the right trigger - reach the same places the keyboard's hotkeys do
/// (see [`crate::input::gamepad::poll_hotkey`]), and so that a pad can drive the
/// menu itself while one is open (M7-11, #144).
///
/// `controller_connected` says whether any pad is plugged in, which is the fact the
/// touch overlay hides itself behind (M7-09, #142). The browser only reports a pad
/// after the player has touched it, so an overlay that appears before that is not a
/// bug: it is the player's first chance to press anything.
pub fn use_gamepad(
    menu: Menu,
    toasts: Toasts,
    controller_connected: RwSignal<bool>,
    settings: RwSignal<Settings>,
    capture: RwSignal<Option<CaptureTarget>>,
    screen: Screen,
    state_slots: Slots,
) {
    WATCHER.with(|watcher| {
        *watcher.borrow_mut() = Some(Watcher {
            slots: PadSlots::new(),
            settings,
            capture,
            screen,
            state_slots,
            capture_baseline: None,
            menu_seen: Vec::new(),
            menu_repeat: RepeatTimer::default(),
            seen: (0..crate::input::PORTS)
                .map(|_| PadSeen::default())
                .collect(),
            menu,
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

/// Reads every pad the browser can see, once, and gives the frame's answer to
/// whichever half of the shell is listening.
///
/// In play that means the page's input state and the hotkey events this frame's
/// readings are: the start of a hold, and the end of one - which is the only way a
/// pad's fast forward ever stops (M7-17, #150). With a menu open it means
/// [`UiAction`](crate::nav::UiAction)s instead, and nothing at all reaches the game
/// (M7-11, #144).
///
/// `now_ms` is the animation frame's own timestamp, which is what a held
/// direction is timed against - see [`RepeatTimer`](crate::nav::RepeatTimer).
///
/// The Gamepad API is poll based: `get_gamepads()` is the only way to learn what
/// a pad is doing, and a pad is not reported at all until the player has pressed
/// something on the page ("Gamepad API", "Acquiring and processing events").
/// This is the animation frame's first move, before the running console is
/// borrowed and before it reads its ports (see [`crate::emulator`]).
pub fn poll_gamepads(now_ms: f64) {
    let asked = with_watcher(|watcher| {
        let mut hotkeys = Vec::new();
        let mut actions = Vec::new();
        // One mode per frame, and one owner per button: while a menu is open the
        // pad belongs to [`Menu`](crate::menu) and `poll_hotkey` is not asked at
        // all, because the menu's own `home` and `select` + `start` rule is
        // [`nav::pad_actions`] and running both would be a drawer that opens and
        // shuts itself in the same frame (M7-11, #144).
        let mode = watcher.menu.mode.get_untracked();
        for reading in readings() {
            match mode {
                UiMode::Playing => hotkeys.extend(watcher.poll_pad(&reading)),
                UiMode::Menu => actions.extend(watcher.poll_menu(&reading, now_ms)),
            }
        }
        // The shell travels out with the answers, so that they are dealt with once
        // the watcher is put back: a hotkey and a menu action both write signals,
        // and a signal write is a render - which is the one thing that must not
        // happen while the watcher is borrowed, for the reason
        // [`crate::emulator`] spells out about the console.
        (
            hotkeys,
            actions,
            watcher.menu,
            watcher.toasts,
            watcher.screen,
            watcher.state_slots,
            watcher.settings,
        )
    });
    let Some((hotkeys, actions, menu, toasts, screen, state_slots, settings)) = asked else {
        return;
    };
    if !actions.is_empty() {
        // The pad that got this far is the pad the player is holding, which is
        // what the hint bar wants to hear.
        menu.announce(LastDevice::Gamepad);
    }
    for event in hotkeys {
        match event {
            HotkeyEvent::Pressed(hotkey) => {
                hotkey_do(hotkey, menu, toasts, screen, state_slots, settings)
            }
            // The end of a hold. Fast forward is the only hotkey a pad holds, so
            // every other release has nothing to undo (M7-17, #150).
            HotkeyEvent::Released(Hotkey::FastForward) => menu.console.set_fast_forward(false),
            HotkeyEvent::Released(_) => {}
        }
    }
    for action in actions {
        menu.run(action);
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
