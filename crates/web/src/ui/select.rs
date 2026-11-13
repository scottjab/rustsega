//! A labelled native `<select>`.

use leptos::html;
use leptos::prelude::*;

/// One choice in a [`Select`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOption {
    /// The machine-readable value reported by [`Select`]'s change callback.
    pub value: String,
    /// The text shown in the list.
    pub label: String,
}

impl SelectOption {
    /// An option from a machine value and its display text.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// A `<label>`-wrapped `<select>`.
///
/// The selected option is written to the `<select>`'s `value` *property* after
/// mount, which is what the browser uses to pick an option; the `selected`
/// attribute on an `<option>` is only read while the option is being parsed.
#[component]
pub fn Select(
    /// The visible field label.
    label: String,
    /// The choices, in display order.
    options: Vec<SelectOption>,
    /// The current value; the select follows it.
    selected: Signal<String>,
    /// Called with the new value when the user picks an option.
    on_change: Callback<String>,
) -> impl IntoView {
    let select_ref = NodeRef::<html::Select>::new();

    Effect::new(move |_| {
        if let Some(select) = select_ref.get() {
            select.set_value(&selected.get());
        }
    });

    view! {
        <label class="field">
            <span class="field__label">{label}</span>
            <select
                class="select"
                node_ref=select_ref
                on:change=move |ev| on_change.run(event_target_value(&ev))
            >
                {options
                    .into_iter()
                    .map(|option| {
                        view! { <option value=option.value>{option.label}</option> }
                    })
                    .collect_view()}
            </select>
        </label>
    }
}
