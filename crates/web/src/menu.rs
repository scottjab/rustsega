//! The menu mode: what an open menu does to the console, and what a
//! [`UiAction`] does to the page (M7-11, #144).
//!
//! [`crate::nav`] decided all of this as data - which control a direction picks,
//! how fast a held direction walks, what a key or a pad button means. This is the
//! half that owns the DOM: it moves the real focus, so the `:focus-visible` ring
//! the browser draws follows the d-pad and the page never has to know that
//! spatial navigation exists. A class that looks like the focus would have been
//! the easy version, and it would have left a screen reader and a `Tab` key
//! agreeing with neither.
//!
//! Two rules shape it. The walk goes to the page's own focusable controls rather
//! than to a list the drawer keeps, because `Tab` already reaches exactly those
//! and a d-pad that went somewhere else would be a second, worse way round. And
//! the console is only ever held by a menu, never started by one - see
//! [`MenuPause`](crate::nav::MenuPause).

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlElement;

use crate::controls::CaptureTarget;
use crate::emulator::Controls;
use crate::input::Source;
use crate::input::with_state;
use crate::library::LibraryOpen;
use crate::nav::Dir;
use crate::nav::LastDevice;
use crate::nav::MenuPause;
use crate::nav::Rect;
use crate::nav::UiAction;
use crate::nav::UiMode;
use crate::nav::next_focus;
use crate::ui::select::Listboxes;

/// The page's focusable controls: real controls from M7-02 (#135), plus anything
/// else that asks for the tab order.
const FOCUSABLE: &str = "button, input, select, [tabindex]:not([tabindex='-1'])";

/// The class [`crate::ui::drawer::Drawer`] puts on itself while it is showing, and
/// the selector [`inside_drawer`] asks controls about.
const OPEN_DRAWER: &str = ".drawer--open";

/// The control a menu focuses first when the page marks one (M8-04, #158).
///
/// The library's search box is the case: a walk that started on the top bar would
/// leave the first keystroke nowhere, and a walk that always focused the first
/// control of the page would put it on the Close button. Marking it is the view's
/// business, so this is a selector rather than a list of components the menu knows
/// about by name.
const FOCUS_FIRST: &str = ".menu-focus-first";

/// How many times [`Menu::focus_first`] asks for the focus before it gives up.
const FOCUS_TRIES: u32 = 16;
/// How long it waits between those asks, in ms.
const FOCUS_WAIT_MS: i32 = 25;

/// The shell's menu mode: the one place that turns a [`UiAction`] into something
/// done to the page, and the one place that knows whether the menu or the player
/// is holding the console.
///
/// `Copy`, and holding only signals, so it travels into listeners and callbacks
/// the way [`Controls`] does.
#[derive(Clone, Copy)]
pub struct Menu {
    /// The console, which an open menu holds.
    pub console: Controls,
    /// Whether the save state panel is showing: the other menu this shell has,
    /// and the innermost thing `Escape` has to close (M7-15, #148).
    pub states_open: RwSignal<bool>,
    /// Whether the ROM library is on screen (M8-04, #158): the third thing this
    /// shell walks, and what makes the library's rows the d-pad's to move.
    pub library_open: LibraryOpen,
    /// Whether the settings drawer is open, which is what `mode` is read from.
    pub drawer_open: RwSignal<bool>,
    /// The controls page's rebind capture: the innermost thing the page has.
    pub capture: RwSignal<Option<CaptureTarget>>,
    /// The page's lists of options, so that [`Menu::back`] can close one.
    lists: Listboxes,
    /// Which half of the page the shell is in.
    ///
    /// Derived from the three signals that can open something rather than set beside
    /// them, because a fourth signal saying "a menu is open" is a fourth fact to keep
    /// true. Anything new that takes the page - a dialog - adds itself here and to
    /// [`Menu::back`].
    pub mode: Signal<UiMode>,
    /// What the player last touched, which decides the hint bar's glyphs.
    pub hints: RwSignal<LastDevice>,
    /// Whether the console is held by the menu rather than by the player.
    paused_by_menu: RwSignal<MenuPause>,
    /// Where the focus was when the menu last moved it.
    ///
    /// The browser drops the focus to `<body>` when the focused control is taken
    /// out of the page - which is what closing a list of options does - and a walk
    /// with nowhere to call `from` has nothing to measure `nearest` from either.
    last_box: RwSignal<Option<Rect>>,
}

/// The shell's [`Menu`], mounted for as long as the component that calls this is.
///
/// Call it from `App`, next to [`crate::keyboard_events::use_keyboard`]: entering
/// and leaving the menu is one effect on the one signal that decides it, and that
/// effect belongs to the whole shell rather than to any one control.
pub fn use_menu(
    console: Controls,
    drawer_open: RwSignal<bool>,
    states_open: RwSignal<bool>,
    library_open: LibraryOpen,
    capture: RwSignal<Option<CaptureTarget>>,
    lists: Listboxes,
) -> Menu {
    let mode = Signal::derive(move || {
        if drawer_open.get() || states_open.get() || library_open.0.get() {
            UiMode::Menu
        } else {
            UiMode::Playing
        }
    });
    let menu = Menu {
        console,
        drawer_open,
        states_open,
        library_open,
        capture,
        lists,
        mode,
        hints: RwSignal::new(LastDevice::default()),
        paused_by_menu: RwSignal::new(MenuPause::default()),
        last_box: RwSignal::new(None),
    };

    // The first run only records the mode: the shell starts with no menu open, and
    // a menu that has not been entered has nothing to leave.
    Effect::new(move |previous: Option<UiMode>| {
        let mode = mode.get();
        if let Some(previous) = previous
            && previous != mode
        {
            match mode {
                UiMode::Menu => menu.enter(),
                UiMode::Playing => menu.leave(),
            }
        }
        mode
    });

    menu
}

impl Menu {
    /// Does what the menu was asked to do, whatever device asked for it.
    ///
    /// `Escape` arrives here as [`UiAction::ToggleMenu`] and a pad's `C` as
    /// [`UiAction::Back`], and inside an open menu they are the same act seen from
    /// opposite ends: the drawer is the outermost thing there is, so closing the
    /// innermost thing *is* leaving the menu once nothing else is open.
    pub fn run(self, action: UiAction) {
        match action {
            UiAction::Move(dir) => self.move_focus(dir),
            UiAction::Confirm => self.confirm(),
            UiAction::Back => self.back(),
            UiAction::ToggleMenu => self.toggle_menu(),
        }
    }

    /// Opens the menu, or leaves it: the shell's `Escape`, and a pad's `home`.
    pub fn toggle_menu(self) {
        match self.mode.get_untracked() {
            UiMode::Menu => self.back(),
            UiMode::Playing => self.drawer_open.update(|open| *open = !*open),
        }
    }

    /// Opens the save state panel, and shuts the settings drawer on the way past
    /// it (M7-15, #148).
    ///
    /// The two are mutually exclusive because both are a `.drawer--open`, and
    /// [`crate::menu`]'s own focus rule - "the first control of the open drawer" -
    /// has one drawer in mind. Two open at once would be a panel whose focus
    /// could be in either.
    pub fn open_states(self) {
        self.drawer_open.set(false);
        self.states_open.set(true);
    }

    /// Closes the innermost open thing, and nothing outside it.
    ///
    /// The order is the page's own depth: a list of options is inside a settings
    /// row, a rebind capture is inside a binding cell, and both are inside the
    /// drawer. A dialog goes between the capture and the drawer when there is one
    /// to close (M7-15, #148).
    pub fn back(self) {
        if self.lists.close_any() {
            return;
        }
        if self.capture.get().is_some() {
            self.capture.set(None);
            return;
        }
        // The save state panel is a drawer of its own and it is on top of the
        // settings drawer, which is why it closes first (M7-15, #148).
        if self.states_open.get() {
            self.states_open.set(false);
            return;
        }
        if self.drawer_open.get() {
            self.drawer_open.set(false);
            return;
        }
        // The library is the outermost thing of all: it is the page behind the
        // drawer, so it is the last thing `Back` reaches rather than the first.
        if self.library_open.0.get() {
            self.library_open.0.set(false);
        }
    }

    /// The player pressed the shell's pause, so the console is theirs again.
    ///
    /// Without this the menu would let go of a game on the way out that the player
    /// stopped on purpose while it was open, and the top bar would be showing
    /// `Play` over a game nobody asked to run (M7-11, #144).
    pub fn pause_by_hand(self) {
        self.edit_pause(MenuPause::by_hand);
        self.console.toggle();
    }

    /// Reads, changes and writes back the one signal that says who is holding the
    /// console, and answers what the change had to say.
    ///
    /// [`MenuPause`] is a value rather than a signal of booleans because it is a
    /// rule with three cases, and a rule is easier to read - and to test, see
    /// [`crate::nav`] - than three places that have to agree about a `bool`.
    fn edit_pause<T>(self, change: impl FnOnce(&mut MenuPause) -> T) -> T {
        let mut pause = self.paused_by_menu.get_untracked();
        let answer = change(&mut pause);
        self.paused_by_menu.set(pause);
        answer
    }

    /// Records what the player is holding now, for the hint bar.
    pub fn announce(self, device: LastDevice) {
        if self.hints.get_untracked() != device {
            self.hints.set(device);
        }
    }

    /// A menu has opened: hold the console, let go of the game's inputs, and put
    /// the focus where the player is looking.
    fn enter(self) {
        if self.edit_pause(|pause| pause.enter(!self.console.paused.get())) {
            self.console.toggle();
        }
        // Fast forward is a hold, and a menu takes the pads away from
        // `poll_hotkey` - so nothing would ever report the release of a trigger held
        // on the way into the menu, and the console would come back out of it at 4x
        // with nobody asking. Letting go is the same release the keyboard's keyup
        // makes (M7-17, #150).
        self.console.set_fast_forward(false);
        release_game_input();
        // The drawer's open class and its `visibility` are applied by effects
        // registered after this one, because `App` mounts the menu before it
        // renders the drawer. A microtask is the first moment its controls are
        // focusable at all, and it is still inside the same frame.
        spawn_local(async move {
            self.focus_first();
        });
    }

    /// A menu has closed: let go of only what the menu picked up, and let nothing
    /// the player was holding on the way in leak out to the game.
    fn leave(self) {
        if self.edit_pause(MenuPause::leave) {
            self.console.toggle();
        }
        release_game_input();
        if let Some(element) = active_element() {
            let _ = element.blur();
        }
    }

    /// Moves the focus to the neighbour of the focused control in `dir`.
    ///
    /// Does nothing when [`next_focus`] says there is nothing that way, which is
    /// how a walk stops at the edge of the page instead of wrapping round it.
    fn move_focus(self, dir: Dir) {
        let focusables = focusables();
        let Some(current) = self.current_box(&focusables) else {
            return;
        };
        let boxes: Vec<Rect> = focusables.iter().map(|(_, rect)| *rect).collect();
        let Some(index) = next_focus(current, &boxes, dir) else {
            return;
        };
        if let Some((element, _)) = focusables.get(index) {
            self.focus(element);
        }
    }

    /// Puts the focus on the control the open page asks for, or on the first control
    /// of the open drawer - which is the first thing a player who has just opened the
    /// menu is looking at.
    ///
    /// Tries a few times, because a drawer that is sliding in is still
    /// `visibility: hidden` for the length of its transition - see the transition
    /// on `.drawer` in `style.css`, which says it should flip at once on the way
    /// in and in practice does not - and a hidden control cannot take the focus.
    /// A retry lands as soon as the drawer is focusable and then stops, which is
    /// what a guessed wait of exactly one transition would have been for.
    fn focus_first(self) {
        self.focus_first_trying(FOCUS_TRIES);
    }

    fn focus_first_trying(self, tries: u32) {
        if self.focus_first_now() || tries == 0 {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let menu = self;
        let callback =
            Closure::wrap(Box::new(move || menu.focus_first_trying(tries - 1)) as Box<dyn FnMut()>);
        let on_fire: &js_sys::Function = callback.as_js_value().unchecked_ref();
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(on_fire, FOCUS_WAIT_MS)
            .is_err()
        {
            return;
        }
        // The timer outlives this scope, so ownership of the closure goes to JS
        // and the garbage collector frees it - the same bargain a toast's timer
        // strikes (see [`crate::ui::toast`]).
        callback.into_js_value();
    }

    /// One attempt at [`focus_first`], saying whether the focus moved.
    fn focus_first_now(self) -> bool {
        let showing = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(OPEN_DRAWER).ok())
            .flatten()
            .is_some();
        let focusables = focusables();
        let reachable = |element: &HtmlElement| !showing || inside_drawer(element);
        let marked = focusables.iter().find(|(element, _)| {
            reachable(element) && element.matches(FOCUS_FIRST).unwrap_or(false)
        });
        let first = marked.or_else(|| focusables.iter().find(|(element, _)| reachable(element)));
        match first {
            Some((element, _)) => {
                self.focus(element);
                true
            }
            None => false,
        }
    }

    /// Activates the focused control: what `Enter` and a pad's `B` do to a button,
    /// a switch and an option in a list - and to a slider, where a click is worth
    /// nothing and does no harm either.
    fn confirm(self) {
        if let Some(element) = active_element() {
            element.click();
        }
    }

    /// The box the walk starts from: the focused control's, or where the focus was
    /// when the menu last moved it, or the first control on the page.
    fn current_box(self, focusables: &[(HtmlElement, Rect)]) -> Option<Rect> {
        if let Some(active) = active_element()
            && let Some(rect) = visible_box(&active)
            // Matched by identity, because two controls can be the same size in
            // the same place and it is the element that has the focus.
            && focusables
                .iter()
                .any(|(element, _)| element.is_same_node(Some(&active)))
        {
            return Some(rect);
        }
        self.last_box
            .get()
            .or_else(|| focusables.first().map(|(_, rect)| *rect))
    }

    /// Focuses `element` and remembers where it was, so the walk can go on from
    /// there even after the browser has taken the focus back out of the page.
    fn focus(self, element: &HtmlElement) {
        if element.focus().is_ok()
            && let Some(rect) = visible_box(element)
        {
            self.last_box.set(Some(rect));
        }
    }
}

/// Every control the walk can reach, in DOM order, with its box.
///
/// A `Vec` of pairs rather than a list of elements because [`next_focus`] wants
/// the boxes and the DOM does not hand them out in bulk; the two stay paired so
/// the index it answers with is the index of the control to focus.
fn focusables() -> Vec<(HtmlElement, Rect)> {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return Vec::new();
    };
    let Ok(nodes) = document.query_selector_all(FOCUSABLE) else {
        return Vec::new();
    };
    let mut found: Vec<(HtmlElement, Rect)> = Vec::new();
    for index in 0..nodes.length() {
        let Some(element) = nodes.item(index).and_then(|node| node.dyn_into().ok()) else {
            continue;
        };
        if let Some(rect) = visible_box(&element) {
            found.push((element, rect));
        }
    }
    found
}

/// `element`'s box, or `None` when no player can see it or reach it.
///
/// `visibility` is the one that matters here: the drawer hides itself that way
/// rather than by going away, so that the slide-in has something to animate, and
/// the browser still measures the box of a hidden element and still matches it
/// with `querySelectorAll`. A box of no size and `display: none` cover the rest,
/// and `:disabled` is how the top bar says *no cartridge, no Reset* - a control
/// that cannot be pressed is not a step in a walk.
fn visible_box(element: &HtmlElement) -> Option<Rect> {
    let edge = element.get_bounding_client_rect();
    if edge.width() <= 0.0 || edge.height() <= 0.0 {
        return None;
    }
    if element.matches(":disabled").unwrap_or(false) {
        return None;
    }
    let style = web_sys::window()?
        .get_computed_style(element)
        .ok()
        .flatten()?;
    // A property the browser would not name counts as an element that cannot be
    // seen, which is the safe way round for a walk that would otherwise focus
    // something off the screen.
    if style.get_property_value("visibility").unwrap_or_default() != "visible"
        || style.get_property_value("display").unwrap_or_default() == "none"
    {
        return None;
    }
    Some(Rect::new(
        edge.left(),
        edge.top(),
        edge.width(),
        edge.height(),
    ))
}

/// Whether `element` is in the drawer that is showing right now.
fn inside_drawer(element: &HtmlElement) -> bool {
    element.closest(OPEN_DRAWER).ok().flatten().is_some()
}

/// Whether the focused control is a slider, whose left and right arrows the
/// browser spends on its value - which is what those two arrows are for on one
/// (M7-11, #144), and why the walk leaves them to the browser.
pub fn slider_has_focus() -> bool {
    active_element().is_some_and(|element| {
        element.tag_name() == "INPUT"
            && element
                .get_attribute("type")
                .is_some_and(|kind| kind == "range")
    })
}

/// The focused control, when there is one and it is something with a box.
fn active_element() -> Option<HtmlElement> {
    web_sys::window()?
        .document()?
        .active_element()
        .and_then(|element| element.dyn_into().ok())
}

/// Every game input let go at once.
///
/// The menu is the shell's, so nothing held on the way in may still be held on
/// the way out: a direction into a menu is not a direction out of one. The pad's
/// own poll cannot be the only thing that notices, because while a menu is open it
/// deliberately reports nothing (see [`crate::gamepad_events`]).
fn release_game_input() {
    with_state(|input| {
        input.clear_source(Source::Keyboard);
        input.clear_source(Source::Touch);
        // The Gamepad API numbers pads from 0 and has not been seen to go past 3
        // (see [`crate::gamepad_events`], which makes the same assumption).
        for index in 0..4 {
            input.clear_source(Source::Gamepad(index));
        }
    });
}
