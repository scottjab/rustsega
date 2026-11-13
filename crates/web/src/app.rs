//! The root Leptos component.

use leptos::prelude::*;

/// Root component of the app.
///
/// M7-01 only: the heading proves the Leptos/trunk/WASM pipeline runs in the
/// browser. Emulator integration comes in later M7 tickets.
#[component]
pub fn App() -> impl IntoView {
    view! {
        <main class="app">
            <h1>{crate::app_title()}</h1>
        </main>
    }
}
