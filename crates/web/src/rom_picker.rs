//! The browser half of ROM loading (M7-03, #136): the file picker, the drop
//! target, and the empty state that invites both.
//!
//! Nothing here decides whether a file is a ROM; [`crate::rom`] does that from
//! the bytes alone. This module only moves bytes out of a `File` and reports
//! what came back, which is why every browser type is used here and nowhere
//! else in the crate.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::File;

use crate::rom::{LoadedRom, NOT_A_ROM, load_rom};
use crate::ui::button::Button;
use crate::ui::button::ButtonVariant;
use crate::ui::icons;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// The id of the single hidden file input, shared by [`RomFileInput`] and
/// [`open_picker`].
const INPUT_ID: &str = "rom-input";

/// The `accept` list of that input: the extensions ROM sets use, `.md` above
/// all because Mega Drive dumps are named that way. It is advice to the file
/// picker only - a dropped file is judged by its bytes, by [`load_rom`].
const ACCEPT: &str = ".md,.bin,.gen,.smd";

/// The same list as the empty state shows it.
const ACCEPT_HINT: &str = ".md, .bin, .gen, .smd";

/// The shared slot the loaded cartridge lives in, provided by [`crate::app`].
fn rom_context() -> RwSignal<Option<LoadedRom>> {
    expect_context::<RwSignal<Option<LoadedRom>>>()
}

/// Opens the browser's file picker.
///
/// Two buttons open it - the top bar's and the empty state's - and there is one
/// input, so the click is made through the document rather than a `NodeRef`,
/// which only the component holding it could see.
pub fn open_picker() {
    let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(INPUT_ID))
    else {
        return;
    };
    element
        .unchecked_into::<web_sys::HtmlInputElement>()
        .click();
}

/// The hidden `<input type="file">` that [`open_picker`] clicks. Mounted once,
/// by [`crate::app`], so it outlives the empty state whose button drives it.
#[component]
pub fn RomFileInput() -> impl IntoView {
    let input_ref = NodeRef::<html::Input>::new();
    let roms = rom_context();
    let toasts = expect_context::<Toasts>();

    view! {
        <input
            id=INPUT_ID
            class="file-input"
            type="file"
            accept=ACCEPT
            // Out of the tab order: the control is invisible, so a keyboard
            // user must meet the two Open ROM buttons that drive it instead.
            tabindex="-1"
            node_ref=input_ref
            on:change=move |_| {
                let Some(input) = input_ref.get() else { return };
                let file = input.files().and_then(|files| files.item(0));
                // Cleared before anything else, so picking the same file twice
                // in a row is a second change rather than nothing at all.
                input.set_value("");
                if let Some(file) = file {
                    load_file(file, roms, toasts);
                }
            }
        />
    }
}

/// Makes the whole window the ROM drop target, and reports whether a file is
/// over it right now - which is what the screen area's dashed border shows.
///
/// Call from a component body, so the listeners are removed with it. They are
/// on the window rather than on the visible target because a `dragover` or
/// `drop` that is not cancelled makes the browser navigate away from the app to
/// show the file instead.
pub fn window_drop_target() -> RwSignal<bool> {
    let dragging = RwSignal::new(false);
    let roms = rom_context();
    let toasts = expect_context::<Toasts>();

    // Any drag highlights, not only drags of files: what is being dragged is
    // not worth reading on the way in, and highlighting for a drag that turns
    // out to carry text rather than a ROM costs nothing.
    let over = window_event_listener(ev::dragover, move |event: ev::DragEvent| {
        event.prevent_default();
        dragging.set(true);
    });
    let left = window_event_listener(ev::dragleave, move |event: ev::DragEvent| {
        // `dragleave` fires between elements too; only a `related_target` of
        // `None` means the pointer has left the window.
        if event.related_target().is_none() {
            dragging.set(false);
        }
    });
    let dropped = window_event_listener(ev::drop, move |event: ev::DragEvent| {
        event.prevent_default();
        dragging.set(false);
        let files = event.data_transfer().and_then(|transfer| transfer.files());
        if let Some(file) = files.and_then(|files| files.item(0)) {
            load_file(file, roms, toasts);
        }
    });

    on_cleanup(move || {
        over.remove();
        left.remove();
        dropped.remove();
    });

    dragging
}

/// What the screen area shows with nothing loaded: the invitation to drop a ROM
/// or open one. `style.css` makes it work in both themes and at phone width.
#[component]
pub fn EmptyScreen() -> impl IntoView {
    view! {
        <div class="empty-state">
            <span class="empty-state__icon" aria-hidden="true">
                <svg class="icon" viewBox="0 0 24 24" focusable="false">
                    <path d=icons::GAMEPAD/>
                </svg>
            </span>
            <h2 class="empty-state__title">Drop a ROM here or open one</h2>
            <Button variant=ButtonVariant::Primary on_click=Callback::new(move |_| open_picker())>
                "Open ROM"
            </Button>
            <p class="empty-state__hint">{ACCEPT_HINT}</p>
        </div>
    }
}

/// Reads `file` and, if it is a ROM, puts it in the shared slot.
///
/// The read is asynchronous - a `File` is a handle to disk, not to bytes - so
/// the answer arrives in a spawned task. Failure is one danger toast and no
/// other state change: whatever was loaded before is still loaded.
fn load_file(file: File, roms: RwSignal<Option<LoadedRom>>, toasts: Toasts) {
    let file_name = file.name();
    spawn_local(async move {
        // The file can go away between the drop and the read; the user needs
        // the same answer either way, and only one message.
        let Ok(bytes) = read_file(&file).await else {
            toasts.push(ToastKind::Danger, NOT_A_ROM);
            return;
        };
        // `Err` here is a file that is not a ROM: too small, or too big.
        match load_rom(&file_name, bytes) {
            Ok(rom) => roms.set(Some(rom)),
            Err(_) => toasts.push(ToastKind::Danger, NOT_A_ROM),
        }
    });
}

/// The bytes behind a `File` handle, from `File::array_buffer` (a `Promise`),
/// into a `Vec`.
async fn read_file(file: &File) -> Result<Vec<u8>, ()> {
    let buffer = file.array_buffer();
    let bytes = wasm_bindgen_futures::JsFuture::from(buffer)
        .await
        .map_err(|_| ())?;
    Ok(js_sys::Uint8Array::new(&bytes).to_vec())
}
