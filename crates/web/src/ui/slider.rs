//! A labelled native range input bound to a number signal.

use leptos::prelude::*;

/// A `<label>`-wrapped `<input type="range">`.
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
    /// The value the control reads and writes.
    value: RwSignal<f64>,
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
                        value.set(next);
                    }
                }
            />
        </label>
    }
}
