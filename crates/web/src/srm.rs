//! Exporting and importing a cart's SRAM as a `.srm` file (M7-#398).
//!
//! The `.srm` format is the raw SRAM image - no header, no container - so this is
//! a `Blob` over the bytes the cart is holding and a file read back into
//! [`crate::storage::Db::put_sram`]. Both ends of #147 already exist; what lives
//! here is the part between them that can go wrong: what a `.srm` is called, and
//! what is refused before it can touch anything.
//!
//! ## Why the size check is done twice
//!
//! [`genesis::Genesis::load_sram`] reaches [`genesis::cart::sram::Sram::load`],
//! which refuses a blob of any other length whole - "a blob of any other length
//! is another cart's save" - and answers **nothing**. A caller cannot tell an
//! accepted import from a refused one, so a UI that only called `load_sram`
//! would have to report success for a save that was silently dropped. This module
//! therefore applies the same rule itself, against the length of the cart's RAM,
//! before a byte of an import is written or loaded; the cart's own refusal stays
//! as the backstop for anything this layer fails to notice.
//!
//! ## Why nothing happens on a refusal
//!
//! An import that is rejected must leave IndexedDB exactly as it was, so a player
//! who picks the wrong file still has the save they were worried about losing.
//! That means the check runs before `put_sram`, not after, and a failed *write*
//! likewise leaves cart RAM alone: the two are ordered storage-then-machine so
//! that the pair can be briefly inconsistent (written, not yet applied) but can
//! never end up claiming a save exists that was never stored.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::emulator::Controls;
use crate::ui::toast::{ToastKind, Toasts};

/// The extension of a save image. `.srm` is what other emulators use, which is
/// the only reason it is not `.sav` or nothing.
pub const SRAM_SUFFIX: &str = ".srm";

/// The MIME of a `.srm`: the raw image. `application/octet-stream` rather than
/// `application/x-genesis-sram` (which exists in the wild and means nothing to
/// any browser) so the download is a file and not something a browser tries to
/// render.
pub const SRAM_MIME: &str = "application/octet-stream";

/// The name a `.srm` is downloaded and expected back under: the ROM's SHA-1,
/// which is the key #147 stores it under.
///
/// Deliberately not the cartridge's display title. The id is the identity the
/// save belongs to, it survives a renamed or re-downloaded ROM, and a name a
/// player can match against `IndexedDB` by eye is worth more than a friendly
/// one. Re-importing never reads the name anyway - the size check and the key
/// decide - so a second, prettier identity here would only be a way to be wrong.
pub fn file_name(rom_id: &str) -> String {
    format!("{rom_id}{SRAM_SUFFIX}")
}

/// Why an import was refused, in the one sentence a toast shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// No cartridge is running, so there is no cart to give a save to and no key
    /// to store it under.
    NoCartridge,
    /// A cart is running but has no battery-backed RAM, so an import would have
    /// nowhere to go. The same `None` that keeps #147 from writing at all.
    NoSram,
    /// The image is not this cart's save: the cart's RAM is a different size, so
    /// applying it would be loading another game's save over this one.
    WrongSize {
        /// Bytes the file held.
        got: usize,
        /// Bytes this cart's RAM is.
        want: usize,
    },
}

impl core::fmt::Display for ImportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoCartridge => write!(f, "Load a cartridge before importing a save"),
            Self::NoSram => write!(f, "This cartridge has no battery-backed save to replace"),
            Self::WrongSize { got, want } => write!(
                f,
                "That save is {got} bytes and this cartridge uses {want}, so it is for another game"
            ),
        }
    }
}

/// The cart's own rule, applied to an import before anything is written: a save is
/// for this cart only if it is exactly as long as the cart's RAM.
///
/// Exactly, not "at most" and not "at least": [`genesis::cart::sram::Sram::load`]
/// refuses any other length rather than truncating or padding, and agreeing with
/// it is the whole of what this has to get right.
pub fn check_import(got: usize, want: usize) -> Result<(), ImportError> {
    if got == want {
        Ok(())
    } else {
        Err(ImportError::WrongSize { got, want })
    }
}

/// The [`web_sys::Blob`] a `.srm` download is made of.
///
/// Split out from [`save_to_disk`] so that the MIME a player's browser is handed
/// is assertable rather than believed: `""` (the default) and a wrong type both
/// download a file that is 0 bytes to some programs and unopenable to others.
pub fn blob(bytes: &[u8]) -> web_sys::Blob {
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(SRAM_MIME);
    web_sys::Blob::new_with_u8_array_sequence_and_options(&parts.into(), &options)
        .expect("a Blob over bytes already in memory cannot fail")
}

/// The `<a download>` that hands `bytes` to the browser under `name`, appended to
/// the page and ready to click.
///
/// Kept apart from the click so both are testable: an anchor that is never
/// clicked has leaked nothing, and a test can read the name and the URL back off
/// it without a download happening.
pub fn download_anchor(name: &str, bytes: &[u8]) -> web_sys::HtmlAnchorElement {
    use wasm_bindgen::JsCast;
    let document = web_sys::window()
        .expect("a browser window")
        .document()
        .expect("a document");
    let anchor = document
        .create_element("a")
        .expect("create an anchor")
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .expect("an `a` element is an HtmlAnchorElement");
    anchor.set_download(name);
    anchor.set_href(
        &web_sys::Url::create_object_url_with_blob(&blob(bytes))
            .expect("an object URL over a Blob of our own making"),
    );
    anchor
}

/// Hand `bytes` to the browser as a file called `name`: an anchor, a click, and
/// both taken away again.
///
/// The object URL is revoked after a tick, not immediately - a URL revoked
/// before the navigation is queued is a download of nothing - and the anchor has
/// to be in the document for Firefox to honour the click at all.
pub fn save_to_disk(name: &str, bytes: &[u8]) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    let window = web_sys::window().expect("a browser window");
    let document = window.document().expect("a document");
    let anchor = download_anchor(name, bytes);
    let url = anchor.href();
    let body = document.body().expect("a page with a body");
    body.append_child(&anchor).expect("anchor the download");
    anchor.click();
    body.remove_child(&anchor)
        .expect("the anchor we added a moment ago");
    let revoke = Closure::wrap(Box::new(move || {
        web_sys::Url::revoke_object_url(&url).expect("revoking an object URL we made");
    }) as Box<dyn FnMut()>);
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(revoke.as_ref().unchecked_ref(), 0);
    revoke.into_js_value();
}

/// The id of the hidden input [`SrmFileInput`] mounts and [`open_srm_picker`]
/// clicks - one input, two callers is impossible, but the id is how the button
/// reaches it, exactly as the ROM picker does it (#136).
pub const SRAM_INPUT_ID: &str = "srm-input";

/// Opens the browser's `.srm` picker.
pub fn open_srm_picker() {
    use wasm_bindgen::JsCast;
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(element) = document.get_element_by_id(SRAM_INPUT_ID) else {
        return;
    };
    element
        .dyn_into::<web_sys::HtmlInputElement>()
        .expect("`#srm-input` is the file input `SrmFileInput` mounts")
        .click();
}

/// The hidden `<input type="file">` an import reads a `.srm` from, mounted once
/// by [`crate::app`] beside the button that opens it.
///
/// A second input rather than reusing `rom_picker`'s, because the ROM picker
/// routes what it is given to `load_rom` and would answer a save file with "that
/// doesn't look like a Genesis ROM". `accept` is the extension only - a MIME type
/// for a `.srm` is not registered anywhere a browser has heard of, and
/// `application/octet-stream` would let anything through.
#[component]
pub fn SrmFileInput() -> impl IntoView {
    let input_ref = NodeRef::<html::Input>::new();
    let console = expect_context::<Controls>();
    let toasts = expect_context::<Toasts>();

    view! {
        <input
            id=SRAM_INPUT_ID
            class="file-input"
            type="file"
            accept=".srm"
            // Out of the tab order, exactly as the ROM input is: the control is
            // invisible, so a keyboard user reaches the import button instead.
            tabindex="-1"
            node_ref=input_ref
            on:change=move |_| {
                let Some(input) = input_ref.get() else { return };
                let file = input.files().and_then(|files| files.item(0));
                // Cleared first, so importing the same file twice - after
                // rejecting the wrong one, say - is a second change rather than
                // nothing at all.
                input.set_value("");
                let Some(file) = file else { return };
                let toasts = toasts;
                spawn_local(read_and_import(file, console, toasts));
            }
        />
    }
}

/// One picked file, into bytes, into [`Controls::import_sram`].
///
/// The only failure before the cart is a `File` that went away between the pick
/// and the read; everything else - no cart, no battery, the wrong size - is the
/// import's own answer, and it says so in the toast.
async fn read_and_import(file: web_sys::File, console: Controls, toasts: Toasts) {
    let name = file.name();
    let Ok(bytes) = crate::rom_picker::read_file(&file).await else {
        toasts.push(ToastKind::Danger, format!("{name} could not be read"));
        return;
    };
    console.import_sram(bytes, toasts);
}
