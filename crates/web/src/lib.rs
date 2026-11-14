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
//! of those reads, [`controls`] is the drawer page that edits it, [`fps`] is the
//! reading the frame loop publishes for the overlay to show, and [`perf`] is the
//! arithmetic that turns the loop's own frame timings into the number the browser
//! budget test asserts on. Bytes that outlive the page - a cart's saved RAM today, save states tomorrow - go through
//! [`storage`], and [`emulator`] is the half that gets them there.

pub mod app;
pub mod audio;
pub mod audio_pacing;
pub mod controls;
pub mod crash;
pub mod emulator;
pub mod fps;
pub mod frame_pacer;
pub mod gamepad_events;
pub mod input;
pub mod keyboard_events;
pub mod library;
pub mod menu;
pub mod nav;
pub mod perf;
pub mod recents;
pub mod render;
pub mod rom;
pub mod rom_picker;
pub mod scale;
pub mod screen;
pub mod settings;
pub mod slots;
pub mod storage;
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

/// The version to show in the settings drawer's About section (M7-19, #152).
///
/// `CARGO_PKG_VERSION` is the workspace version, which is the version `nix build`
/// built the binary with and therefore the only version the page can honestly
/// name: the wasm, the server and the crate are one build of one checkout.
pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The repository, linked from the same About section.
pub const REPOSITORY_URL: &str = "https://github.com/scottjab/rustsega";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_title_is_rustsega() {
        assert_eq!(app_title(), "rustsega");
    }

    /// The About section has to name this build and point at its source, and both
    /// are only worth having if they are the real thing: an empty version is a
    /// version that never reached the page, and the repository is the one place a
    /// player who has just read "the emulator crashed" can go.
    #[test]
    fn the_about_section_names_the_build_and_its_repo() {
        let version = app_version();
        assert!(!version.is_empty(), "no version reached the page");
        assert!(
            version
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit()),
            "{version:?} does not read like a version"
        );
        assert_eq!(REPOSITORY_URL, "https://github.com/scottjab/rustsega");
    }
}
