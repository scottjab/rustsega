//! The warning a page served over plain http gets (#629).
//!
//! Measured on an iPhone (Safari 27.0.1, iOS 18.7) with the same build served
//! both ways: over `http://` to a tailnet IP a frame took 50 ms (19 fps) and a
//! plain JS loop ran 18x slower; over `https://` the frame took 4.8 ms at 60 fps
//! and the loop matched an M3 Pro Mac. Safari ran the insecure page without
//! its JIT. An insecure page also has no `AudioWorkletNode`, so WebKit loses
//! the audio ring (`crate::audio_ring`) and falls back to scheduled buffers.
//! Neither shows up as an error, only as a game that is slow, so the shell
//! says so once at start-up. `localhost` counts as secure, so `trunk serve`
//! stays quiet.

use crate::ui::toast::{ToastKind, Toasts};

/// What the shell says when the page is not a secure context.
pub const INSECURE_PAGE: &str = "This page isn't served over https, so Safari may run \
    the emulator slowly and without its best audio path. Load it from an https URL.";

/// The warning for a page whose `isSecureContext` is `secure`, if it needs one.
pub fn insecure_page_warning(secure: bool) -> Option<&'static str> {
    (!secure).then_some(INSECURE_PAGE)
}

/// Pushes [`INSECURE_PAGE`] when this page is not a secure context.
pub fn warn_if_insecure(toasts: Toasts) {
    let secure = web_sys::window().is_none_or(|window| window.is_secure_context());
    if let Some(text) = insecure_page_warning(secure) {
        toasts.push(ToastKind::Warn, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_insecure_page_is_told_to_use_https() {
        let text = insecure_page_warning(false).expect("an http page gets a warning");
        assert!(text.contains("https"), "{text}");
    }

    #[test]
    fn a_secure_page_gets_no_warning() {
        assert_eq!(insecure_page_warning(true), None);
    }
}
