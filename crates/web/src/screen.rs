//! The screen's browser half: the box the picture is drawn in, and fullscreen
//! (M7-12, #145).
//!
//! [`crate::scale`] is the arithmetic - given the room and the frame, what is the
//! picture's box. This is the part that cannot be arithmetic: a `ResizeObserver`
//! that reports the room, an effect that writes the answer into the canvas's
//! `style` attribute, and the fullscreen request that the top bar's button and
//! `F11` both ask for.
//!
//! Two rules keep the shell honest about what it draws.
//!
//! * The canvas's `style.width` and `style.height` have exactly one writer -
//!   [`use_screen`] - and `style.css` sets neither. The box comes from
//!   [`display_rect`], so a CSS rule with an opinion about the picture's aspect
//!   would be a second answer to the same question, which is how a canvas ends up
//!   stretched.
//! * The frame's size moves while a game runs (H32 is 256 wide, interlace is 448
//!   lines - see [`Controls::frame`]), and a container resize cannot see that.
//!   Both are inputs to the same effect, so both get the picture redone.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::HtmlElement;
use web_sys::ResizeObserver;
use web_sys::ResizeObserverEntry;

use crate::emulator::Controls;
use crate::scale::display_rect;
use crate::settings::Settings;

/// How long the top bar stays on screen in fullscreen after the last mouse move,
/// in ms - the "2s" of the ticket.
const REVEAL_MS: i32 = 2000;

/// The top bar, as [`Reveal`] asks the pointer where it is.
const TOPBAR: &str = ".topbar";

/// The screen: whether it is fullscreen, whether the top bar is being held out,
/// and the one control that changes either.
///
/// `Copy`, and holding only signals and a [`NodeRef`], so it travels into the
/// keyboard and pad listeners the way [`Controls`] and
/// [`Menu`](crate::menu::Menu) do.
#[derive(Clone, Copy)]
pub struct Screen {
    /// Whether the shell is fullscreen, as `fullscreenchange` reports it - the
    /// only account that survives `Escape` and the browser's own exit control.
    pub fullscreen: RwSignal<bool>,
    /// Whether the top bar is being held on screen while fullscreen.
    ///
    /// The class this drives carries the reveal and nothing else: `:fullscreen`
    /// in `style.css` is what takes the bar out of the layout, so there is no
    /// moment in which a class left behind by a missed event could hide the bar
    /// in a window that is not fullscreen at all.
    pub revealed: RwSignal<bool>,
    /// What goes fullscreen: the app shell.
    ///
    /// Not `main.screen-area`, which is what the ticket's `request_fullscreen`
    /// line names. Only the fullscreen element and its descendants are rendered,
    /// so a top bar outside the subtree could not be hidden *and* shown again on
    /// mouse move - and the ticket asks for both. The shell is the smallest
    /// element that contains the screen and the bar, so the bar can slide back
    /// over the picture instead of being missing from the page.
    shell: NodeRef<html::Div>,
}

impl Screen {
    /// Goes fullscreen, or comes back out: the top bar's button and `F11`.
    pub fn toggle_fullscreen(self) {
        if self.fullscreen.get_untracked() {
            self.exit();
        } else {
            self.enter();
        }
    }

    fn enter(self) {
        let Some(shell) = self.shell.get_untracked() else {
            return;
        };
        // `RequestFullscreen` is gated on user activation, and both callers are a
        // click or a keypress, so this is allowed by the time it is reached. A
        // browser that still says no has said no for the session; there is nothing
        // here to retry, and no second fullscreen to fall back to.
        let _ = shell.request_fullscreen();
    }

    fn exit(self) {
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            document.exit_fullscreen();
        }
    }
}

/// The observer and the callback it was built from, held together.
///
/// The callback is what JS calls; dropping it while the observer is alive would
/// leave a call to nothing, so neither can outlive the other.
struct Room {
    observer: ResizeObserver,
    _callback: Closure<dyn FnMut(js_sys::Array, ResizeObserver)>,
}

/// Mounts the screen: the resize-following canvas box, and the fullscreen
/// machine the top bar and the hotkeys drive.
///
/// Mount it next to the canvas, after [`use_cartridge`](crate::emulator::use_cartridge):
/// it reads the frame size the console publishes, and it needs the canvas and the
/// screen area to be the ones the shell just made.
///
/// `drawer_open` is the exception to the reveal timer: a player in the settings
/// drawer with a keyboard or a pad must not have the bar - which is how they leave
/// - taken away under them.
pub fn use_screen(
    canvas: NodeRef<html::Canvas>,
    area: NodeRef<html::Main>,
    shell: NodeRef<html::Div>,
    console: Controls,
    drawer_open: RwSignal<bool>,
) -> Screen {
    let settings = expect_context::<RwSignal<Settings>>();
    let fullscreen = RwSignal::new(false);
    let revealed = RwSignal::new(false);
    let room = RwSignal::new((0.0_f64, 0.0_f64));
    let over_bar = RwSignal::new(false);
    let pending = RwSignal::new(None::<i32>);
    let reveal = Reveal::new(pending, revealed, drawer_open, over_bar);

    // The room, from the container - not the window. The screen area's own
    // padding changes with the touch overlay (#142), and it is the only thing
    // that reports a drawer, a rotation and a fullscreen as the same event: a new
    // size. `content_rect` is inside the padding, which is exactly the room the
    // picture has.
    Effect::new(move |previous: Option<Option<Room>>| {
        let Some(area) = area.get() else {
            return previous.flatten();
        };
        let callback = Closure::wrap(Box::new(move |entries: js_sys::Array, _: ResizeObserver| {
            // One element is observed, so there is one entry.
            if let Ok(entry) = entries.get(0).dyn_into::<ResizeObserverEntry>() {
                let box_ = entry.content_rect();
                room.set((box_.width(), box_.height()));
            }
        }) as Box<dyn FnMut(js_sys::Array, ResizeObserver)>);
        let on_resize: &js_sys::Function = callback.as_ref().unchecked_ref();
        let Ok(observer) = ResizeObserver::new(on_resize) else {
            return previous.flatten();
        };
        observer.observe(&area);
        // Replacing is the cleanup: the screen area is one element, and putting a
        // new observer in front of the old one only makes sense once the old one
        // has stopped reporting.
        if let Some(old) = previous.flatten() {
            old.observer.disconnect();
        }
        Some(Room {
            observer,
            _callback: callback,
        })
    });

    // The box itself. Four inputs, one writer: the room, the frame, and the two
    // settings that decide what to do with them. `canvas` is read reactively, so
    // a canvas mounted by the `Show` that a cartridge arrives through is
    // measured like any other resize.
    Effect::new(move |_| {
        let Some(canvas) = canvas.get() else { return };
        let (container_w, container_h) = room.get();
        let (frame_w, frame_h) = console.frame.get();
        let (mode, aspect) = settings.with(|settings| (settings.scale, settings.aspect));
        let (width, height) =
            display_rect(container_w, container_h, frame_w, frame_h, mode, aspect);
        // `(0, 0)` is [`display_rect`]'s "there is no room yet"; writing it would
        // take the picture away rather than size it.
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        // UFCS because `ElementExt::style` - the Leptos one, for adding a style
        // attribute - is what `canvas.style()` otherwise resolves to.
        let style = HtmlElement::style(&canvas);
        let _ = style.set_property("width", &format!("{width}px"));
        let _ = style.set_property("height", &format!("{height}px"));
    });

    // The fullscreen state, from the browser. This is the only place it is
    // written, because it is the only one that sees the player pressing `Escape`
    // or using the browser's own control: our own button is the minority case.
    let fullscreen_changed = window_event_listener_untyped("fullscreenchange", {
        let reveal = reveal.clone();
        move |_| {
            let on = web_sys::window()
                .and_then(|window| window.document())
                .is_some_and(|document| document.fullscreen_element().is_some());
            fullscreen.set(on);
            if on {
                lock_landscape();
            } else {
                revealed.set(false);
                reveal.cancel();
            }
        }
    });

    // The reveal. A mousemove is the whole gesture: the bar comes back, and the
    // 2s starts again - including while the pointer is over the bar itself, which
    // is what stops it sliding away mid-click.
    let moved = window_event_listener(ev::mousemove, {
        let reveal = reveal.clone();
        move |event: web_sys::MouseEvent| {
            if !fullscreen.get_untracked() {
                return;
            }
            over_bar.set(over_topbar(&event));
            revealed.set(true);
            reveal.arm();
        }
    });

    // The drawer outranks the timer, in both directions: while it is open nothing
    // may hide the bar, and when it shuts the 2s starts again.
    Effect::new({
        let reveal = reveal.clone();
        move |_| {
            if drawer_open.get() {
                reveal.cancel();
            } else if fullscreen.get() {
                reveal.arm();
            }
        }
    });

    on_cleanup(move || {
        reveal.cancel();
        fullscreen_changed.remove();
        moved.remove();
    });

    Screen {
        fullscreen,
        revealed,
        shell,
    }
}

/// Whether the pointer is over the top bar, which decides whether the reveal may
/// expire. `closest` rather than a remembered element, because the bar is
/// re-rendered around its buttons.
fn over_topbar(event: &web_sys::MouseEvent) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .is_some_and(|target| target.closest(TOPBAR).ok().flatten().is_some())
}

/// The mobile half of fullscreen: a phone is usually held portrait and a Genesis
/// is not landscape.
///
/// Desktop Chrome rejects this, which is why the promise is awaited nowhere but
/// here: an ignored `Result` leaves an unhandled rejection in the console, and
/// this runs on every fullscreen entry on a desktop. Android Chrome is untested
/// (M7-12, #145).
fn lock_landscape() {
    let orientation = web_sys::window()
        .and_then(|window| window.screen().ok())
        .map(|screen| screen.orientation());
    let Some(orientation) = orientation else {
        return;
    };
    if let Ok(locked) = orientation.lock(web_sys::OrientationLockType::Landscape) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(locked).await;
        });
    }
}

/// The 2s the top bar stays out, as one owner of one timer.
///
/// A `mousemove` is not a place to allocate a closure, so the callback is made
/// once and left with JS (`into_js_value`) for the life of the shell; what is
/// tracked is the handle, so a second move cancels the first expiry rather than
/// queueing an earlier one behind it.
#[derive(Clone)]
struct Reveal {
    /// The armed expiry, `None` when nothing is pending.
    pending: RwSignal<Option<i32>>,
    on_fire: js_sys::Function,
}

impl Reveal {
    fn new(
        pending: RwSignal<Option<i32>>,
        revealed: RwSignal<bool>,
        drawer_open: RwSignal<bool>,
        over_bar: RwSignal<bool>,
    ) -> Self {
        let callback = Closure::wrap(Box::new(move || {
            // Cleared first: the timer has fired, so it is no longer pending,
            // whatever the pointer is doing now.
            pending.set(None);
            // Over the bar is a player about to click it; in the drawer is a
            // player who cannot see the bar and must not lose it (#145).
            if over_bar.get_untracked() || drawer_open.get_untracked() {
                return;
            }
            revealed.set(false);
        }) as Box<dyn FnMut()>);
        let on_fire: js_sys::Function = callback.as_js_value().clone().unchecked_into();
        // Ownership of the callback goes to JS: the timer may outlive this scope.
        callback.into_js_value();
        Self { pending, on_fire }
    }

    /// Restarts the 2s.
    fn arm(&self) {
        let Some(window) = web_sys::window() else {
            return;
        };
        self.cancel();
        if let Ok(handle) =
            window.set_timeout_with_callback_and_timeout_and_arguments_0(&self.on_fire, REVEAL_MS)
        {
            self.pending.set(Some(handle));
        }
    }

    fn cancel(&self) {
        let handle = self.pending.update_untracked(|handle| handle.take());
        if let (Some(window), Some(handle)) = (web_sys::window(), handle) {
            window.clear_timeout_with_handle(handle);
        }
    }
}
