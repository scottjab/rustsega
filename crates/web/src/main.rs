//! WASM entry point: install the panic hooks, then mount the app.
//!
//! Deliberately nothing else. Everything the frontend does lives in the `web`
//! library crate so it can be compiled and tested natively.

fn main() {
    console_error_panic_hook::set_once();
    // Second, so that it wraps the logger rather than replacing it: the console
    // keeps the whole report and the shell gains the card of `web::crash` (#152).
    web::crash::install_hook();
    leptos::mount::mount_to_body(web::app::App);
}
