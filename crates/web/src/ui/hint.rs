//! The line at the foot of a menu that says what the buttons do (M7-11, #144).
//!
//! A d-pad has no obvious way to say "that one", and the answer to "how do I get
//! out of here" should not need a manual. The glyphs come from
//! [`crate::nav::hints`], which is the only reason the hint bar knows whether the
//! player is holding a pad or a keyboard.

use leptos::prelude::*;

use crate::nav::LastDevice;
use crate::nav::hints;

/// The hints for the menu they are at the bottom of.
///
/// `device` is what the player used last, so a pad shows `B` and `C` and a
/// keyboard shows `Enter` and `Esc` - never both, which would be a legend for two
/// instruments when the player is holding one.
#[component]
pub fn HintBar(device: Signal<LastDevice>) -> impl IntoView {
    view! {
        <div class="hint">
            {move || {
                hints(device.get())
                    .into_iter()
                    .map(|hint| {
                        view! {
                            <span class="hint__item">
                                <kbd class="hint__key">{hint.glyph}</kbd>
                                <span class="hint__meaning">{hint.meaning}</span>
                            </span>
                        }
                    })
                    .collect_view()
            }}
        </div>
    }
}
