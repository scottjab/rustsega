//! The app shell: top bar, screen area, settings drawer and toast area.
//!
//! The console screen is the hero: everything else is 56px of chrome above it
//! or an overlay beside it. The shell's own state - theme, drawer, toasts - is
//! held in signals here. ROM loading, emulation and persistence arrive in later
//! M7 tickets.

use leptos::prelude::*;

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

    // Writes `data-theme` on `<html>` now, and again whenever the theme changes.
    Effect::new(move |_| {
        theme.get().apply_to_document();
    });

    // The top bar names the loaded cartridge; M7-03 (#136) fills this in.
    let game_title = signal(None::<String>).0;

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
                        if game_title.get().is_some() {
                            "now-playing now-playing--loaded"
                        } else {
                            "now-playing"
                        }
                    }
                >
                    {move || game_title
                        .get()
                        .unwrap_or_else(|| NO_CARTRIDGE.to_string())}
                </p>
                <nav class="topbar__actions" aria-label="Console">
                    <IconButton
                        label="Open ROM".to_string()
                        path=icons::OPEN
                        on_click=not_wired_yet(toasts, "Opening a ROM")
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

            <main class="screen-area">
                <canvas id="screen" class="screen" width=320 height=224></canvas>
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
