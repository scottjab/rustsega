//! The settings drawer: a panel that slides in from the right, or up from the
//! bottom on narrow screens.
//!
//! It owns no key handling: `Escape` is the shell's `ToggleMenu` hotkey, which
//! opens this as well as closing it (M7-07, #140), so a drawer that listened for
//! `Escape` itself would fight it over the signal - see [`crate::keyboard_events`].

use leptos::prelude::*;

use crate::ui::button::IconButton;
use crate::ui::icons;

/// A drawer over the shell, `380px` wide on the right and a full-width bottom
/// sheet under 640px (`style.css` decides which).
///
/// It is always mounted so the slide-in transition has something to animate;
/// while closed it is `visibility: hidden`, which keeps its controls out of the
/// tab order and its off-screen box out of the page's scroll area.
#[component]
pub fn Drawer(
    /// The panel's heading, and its accessible name.
    title: String,
    /// Whether the drawer is showing. The drawer clears it on a backdrop click
    /// and on its close button; the caller sets it, and `Escape` toggles it from
    /// the shell's keyboard.
    open: RwSignal<bool>,
    /// The accessible name of the close control. Every drawer has to be named
    /// something other than "Close", and the settings drawer was the only one
    /// until the save state panel arrived (M7-15, #148).
    #[prop(into)]
    close_label: String,
    children: Children,
) -> impl IntoView {
    let drawer_label = title.clone();

    view! {
        <div class="overlay">
            <div
                class="drawer-backdrop"
                class:drawer-backdrop--open=move || open.get()
                on:click=move |_| open.set(false)
            />

            <aside
                class="drawer"
                class:drawer--open=move || open.get()
                role="dialog"
                aria-modal="true"
                aria-label=drawer_label
            >
                <header class="drawer__header">
                    <h2 class="drawer__title">{title}</h2>
                    <IconButton
                        label=close_label
                        path=icons::CLOSE
                        on_click=Callback::new(move |_| open.set(false))
                    />
                </header>
                <div class="drawer__body">
                    {children()}
                </div>
            </aside>
        </div>
    }
}
