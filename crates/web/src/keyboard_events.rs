//! The keyboard's browser half: the `window` listeners that turn a key event
//! into a [`crate::input`] call, and the shell action a hotkey asks for
//! (M7-07, #140).
//!
//! The policy is elsewhere and pure - [`crate::input::keyboard`] decides what a
//! `code` means, whether an element has taken the key for itself, and whether
//! the browser's own action is cancelled, and [`crate::nav`] decides what a key
//! means to an open menu. This file is the part that cannot be pure: it holds a
//! `KeyboardEvent`, reads its `code` and its target, and writes to the page's
//! [`crate::input`], to [`crate::menu`] and to the shell's signals.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::controls::CaptureDevice;
use crate::controls::CaptureTarget;
use crate::controls::bind_key;
use crate::emulator::Slots;
use crate::input::Source;
use crate::input::keyboard::CaptureKey;
use crate::input::keyboard::Hotkey;
use crate::input::keyboard::KeyAction;
use crate::input::keyboard::capture_key;
use crate::input::keyboard::element_takes_key;
use crate::input::keyboard::map_key;
use crate::input::keyboard::should_prevent_default;
use crate::menu::Menu;
use crate::menu::slider_has_focus;
use crate::nav::Dir;
use crate::nav::LastDevice;
use crate::nav::UiAction;
use crate::nav::UiEvent;
use crate::nav::UiMode;
use crate::nav::ui_action;
use crate::screen::Screen;
use crate::settings::Settings;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// What a player is told when they try to bind one of the shell's fixed keys.
///
/// [`map_key`] gives a hotkey its key whatever the bindings say, so the binding
/// would be dead the moment it was written; saying so is cheaper than a row in
/// the table that does nothing (M7-10, #143).
const REFUSED_KEY: &str =
    "That key stays the shell's: Escape, Tab, `, F1, F2, F3, F4, F11 and P are fixed.";

/// Listens to the keyboard for as long as the component that calls this is
/// mounted: `keydown` and `keyup` to move the pad, and `blur` to let go of it.
///
/// On the `window`, not on the canvas: a player aims at the game rather than at
/// the canvas element, so focus may be on the top bar or nowhere at all and the
/// pad has to answer either way. The canvas could not be the listener for a
/// second reason either - it is unmounted and remounted around the empty screen
/// (`app.rs`), and a key held across that change would be held by nothing.
///
/// `settings` is the store the controls page writes, read on every event so a
/// rebind takes effect on the next keypress rather than the next page load;
/// `capture` is the cell of that page waiting for a key, if there is one, and it
/// outranks everything below it. `playing` is whether a cartridge is loaded,
/// which decides how much of the page's keyboard the game gets - see
/// [`should_prevent_default`]. `menu` is the shell's menu mode, which owns the
/// keyboard outright while a menu is open (M7-11, #144).
pub fn use_keyboard(
    menu: Menu,
    playing: Signal<bool>,
    toasts: Toasts,
    settings: RwSignal<Settings>,
    capture: RwSignal<Option<CaptureTarget>>,
    screen: Screen,
    slots: Slots,
) {
    let pressed = {
        window_event_listener(ev::keydown, move |event: web_sys::KeyboardEvent| {
            let code = event.code();
            // A cell of the controls page is capturing, so it gets the key ahead
            // of the hotkeys - `Escape` cancels the rebind rather than shutting
            // the drawer, and a key that is only a hotkey is refused rather than
            // bound to something it can never drive. Before the focus rule,
            // because a click on a cell leaves the focus wherever it was.
            if let Some(target) = capture.get() {
                match capture_key(&code) {
                    CaptureKey::Cancel => {
                        event.prevent_default();
                        capture.set(None);
                    }
                    CaptureKey::Bind { code } => {
                        // `Tab` would move the focus out of the drawer mid-rebind
                        // and an arrow key would scroll it, so the browser is
                        // told nothing about a key the player is spending here.
                        event.prevent_default();
                        if target.device == CaptureDevice::Keyboard {
                            bind_key(target, &code, settings, toasts);
                            capture.set(None);
                        }
                    }
                    CaptureKey::Refused => toasts.push(ToastKind::Warn, REFUSED_KEY),
                }
                return;
            }
            if takes_key_for_itself(&event) {
                // A key aimed at a text entry or a `select` is that element's
                // business, so the pad never hears of it. Saying nothing here is
                // also what keeps the pad clean: a press reported from here
                // would be released by the `keyup` below, which ignores focus,
                // but the browser would have spent the key on the field as well
                // - which is a character the player did not want.
                return;
            }
            let action = settings.with(|settings| map_key(&code, &settings.bindings));
            if menu.mode.get_untracked() == UiMode::Menu {
                // A menu is open, so the keyboard belongs to the menu (M7-11,
                // #144): the arrows move the focus and Enter or Space activates
                // what it landed on, and a key bound to a pad button is not game
                // input while a menu is on screen - which is the acceptance
                // criterion, not an optimisation.
                if let Some(action) = ui_action(&UiEvent::Key(&code)) {
                    // Left and right on a slider are the exception, because that
                    // is what those two keys are for on one and the browser moves
                    // the value itself. Up and down are not: they would move the
                    // value too, and the walk wants them.
                    let slider_arrows = slider_has_focus()
                        && matches!(action, UiAction::Move(Dir::Left | Dir::Right));
                    if !slider_arrows {
                        // And the browser is told not to activate the focused
                        // button as well, or a Space would press it twice.
                        event.prevent_default();
                        menu.announce(LastDevice::Keyboard);
                        menu.run(action);
                    }
                    return;
                }
                // Everything else the menu does not want is the shell's or
                // nobody's: `Tab` in particular, which the browser still uses to
                // move focus, and `F1` and friends still reach the shell.
                if let KeyAction::Hotkey(hotkey) = action {
                    hotkey_do(hotkey, menu, toasts, screen, slots, settings);
                }
                return;
            }
            if should_prevent_default(&action, playing.get(), menu.drawer_open.get()) {
                event.prevent_default();
            }
            match action {
                KeyAction::Pad { port, button } => crate::input::with_state(|input| {
                    input.set(Source::Keyboard, port, button, true);
                }),
                KeyAction::Hotkey(hotkey) => {
                    hotkey_do(hotkey, menu, toasts, screen, slots, settings)
                }
                KeyAction::None => {}
            }
        })
    };

    let released = {
        window_event_listener(ev::keyup, move |event: web_sys::KeyboardEvent| {
            // No focus rule and no `preventDefault` here: a release always
            // belongs to the game. A direction held on the way into a field -
            // the player clicked the theme list without letting go of
            // `ArrowDown` - has to stop when the key does, or the game walks on
            // for the rest of the session.
            let action = settings.with(|settings| map_key(&event.code(), &settings.bindings));
            match action {
                KeyAction::Pad { port, button } => crate::input::with_state(|input| {
                    input.set(Source::Keyboard, port, button, false);
                }),
                // Letting go is the act, for the one hotkey that is held rather than
                // tapped: the console goes back to its own rate, with the audio
                // schedule started again from the play head (M7-17, #150). The
                // release ignores the menu and the focus for the same reason the pad
                // release does - a keyup that never arrives is a console stuck at 4x.
                KeyAction::Hotkey(Hotkey::FastForward) => menu.console.set_fast_forward(false),
                // Every other hotkey is one act per keydown and has nothing to undo.
                KeyAction::Hotkey(_) | KeyAction::None => {}
            }
        })
    };

    // Alt-tabbing away swallows the `keyup` of every key still held: the window
    // has lost focus, so nothing arrives when it comes back, and a player who
    // pressed `Right` on the way out is running right until they press it again.
    let blurred = window_event_listener_untyped("blur", move |_event: web_sys::Event| {
        crate::input::with_state(|input| input.clear_source(Source::Keyboard));
    });

    on_cleanup(move || {
        pressed.remove();
        released.remove();
        blurred.remove();
    });
}

/// Whether the element this key landed on wants the key for itself: the
/// `KeyboardEvent` side of [`element_takes_key`], which wants a tag name and an
/// `input` type rather than an event.
fn takes_key_for_itself(event: &web_sys::KeyboardEvent) -> bool {
    let Some(target) = event.target() else {
        return false;
    };
    let input_type = target
        .dyn_ref::<web_sys::HtmlInputElement>()
        .map(|input| input.type_());
    let Some(element) = target.dyn_ref::<web_sys::Element>() else {
        return false;
    };
    element_takes_key(&element.tag_name(), input_type.as_deref())
}

/// Does what a hotkey asks for.
///
/// Every one of them is real in this build now: fullscreen is #145, the four save
/// state keys are #148, and fast forward and the overlay are #150. A hotkey the
/// shell ever adds without a feature behind it should report itself the way the top
/// bar reports the features it has not got, rather than do nothing.
///
/// `pub(crate)` because a pad asks for these too - see
/// [`crate::gamepad_events`] - and a hotkey means the same thing whoever
/// reported it. That sharing is what keeps a pad's `home` and the keyboard's
/// `Escape` from both toggling the menu on the way into it (M7-11, #144): one of
/// them owns the act, and it is this function while a game is running.
///
/// `ToggleMenu` and `Pause` go through the [`Menu`] rather than straight at the
/// drawer and the console, because inside a menu the first means "close the
/// innermost thing" and either one changes who is holding the console.
///
/// `settings` is for the one hotkey that is a setting with a key: the overlay
/// switch in the drawer and the `` ` `` key are the same write, so they cannot
/// disagree about whether the overlay is on (M7-17, #150).
pub(crate) fn hotkey_do(
    hotkey: Hotkey,
    menu: Menu,
    toasts: Toasts,
    screen: Screen,
    slots: Slots,
    settings: RwSignal<Settings>,
) {
    match hotkey {
        Hotkey::Pause => menu.pause_by_hand(),
        Hotkey::ToggleMenu => menu.toggle_menu(),
        // The shell goes fullscreen, not the canvas: the top bar has to be inside
        // what is fullscreen for it to hide and then come back on mouse move, and
        // `Screen` is the one owner of that state (#145).
        Hotkey::Fullscreen => screen.toggle_fullscreen(),
        // The three that are real as of #148. A save and a load do not announce
        // themselves here: the console has the only say in whether either
        // happened, and it reports through [`crate::emulator`]'s own toast -
        // including the one sentence of why a load did not (M7-15, #148).
        Hotkey::SaveState => slots.save_here(),
        Hotkey::LoadState => slots.load_here(),
        Hotkey::PreviousSlot => slots.step(-1, toasts),
        Hotkey::NextSlot => slots.step(1, toasts),
        // A setting, because the drawer shows it and a key that disagreed with the
        // drawer would be a second answer to whether the overlay is on.
        Hotkey::ToggleFps => {
            settings.update(|settings| settings.fps_overlay = !settings.fps_overlay);
        }
        // Held rather than tapped, so this is half the pair: the other half is the
        // `keyup` above, and a pad's release is [`HotkeyEvent::Released`] over in
        // [`crate::gamepad_events`] (M7-17, #150).
        Hotkey::FastForward => menu.console.set_fast_forward(true),
    }
}
