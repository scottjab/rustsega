//! The app shell: top bar, screen area, settings drawer and toast area.
//!
//! The console screen is the hero: everything else is 56px of chrome above it
//! or an overlay beside it. The shell's own state - theme, drawer, toasts, the
//! loaded cartridge - is held in signals here. The machine itself is not: it is
//! owned by [`crate::emulator`], and the shell only holds the pause state its
//! buttons show.

use leptos::html;
use leptos::prelude::*;

use crate::controls::CaptureTarget;
use crate::controls::Controls;
use crate::emulator::use_cartridge;
use crate::gamepad_events::use_gamepad;
use crate::keyboard_events::use_keyboard;
use crate::menu::use_menu;
use crate::rom::LoadedRom;
use crate::rom_picker::EmptyScreen;
use crate::rom_picker::RomFileInput;
use crate::rom_picker::open_picker;
use crate::rom_picker::window_drop_target;
use crate::scale::Aspect;
use crate::scale::ScaleMode;
use crate::screen::use_screen;
use crate::settings::Settings;
use crate::theme::Theme;
use crate::touch_events::TouchOverlay;
use crate::touch_events::coarse_pointer;
use crate::ui::button::IconButton;
use crate::ui::drawer::Drawer;
use crate::ui::hint::HintBar;
use crate::ui::icons;
use crate::ui::select::Listboxes;
use crate::ui::select::Select;
use crate::ui::select::SelectOption;
use crate::ui::select::options;
use crate::ui::select::position;
use crate::ui::slider::Slider;
use crate::ui::toast::ToastArea;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;
use crate::ui::toggle::Toggle;

/// Shown in the top bar's centre where the game title goes when nothing is loaded.
const NO_CARTRIDGE: &str = "No cartridge loaded";

/// A control whose action belongs to a later M7 ticket: it reports that through
/// a toast rather than doing nothing at all when pressed.
fn not_wired_yet(toasts: Toasts, what: &str) -> Callback<()> {
    let text = format!("{what} is not wired up in this build yet.");
    Callback::new(move |_| toasts.push(ToastKind::Info, text.clone()))
}

/// Root component of the app.
#[component]
pub fn App() -> impl IntoView {
    let toasts = Toasts::default();
    provide_context(toasts);

    let drawer_open = RwSignal::new(false);

    // The one settings store the shell reads: theme, volume, bindings, pad type
    // and all (M7-10, #143). Loaded from `localStorage` before anything reads it,
    // and provided before the console is mounted because [`use_cartridge`] reads
    // it as it starts a console (M7-05, #138).
    let settings = RwSignal::new(Settings::load_local());
    provide_context(settings);

    // The controls page's capture: the cell waiting for its next key or pad
    // button, which the keyboard listener and the pad poll both have to know
    // about. A context because the cell is three components down and the
    // listeners are one.
    let capture = RwSignal::new(None::<CaptureTarget>);
    provide_context(capture);

    // The settings page's lists of options, which are the innermost thing the Back
    // button of a menu has to close (M7-11, #144).
    let listboxes = Listboxes::new();
    provide_context(listboxes);

    // The cartridge, if one has been opened. Context rather than a prop so M7-04
    // (run it) and M7-16 (the library) can react to it without the shell
    // carrying it down the tree (M7-03, #136).
    let roms = RwSignal::new(None::<LoadedRom>);
    provide_context(roms);

    // The canvas the console draws to, and the loop that drives it. The emulator
    // itself is not in a signal here - see [`crate::emulator`] for why - only the
    // pause state its buttons show.
    let canvas = NodeRef::<html::Canvas>::new();
    let console = use_cartridge(canvas);
    // The two elements the screen's own half needs: the shell, which is what goes
    // fullscreen, and the screen area, which is what reports how much room the
    // picture has (M7-12, #145).
    let shell = NodeRef::<html::Div>::new();
    let screen_area = NodeRef::<html::Main>::new();

    // Window-wide drop handling; the screen area wears the result.
    let dragging = window_drop_target();

    // Writes `data-theme` on `<html>` now, and again whenever the theme changes.
    Effect::new(move |_| {
        settings.get().theme.apply_to_document();
    });

    // Everything the player changes is saved as it changes. No debounce: a
    // `Settings` is a few hundred bytes of JSON, and `localStorage` is
    // synchronous anyway (M7-10, #143).
    Effect::new(move |_| settings.get().save_local());

    // The d-pad conflict rule lives in the store and is applied to the page's
    // input state, which is where [`crate::input::InputState::buttons`] reads it.
    Effect::new(move |_| {
        let socd = settings.get().socd;
        crate::input::with_state(|input| input.set_socd_mode(socd));
    });

    // What the top bar and the screen area show for the cartridge. Read with
    // `with`, never `get`: a `LoadedRom` carries the whole image, which is
    // megabytes.
    //
    // `loaded` chooses between the canvas and the empty state - an empty black
    // frame is not worth the space until M7-04 (#137) has a frame to draw.
    let loaded = Signal::derive(move || roms.with(|rom| rom.is_some()));
    let title = Signal::derive(move || roms.with(|rom| rom.as_ref().map(|rom| rom.title.clone())));
    let region =
        Signal::derive(move || roms.with(|rom| rom.as_ref().map(|rom| rom.region.clone())));

    // The menu mode, mounted once for the shell (M7-11, #144): opening a menu
    // holds the console and takes the arrow keys off the game, and both the
    // keyboard and the pads have to ask the same one about it.
    let menu = use_menu(console, drawer_open, capture, listboxes);

    // The keyboard, on the window, for as long as the shell is up (M7-07, #140).
    // It takes the menu and the cartridge because a form and a game cannot
    // both have the arrow keys: while a game runs the keys are swallowed and go
    // to the pad, and while a menu is open they move its focus.
    // The canvas box, fullscreen, and the top bar's reveal - all from the same
    // owner, because the F11 hotkey, the top bar's button and the browser's own
    // `Escape` have to agree about what state the shell is in (#145).
    let screen = use_screen(canvas, screen_area, shell, console, drawer_open);

    use_keyboard(menu, loaded, toasts, settings, capture, screen);

    // Whether a controller is plugged in, which the touch overlay hides itself
    // behind and nothing else asks about (M7-09, #142).
    let controller_connected = RwSignal::new(false);

    // The gamepads, on the window and polled once per animation frame, for as
    // long as the shell is up (M7-08, #141). No `loaded` this time: a pad that
    // is plugged in is a pad the player is holding, whether or not a cartridge
    // is in, and its connect/disconnect news is worth a toast either way.
    use_gamepad(
        menu,
        toasts,
        controller_connected,
        settings,
        capture,
        screen,
    );

    // The on-screen controller: on for a phone, on for a desktop that asked, off
    // for a phone whose player picked up a pad. `coarse` is read once - a browser
    // does not change what kind of pointer the machine has while the page is open,
    // and the setting is there for the machine that does (M7-09, #142).
    let coarse = coarse_pointer();
    let touch_visible = Signal::derive(move || {
        settings.with(|settings| settings.touch_visible(coarse, controller_connected.get()))
    });

    // The two console controls. Re-rendered as a unit because both the icon of
    // one and the `disabled` of the other follow the console, and an icon-only
    // button with no accessible name is unusable - so the label changes with it.
    let console_buttons = move || {
        let loaded = loaded.get();
        let paused = console.paused.get();
        let (label, path) = if paused {
            ("Play", icons::PLAY)
        } else {
            ("Pause", icons::PAUSE)
        };
        view! {
            <IconButton
                label=label.to_string()
                path=path
                disabled=!loaded
                on_click=Callback::new(move |_| console.toggle())
            />
            <IconButton
                label="Reset".to_string()
                path=icons::RESET
                disabled=!loaded
                on_click=Callback::new(move |_| console.reset())
            />
        }
    };

    let theme_options = Theme::ALL
        .iter()
        .map(|option| SelectOption::new(option.to_string(), option.label()))
        .collect();
    let selected_theme = Signal::derive(move || settings.get().theme.to_string());

    view! {
        // `app--touch` is what tells the screen area to give the controls room of
        // their own in portrait, where they sit below the picture rather than on it.
        <div
            class="app"
            node_ref=shell
            class:app--touch=move || touch_visible.get()
            class:app--bar-shown=move || screen.revealed.get()
        >
            <header class="topbar">
                <span class="wordmark">
                    {crate::app_title()}
                    <span class="wordmark__dot" aria-hidden="true"></span>
                </span>
                <p
                    class=move || {
                        if loaded.get() {
                            "now-playing now-playing--loaded"
                        } else {
                            "now-playing"
                        }
                    }
                >
                    {move || title.get().unwrap_or_else(|| NO_CARTRIDGE.to_string())}
                    {move || {
                        region
                            .get()
                            .map(|region| view! { <span class="now-playing__region">{region}</span> })
                    }}
                </p>
                // The autoplay nudge: until the browser lets the audio clock run,
                // the console is silent however loud the volume is (M7-05, #138).
                {move || {
                    (loaded.get() && !console.sound_on.get()).then(|| {
                        view! {
                            <button
                                type="button"
                                class="chip"
                                on:click=move |_| console.enable_audio()
                            >
                                "Click to enable sound"
                            </button>
                        }
                    })
                }}
                <nav class="topbar__actions" aria-label="Console">
                    {console_buttons}
                    {move || {
                        let muted = settings.with(|settings| settings.muted);
                        let (label, path) = if muted {
                            ("Unmute", icons::VOLUME_OFF)
                        } else {
                            ("Mute", icons::VOLUME)
                        };
                        view! {
                            <IconButton
                                label=label.to_string()
                                path=path
                                on_click=Callback::new(move |_| {
                                    settings.update(|settings| settings.muted = !settings.muted);
                                })
                            />
                        }
                    }}
                    <IconButton
                        label="Open ROM".to_string()
                        path=icons::OPEN
                        on_click=Callback::new(move |_| open_picker())
                    />
                    <IconButton
                        label="Save states".to_string()
                        path=icons::SAVE
                        on_click=not_wired_yet(toasts, "Save states")
                    />
                    <IconButton
                        label="Fullscreen".to_string()
                        path=icons::FULLSCREEN
                        on_click=Callback::new(move |_| screen.toggle_fullscreen())
                    />
                    <IconButton
                        label="Settings".to_string()
                        path=icons::SETTINGS
                        on_click=Callback::new(move |_| drawer_open.update(|open| *open = !*open))
                    />
                </nav>
            </header>

            <main
                class="screen-area"
                node_ref=screen_area
                class:screen-area--drag=move || dragging.get()
            >
                <Show when=move || loaded.get() fallback=|| view! { <EmptyScreen/> }>
                    <canvas
                        id="screen"
                        class="screen"
                        class:screen--smooth=move || settings.with(|settings| settings.smoothing)
                        width=320
                        height=224
                        node_ref=canvas
                    >
                    </canvas>
                </Show>
                <RomFileInput/>
            </main>

            <Drawer title="Settings".to_string() open=drawer_open>
                <section class="settings-section">
                    <h3 class="settings-section__title">Appearance</h3>
                    <Select
                        label="Theme".to_string()
                        options=theme_options
                        selected=selected_theme
                        on_change=Callback::new(move |value: String| {
                            if let Ok(next) = value.parse::<Theme>() {
                                settings.update(|settings| settings.theme = next);
                            }
                        })
                    />
                    <p class="settings-section__note">
                        "Every setting on this page is saved in this browser."
                    </p>
                </section>

                <section class="settings-section">
                    <h3 class="settings-section__title">Picture</h3>
                    // The same listbox as the theme and the save slots, so the d-pad
                    // reaches the scaler too: a player in the drawer with a pad and
                    // no mouse would otherwise be shut out of it (#144, #145).
                    {
                        let settings = settings;
                        let choices = options(&ScaleMode::ALL, |mode| mode.label());
                        let chosen = Signal::derive(move || {
                            position(&ScaleMode::ALL, settings.with(|settings| settings.scale))
                        });
                        view! { <Select label="Scale".to_string() options=choices selected=chosen on_change=Callback::new(move |value: String| {
                            let mode = ScaleMode::ALL
                                .get(value.parse::<usize>().unwrap_or(0))
                                .copied()
                                .unwrap_or_default();
                            settings.update(|settings| settings.scale = mode);
                        })/> }
                    }
                    {
                        let settings = settings;
                        let choices = options(&Aspect::ALL, |aspect| aspect.label());
                        let chosen = Signal::derive(move || {
                            position(&Aspect::ALL, settings.with(|settings| settings.aspect))
                        });
                        view! { <Select label="Aspect".to_string() options=choices selected=chosen on_change=Callback::new(move |value: String| {
                            let aspect = Aspect::ALL
                                .get(value.parse::<usize>().unwrap_or(0))
                                .copied()
                                .unwrap_or_default();
                            settings.update(|settings| settings.aspect = aspect);
                        })/> }
                    }
                    <Toggle
                        label="Smoothing".to_string()
                        checked=Signal::derive(move || settings.with(|settings| settings.smoothing))
                        on_toggle=Callback::new(move |()| {
                            settings.update(|settings| settings.smoothing = !settings.smoothing);
                        })
                    />
                </section>

                <section class="settings-section">
                    <h3 class="settings-section__title">Sound</h3>
                    <Slider
                        label="Volume".to_string()
                        min=0.0
                        max=1.0
                        step=0.05
                        value=Signal::derive(move || settings.with(|settings| settings.volume))
                        on_input=Callback::new(move |volume: f64| {
                            settings.update(|settings| settings.volume = volume);
                        })
                    />
                    <Toggle
                        label="Mute".to_string()
                        checked=Signal::derive(move || settings.with(|settings| settings.muted))
                        on_toggle=Callback::new(move |()| {
                            settings.update(|settings| settings.muted = !settings.muted);
                        })
                    />
                    <p class="settings-section__note">
                        Sound waits for a click, because no browser will start an audio
                        clock until the page has had one.
                    </p>
                </section>

                <Controls drawer_open=drawer_open />

                // What the buttons of whichever device is in hand do in here,
                // which is the part a d-pad cannot work out for itself (M7-11, #144).
                <HintBar device=Signal::derive(move || menu.hints.get()) />
            </Drawer>

            // The touch overlay, mounted exactly while it is on screen: its pointer
            // listeners and its release-on-unmount come and go with it.
            <Show when=move || touch_visible.get()>
                <TouchOverlay settings=settings drawer_open=drawer_open />
            </Show>

            <ToastArea/>
        </div>
    }
}
