//! A switch: a real `<button>` carrying `role="switch"`, so Space and Enter work
//! for free and the accessible state is the `aria-checked` value.

use leptos::prelude::*;

/// A two-state switch bound to a `bool` signal.
///
/// Read and write are separate - a [`Signal`] and a [`Callback`] rather than an
/// `RwSignal` - because the settings store (M7-10, #143) is one struct behind one
/// signal and a switch is one field of it. Same shape as [`crate::ui::select::Select`].
#[component]
pub fn Toggle(
    /// The switch's visible label, also its accessible name.
    label: String,
    /// The state the switch reads.
    checked: Signal<bool>,
    /// Called when the switch is activated: click, Space or Enter.
    on_toggle: Callback<()>,
) -> impl IntoView {
    view! {
        <button
            type="button"
            class="toggle"
            role="switch"
            attr:aria-checked=move || checked.get().to_string()
            on:click=move |_| on_toggle.run(())
        >
            <span class="toggle__track" aria-hidden="true">
                <span class="toggle__thumb"></span>
            </span>
            <span class="toggle__label">{label}</span>
        </button>
    }
}
