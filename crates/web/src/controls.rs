//! The Controls section of the settings drawer, and the capture the shell feeds
//! back into it (M7-10, #143).
//!
//! One table per player: rows are the twelve Genesis buttons, the two columns are
//! the keyboard key and the controller button bound to them. A cell is a button;
//! clicking it starts a capture, and the capture is finished by
//! [`crate::keyboard_events`] or [`crate::gamepad_events`] - the two places a key
//! or a pad button can actually arrive. They are given [`CaptureTarget`] by the
//! `RwSignal<Option<CaptureTarget>>` context provided by `App`, which is also what
//! tells them to stand down from the shell's own hotkeys for the duration (see
//! [`crate::input::keyboard::capture_key`] for why that ordering matters).
//!
//! The pad type select is remembered but not yet acted on: `Genesis` takes a
//! 3-button pad on both ports until #364 gives it a 6-button setter.

use leptos::prelude::*;

use crate::input::PORTS;
use crate::input::PadButton;
use crate::input::SocdMode;
use crate::settings::PadType;
use crate::settings::Settings;
use crate::settings::TouchOverlayMode;
use crate::settings::gamepad_button_label;
use crate::settings::keycap_label;
use crate::ui::button::Button;
use crate::ui::select::Select;
use crate::ui::select::options;
use crate::ui::select::position;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;
use crate::ui::toggle::Toggle;

/// Which device a capturing cell is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDevice {
    /// The keyboard column: the next `keydown` finishes it.
    Keyboard,
    /// The controller column: the next pad button press finishes it.
    Gamepad,
}

impl CaptureDevice {
    /// What a cell says while it waits, so the player knows which hand to move.
    fn waiting(self) -> &'static str {
        match self {
            Self::Keyboard => "Press a key...",
            Self::Gamepad => "Press a button...",
        }
    }
}

/// The table cell that is capturing, when one is.
///
/// Kept on the shell rather than on the cell because neither the keydown nor the
/// pad poll arrives at the element that was clicked: the window listener and the
/// animation frame have to be told whose the next one is. `Copy` because it
/// travels into the poll with every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureTarget {
    /// Which column of the row is capturing.
    pub device: CaptureDevice,
    /// Which player's table it is in.
    pub port: usize,
    /// Which Genesis button the row is.
    pub button: PadButton,
}

/// Binds the key a cell was capturing to, and says what moved.
///
/// "One key, one action" is otherwise invisible: binding `B` to the key `A` had
/// leaves `A` with nothing, and a player who finds their d-pad gone ten seconds
/// later is not going to remember which cell caused it.
pub fn bind_key(target: CaptureTarget, code: &str, settings: RwSignal<Settings>, toasts: Toasts) {
    // `update` writes and returns nothing, so what was displaced is carried out
    // in a variable of its own.
    let mut displaced = None;
    settings.update(|settings| {
        displaced = settings
            .bindings
            .rebind_key(target.port, target.button, code);
    });
    let text = match displaced {
        Some(displaced) => format!(
            "Player {} - {} is now {}. Player {} - {} is unbound.",
            target.port + 1,
            target.button.label(),
            keycap_label(code),
            displaced.port + 1,
            displaced.button.label()
        ),
        None => format!(
            "Player {} - {} is now {}.",
            target.port + 1,
            target.button.label(),
            keycap_label(code)
        ),
    };
    toasts.push(ToastKind::Ok, text);
}

/// Binds the pad button a cell was capturing to, as [`bind_key`] does for a key.
pub fn bind_pad(target: CaptureTarget, index: u8, settings: RwSignal<Settings>, toasts: Toasts) {
    let mut displaced = None;
    settings.update(|settings| {
        displaced = settings.bindings.rebind_gamepad(target.button, index);
    });
    let text = match displaced {
        Some(displaced) => format!(
            "Player {} - {} is now {}. {} is unbound.",
            target.port + 1,
            target.button.label(),
            gamepad_button_label(index),
            displaced.label()
        ),
        None => format!(
            "Player {} - {} is now {}.",
            target.port + 1,
            target.button.label(),
            gamepad_button_label(index)
        ),
    };
    toasts.push(ToastKind::Ok, text);
}

/// One cell of a player's table: the key or pad button bound to this button, as a
/// button that starts capturing the next one when clicked.
#[component]
fn BindingCell(
    /// Which of the two columns this cell is.
    device: CaptureDevice,
    /// The player whose table this is in.
    port: usize,
    /// The Genesis button of the row.
    button: PadButton,
) -> impl IntoView {
    let settings = expect_context::<RwSignal<Settings>>();
    let capture = expect_context::<RwSignal<Option<CaptureTarget>>>();
    let target = CaptureTarget {
        device,
        port,
        button,
    };
    let capturing = Signal::derive(move || capture.get() == Some(target));
    // `capturing` is read through `capture` directly rather than through a
    // derived `Signal<bool>` in the class below, because the two reads have to
    // agree and one source is easier to keep true than two.
    let label = Signal::derive(move || {
        if capturing.get() {
            return device.waiting().to_string();
        }
        match device {
            CaptureDevice::Keyboard => settings
                .with(|settings| settings.bindings.key_of(port, button).map(keycap_label))
                .unwrap_or_else(|| UNBOUND.to_string()),
            CaptureDevice::Gamepad => settings
                .with(|settings| {
                    settings
                        .bindings
                        .gamepad_of(button)
                        .map(gamepad_button_label)
                })
                .unwrap_or_else(|| UNBOUND.to_string()),
        }
    });

    view! {
        <td>
            <button
                type="button"
                class="keycap"
                class:keycap--capturing=capturing
                aria-label=format!("{button:?} {device_word}", device_word = match device {
                    CaptureDevice::Keyboard => "keyboard",
                    CaptureDevice::Gamepad => "controller",
                })
                on:click=move |_| {
                    // A second click on the cell that is already waiting gives up,
                    // which is the other half of "Escape cancels" for a mouse.
                    capture.update(|current| {
                        *current = if *current == Some(target) { None } else { Some(target) }
                    });
                }
            >
                {label}
            </button>
        </td>
    }
}

/// What a cell shows for a button with nothing bound to it.
const UNBOUND: &str = "-";

/// The Controls section: a table per player, the pad type and d-pad rules, and
/// the on-screen controller.
///
/// Mounted inside [`crate::ui::drawer::Drawer`], and it reads the settings store
/// and the capture from context, so nothing above it has to carry either down.
#[component]
pub fn Controls(
    /// Whether the drawer is open, which a capture cannot outlive: the close
    /// button and the backdrop are not key presses, and a capture the player
    /// cannot see would go on eating the shell's keys.
    drawer_open: RwSignal<bool>,
) -> impl IntoView {
    let settings = expect_context::<RwSignal<Settings>>();
    let capture = expect_context::<RwSignal<Option<CaptureTarget>>>();
    let toasts = expect_context::<Toasts>();

    Effect::new(move |_| {
        if !drawer_open.get() {
            capture.set(None);
        }
    });

    let pad_options = options(&PadType::ALL, PadType::label);
    let socd_options = options(&SocdMode::ALL, SocdMode::label);
    let touch_options = options(&TouchOverlayMode::ALL, TouchOverlayMode::label);

    let players = (0..PORTS).map(|port| {
        let pad_selected = Signal::derive(move || position(&PadType::ALL, settings.get().pad_type[port]));
        let reset = Callback::new(move |()| {
            settings.update(|settings| settings.bindings.reset_port(port));
            toasts.push(
                ToastKind::Info,
                format!("Player {} controls are back to their defaults.", port + 1),
            );
        });
        let set_pad = Callback::new(move |value: String| {
            let index: usize = value.parse().unwrap_or(0);
            if let Some(pad_type) = PadType::ALL.get(index) {
                settings.update(|settings| settings.pad_type[port] = *pad_type);
            }
        });
        view! {
            <div class="binding-player">
                <h4 class="binding-player__title">{format!("Player {}", port + 1)}</h4>
                <table class="bindings">
                    <thead>
                        <tr>
                            <th scope="col">"Button"</th>
                            <th scope="col">"Keyboard"</th>
                            <th scope="col">"Controller"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {PadButton::ALL
                            .iter()
                            .map(|button| {
                                view! {
                                    <tr>
                                        <th scope="row">{button.label()}</th>
                                        <BindingCell
                                            device=CaptureDevice::Keyboard
                                            port=port
                                            button=*button
                                        />
                                        <BindingCell
                                            device=CaptureDevice::Gamepad
                                            port=port
                                            button=*button
                                        />
                                    </tr>
                                }
                            })
                            .collect_view()}
                    </tbody>
                </table>
                <Select
                    label="Pad type".to_string()
                    options=pad_options.clone()
                    selected=pad_selected
                    on_change=set_pad
                />
                <p class="settings-section__note">
                    "The pad type is remembered; the core has a 3-button pad on both ports until #364 gives it a 6-button setter."
                </p>
                <Button on_click=reset>"Reset to defaults"</Button>
            </div>
        }
    });

    let socd_selected = Signal::derive(move || position(&SocdMode::ALL, settings.get().socd));
    let set_socd = Callback::new(move |value: String| {
        let index: usize = value.parse().unwrap_or(0);
        if let Some(socd) = SocdMode::ALL.get(index) {
            settings.update(|settings| settings.socd = *socd);
        }
    });

    let touch_selected =
        Signal::derive(move || position(&TouchOverlayMode::ALL, settings.get().touch_overlay));
    let set_touch = Callback::new(move |value: String| {
        let index: usize = value.parse().unwrap_or(0);
        if let Some(mode) = TouchOverlayMode::ALL.get(index) {
            settings.update(|settings| settings.touch_overlay = *mode);
        }
    });

    let haptics = Signal::derive(move || settings.with(|settings| settings.haptics));
    let set_haptics = Callback::new(move |()| {
        settings.update(|settings| settings.haptics = !settings.haptics);
    });
    let hide_with_controller =
        Signal::derive(move || settings.with(|settings| settings.hide_touch_with_controller));
    let set_hide_with_controller = Callback::new(move |()| {
        settings.update(|settings| {
            settings.hide_touch_with_controller = !settings.hide_touch_with_controller
        });
    });

    view! {
        <section class="settings-section">
            <h3 class="settings-section__title">"Controls"</h3>
            <p class="settings-section__note">
                "Click a binding, then press the key or pad button you want. Escape gives up."
            </p>
            {players.collect_view()}
            <Select
                label="Two directions on one axis".to_string()
                options=socd_options
                selected=socd_selected
                on_change=set_socd
            />
            <p class="settings-section__note">
                "A Genesis d-pad cannot press both halves of an axis at once, so the page picks one."
            </p>
            <Select
                label="Show touch controls".to_string()
                options=touch_options
                selected=touch_selected
                on_change=set_touch
            />
            <Toggle
                label="Vibrate on press".to_string()
                checked=haptics
                on_toggle=set_haptics
            />
            <Toggle
                label="Hide touch controls when a controller is connected".to_string()
                checked=hide_with_controller
                on_toggle=set_hide_with_controller
            />
            <p class="settings-section__note">
                "The on-screen pad appears by itself when the browser says your pointer is a finger."
            </p>
        </section>
    }
}
