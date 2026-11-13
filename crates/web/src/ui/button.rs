//! Text buttons and icon-only buttons.

use leptos::prelude::*;

/// Visual weight of a [`Button`] or [`IconButton`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    /// The one action in a group: filled with the accent colour.
    Primary,
    /// No chrome until it is hovered or focused; the shell's default.
    #[default]
    Ghost,
    /// Destructive: outlined in the danger colour.
    Danger,
}

impl ButtonVariant {
    /// The classes for this variant, base class first.
    pub fn class(self) -> &'static str {
        match self {
            ButtonVariant::Primary => "btn btn--primary",
            ButtonVariant::Ghost => "btn btn--ghost",
            ButtonVariant::Danger => "btn btn--danger",
        }
    }
}

/// A text button.
#[component]
pub fn Button(
    /// Visual weight; [`ButtonVariant::Ghost`] unless the caller says otherwise.
    #[prop(optional)]
    variant: ButtonVariant,
    /// Called when the button is activated: click, Enter or Space.
    on_click: Callback<()>,
    children: Children,
) -> impl IntoView {
    view! {
        <button
            type="button"
            class=variant.class()
            on:click=move |_| on_click.run(())
        >
            {children()}
        </button>
    }
}

/// An icon-only button, for the top bar and the drawer's close control.
///
/// `label` is required: an icon with no accessible name is unusable to a screen
/// reader, so it becomes both `aria-label` and the `title` tooltip.
#[component]
pub fn IconButton(
    /// Accessible name, and the tooltip text.
    label: String,
    /// Path data, from [`crate::ui::icons`].
    path: &'static str,
    /// Visual weight; [`ButtonVariant::Ghost`] unless the caller says otherwise.
    #[prop(optional)]
    variant: ButtonVariant,
    /// Inert: the real `disabled` attribute, so the button is out of the tab
    /// order as well as unclickable. A control for something that has not been
    /// loaded yet says so rather than answering a press with nothing.
    #[prop(optional, into)]
    disabled: bool,
    /// Called when the button is activated: click, Enter or Space.
    on_click: Callback<()>,
) -> impl IntoView {
    let classes = format!("{} btn--icon", variant.class());
    view! {
        <button
            type="button"
            class=classes
            aria-label=label.clone()
            title=label
            disabled=disabled
            on:click=move |_| on_click.run(())
        >
            <svg class="icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                <path d=path />
            </svg>
        </button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_carry_the_base_class_and_their_own() {
        assert_eq!(ButtonVariant::Primary.class(), "btn btn--primary");
        assert_eq!(ButtonVariant::Ghost.class(), "btn btn--ghost");
        assert_eq!(ButtonVariant::Danger.class(), "btn btn--danger");
    }

    #[test]
    fn the_default_variant_is_ghost() {
        assert_eq!(ButtonVariant::default(), ButtonVariant::Ghost);
    }
}
