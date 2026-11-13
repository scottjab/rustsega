//! The save state panel: four cards, one per slot (M7-15, #148).
//!
//! It is a [`Drawer`], the settings drawer's own component, on purpose: the drawer
//! is what already knows how to take the keyboard and the d-pad from the game and
//! give them to a panel (M7-11, #144) - `.drawer--open` is the class
//! [`crate::menu`] looks for when it puts the first focus somewhere - and a second
//! kind of overlay would have to learn that a second time. The top bar's Save icon
//! and the settings drawer are therefore mutually exclusive, and
//! [`crate::menu::Menu::back`] closes this one before the settings drawer, which
//! is the page's real depth.
//!
//! Thumbnails never go through an `<img>` and a data URL: a card's picture is
//! RGBA8888 out of [`crate::slots::make_thumbnail`], which a `<canvas>` takes
//! directly with [`web_sys::ImageData`]. They are taken from the CPU-side frame
//! in [`crate::emulator`] and never read back off the game's canvas - the WebGL
//! path draws through a shader, and reading it back is not the picture a person is
//! looking at (#146, which spent a session finding that out).
//!
//! One trap is worth naming where it can be met: `style.css` says
//! `canvas { display: block }`, which beats the UA's `[hidden] { display: none }`,
//! so a card's canvas is never hidden by attribute. An empty slot draws nothing
//! and the card's own `slot-card--empty` class says so instead (#146).

use leptos::html;
use leptos::prelude::*;
use std::sync::Arc;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::CanvasRenderingContext2d;
use web_sys::HtmlCanvasElement;

use crate::emulator::Slots;
use crate::slots;
use crate::slots::SLOT_COUNT;
use crate::storage;
use crate::ui::button::Button;
use crate::ui::button::ButtonVariant;
use crate::ui::drawer::Drawer;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// A card's picture, shared rather than copied: it is 71,680 bytes of RGBA, and a
/// card that cloned one on every read of the four marks would be cloning half a
/// megabyte per render of a panel that is mostly text.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumb {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

/// What one card says about its slot.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SlotCard {
    /// Whether the slot holds a record.
    pub filled: bool,
    /// When it was written, on the clock of [`slots::clock_ms`].
    pub created_ms: u64,
    /// The card's picture, once it has been read.
    pub thumb: Option<Arc<Thumb>>,
}

impl SlotCard {
    /// A slot with nothing in it, which is what every card shows until the store
    /// has answered.
    pub const EMPTY: SlotCard = SlotCard {
        filled: false,
        created_ms: 0,
        thumb: None,
    };
}

/// The four cards of the panel, read from storage.
///
/// Provided as a context so a card can be told to be read again after a write
/// without the panel threading a callback down to it - and so the panel and the
/// quick keys agree about what the slots hold, which is the one state a player
/// checks twice: F1, then the card, then the card again.
#[derive(Clone, Copy)]
pub struct SlotCards {
    /// One entry per slot, in slot order.
    pub marks: RwSignal<[SlotCard; SLOT_COUNT as usize]>,
}

impl Default for SlotCards {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotCards {
    /// The state of a panel that has not read the store yet.
    pub fn new() -> Self {
        Self {
            marks: RwSignal::new([SlotCard::EMPTY; SLOT_COUNT as usize]),
        }
    }

    /// What `slot` holds right now, without subscribing to the answer.
    pub fn get(&self, slot: u32) -> SlotCard {
        self.marks
            .get_untracked()
            .get(slot as usize)
            .cloned()
            .unwrap_or(SlotCard::EMPTY)
    }

    /// Reads all four slots of `slots` again, off the frame loop and off the
    /// console.
    ///
    /// Four `get`s of keys this build wrote, rather than a listing of the store:
    /// the store holds other carts' slots too - every game this browser has ever
    /// been given - and a prefix match on a ROM id is a way to read someone
    /// else's save by accident.
    pub fn read(self, slots: Slots, toasts: Toasts) {
        let Some(target) = slots.console.slot_target() else {
            return;
        };
        let marks = self.marks;
        spawn_local(async move {
            let mut read = [SlotCard::EMPTY; SLOT_COUNT as usize];
            for slot in 0..SLOT_COUNT {
                let found = target
                    .db
                    .get(storage::Store::States, &target.key(slot))
                    .await;
                read[slot as usize] = match found {
                    Ok(Some(bytes)) => match slots::decode(&bytes) {
                        Ok(record) => SlotCard {
                            filled: true,
                            created_ms: record.created_ms,
                            thumb: (record.thumb_w > 0 && record.thumb_h > 0).then(|| {
                                Arc::new(Thumb {
                                    rgba: record.thumb_rgba,
                                    w: record.thumb_w,
                                    h: record.thumb_h,
                                })
                            }),
                        },
                        // A slot that will not decode is a card that says
                        // "Empty" here and the one sentence of why when it is
                        // loaded; four cards all explaining the same damaged
                        // record is four ways to miss the message.
                        Err(_) => SlotCard::EMPTY,
                    },
                    // Absent is the ordinary case and says nothing. A read that
                    // failed is reported once, by the panel's own toast, and the
                    // card stays as empty rather than claiming a save nobody can
                    // see.
                    Ok(None) => SlotCard::EMPTY,
                    Err(error) => {
                        toasts.push(ToastKind::Warn, error.to_string());
                        SlotCard::EMPTY
                    }
                };
            }
            marks.set(read);
        });
    }
}

/// The save state panel: a 2x2 grid of cards, four across two rows, opened from
/// the top bar's Save icon and closed by `Escape` like any other menu.
#[component]
pub fn SlotPanel(
    /// Whether this panel is showing. The drawer slides on it, the shell owns it,
    /// and [`crate::menu::Menu::back`] clears it.
    open: RwSignal<bool>,
    /// The running game's slots: which one the quick keys are on, and the console
    /// to write and read.
    slots: Slots,
) -> impl IntoView {
    let toasts = expect_context::<Toasts>();
    let cards = SlotCards::new();
    // The card whose Save is waiting on an inline "overwrite?" answer. One
    // `Option` rather than four booleans because at most one card asks, and a
    // panel that asked twice would be a panel that wrote twice.
    let confirming = RwSignal::new(None::<u32>);

    // Read when the panel opens. Reading on open rather than on a timer is what
    // makes a slot written by F1 while the panel was shut show up on the card.
    Effect::new(move |was_open: Option<bool>| {
        let showing = open.get();
        if showing && was_open != Some(true) {
            cards.read(slots, toasts);
        }
        showing
    });

    let thumbs: [NodeRef<html::Canvas>; SLOT_COUNT as usize] =
        std::array::from_fn(|_| NodeRef::new());

    view! {
        <Drawer
            title="Save states".to_string()
            open=open
            close_label="Close save states".to_string()
        >
            <section class="settings-section">
                <ul class="slots" role="list">
                    { (0..SLOT_COUNT)
                        .map(|slot| {
                            view! {
                                <SlotCardView
                                    slot_number=slot
                                    slot_state=slots
                                    cards=cards
                                    confirming=confirming
                                    thumb=thumbs[slot as usize]
                                />
                            }
                        })
                        .collect_view() }
                </ul>
                <p class="settings-section__note">
                    "F1 saves and F4 loads the highlighted slot; F2 and F3 move the highlight. A slot holds the whole machine, so it works for a game that keeps no save of its own."
                </p>
            </section>
        </Drawer>
    }
}

/// One card: its picture, its slot's number and time, and its two buttons.
#[component]
fn SlotCardView(
    /// Which slot this card is: 0 to 3. Named `slot_number` because `slot` is the
    /// view macro's own attribute.
    slot_number: u32,
    /// The game's slots, which is where a Save and a Load are asked for. Named
    /// for what it is rather than `slots` because `slot_number` is the view macro's own.
    slot_state: Slots,
    /// What all four slots hold; this card reads its own entry.
    cards: SlotCards,
    /// The card waiting for an overwrite answer, if there is one.
    confirming: RwSignal<Option<u32>>,
    /// Where this card's picture is drawn.
    thumb: NodeRef<html::Canvas>,
) -> impl IntoView {
    let toasts = expect_context::<Toasts>();
    let current = Signal::derive(move || slot_state.current.get() == slot_number);
    let asking = Signal::derive(move || confirming.get() == Some(slot_number));
    let record = Signal::derive(move || {
        cards
            .marks
            .get()
            .get(slot_number as usize)
            .cloned()
            .unwrap_or(SlotCard::EMPTY)
    });
    let label = format!("Slot {}", slot_number + 1);

    // The picture, drawn whenever the card's record changes. Every card holds one
    // canvas for the life of the panel, so a slot that goes empty has to have its
    // canvas erased as well as a slot that goes full has to have it drawn:
    // a canvas keeps whatever it was last given.
    Effect::new(move |_: Option<()>| {
        let Some(canvas) = thumb.get() else { return };
        match record.with(|record| record.thumb.clone()) {
            Some(thumb) => paint_thumb(&canvas, &thumb),
            None => clear_thumb(&canvas),
        }
    });

    let write = Callback::new(move |()| {
        slot_state.save(slot_number);
        cards.read(slot_state, toasts);
    });
    let save = Callback::new(move |()| {
        // Confirm first over a slot that holds something: a save state is the
        // only copy of a moment, and the ticket's "not `window.confirm`" is about
        // a dialog that steals the page rather than about asking at all.
        if cards.get(slot_number).filled {
            confirming.set(Some(slot_number));
        } else {
            write.run(());
        }
    });

    view! {
        <li
            class="slot-card"
            class:slot-card--current=current
            // `record`, not `cards.get`: the latter reads untracked, and a class
            // bound to it would be computed once and then keep saying "empty" at a
            // slot the player had just filled.
            class:slot-card--empty=move || !record.get().filled
            aria-current=move || current.get().then_some("true")
        >
            <canvas
                class="slot-card__thumb"
                node_ref=thumb
                width=160
                height=112
                aria-hidden="true"
            >
            </canvas>
            <p class="slot-card__name">{label}</p>
            <p class="slot-card__time">
                { move || {
                    let record = record.get();
                    if record.filled {
                        slots::relative_time(slots::clock_ms(), record.created_ms as f64)
                    } else {
                        "Empty".to_string()
                    }
                } }
            </p>
            { move || {
                if asking.get() {
                    view! {
                        <p class="slot-card__ask">{format!("Overwrite slot {}?", slot_number + 1)}</p>
                        <div class="slot-card__row">
                            <Button
                                variant=ButtonVariant::Danger
                                on_click=Callback::new(move |_| {
                                    confirming.set(None);
                                    write.run(());
                                })
                            >
                                "Overwrite"
                            </Button>
                            <Button on_click=Callback::new(move |_| confirming.set(None))>
                                "Cancel"
                            </Button>
                        </div>
                    }
                    .into_any()
                } else {
                    view! {
                        <div class="slot-card__row">
                            <Button on_click=save>"Save"</Button>
                            <Button
                                variant=ButtonVariant::Primary
                                disabled=move || !cards.get(slot_number).filled
                                on_click=Callback::new(move |_| {
                                    confirming.set(None);
                                    slot_state.load(slot_number);
                                })
                            >
                                "Load"
                            </Button>
                        </div>
                    }
                    .into_any()
                }
            } }
        </li>
    }
}

/// Draws a card's picture into `canvas` at the picture's own size.
///
/// `put_image_data` rather than a scaled `draw_image`: the canvas is the size of
/// the thumbnail and CSS does the scaling up to the card, so there is exactly one
/// filter applied to these pixels and it is the browser's.
fn paint_thumb(canvas: &HtmlCanvasElement, thumb: &Thumb) {
    canvas.set_width(thumb.w);
    canvas.set_height(thumb.h);
    let Some(context) = context2d(canvas) else {
        return;
    };
    let image =
        web_sys::ImageData::new_with_u8_clamped_array(wasm_bindgen::Clamped(&thumb.rgba), thumb.w);
    if let Ok(image) = image {
        let _ = context.put_image_data(&image, 0.0, 0.0);
    }
}

/// Erases a card's picture.
fn clear_thumb(canvas: &HtmlCanvasElement) {
    if let Some(context) = context2d(canvas) {
        context.clear_rect(
            0.0,
            0.0,
            f64::from(canvas.width()),
            f64::from(canvas.height()),
        );
    }
}

/// The 2D context of a card's canvas, `None` for a browser that will not give one
/// or a canvas that is not there.
///
/// The card's canvas is a plain element of its own and is never asked for
/// anything else, so the one-context-type rule a canvas lives by (#146) is not
/// what this can get wrong.
fn context2d(canvas: &HtmlCanvasElement) -> Option<CanvasRenderingContext2d> {
    canvas
        .get_context("2d")
        .ok()
        .flatten()
        .and_then(|context| context.dyn_into::<CanvasRenderingContext2d>().ok())
}
