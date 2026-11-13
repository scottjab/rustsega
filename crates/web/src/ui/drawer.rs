//! The settings drawer: a panel that slides in from the right, or up from the
//! bottom on narrow screens, and closes on Escape or a backdrop click.

use leptos::ev;
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
    /// Whether the drawer is showing. The drawer clears it on Escape and on a
    /// backdrop click; the caller sets it.
    open: RwSignal<bool>,
    children: Children,
) -> impl IntoView {
    let drawer_label = title.clone();

    let handle = window_event_listener(ev::keydown, move |event: web_sys::KeyboardEvent| {
        if event.key() == "Escape" {
            open.set(false);
        }
    });
    on_cleanup(move || handle.remove());

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
                        label="Close settings".into()
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
