//! A labelled native range input bound to a number signal.

use leptos::prelude::*;

/// A `<label>`-wrapped `<input type="range">`.
///
/// Read and write are separate - a [`Signal`] and a [`Callback`] rather than an
/// `RwSignal` - because the settings store (M7-10, #143) is one struct behind one
/// signal, and a slider is one field of it. Same shape as [`crate::ui::select::Select`].
#[component]
pub fn Slider(
    /// The visible field label.
    label: String,
    /// Lowest value the control allows.
    min: f64,
    /// Highest value the control allows.
    max: f64,
    /// The step between values, as the browser sees them.
    step: f64,
    /// The value the control reads.
    value: Signal<f64>,
    /// Called with the new value on every `input` event the browser reports.
    on_input: Callback<f64>,
) -> impl IntoView {
    view! {
        <label class="field">
            <span class="field__label">{label}</span>
            <input
                class="slider"
                type="range"
                prop:min=min
                prop:max=max
                prop:step=step
                prop:value=move || value.get().to_string()
                on:input=move |ev| {
                    if let Ok(next) = event_target_value(&ev).parse::<f64>() {
                        on_input.run(next);
                    }
                }
            />
        </label>
    }
}
