//! The keyboard's browser half: the `window` listeners that turn a key event
//! into a [`crate::input`] call, and the shell action a hotkey asks for
//! (M7-07, #140).
//!
//! The policy is elsewhere and pure - [`crate::input::keyboard`] decides what a
//! `code` means, whether an element has taken the key for itself, and whether
//! the browser's own action is cancelled. This file is the part that cannot be
//! pure: it holds a `KeyboardEvent`, reads its `code` and its target, and writes
//! to the page's [`crate::input`] and to the shell's signals.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::emulator::Controls;
use crate::input::Bindings;
use crate::input::Source;
use crate::input::keyboard::Hotkey;
use crate::input::keyboard::KeyAction;
use crate::input::keyboard::element_takes_key;
use crate::input::keyboard::map_key;
use crate::input::keyboard::should_prevent_default;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// Listens to the keyboard for as long as the component that calls this is
/// mounted: `keydown` and `keyup` to move the pad, and `blur` to let go of it.
///
/// On the `window`, not on the canvas: a player aims at the game rather than at
/// the canvas element, so focus may be on the top bar or nowhere at all and the
/// pad has to answer either way. The canvas could not be the listener for a
/// second reason either - it is unmounted and remounted around the empty screen
/// (`app.rs`), and a key held across that change would be held by nothing.
///
/// `bindings` is [`Bindings::default`] until the controls page (M7-10, #143)
/// stores a table of the player's own; `playing` is whether a cartridge is
/// loaded, which decides how much of the page's keyboard the game gets - see
/// [`should_prevent_default`].
pub fn use_keyboard(
    console: Controls,
    drawer_open: RwSignal<bool>,
    playing: Signal<bool>,
    toasts: Toasts,
) {
    // One table, read on every event and written by nobody until M7-10 (#143).
    let bindings = Bindings::default();

    let pressed = {
        let bindings = bindings.clone();
        window_event_listener(ev::keydown, move |event: web_sys::KeyboardEvent| {
            let action = map_key(&event.code(), &bindings);
            if takes_key_for_itself(&event) {
                // A key aimed at a text entry or a `select` is that element's
                // business, so the pad never hears of it. Saying nothing here is
                // also what keeps the pad clean: a press reported from here
                // would be released by the `keyup` below, which ignores focus,
                // but the browser would have spent the key on the field as well
                // - which is a character the player did not want.
                return;
            }
            if should_prevent_default(&action, playing.get(), drawer_open.get()) {
                event.prevent_default();
            }
            match action {
                KeyAction::Pad { port, button } => crate::input::with_state(|input| {
                    input.set(Source::Keyboard, port, button, true);
                }),
                KeyAction::Hotkey(hotkey) => hotkey_do(hotkey, console, drawer_open, toasts),
                KeyAction::None => {}
            }
        })
    };

    let released = {
        let bindings = bindings.clone();
        window_event_listener(ev::keyup, move |event: web_sys::KeyboardEvent| {
            // No focus rule and no `preventDefault` here: a release always
            // belongs to the game. A direction held on the way into a field -
            // the player clicked the theme list without letting go of
            // `ArrowDown` - has to stop when the key does, or the game walks on
            // for the rest of the session.
            if let KeyAction::Pad { port, button } = map_key(&event.code(), &bindings) {
                crate::input::with_state(|input| input.set(Source::Keyboard, port, button, false));
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
/// Two of the six are real in this build. The others report themselves the way
/// the top bar reports the features it has not got either: the key is mapped
/// already, so the ticket that builds the feature has somewhere to put it
/// (Fullscreen M7-12 #145, save states M7-15 #148, fast forward M7-17 #150).
///
/// `pub(crate)` because a pad asks for these too - see
/// [`crate::gamepad_events`] - and a hotkey means the same thing whoever
/// reported it.
pub(crate) fn hotkey_do(
    hotkey: Hotkey,
    console: Controls,
    drawer_open: RwSignal<bool>,
    toasts: Toasts,
) {
    match hotkey {
        Hotkey::Pause => console.toggle(),
        Hotkey::ToggleMenu => drawer_open.update(|open| *open = !*open),
        Hotkey::Fullscreen => toasts.push(
            ToastKind::Info,
            "Fullscreen is not wired up in this build yet.",
        ),
        Hotkey::SaveState => toasts.push(
            ToastKind::Info,
            "Save states are not wired up in this build yet.",
        ),
        Hotkey::LoadState => toasts.push(
            ToastKind::Info,
            "A load key is not wired up in this build yet.",
        ),
        Hotkey::FastForward => toasts.push(
            ToastKind::Info,
            "Fast forward is not wired up in this build yet.",
        ),
    }
}
