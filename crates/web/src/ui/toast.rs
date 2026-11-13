//! Toasts: short messages stacked bottom-centre that dismiss themselves.
//!
//! The stack is a context provided at the app root, so any component can push
//! to it with [`Toasts::push`] instead of threading a callback down the tree.

use leptos::prelude::*;

use crate::ui::button::IconButton;
use crate::ui::icons;

/// How long a toast stays up, in milliseconds. The 4s in the ticket.
#[cfg(target_arch = "wasm32")]
const AUTO_DISMISS_MS: i32 = 4000;

/// What a toast means, and which token colours it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral news.
    Info,
    /// Something worked.
    Ok,
    /// Something odd happened, nothing was lost.
    Warn,
    /// Something failed.
    Danger,
}

impl ToastKind {
    /// The classes for this kind, base class first.
    pub fn class(self) -> &'static str {
        match self {
            ToastKind::Info => "toast toast--info",
            ToastKind::Ok => "toast toast--ok",
            ToastKind::Warn => "toast toast--warn",
            ToastKind::Danger => "toast toast--danger",
        }
    }
}

/// One message in the stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    id: u64,
    kind: ToastKind,
    text: String,
}

impl Toast {
    /// The message text, as rendered.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What kind of message this is.
    pub fn kind(&self) -> ToastKind {
        self.kind
    }
}

/// The toast stack, shared through context.
#[derive(Clone, Copy, Default)]
pub struct Toasts {
    items: RwSignal<Vec<Toast>>,
    next_id: RwSignal<u64>,
}

impl Toasts {
    /// Adds a message to the stack. In the browser it disappears by itself
    /// after [`AUTO_DISMISS_MS`]. [`Toasts::dismiss`] is the manual way out.
    pub fn push(&self, kind: ToastKind, text: impl Into<String>) {
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        self.items.update(|items| {
            items.push(Toast {
                id,
                kind,
                text: text.into(),
            })
        });
        self.schedule_dismiss(id);
    }

    /// Removes a message. Does nothing if it has already gone.
    pub fn dismiss(&self, id: u64) {
        self.items
            .update(|items| items.retain(|toast| toast.id != id));
    }

    /// The stack, as a signal, for rendering.
    pub fn list(&self) -> Signal<Vec<Toast>> {
        let items = self.items;
        Signal::derive(move || items.get())
    }

    #[cfg(target_arch = "wasm32")]
    fn schedule_dismiss(&self, id: u64) {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;

        let toasts = *self;
        let callback = Closure::wrap(Box::new(move || toasts.dismiss(id)) as Box<dyn FnMut()>);
        if let Some(window) = web_sys::window() {
            let on_fire: &js_sys::Function = callback.as_js_value().unchecked_ref();
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(on_fire, AUTO_DISMISS_MS);
        }
        // The timer outlives this scope, so ownership of the closure goes to JS
        // and the GC frees it; dropping it here would invalidate the callback.
        callback.into_js_value();
    }

    /// Natively there is no timer to arm: tests dismiss explicitly.
    #[cfg(not(target_arch = "wasm32"))]
    fn schedule_dismiss(&self, _id: u64) {}
}

/// The toast area: a bottom-centre stack that screen readers announce.
#[component]
pub fn ToastArea() -> impl IntoView {
    let toasts = expect_context::<Toasts>();
    let list = toasts.list();
    view! {
        <div class="toasts" role="status" aria-live="polite">
            <For
                each=move || list.get()
                key=|toast| toast.id
                children=move |toast: Toast| {
                    let id = toast.id;
                    let text = toast.text;
                    view! {
                        <div class=toast.kind.class()>
                            <p class="toast__text">{text}</p>
                            <IconButton
                                label="Dismiss".into()
                                path=icons::CLOSE
                                on_click=Callback::new(move |_| toasts.dismiss(id))
                            />
                        </div>
                    }
                }
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_carry_the_base_class_and_their_own() {
        assert_eq!(ToastKind::Info.class(), "toast toast--info");
        assert_eq!(ToastKind::Ok.class(), "toast toast--ok");
        assert_eq!(ToastKind::Warn.class(), "toast toast--warn");
        assert_eq!(ToastKind::Danger.class(), "toast toast--danger");
    }

    #[test]
    fn push_appends_and_dismiss_removes() {
        let owner = Owner::new();
        owner.set();
        let toasts = Toasts::default();
        toasts.push(ToastKind::Info, "first");
        toasts.push(ToastKind::Danger, "second");

        let items = toasts.list().get_untracked();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text(), "first");
        assert_eq!(items[1].kind(), ToastKind::Danger);

        let first = items[0].id;
        toasts.dismiss(first);
        let items = toasts.list().get_untracked();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text(), "second");
    }
}
