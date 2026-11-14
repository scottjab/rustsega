//! The library view: search, region chips, and a list of thousands (M8-04, #158).
//!
//! It is an overlay over the screen area rather than a replacement for it. The
//! canvas underneath belongs to [`crate::emulator`] by `NodeRef`, and unmounting it
//! to show a list would stop a running console; `position: absolute` means the
//! library can cover the picture without disturbing the one element that cannot move.
//!
//! Two rules shape the rest. The list is virtualized - only the rows inside the
//! viewport plus [`crate::library::OVERSCAN`] are in the DOM - because the owner's
//! collection is about 2,775 games and a row each is thousands of elements doing
//! nothing. And a row is a real `<button>`, which is what makes the shell's existing
//! spatial walk (M7-11, #144) this view's keyboard and gamepad support: the arrow
//! keys and the d-pad move the focus from row to row, `Enter` and `B` activate the
//! focused one, and `C` and `Escape` come back - all of it by way of [`crate::nav`],
//! with no second set of bindings for this view alone.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::library::Library;
use crate::library::LibraryOpen;
use crate::library::OVERSCAN;
use crate::library::ROW_HEIGHT_PX;
use crate::library::RegionFilter;
use crate::library::RomEntry;
use crate::library::format_count;
use crate::library::format_size;
use crate::library::parse_tags;
use crate::library::tag_kind;
use crate::library::visible_window;
use crate::rom::LoadedRom;
use crate::touch_events::coarse_pointer;
use crate::ui::button::Button;
use crate::ui::button::IconButton;
use crate::ui::icons;

/// The selector of the first row button, which is where an arrow key that started
/// in the search box goes.
const ROW_SELECTOR: &str = ".library-row__launch";

/// The library, when the server has one.
///
/// Draws nothing while `/api/info` is still on its way, and nothing for good on a
/// page with no API behind it - see [`crate::library::use_library`].
#[component]
pub fn LibraryView() -> impl IntoView {
    let library = expect_context::<Library>();
    let open = expect_context::<LibraryOpen>();
    let roms = expect_context::<RwSignal<Option<LoadedRom>>>();

    let scroller = NodeRef::<html::Div>::new();
    let search = NodeRef::<html::Input>::new();

    // What the virtualized window is measured against. Both start at nothing and are
    // filled in as soon as the scroller has a box; a scroll and a resize are the two
    // things that change them, and both are listened for.
    let scroll_top = RwSignal::new(0.0);
    let viewport_h = RwSignal::new(0.0);
    let measure = move || {
        if let Some(element) = scroller.get() {
            scroll_top.set(f64::from(element.scroll_top()));
            viewport_h.set(f64::from(element.client_height()));
        }
    };
    // Once on the way in, because the scroller is only laid out after this component
    // is mounted and there is no `resize` for the first time anything is drawn.
    Effect::new(move |_| measure());
    let resized = {
        let measure = move || measure();
        window_event_listener_untyped("resize", move |_| measure())
    };
    on_cleanup(move || resized.remove());

    let shown = library.shown();
    let focus_search = !coarse_pointer();

    view! {
        <section class="library" aria-label="ROM library">
            <div class="library__bar">
                <IconButton
                    label="Close library".to_string()
                    path=icons::CLOSE
                    on_click=Callback::new(move |_| open.0.set(false))
                />
                <input
                    class="library__search"
                    // `menu-focus-first` is what [`crate::menu::Menu::focus_first`] looks
                    // for: on a desktop the first keystroke after opening the library is
                    // meant to type, and on a phone it is not, because focusing an input
                    // raises the soft keyboard over half the list (M7-09, #142).
                    class:menu-focus-first=focus_search
                    type="search"
                    placeholder="Search"
                    aria-label="Search the library"
                    node_ref=search
                    // Typed keys are this element's own business - see
                    // [`crate::input::keyboard::element_takes_key`], which is what keeps
                    // the shell from spending them on the pad. Exactly three keys this
                    // box wants for itself have to be taken back here: the arrow that
                    // leaves for the list, `Enter`, and `Escape`.
                    on:keydown=move |event: ev::KeyboardEvent| {
                        match event.code().as_str() {
                            "ArrowDown" => {
                                event.prevent_default();
                                focus_first_row(&scroller);
                            }
                            "Enter" => {
                                // The launcher convention: the first row of the result is
                                // the game the player was about to click.
                                event.prevent_default();
                                if let Some(id) = first_shown(&library) {
                                    library.launch(roms, open, &id);
                                }
                            }
                            "Escape" => {
                                event.prevent_default();
                                open.0.set(false);
                            }
                            _ => {}
                        }
                    }
                    on:input=move |event| {
                        let value = event
                            .target()
                            .and_then(|target| {
                                target.dyn_into::<web_sys::HtmlInputElement>().ok()
                            })
                            .map(|input| input.value())
                            .unwrap_or_default();
                        library.query.set(value);
                    }
                />
                <div class="library__chips" role="group" aria-label="Region">
                    {RegionFilter::ALL
                        .iter()
                        .map(|filter| {
                            let filter = *filter;
                            view! {
                                <button
                                    type="button"
                                    class=move || {
                                        if library.region.get() == filter {
                                            "library__chip library__chip--on"
                                        } else {
                                            "library__chip"
                                        }
                                    }
                                    aria-pressed=move || library.region.get() == filter
                                    on:click=move |_| library.region.set(filter)
                                >
                                    {filter.label()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                <p class="library__count" role="status">
                    {move || format_count(shown.get().len())}
                    " games"
                </p>
            </div>

            <div class="library__scroller" node_ref=scroller on:scroll=move |_| measure()>
                {move || {
                    let shown = shown.get();
                    let rows = visible_window(
                        shown.len(),
                        scroll_top.get(),
                        ROW_HEIGHT_PX,
                        // Before the first measurement there is no viewport to speak
                        // of, and one row is enough to draw something.
                        viewport_h.get().max(ROW_HEIGHT_PX),
                        OVERSCAN,
                    );
                    let fetching = library.fetching.get();
                    let height = shown.len() as f64 * ROW_HEIGHT_PX;
                    view! {
                        <ul class="library__track" role="list" style:height=format!("{height}px")>
                            {rows
                                .filter_map(|index| {
                                    library.with_entries(|all| all.get(index).cloned())
                                        .map(|entry| (entry, index))
                                })
                                .map(|(entry, index)| {
                                    let loading = fetching.as_deref() == Some(entry.id.as_str());
                                    view! {
                                        <LibraryRow
                                            entry=entry
                                            top=index as f64 * ROW_HEIGHT_PX
                                            loading=loading
                                            on_launch=Callback::new(move |id: String| {
                                                library.launch(roms, open, &id);
                                            })
                                        />
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                }}
            </div>

            <p class="library__empty" hidden=move || !shown.get().is_empty()>
                "Nothing in the library matches that."
            </p>
        </section>
    }
}

/// One row of the list: the title without its tags, the tags, and the size.
#[component]
fn LibraryRow(
    /// What this row is.
    entry: RomEntry,
    /// Where it sits, in the scroller's own pixels - the virtualized position.
    top: f64,
    /// Whether this ROM's bytes are on their way, which is the row's to show.
    loading: bool,
    /// Called with the entry's id when the row is activated.
    on_launch: Callback<String>,
) -> impl IntoView {
    let (title, tags) = parse_tags(&entry.name);
    let size = format_size(entry.size);
    let id = entry.id.clone();
    view! {
        <li class="library-row" style:top=format!("{top}px")>
            <button
                type="button"
                class="library-row__launch"
                disabled=loading
                on:click=move |_| on_launch.run(id.clone())
            >
                <span class="library-row__title">{title}</span>
                <span class="library-row__tags">
                    {tags
                        .into_iter()
                        .map(|tag| {
                            let class = if tag_kind(&tag).is_muted() {
                                "library-row__tag library-row__tag--muted"
                            } else {
                                "library-row__tag"
                            };
                            view! { <span class=class>{tag}</span> }
                        })
                        .collect_view()}
                </span>
                <span class="library-row__size" hidden=loading>{size}</span>
                <span class="library-row__loading" role="status" hidden=!loading>
                    "Loading"
                </span>
            </button>
        </li>
    }
}

/// The section the empty state offers the library by (M8-04, #158).
///
/// The other way in is the top bar's button, which a phone's top bar has no room
/// for - so this is how a phone reaches the list at all, and it says what the button
/// cannot say: how many games are on the server.
#[component]
pub fn LibraryEntry() -> impl IntoView {
    let library = expect_context::<Library>();
    let open = expect_context::<LibraryOpen>();
    let shown = library.shown();
    view! {
        <Show when=move || library.available.get() fallback=|| ()>
            <section class="library-entry" aria-label="ROM library">
                <p class="library-entry__count">{move || format_count(shown.get().len())}</p>
                <p class="library-entry__hint">"games on this server"</p>
                <Button on_click=Callback::new(move |_| open.0.set(true))>
                    "Browse library"
                </Button>
            </section>
        </Show>
    }
}

/// The id of the first row of the current result, for `Enter` in the search box.
fn first_shown(library: &Library) -> Option<String> {
    let index = *library.shown().get_untracked().first()?;
    library.with_entries(|all| all.get(index).map(|entry| entry.id.clone()))
}

/// Moves the focus to the first row of the list.
fn focus_first_row(scroller: &NodeRef<html::Div>) {
    let Some(element) = scroller
        .get()
        .and_then(|scroller| scroller.query_selector(ROW_SELECTOR).ok())
        .flatten()
    else {
        return;
    };
    if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
        let _ = element.focus();
    }
}
