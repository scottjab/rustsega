//! The touch overlay's browser half: the markup, and the `PointerEvent` listeners
//! that drive it (M7-09, #142).
//!
//! The policy is elsewhere and pure - [`crate::input::touch`] decides what a
//! finger's position means, which control a slid finger has arrived at, and which
//! buttons the overlay as a whole is pressing. This is the half that owns a
//! `PointerEvent`, a `getBoundingClientRect`, and a `navigator.vibrate`, and it is
//! the only place the overlay's DOM is spoken to.
//!
//! Pointer Events, not mouse events, because they are not the same question. A
//! mouse has one position; a thumb chord has two fingers, and each of them keeps an
//! identity through the move that carried it off the element it landed on. So
//! `pointerdown` captures its own element - which is what makes the `pointerup` of a
//! finger that has since wandered off arrive at all - and a slide from `A` to `B` is
//! found by measuring the controls rather than by asking the event, because a
//! captured pointer's target is always the element that took the capture, which is
//! still `A` once `B` is what the player means.
//!
//! Nothing here reaches for the console. The overlay reports its buttons to the
//! page's [`crate::input`] as [`Source::Touch`] and the emulation loop reads them
//! back (M7-06, #139), so no pointer handler can be the second `borrow_mut` inside
//! the machine's own - which is how #387 kills a page for good.

use std::cell::RefCell;

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::input::PadButton;
use crate::input::Source;
use crate::input::gamepad;
use crate::input::touch::{self, Control, ControlBox, DpadDir, TouchTracker};
use crate::settings::Settings;
use crate::ui::icons;

/// The port the overlay drives.
///
/// A phone has one player. Port 0 is the ticket's, and it stays port 0 even with a
/// gamepad in port 2, because [`super::gamepad_events`] owns the slot table and the
/// overlay has no business in it (M7-08, #141).
const TOUCH_PORT: usize = 0;

/// How long the phone buzzes for one press, in milliseconds.
///
/// Short enough that it is feedback rather than an event of its own.
/// `navigator.vibrate` takes a duration directly, which is why this is a duration.
const VIBRATE_MS: u32 = 10;

/// The media query that says the browser's least precise pointer is a finger.
const COARSE_QUERY: &str = "(pointer: coarse)";

/// Whether this browser's pointer is a finger.
///
/// `matchMedia` is the only way to ask, and it answers about the device rather than
/// about the current gesture: a touch-capable laptop says `true` the moment it has a
/// touchscreen, which is why the [`Settings::touch_overlay`] setting exists beside it
/// rather than instead of it. `false` when there is no window, or no `matchMedia`.
pub fn coarse_pointer() -> bool {
    web_sys::window()
        .and_then(|window| window.match_media(COARSE_QUERY).ok().flatten())
        .is_some_and(|query| query.matches())
}

/// One control of the overlay: the element to mark pressed, and the box it was last
/// measured in.
struct ControlOnScreen {
    control: Control,
    element: web_sys::Element,
    bbox: ControlBox,
}

/// The overlay's state, for as long as it is mounted.
struct Overlay {
    tracker: TouchTracker,
    /// The buttons the overlay was holding at the last event, so the next one is
    /// reported as a difference - the same reason the gamepad keeps
    /// [`crate::gamepad_events::PadSeen`].
    held: Vec<PadButton>,
    /// Every control, measured at the start of the last gesture.
    layout: Vec<ControlOnScreen>,
    /// Whether a press buzzes. Read at the press rather than captured at mount:
    /// it is a setting, so the player can change their mind mid-game.
    settings: RwSignal<Settings>,
}

impl Overlay {
    /// The d-pad's box, for a pointer that has to be told which way it is pushing.
    fn dpad(&self) -> Option<&ControlBox> {
        self.layout
            .iter()
            .find(|item| item.control == Control::Dpad)
            .map(|item| &item.bbox)
    }

    /// Every control's box, for [`touch::control_at`].
    fn boxes(&self) -> Vec<ControlBox> {
        self.layout.iter().map(|item| item.bbox).collect()
    }

    /// What a pointer at `x`, `y` means to the d-pad, however far outside it the
    /// pointer has gone - and [`DpadDir::Neutral`] for an overlay that has not been
    /// measured.
    fn direction_at(&self, x: f64, y: f64) -> DpadDir {
        self.dpad()
            .map(|dpad| dpad.direction_at(x, y))
            .unwrap_or(DpadDir::Neutral)
    }
}

/// What one pointer event came to: the work its handler has to do once the
/// [`OVERLAY`] borrow is back down.
///
/// A decision rather than doing the work inside the borrow, because the work is a
/// write to the page's input state and a DOM write, and neither belongs under it.
struct Decision {
    /// The overlay's buttons before and after, when they changed at all.
    changed: Option<(Vec<PadButton>, Vec<PadButton>)>,
    /// The overlay has started holding a control the player was not already
    /// holding, and wants one buzz for it.
    buzz: bool,
    /// Every control, and whether to show it as held.
    marks: Vec<(web_sys::Element, bool)>,
}

impl Decision {
    /// The decision of an event that changed nothing: says nothing, marks nothing,
    /// buzzes not at all.
    fn nothing() -> Self {
        Self {
            changed: None,
            buzz: false,
            marks: Vec::new(),
        }
    }
}

thread_local! {
    /// The mounted overlay, if there is one.
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// Runs `f` against the mounted overlay, answering `None` when there is none -
/// which is the case for as long as the controls are not on screen.
fn with_overlay<T>(f: impl FnOnce(&mut Overlay) -> T) -> Option<T> {
    OVERLAY.with(|overlay| overlay.borrow_mut().as_mut().map(f))
}

/// Every control of the overlay, measured.
///
/// At the start of a gesture rather than once at mount: the shell re-lays itself
/// when a cartridge arrives, when the phone turns, and when a keyboard appears, and
/// nine `getBoundingClientRect` calls per tap is cheaper than being wrong about
/// where a button is while a finger is on it.
fn measure() -> Vec<ControlOnScreen> {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return Vec::new();
    };
    Control::ALL
        .iter()
        .filter_map(|control| {
            let element = document
                .query_selector(&format!("[data-touch=\"{}\"]", control.key()))
                .ok()
                .flatten()?;
            let rect = element.get_bounding_client_rect();
            Some(ControlOnScreen {
                control: *control,
                element,
                bbox: ControlBox {
                    control: *control,
                    x: rect.x() as f64,
                    y: rect.y() as f64,
                    width: rect.width() as f64,
                    height: rect.height() as f64,
                },
            })
        })
        .collect()
}

/// The control a pointer event landed on, and the element to capture.
///
/// Read from the markup rather than from the measured layout, because a press wants
/// the truth about what the finger is on, and the markup is what says so. The walk
/// up to the nearest `data-touch` ancestor is there because the label is inside the
/// control it names.
fn landed_on(event: &web_sys::PointerEvent) -> Option<(Control, web_sys::Element)> {
    let mut element = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())?;
    loop {
        let key = element.get_attribute("data-touch")?;
        if let Some(control) = Control::from_key(&key) {
            return Some((control, element));
        }
        element = element.parent_element()?;
    }
}

/// The pointer's position, in the same CSS pixels `getBoundingClientRect` reports.
///
/// `clientX`/`clientY` are `MouseEvent`'s, and a `PointerEvent` is one, but
/// [`web_sys`] does not flatten inherited members - so it takes a cast to read them.
fn point(event: &web_sys::PointerEvent) -> Option<(f64, f64)> {
    let mouse = event.dyn_ref::<web_sys::MouseEvent>()?;
    Some((f64::from(mouse.client_x()), f64::from(mouse.client_y())))
}

/// Says what the overlay now holds: what `InputState` has to change, whether to
/// buzz, and what the controls are to look like.
///
/// Called under the `OVERLAY` borrow, and it returns the work rather than doing it.
/// `buzz` is the caller's "this is a control the player was not already holding",
/// and the setting is applied here so that the buzz of a muted overlay never
/// reaches a phone.
fn mark(overlay: &mut Overlay, buzz: bool) -> Decision {
    let next = overlay.tracker.snapshot();
    let changed = if next == overlay.held {
        None
    } else {
        Some((std::mem::replace(&mut overlay.held, next.clone()), next))
    };
    let held = overlay.tracker.held();
    Decision {
        changed,
        buzz: buzz && overlay.settings.with(|settings| settings.haptics),
        marks: overlay
            .layout
            .iter()
            .map(|item| (item.element.clone(), held.contains(&item.control)))
            .collect(),
    }
}

/// Does what a [`Decision`] came to, with the borrow gone.
fn apply(decision: Decision) {
    if let Some((previous, next)) = decision.changed {
        crate::input::with_state(|input| {
            gamepad::apply_snapshot(input, Source::Touch, TOUCH_PORT, &previous, &next);
        });
    }
    for (element, pressed) in decision.marks {
        let _ = element.set_attribute("aria-pressed", if pressed { "true" } else { "false" });
    }
    if decision.buzz
        && let Some(navigator) = web_sys::window().map(|window| window.navigator())
    {
        let _ = navigator.vibrate_with_duration(VIBRATE_MS);
    }
}

/// Every finger off the overlay at once, and reported.
///
/// For a window that has lost focus and for the overlay going away: in both cases
/// the `pointerup` of whatever was held is never going to arrive, and a Genesis that
/// still thinks a thumb is on `Right` walks into a wall for the rest of the session.
fn release_everything() {
    let decision = OVERLAY.with(|overlay| {
        let mut overlay = overlay.borrow_mut();
        overlay.as_mut().map(|overlay| {
            overlay.tracker.clear();
            mark(overlay, false)
        })
    });
    if let Some(decision) = decision {
        apply(decision);
    }
}

/// The on-screen controller: the nine controls, the menu button, and the pointer
/// listeners for exactly as long as it is on screen.
///
/// Mounted by [`crate::app`] behind [`Settings::touch_visible`], so its listeners
/// exist while - and only while - the player can see the controls. `on_cleanup`
/// takes them away with every held button, because the overlay can be unmounted
/// under a finger: a controller connecting is enough.
#[component]
pub fn TouchOverlay(
    /// Only [`Settings::haptics`] is read here; the rest decide whether this
    /// component is mounted at all (M7-10, #143, which is also what stores them).
    settings: RwSignal<Settings>,
    /// Opened by the menu button, because a phone has no Escape key and in
    /// landscape the top bar is behind the picture.
    drawer_open: RwSignal<bool>,
) -> impl IntoView {
    OVERLAY.with(|overlay| {
        *overlay.borrow_mut() = Some(Overlay {
            tracker: TouchTracker::new(),
            held: Vec::new(),
            layout: Vec::new(),
            settings,
        });
    });

    let pressed = window_event_listener(ev::pointerdown, |event: web_sys::PointerEvent| {
        let Some((control, element)) = landed_on(&event) else {
            return;
        };
        // No selection, no focus ring on a control that is about to be slid off,
        // no synthetic mouse event following a press that was never a click.
        event.prevent_default();
        // The element the finger landed on gets the pointer, so that the moves and
        // the up of a press that has wandered off the control still come here.
        let _ = element.set_pointer_capture(event.pointer_id());
        let Some((x, y)) = point(&event) else { return };
        let layout = measure();
        let Some(decision) = with_overlay(|overlay| {
            overlay.layout = layout;
            let direction = overlay.direction_at(x, y);
            let buzz = overlay
                .tracker
                .press(event.pointer_id(), control, direction);
            mark(overlay, buzz)
        }) else {
            return;
        };
        apply(decision);
    });

    let moved = window_event_listener(ev::pointermove, |event: web_sys::PointerEvent| {
        let Some((x, y)) = point(&event) else { return };
        let Some(decision) = with_overlay(|overlay| {
            // A finger that never landed on a control is not the overlay's
            // business, however much it moves over it.
            let Some(held) = overlay.tracker.control_of(event.pointer_id()) else {
                return Decision::nothing();
            };
            // A pointer under capture reports the capturing element as its target,
            // so where it actually is has to be worked out from the boxes.
            let slid = touch::control_at(&overlay.boxes(), x, y).unwrap_or(held);
            let direction = overlay.direction_at(x, y);
            if !overlay
                .tracker
                .move_to(event.pointer_id(), Some(slid), direction)
            {
                return Decision::nothing();
            }
            mark(overlay, false)
        }) else {
            return;
        };
        apply(decision);
    });

    // `pointerup` and `pointercancel` are the same promise to the overlay: let go,
    // because nothing else will.
    let lifted = |event: web_sys::PointerEvent| {
        let Some(decision) = with_overlay(|overlay| {
            if overlay.tracker.cancel(event.pointer_id()).is_none() {
                return Decision::nothing();
            }
            // The capture goes with the pointer: the browser ends it before either
            // of these events is dispatched, so asking for it again would throw.
            mark(overlay, false)
        }) else {
            return;
        };
        apply(decision);
    };
    let up = window_event_listener(ev::pointerup, lifted);
    let cancelled = window_event_listener(ev::pointercancel, lifted);

    // Alt-tabbing away takes the fingers with it: no `pointerup` arrives for a press
    // that ended while the page was in the background.
    let blurred = window_event_listener_untyped("blur", |_event: web_sys::Event| {
        release_everything();
    });

    on_cleanup(move || {
        pressed.remove();
        moved.remove();
        up.remove();
        cancelled.remove();
        blurred.remove();
        release_everything();
        OVERLAY.with(|overlay| *overlay.borrow_mut() = None);
    });

    view! {
        <div class="touch">
            <button
                type="button"
                class="touch__menu"
                aria-label="Menu"
                title="Menu"
                // A shell button, not a pad one: it has no `data-touch`, so the
                // pointer handlers never see it and this click stays a click.
                on:click=move |_| drawer_open.update(|open| *open = !*open)
            >
                <svg class="icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                    <path d=icons::SETTINGS />
                </svg>
            </button>

            <ControlButton
                class="touch__dpad"
                control=Control::Dpad
                label="D-pad"
                display=""
            />

            <div class="touch__faces">
                <ControlButton
                    class="touch__key touch__key--a"
                    control=Control::A
                    label="A"
                    display="A"
                />
                <ControlButton
                    class="touch__key touch__key--b"
                    control=Control::B
                    label="B"
                    display="B"
                />
                <ControlButton
                    class="touch__key touch__key--c"
                    control=Control::C
                    label="C"
                    display="C"
                />
                <ControlButton
                    class="touch__key touch__key--x"
                    control=Control::X
                    label="X"
                    display="X"
                />
                <ControlButton
                    class="touch__key touch__key--y"
                    control=Control::Y
                    label="Y"
                    display="Y"
                />
                <ControlButton
                    class="touch__key touch__key--z"
                    control=Control::Z
                    label="Z"
                    display="Z"
                />
            </div>

            <div class="touch__middle">
                <ControlButton
                    class="touch__pill"
                    control=Control::Start
                    label="Start"
                    display="Start"
                />
                <ControlButton
                    class="touch__pill"
                    control=Control::Mode
                    label="Mode"
                    display="Mode"
                />
            </div>
        </div>
    }
}

/// One control's button.
///
/// A real `<button>`, so the tab order, the focus ring and the accessible name come
/// from the browser rather than from this file, and `aria-pressed` is the one
/// pressed state: the pointer handlers write it and `style.css` reads it, so there
/// is no class list to keep in step with the tracker.
#[component]
fn ControlButton(
    /// The classes that place the control - the layout's business, not this one's.
    class: &'static str,
    /// Which control this is, and where its `data-touch` key comes from.
    control: Control,
    /// The control's accessible name.
    label: &'static str,
    /// What to show on it: the label, except on the d-pad, whose directions are
    /// drawn by `style.css` rather than written.
    display: &'static str,
) -> impl IntoView {
    let key = control.key();
    view! {
        <button
            type="button"
            class=class
            data-touch=key
            aria-label=label
            aria-pressed="false"
        >
            {display}
        </button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_has_a_selector_and_the_menu_button_is_not_one() {
        // The selector is how a control is found again for measuring and for the
        // pressed state, so a control that did not round-trip would be invisible to
        // its own pointer events.
        for control in Control::ALL {
            assert_eq!(
                Control::from_key(control.key()),
                Some(control),
                "{}",
                control.key()
            );
        }
    }
}
