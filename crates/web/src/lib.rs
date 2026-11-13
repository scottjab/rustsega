//! Browser frontend, as a native-testable library.
//!
//! The `web` crate is a Leptos CSR app bundled by `trunk`. Everything that can
//! be reasoned about without a browser lives here, so `cargo test -p web` runs
//! it on the host (clippy lints the whole workspace natively too). `main.rs` is
//! the WASM entry point and does nothing but mount the app.

pub mod app;
pub mod theme;
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
