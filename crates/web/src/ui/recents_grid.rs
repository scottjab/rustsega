//! The recent games grid: the cards above the empty state's drop zone (M7-16, #149).
//!
//! It is a grid of twelve at most, and each card is the whole of one decision -
//! this game, or not - so a card carries only what a player is deciding with: the
//! title, the region the header claims, and how long ago it was last opened. The
//! image is not read to draw any of that; it is only read when a card is clicked
//! (see [`crate::recents::Recents::launch`], which is why the empty state paints
//! without a megabyte of ROM in hand).
//!
//! The card itself is the button, with the remove control beside it rather than
//! inside it: nested buttons are not markup a browser will do anything sensible
//! with, and a (x) that is a third of the card's width is a (x) that is pressed by
//! accident on a phone.

use leptos::prelude::*;

use crate::recents;
use crate::recents::RecentMeta;
use crate::recents::Recents;
use crate::rom::LoadedRom;
use crate::slots;
use crate::ui::button::IconButton;
use crate::ui::icons;

/// The grid, with one card per game this browser still holds an image of.
///
/// Mounted by the empty state, and so unmounted the moment a cartridge starts: the
/// list is read from `localStorage` once, at boot, and nothing about it needs to
/// be looked at while a game is running.
#[component]
pub fn RecentsGrid() -> impl IntoView {
    let recents = expect_context::<Recents>();
    // The cartridge signal, which is what a launch writes. The same one the file
    // picker and the drop target write, and the only way into the load path.
    let roms = expect_context::<RwSignal<Option<LoadedRom>>>();

    view! {
        <Show when=move || !recents.items.with(|items| items.is_empty()) fallback=|| ()>
            <section class="recents" aria-label="Recently played">
                <h2 class="recents__heading">Recently played</h2>
                <ul class="recents__list" role="list">
                    {move || {
                        recents
                            .items
                            .get()
                            .into_iter()
                            .map(|meta| {
                                view! { <RecentCard meta=meta recents=recents roms=roms /> }
                            })
                            .collect_view()
                    }}
                </ul>
            </section>
        </Show>
    }
}

/// One card: its title, its region, its time, and the two things that can be done
/// to it.
#[component]
fn RecentCard(
    /// What this card is. A card is rebuilt when the list is, so its record is
    /// fixed for the life of the view - which is why the three labels below are
    /// plain strings rather than signals, and why nothing here reads a clock on
    /// every frame.
    meta: RecentMeta,
    /// The list, for the remove button and the launch.
    recents: Recents,
    /// The cartridge signal a launch writes.
    roms: RwSignal<Option<LoadedRom>>,
) -> impl IntoView {
    let title = meta.display_title().to_string();
    // A badge only when there is a region to badge: a header that claims nothing
    // shows "Unknown", and an empty badge is a box around nothing.
    let region = (!meta.region.is_empty()).then(|| meta.region.clone());
    let time = recents::played_label(slots::clock_ms(), &meta);
    let launch_id = meta.id.clone();
    let remove_id = meta.id.clone();
    let remove_label = format!("Remove {title} from the list");

    view! {
        <li class="recent-card">
            <button
                type="button"
                class="recent-card__launch"
                on:click=move |_| recents.launch(roms, &launch_id)
            >
                <span class="recent-card__title">{title}</span>
                {
                    region.map(|region| view! { <span class="recent-card__region">{region}</span> })
                }
                <span class="recent-card__time">{time}</span>
            </button>
            <IconButton label=remove_label path=icons::CLOSE on_click=Callback::new(move |_| {
                recents.remove(&remove_id)
            }) />
        </li>
    }
}
