//! Browser frontend, as a native-testable library.
//!
//! The `web` crate is a Leptos CSR app bundled by `trunk`. Everything that can
//! be reasoned about without a browser lives here, so `cargo test -p web` runs
//! it on the host (clippy lints the whole workspace natively too). `main.rs` is
//! the WASM entry point and does nothing but mount the app.
//!
//! Of the ROM-loading pair, [`rom`] is the pure half - bytes in, [`rom::LoadedRom`]
//! out - and [`rom_picker`] is the browser half that hands it a file. The input
//! devices are the same pair: [`input::keyboard`], [`input::gamepad`] and
//! [`input::touch`] say what a key, a pad reading and a finger mean, and
//! [`keyboard_events`], [`gamepad_events`] and [`touch_events`] are the halves that
//! listen for them, poll them, and draw them. [`settings`] is the one store every
//! of those reads, and [`controls`] is the drawer page that edits it.

pub mod app;
pub mod audio;
pub mod audio_pacing;
pub mod controls;
pub mod emulator;
pub mod frame_pacer;
pub mod gamepad_events;
pub mod input;
pub mod keyboard_events;
pub mod menu;
pub mod nav;
pub mod rom;
pub mod rom_picker;
pub mod settings;
pub mod theme;
pub mod touch_events;
pub mod ui;

/// The application title, shown as the page heading.
///
/// Kept as a free, pure function so the heading text has a native test: the
/// component itself only renders in a browser.
pub fn app_title() -> &'static str {
    "rustsega"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_title_is_rustsega() {
        assert_eq!(app_title(), "rustsega");
    }
}
