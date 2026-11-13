//! The one thing the shell says when the core panics (M7-19, #152).
//!
//! A panic in the frame loop is not a dead page: `wasm32-unknown-unknown` unwinds,
//! the browser logs it through [`console_error_panic_hook`], and the shell carries on
//! with its buttons and its drawer - while the picture sits frozen on the last frame
//! and nothing on screen says why. #387 is the running list of core bugs that end
//! there.
//!
//! So this module keeps a flag rather than catching anything. There is no
//! `catch_unwind` here and none is wanted: wrapping the frame loop in one would turn
//! a panic into a state the emulator does not know it is in, with a `Genesis` left
//! half-advanced and the canvas borrowed by a `RefCell` nobody will visit again. The
//! panic hook already sees every panic, wherever it happens - in the loop, in an
//! effect, in a click handler - and it is the one place that can say "the emulator
//! is finished with this game" truthfully.
//!
//! The hook is installed by `main.rs` on top of the one
//! [`console_error_panic_hook::set_once`] put there, so the console still gets the
//! full report and the shell only gains a card.

use std::cell::Cell;
use std::cell::RefCell;

use leptos::prelude::*;

use crate::ui::button::Button;
use crate::ui::button::ButtonVariant;

/// The card's headline, and the sentence a player reads after their game froze.
const CARD_TITLE: &str = "The emulator crashed";

/// The card's second line: what is lost, and what is not. Save states and a cart's
/// RAM are written as they happen (M7-14, #147 and M7-15, #148), so the honest
/// promise is that the game is not gone, only this run of it.
const CARD_TEXT: &str = "The console stopped, and the picture will not move again. \
    Anything it had already saved is still here.";

/// The button's label. Reloading is the only recovery: the machine is owned by a
/// frame loop that has stopped running, and starting another one beside it is the
/// second console on one canvas that [`crate::emulator::start`] exists to prevent.
const RELOAD_LABEL: &str = "Reload";

thread_local! {
    /// The flag: `true` from the first panic onwards, and never `false` again.
    ///
    /// A plain `Cell` rather than only a signal because a panic can happen before
    /// the shell has mounted anything to write to.
    static FLAG: Cell<bool> = const { Cell::new(false) };

    /// The shell's half of [`FLAG`], registered by [`use_crash_report`] so the hook
    /// can reach it without owning a reactive owner, and `None` before then.
    static VIEW: RefCell<Option<RwSignal<bool>>> = const { RefCell::new(None) };

    /// What the panic said, for the card to quote. `None` until there is one.
    static DETAIL: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Wraps the panic hook that is already installed, so a panic raises [`crashed`]
/// and still gets the report [`console_error_panic_hook`] writes.
///
/// Call once, from the entry point, after that crate's `set_once`.
pub fn install_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        raise(info);
    }));
}

/// Whether the core has panicked since the page loaded.
pub fn crashed() -> bool {
    FLAG.with(|flag| flag.get())
}

/// What the panic said, in the one line the card can show.
pub fn detail() -> Option<String> {
    DETAIL.with(|detail| detail.borrow().clone())
}

/// Raises the flag, once, and tells the shell.
fn raise(info: &std::panic::PanicHookInfo) {
    if FLAG.with(|flag| flag.get()) {
        // A second panic - usually the first one's own cleanup - adds nothing, and
        // the first message is the one that names the bug.
        return;
    }
    FLAG.with(|flag| flag.set(true));
    DETAIL.with(|detail| *detail.borrow_mut() = Some(describe(info)));
    publish();
}

/// The panic's own words: the payload, and where it was thrown from when the
/// hook was given a location.
fn describe(info: &std::panic::PanicHookInfo) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_string());
    match info.location() {
        Some(location) => format!("{message} ({}:{})", location.file(), location.line()),
        None => message,
    }
}

/// Writes the shell's signal, if the shell has registered one.
///
/// On the browser this is called from a microtask rather than from the hook
/// itself: writing the signal is what re-renders the shell, and re-rendering it
/// while the panicking frame still holds a `RefCell` borrow is how one panic
/// becomes the "already mutably borrowed" panic on top of it.
fn publish() {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async {
        write_signal();
    });
    #[cfg(not(target_arch = "wasm32"))]
    write_signal();
}

fn write_signal() {
    VIEW.with(|view| {
        if let Some(signal) = view.borrow().as_ref() {
            signal.set(true);
        }
    });
}

/// The card's signal, for [`CrashCard`].
///
/// Mounted once by [`crate::app`], which is also why the hook can find the shell:
/// the first thing this does is register itself with [`VIEW`], so a panic before
/// this ran is still a panic the next render reports.
pub fn use_crash_report() -> RwSignal<bool> {
    let crashed = RwSignal::new(crashed());
    VIEW.with(|view| {
        *view.borrow_mut() = Some(crashed);
    });
    crashed
}

/// The card that replaces the screen area once the console is finished with it.
///
/// `role="alert"` rather than the toast area: a crash is not an event that
/// happened to be scrolled past, and it is the last thing the shell has to say
/// about the game that was playing.
#[component]
pub fn CrashCard() -> impl IntoView {
    let detail = detail();
    view! {
        <div class="crash" role="alert">
            <h2 class="crash__title">{CARD_TITLE}</h2>
            <p class="crash__text">{CARD_TEXT}</p>
            {detail.map(|detail| view! { <p class="crash__detail">{detail}</p> })}
            <Button
                variant=ButtonVariant::Primary
                on_click=Callback::new(move |_| reload())
            >
                {RELOAD_LABEL}
            </Button>
        </div>
    }
}

/// As it was before the page broke.
fn reload() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;

    /// The whole point of the module, and the half that cannot be tested in a
    /// browser: a panic reaches the hook, the hook raises the flag and keeps the
    /// message, and the hook that was installed before it still sees every panic -
    /// while a second panic, usually the first one's own cleanup, does not replace
    /// the message the card quotes.
    #[test]
    fn a_panic_raises_the_flag_and_the_first_message_is_the_one_that_counts() {
        let logged = Arc::new(Mutex::new(Vec::new()));
        let recorder = logged.clone();
        // The order `main.rs` uses: the console logger's hook first, then this one
        // wrapped over it.
        std::panic::set_hook(Box::new(move |info| {
            recorder.lock().unwrap().push(describe(info));
        }));
        install_hook();

        let first = std::panic::catch_unwind(|| panic!("the cartridge is not there"));
        let second = std::panic::catch_unwind(|| panic!("and then its cleanup"));
        // Back to the default hook: this test owns the process-wide hook only for
        // as long as it takes to prove the chain works. (`nextest` runs each test
        // in its own process, so nothing else in the crate sees either hook.)
        drop(std::panic::take_hook());

        assert!(first.is_err() && second.is_err(), "both panics are ours");
        assert!(crashed(), "the panic did not raise the flag");
        let detail = detail().expect("the panic message was not kept");
        assert!(
            detail.contains("the cartridge is not there"),
            "the card would quote {detail:?}"
        );
        assert!(
            !detail.contains("cleanup"),
            "the second panic replaced the first: {detail:?}"
        );
        assert_eq!(
            logged.lock().unwrap().len(),
            2,
            "the hook installed before this one must still see every panic"
        );
    }

    #[test]
    fn the_card_says_what_is_lost_and_what_is_not() {
        assert_eq!(CARD_TITLE, "The emulator crashed");
        assert!(
            CARD_TEXT.contains("saved"),
            "the card has to say the save survived: {CARD_TEXT}"
        );
        assert_eq!(RELOAD_LABEL, "Reload");
    }
}
