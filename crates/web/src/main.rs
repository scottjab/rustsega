//! WASM entry point: install the panic hook, then mount the app.
//!
//! Deliberately nothing else. Everything the frontend does lives in the `web`
//! library crate so it can be compiled and tested natively.

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(web::app::App);
}
