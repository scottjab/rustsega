//! A labelled choice that a gamepad can open (M7-11, #144).
//!
//! This used to be a native `<select>`, and a native popup cannot be driven by a
//! gamepad at all: the list of options is the browser's own, opened, moved and
//! closed outside the page, so neither a d-pad nor this shell can reach it. The
//! replacement is a button that opens a list of real buttons - `role="listbox"` on
//! the list and `role="option"` with `aria-selected` on each choice, which is what
//! a screen reader wants from a control that is no longer a `<select>`.
//!
//! The component's own shape is unchanged: a label, the options, a signal to read
//! and a callback to report to. That is what let the four call sites of the old
//! `<select>` stay where they were, and it still answers a pointer and the
//! keyboard exactly as it did.

use std::cell::Cell;

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

/// The options of a [`Select`] built from a list of values and their labels.
///
/// Position rather than a token of the enum's own because a settings list is read
/// back into the same array it was built from, and an index needs no parser that
/// could get out of step with it ([`position`] is the other half).
pub fn options<T: Copy>(list: &[T], label: impl Fn(T) -> &'static str) -> Vec<SelectOption> {
    list.iter()
        .enumerate()
        .map(|(index, item)| SelectOption::new(index.to_string(), label(*item)))
        .collect()
}

/// The position of `value` in `list`, as the [`Select`] value that shows it.
pub fn position<T: PartialEq>(list: &[T], value: T) -> String {
    list.iter()
        .position(|item| *item == value)
        .unwrap_or(0)
        .to_string()
}

thread_local! {
    /// The number to hand out to the next [`Select`] mounted.
    static NEXT_NUMBER: Cell<usize> = const { Cell::new(1) };
}

/// Which of the page's [`Select`]s is open, in the shell's context.
///
/// A page-level fact rather than a signal per control, for two reasons: a second
/// list opening shuts the first the way a popup did while the browser owned the
/// whole control, and [`Listboxes::close_any`] can hand the menu's Back rule the
/// innermost thing to close without the shell having built the control.
///
/// It is a context and not a thread-local because a signal belongs to the reactive
/// owner that made it: a signal in a thread-local is made by whichever component
/// happened to reach it first, and it is disposed with that component - which is a
/// panic waiting on the other side of a re-render.
#[derive(Clone, Copy)]
pub struct Listboxes {
    open: RwSignal<Option<usize>>,
}

impl Listboxes {
    /// An empty set of lists, for the shell to create and provide.
    pub fn new() -> Self {
        Self {
            open: RwSignal::new(None),
        }
    }

    /// Whether the list of the control numbered `number` is showing.
    fn shown(self, number: usize) -> Signal<bool> {
        Signal::derive(move || self.open.get() == Some(number))
    }

    /// Shows the list of `number`, and shuts it if it was already the one open:
    /// the second click on a `<select>` used to close it again.
    fn toggle(self, number: usize) {
        self.open.update(|open| match *open {
            Some(own) if own == number => *open = None,
            _ => *open = Some(number),
        });
    }

    /// Puts the list of `number` away, having picked from it.
    fn shut(self, number: usize) {
        self.open.update(|open| {
            if *open == Some(number) {
                *open = None;
            }
        });
    }

    /// Closes the list that is open, if there is one, and says whether there was.
    ///
    /// This is the menu's Back rule calling: a list of options is the innermost
    /// open thing a settings page has (M7-11, #144).
    pub fn close_any(self) -> bool {
        let was_open = self.open.get_untracked().is_some();
        if was_open {
            self.open.set(None);
        }
        was_open
    }
}

impl Default for Listboxes {
    fn default() -> Self {
        Self::new()
    }
}

/// A label, a button showing the choice, and the list of choices behind it.
///
/// The button carries `aria-expanded` and the list carries `role="listbox"`, so
/// the state a player hears about is the state on the screen. The list is mounted
/// only while it is open, which is what keeps its options out of the tab order and
/// out of the spatial walk at every other moment.
#[component]
pub fn Select(
    /// The visible field label, and the button's accessible name.
    label: String,
    /// The choices, in display order.
    options: Vec<SelectOption>,
    /// The current value; the control follows it.
    selected: Signal<String>,
    /// Called with the new value when the player picks an option.
    on_change: Callback<String>,
) -> impl IntoView {
    let lists = expect_context::<Listboxes>();
    let number = NEXT_NUMBER.with(|next| {
        let number = next.get();
        next.set(number + 1);
        number
    });
    let open = lists.shown(number);
    let trigger = NodeRef::<html::Button>::new();

    // The choice whose value is current, or nothing when the settings store named
    // something this list does not offer - in which case the button shows just the
    // caret and no option claims `aria-selected`, which is the honest answer.
    let choices = options.clone();
    let current = Signal::derive(move || {
        let value = selected.get();
        choices.iter().find(|option| option.value == value).cloned()
    });

    view! {
        <div class="field">
            <span class="field__label">{label.clone()}</span>
            <button
                type="button"
                class="listbox__button"
                role="combobox"
                aria-haspopup="listbox"
                aria-expanded=move || open.get().to_string()
                aria-label=label.clone()
                node_ref=trigger
                on:click=move |_| lists.toggle(number)
            >
                {move || current.get().map(|option| option.label)}
            </button>
            <Show when=move || open.get()>
                <div class="listbox" role="listbox" aria-label=label.clone()>
                    {options
                        .clone()
                        .into_iter()
                        .map(|option| {
                            let value = option.value.clone();
                            let wanted = option.value.clone();
                            let picked = Signal::derive(move || {
                                current.get().is_some_and(|chosen| chosen.value == wanted)
                            });
                            // The option reports and then gets out of the way, and
                            // the focus goes back to the button that opened it: a
                            // player working down a settings page is looking for the
                            // next row, not for the list they just used. `on_change`
                            // is `Copy`, so each option has its own handle on it.
                            view! {
                                <button
                                    type="button"
                                    class="listbox__option"
                                    role="option"
                                    attr:aria-selected=move || picked.get().to_string()
                                    on:click=move |_| {
                                        on_change.run(value.clone());
                                        lists.shut(number);
                                        if let Some(button) = trigger.get() {
                                            button.focus().ok();
                                        }
                                    }
                                >
                                    {option.label.clone()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </Show>
        </div>
    }
}
