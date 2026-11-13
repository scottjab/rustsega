//! The app shell: top bar, screen area, settings drawer and toast area.
//!
//! The console screen is the hero: everything else is 56px of chrome above it
//! or an overlay beside it. The shell's own state - theme, drawer, toasts, the
//! loaded cartridge - is held in signals here. The machine itself is not: it is
//! owned by [`crate::emulator`], and the shell only holds the pause state its
//! buttons show.

use leptos::html;
use leptos::prelude::*;

use crate::emulator::use_cartridge;
use crate::rom::LoadedRom;
use crate::rom_picker::EmptyScreen;
use crate::rom_picker::RomFileInput;
use crate::rom_picker::open_picker;
use crate::rom_picker::window_drop_target;
use crate::theme::Theme;
use crate::ui::button::IconButton;
use crate::ui::drawer::Drawer;
use crate::ui::icons;
use crate::ui::select::Select;
use crate::ui::select::SelectOption;
use crate::ui::toast::ToastArea;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

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

    let theme = RwSignal::new(Theme::default());
    let drawer_open = RwSignal::new(false);

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

    // Window-wide drop handling; the screen area wears the result.
    let dragging = window_drop_target();

    // Writes `data-theme` on `<html>` now, and again whenever the theme changes.
    Effect::new(move |_| {
        theme.get().apply_to_document();
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
    let selected_theme = Signal::derive(move || theme.get().to_string());

    view! {
        <div class="app">
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
                <nav class="topbar__actions" aria-label="Console">
                    {console_buttons}
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
                        on_click=not_wired_yet(toasts, "Fullscreen")
                    />
                    <IconButton
                        label="Settings".to_string()
                        path=icons::SETTINGS
                        on_click=Callback::new(move |_| drawer_open.update(|open| *open = !*open))
                    />
                </nav>
            </header>

            <main class="screen-area" class:screen-area--drag=move || dragging.get()>
                <Show when=move || loaded.get() fallback=|| view! { <EmptyScreen/> }>
                    <canvas
                        id="screen"
                        class="screen"
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
                                theme.set(next);
                            }
                        })
                    />
                    <p class="settings-section__note">
                        Settings live in this tab: M7-10 (#143) is what makes them stick.
                    </p>
                </section>
            </Drawer>

            <ToastArea/>
        </div>
    }
}
