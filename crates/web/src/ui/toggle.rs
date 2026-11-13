//! A switch: a real `<button>` carrying `role="switch"`, so Space and Enter work
//! for free and the accessible state is the `aria-checked` value.

use leptos::prelude::*;

/// A two-state switch bound to a `bool` signal.
#[component]
pub fn Toggle(
    /// The switch's visible label, also its accessible name.
    label: String,
    /// The state the switch reads and writes.
    checked: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <button
            type="button"
            class="toggle"
            role="switch"
            attr:aria-checked=move || checked.get().to_string()
            on:click=move |_| checked.update(|state| *state = !*state)
        >
            <span class="toggle__track" aria-hidden="true">
                <span class="toggle__thumb"></span>
            </span>
            <span class="toggle__label">{label}</span>
        </button>
    }
}
