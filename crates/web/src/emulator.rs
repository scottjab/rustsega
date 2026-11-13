//! The emulation loop: `requestAnimationFrame`, the canvas, pause and reset
//! (M7-04, #137).
//!
//! [`Emulator`] owns one running [`Genesis`] and the canvas it paints. It lives in
//! a `thread_local!` behind an `Rc<RefCell<_>>` rather than in a Leptos signal,
//! which is what the ticket asks for and for two reasons: the console is the size
//! of the machine, so anything that read it as a value would clone megabytes per
//! frame, and neither `Genesis` nor the canvas element it borrows is `Send` - and
//! [`Callback`] holds a `Send + Sync` closure. So the console is owned by the
//! component that mounts the canvas, and what the shell gets instead is
//! [`Controls`]: a `Copy` handle over the pause state, which is a signal.
//!
//! Sound is [`crate::audio`], and while its clock is running that clock is what
//! decides how many frames [`Emulator::tick`] runs. Input is [`crate::input`],
//! the page's one merged pad state: this module only reads it, once per frame
//! (M7-07, #140).
//!
//! [`genesis::Genesis::drain_audio`] is called once per frame, and only while the
//! output is running: the mixer clears its chip buffers at the start of every
//! frame and appends only to a buffer the caller owns, so an undrained console
//! does not grow.

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::rc::Weak;

use genesis::Config;
use genesis::Genesis;
use leptos::html;
use leptos::prelude::*;
use vdp::FrameBuffer;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;

use crate::audio::AudioOut;
use crate::audio_pacing::MAX_FRAMES_PER_TICK;
use crate::audio_pacing::TARGET_AHEAD_SECS;
use crate::audio_pacing::frames_to_run;
use crate::frame_pacer::FramePacer;
use crate::render::FilterSetting;
use crate::render::Renderer;
use crate::render::RendererKind;
use crate::render::RendererSetting;
use crate::render::canvas2d::Canvas2dRenderer;
use crate::render::choose_renderer_latched;
use crate::render::webgl::WebGlRenderer;
use crate::render::webgl::gl_lost;
use crate::render::webgl::webgl2_supported;
use crate::rom::LoadedRom;
use crate::settings::Settings;
use crate::storage;
use crate::storage::SramWatcher;
use crate::ui::toast::ToastKind;
use crate::ui::toast::Toasts;

/// The frame rate of the NTSC machine, as the pacer wants it.
///
/// The exact raster rate is 53_693_175 / (3420 * 262) = 59.92 Hz (docs/
/// ARCHITECTURE.md, "Clocks and timing"), and `FramePacer` is a stand-in for the
/// audio clock that will pace this properly in M7-05 (#138); a tenth of a percent
/// is below anything a person can see, and below anything the pacer tests care
/// about.
const NTSC_HZ: f64 = 60.0;

/// The frame rate of the PAL machine: 53_203_424 / (3420 * 313) = 49.70 Hz
/// exactly, and 50 for the same reason [`NTSC_HZ`] is 60.
const PAL_HZ: f64 = 50.0;

/// The browser events that mean "a person is here, take the audio".
///
/// `pointerdown` covers a mouse and a touch, `keydown` a keyboard-only player; a
/// gamepad button has to be polled and so belongs to the input tickets
/// (M7-07..09). They stay attached for the life of a console: a `resume()` on a
/// context that is already running asks for nothing.
const WAKE_EVENTS: [&str; 2] = ["pointerdown", "keydown"];

/// The events that mean this page may be going away, or going quiet, so the
/// cart's RAM should be offered to storage now rather than on the timer
/// (M7-14, #147).
///
/// Both are on `window`, and both are answered by [`flush_running_sram`].
const LIFECYCLE_EVENTS: [&str; 2] = ["pagehide", "visibilitychange"];

/// One [`LIFECYCLE_EVENTS`] listener: the event it is on `window` for, and the
/// closure `window` is holding - which is what has to be handed back to
/// `remove_event_listener_with_callback` for the listener to actually go away.
type LifecycleListener = (&'static str, Closure<dyn FnMut()>);

/// How often the frame loop asks [`SramWatcher`] whether the cart's RAM has
/// moved: the "check once per second" of the ticket.
///
/// Not once per frame, which is what the loop could do for nothing: a cart's RAM
/// is 8 KiB, and sixty comparisons a second of it would be work for a watcher
/// whose own window is twice as long as that.
const SRAM_CHECK_INTERVAL_MS: f64 = 1_000.0;

/// How long the "Saved" chip stays up, in milliseconds: the 1.5 s of the ticket.
#[cfg(target_arch = "wasm32")]
const SAVED_CHIP_MS: i32 = 1_500;

/// Why a cartridge would not start, in the one sentence a toast shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// [`Genesis::new`] refused the image: too small to hold a header at 0x100,
    /// which is the only thing it can refuse a ROM that [`crate::rom::load_rom`]
    /// has already accepted for display.
    NotARom,
    /// The canvas would not give up a 2d context, so there is nothing to draw
    /// with. Canvas is switched off, or the element is not a canvas.
    NoCanvas,
    /// `requestAnimationFrame` refused the callback, so there is no loop to run.
    NoAnimationFrames,
}

impl core::fmt::Display for StartError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotARom => write!(f, "The console could not start this ROM"),
            Self::NoCanvas => write!(f, "This browser will not draw to the screen"),
            Self::NoAnimationFrames => {
                write!(f, "This browser stopped giving the screen animation frames")
            }
        }
    }
}

/// The console rate `Genesis::run_frame` has to be called at for `bytes` run
/// under `config`: 50 Hz for a PAL machine, 60 Hz for an NTSC one.
///
/// The same three steps [`Genesis::new`] takes - normalize, header,
/// [`Config::resolve_region`] - because `Genesis` does not expose the region it
/// resolved, and it is `config`'s preference order that decides, not the region
/// string in [`LoadedRom::region`]: that lists every region a cartridge *claims*,
/// so a `JUE` release would read as PAL here and run 4 % slow in play.
///
/// `bytes` is the image as it was read, before [`Genesis::new`] normalized it.
pub fn target_hz(config: &Config, bytes: &[u8]) -> f64 {
    let rom = genesis::normalize_rom(bytes);
    match genesis::parse_header(&rom) {
        Ok(header) if config.resolve_region(&header).is_pal() => PAL_HZ,
        // Includes the header-less case [`crate::rom::load_rom`] let through for
        // its own reasons: homebrew on a 512 byte image, which the console itself
        // runs as the NTSC export machine.
        _ => NTSC_HZ,
    }
}

/// `frame` as RGBA8888 bytes, ready for [`web_sys::ImageData`].
///
/// `FrameBuffer::pixels` is `0xAABBGGRR` (docs/ARCHITECTURE.md, "vdp"), and one of
/// those in little endian memory is the bytes R, G, B, A: `to_le_bytes` is the
/// whole conversion, with nothing to swap. `testkit::dump_png` writes PNGs the
/// same way, for the same reason.
///
/// `out` is cleared and refilled rather than returned, because the loop holds one
/// of these for its whole life and must not allocate per frame. Only
/// `width * height` of a frame buffer is valid - it is sized for the largest
/// picture the VDP can make - so only those pixels are read.
pub fn frame_to_rgba_bytes(frame: &FrameBuffer, out: &mut Vec<u8>) {
    let pixels = frame.width * frame.height;
    out.clear();
    out.reserve(pixels * 4);
    for pixel in frame.pixels.iter().take(pixels) {
        out.extend_from_slice(&pixel.to_le_bytes());
    }
}

// The console this tab is running: `None` until a cartridge is on the canvas, and
// replaced - never shared - when another ROM arrives.
//
// A thread local rather than a context or a signal because it holds an `Rc`, and
// because the animation-frame callback, a file drop and a button click are all on
// this one thread and need one owner.
thread_local! {
    static RUNNING: RefCell<Option<Rc<RefCell<Emulator>>>> = const { RefCell::new(None) };
    /// Counts ROM loads, so a load that was superseded while it was waiting for
    /// IndexedDB can see that and not install a console for a cartridge the
    /// player has already moved on from.
    static LOADING: Cell<u32> = const { Cell::new(0) };
    /// The [`LIFECYCLE_EVENTS`] listeners this page put on `window`, held so they
    /// can be taken off it again: `window` outlives the component that added them,
    /// and a `Closure` the page cannot remove is a `Closure` forever.
    static LIFECYCLE: RefCell<Vec<LifecycleListener>> = const { RefCell::new(Vec::new()) };
}

/// The running console, if there is one.
fn running() -> Option<Rc<RefCell<Emulator>>> {
    RUNNING.with(|running| running.borrow().clone())
}

/// Marks a ROM load the newest one and returns its token: see [`load_is_current`].
fn begin_load() -> u32 {
    LOADING.with(|loading| {
        let token = loading.get() + 1;
        loading.set(token);
        token
    })
}

/// Whether `token` still names the newest ROM load.
///
/// Every [`stop`] and every [`start`] takes the token off a load that is still
/// waiting on storage, which is what keeps a quick pair of ROM switches from
/// installing the console of the ROM that was dropped first.
fn load_is_current(token: u32) -> bool {
    LOADING.with(|loading| loading.get() == token)
}

/// Stops the running console, if any: saves it, cancels its animation frame, and
/// lets it go.
fn stop() {
    // Retiring a console is one of the two moments a cart's last frames of RAM
    // have to be handed over - the other is a page going away - and it is
    // synchronous, so the write is offered now and IndexedDB takes it up on the
    // microtask after this borrow is gone.
    //
    // Taking it out of the slot before stopping it is what frees the machine -
    // the loop itself only holds a `Weak` (see [`Emulator::start`]).
    let emulator = RUNNING.with(|running| running.borrow_mut().take());
    if let Some(emulator) = emulator {
        let mut emulator = emulator.borrow_mut();
        emulator.flush_sram(now_ms(), true);
        emulator.stop();
    }
    // ... and a load still waiting on storage for the console that is going away
    // is a load nobody is waiting for any more.
    begin_load();
}

/// Offers the running console's RAM to storage now, if there is a running console
/// and a save to make (M7-14, #147).
fn flush_running_sram() {
    let Some(emulator) = running() else { return };
    emulator.borrow_mut().flush_sram(now_ms(), true);
}

/// What storage had for a cartridge when it was loaded: the database to write
/// back to, and the RAM it holds for this ROM.
///
/// `stored` being `None` is a cart that has never been saved; the whole of this
/// being `None` is a browser that would not open IndexedDB at all.
struct SavedSram {
    db: storage::Db,
    stored: Option<Vec<u8>>,
}

/// The save-RAM half of a running console: the database its RAM goes back to,
/// the key it is stored under, and the watcher that decides when.
///
/// Held as `Option<Save>` by [`Emulator`]: `None` for a cart with no
/// battery-backed RAM, and for a cart whose IndexedDB would not open. In both
/// cases the game runs, nothing is written, and there is nothing to lose by it.
struct Save {
    db: storage::Db,
    rom_id: String,
    watcher: SramWatcher,
}

/// Gives `genesis` the save storage had for it, and returns the [`Save`] half of
/// the console - `None` when there is nothing to save.
///
/// Nothing to save means one of two ordinary things: the cart has no
/// battery-backed RAM, which is what a `None` from [`Genesis::sram`] means and is
/// checked first precisely so that a `None` can never become an empty record that
/// blanks a real save; or this browser would not open IndexedDB, in which case
/// [`read_save`] has already said so once.
///
/// The watcher is seeded with the RAM as it stands *after* the save is loaded, so
/// a cart that is loaded, played and never written to is never written back.
fn install_save(genesis: &mut Genesis, saved: Option<SavedSram>, rom_id: &str) -> Option<Save> {
    genesis.sram()?;
    let SavedSram { db, stored } = saved?;
    if let Some(stored) = stored {
        genesis.load_sram(&stored);
    }
    Some(Save {
        db,
        rom_id: rom_id.to_string(),
        watcher: SramWatcher::new(genesis.sram().unwrap_or_default(), now_ms()),
    })
}

/// The write itself: one cart's RAM, into one record, off the frame loop.
///
/// A failure is a toast and never a panic - private mode and a full disk are
/// ordinary facts about a browser, and a page that dies on one takes the game and
/// the player's place in it with it (M7-14, #147). Success is the "Saved" chip,
/// because the chip means *IndexedDB has this* and only this much is known here.
async fn write_saved_sram(
    db: storage::Db,
    rom_id: String,
    bytes: Vec<u8>,
    console: Controls,
    toasts: Toasts,
) {
    match db.put_sram(&rom_id, &bytes).await {
        Ok(()) => console.mark_saved(),
        Err(error) => toasts.push(ToastKind::Warn, error.to_string()),
    }
}

/// Starts `rom` on `canvas`, replacing whatever was running, with its saved RAM
/// already in the cart.
///
/// Replacing, not adding: a second console drawing into one canvas is the failure
/// mode of "load another ROM without reloading the page", and it also costs
/// another megabyte of machine.
///
/// The new console is made after the save has been read rather than before it,
/// which is the whole reason this is a spawned task: the frames between a cart
/// starting and its save arriving are frames in which the game can overwrite the
/// RAM it is about to be handed. One read of one 8 KiB record is what the first
/// picture waits for (the ticket's "load SRAM before the first frame").
fn start(
    rom: &LoadedRom,
    canvas: &HtmlCanvasElement,
    canvas_gl: &HtmlCanvasElement,
    console: Controls,
    settings: RwSignal<Settings>,
    toasts: Toasts,
) {
    // Before the new console asks for a frame of its own: the old one's request
    // has to be cancelled, not merely dropped along with its callback, or the
    // browser calls back into a closure that has gone - and its RAM has to be
    // offered to storage while it still exists to read it.
    stop();
    let token = begin_load();
    // Owned handles for the task: a borrow of a signal, or of a `NodeRef`, is not
    // something to be holding across an `.await` (#387), and a canvas element is a
    // reference-counted handle that cloning keeps alive if the component is
    // unmounted while storage is being asked.
    let rom = rom.clone();
    let canvas = canvas.clone();
    let canvas_gl = canvas_gl.clone();
    spawn_local(async move {
        let saved = read_save(&rom.id, toasts).await;
        if !load_is_current(token) {
            return;
        }
        match Emulator::start(&rom, &canvas, &canvas_gl, console, settings, saved, toasts) {
            Ok(emulator) => RUNNING.with(|running| *running.borrow_mut() = Some(emulator)),
            // The same one sentence the caller of a synchronous `start` used to
            // show: by now the only reason left is that the browser has nothing
            // to draw on or has stopped giving frames.
            Err(error) => toasts.push(ToastKind::Danger, error.to_string()),
        }
    });
}

/// The database handle and this ROM's saved RAM: what [`Emulator::start`] needs
/// before it will make a console.
///
/// `None` means this browser would not open IndexedDB at all. The toast is pushed
/// here, and once per load only: with no database there is no periodic write
/// either, so a player in private mode is told once rather than every two
/// seconds (M7-14, #147).
async fn read_save(rom_id: &str, toasts: Toasts) -> Option<SavedSram> {
    let db = match storage::open().await {
        Ok(db) => db,
        Err(error) => {
            toasts.push(ToastKind::Warn, error.to_string());
            return None;
        }
    };
    match db.get_sram(rom_id).await {
        Ok(stored) => Some(SavedSram { db, stored }),
        Err(error) => {
            // A save that cannot be read is not a game that cannot be played:
            // one warning, and a cart that starts from its own power-on RAM. The
            // write path stays alive, since a read that failed once is not a
            // write that will.
            toasts.push(ToastKind::Warn, error.to_string());
            Some(SavedSram { db, stored: None })
        }
    }
}

/// The clock the save watcher is timed against: `performance.now()`, which is the
/// timebase an animation frame's own timestamp is on, so a write timed from the
/// loop and a write timed from a page event are on the same ruler.
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or(0.0)
}

/// Whether this page is in the background, which is the half of
/// `visibilitychange` worth flushing (M7-14, #147).
fn page_is_hidden() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_some_and(|document| document.hidden())
}

/// Puts the two last-chance listeners of the ticket on `window`: `pagehide`, and
/// `visibilitychange` to hidden.
///
/// Called from [`use_cartridge`], which takes them off again - `window` outlives
/// the component, and a closure kept by `window` after its owner is gone is a
/// warning in the console at best and a borrowed `RefCell` at worst (#387).
///
/// A write started during `pagehide` is the browser's to finish or not: it is
/// IndexedDB's best-effort moment, not a commit. What it buys is that a tab closed
/// a moment after an in-game save gets that save, where the 2 s watcher would
/// almost certainly not have.
fn use_lifecycle_flush() {
    let Some(window) = web_sys::window() else {
        return;
    };
    for event in LIFECYCLE_EVENTS {
        let listener = Closure::wrap(Box::new(move || {
            // `visibilitychange` fires in both directions, and a page that has
            // just become visible has nothing it was about to lose. `pagehide` is
            // already the way out, so it needs no test.
            if event == "visibilitychange" && !page_is_hidden() {
                return;
            }
            flush_running_sram();
        }) as Box<dyn FnMut()>);
        let _ = window.add_event_listener_with_callback(event, listener.as_ref().unchecked_ref());
        LIFECYCLE.with(|held| held.borrow_mut().push((event, listener)));
    }
    on_cleanup(remove_lifecycle_flush);
}

/// Takes the [`LIFECYCLE_EVENTS`] listeners off `window` again, which the shell's
/// cleanup does.
///
/// A plain `fn`, because `on_cleanup` wants a `Send` closure and a `Closure` is
/// not one: the closures themselves live in a thread local, which is also what
/// keeps a second mount from adding a second pair.
fn remove_lifecycle_flush() {
    LIFECYCLE.with(|held| {
        let held = held.take();
        let Some(window) = web_sys::window() else {
            return;
        };
        for (event, listener) in held.iter() {
            let _ = window
                .remove_event_listener_with_callback(event, listener.as_ref().unchecked_ref());
        }
    });
}

/// What the shell can ask the running console to do.
///
/// `Copy`, and holding only a signal, so it can be captured by a [`Callback`] -
/// which a `Genesis` could not be, being neither `Send` nor cheap to reach.
#[derive(Clone, Copy)]
pub struct Controls {
    /// Whether the console is being held. The top bar's one button shows this:
    /// pause when it is running, play when it is held.
    pub paused: RwSignal<bool>,
    /// Whether the audio output is running, which is how the shell knows to ask
    /// for a gesture: a context the browser is holding back is silent, and the
    /// only thing that can start it is a person (M7-05, #138).
    pub sound_on: RwSignal<bool>,
    /// The size of the frame the console is drawing - the *frame*, which on the
    /// WebGL path is not the size of the canvas's drawing buffer (M7-13, #146) -
    /// or `(0, 0)` until a console has painted once.
    ///
    /// The picture the shell scales is this big, so [`crate::screen`] reads it:
    /// an H32 game is 256 wide rather than 320 and an interlaced one is 448 lines
    /// rather than 224, and both change mid-play (M7-12, #145). It is written
    /// from [`Emulator::start`] and from [`Emulator::paint`] on the frames whose
    /// size really moved, which is the only reason a screen has to be redone at
    /// all - a container resize cannot see it.
    pub frame: RwSignal<(u32, u32)>,
    /// Which drawing path the console is on: the live answer, rather than the
    /// `renderer` setting, which is only what the player asked for (M7-13, #146).
    ///
    /// The shell needs it because the two paths draw on two different canvas
    /// elements - a canvas belongs to one context type for its whole life, so a
    /// WebGL canvas can never be the 2D one - and exactly one of them is on screen.
    pub renderer: RwSignal<RendererKind>,
    /// Whether a save of this cart's RAM has just been committed to IndexedDB,
    /// which is what the "Saved" chip shows, for a second and a half (M7-14, #147).
    ///
    /// Set when the write lands rather than when it is asked for, so the chip
    /// never claims a save that the browser has not taken.
    pub saved: RwSignal<bool>,
}

impl Controls {
    /// Pause a running console, or let a held one go: the Pause/Play button.
    ///
    /// With no console there is nothing to hold, and the signal is left alone -
    /// the button is disabled in that case anyway, and a press must not invent a
    /// paused state for a game that is not running.
    pub fn toggle(self) {
        let held = running().map(|emulator| {
            let mut emulator = emulator.borrow_mut();
            let held = !emulator.paused;
            emulator.set_paused(held);
            held
        });
        if let Some(held) = held {
            self.paused.set(held);
        }
    }

    /// The console's reset button: back to the start of the game, held or running
    /// as it was.
    pub fn reset(self) {
        if let Some(emulator) = running() {
            emulator.borrow_mut().reset();
        }
    }

    /// Takes the running console's sound off autoplay.
    ///
    /// Wired to the "Click to enable sound" chip. The window's own
    /// [`WAKE_EVENTS`] listeners do the same thing for every other click and
    /// keypress, so this is the one the shell can call when it wants to be sure.
    pub fn enable_audio(self) {
        if let Some(emulator) = running() {
            emulator.borrow_mut().enable_audio();
        }
    }

    /// Tells the running console that the picture's box is now `css_w` x `css_h`
    /// CSS pixels at `dpr` device pixels per CSS pixel (M7-13, #146).
    ///
    /// [`crate::screen`] calls this from the same effect that writes the canvas's
    /// CSS box, which is the shell's one resize signal: the WebGL path cannot
    /// learn the size from its own drawing buffer, because following the buffer
    /// would be following the very thing being sized.
    pub fn resize(self, css_w: f64, css_h: f64, dpr: f64) {
        if let Some(emulator) = running() {
            emulator.borrow_mut().resize_surface(css_w, css_h, dpr);
        }
    }

    /// Which drawing path to use and what the picture should look like on it
    /// (M7-13, #146): the drawer's three picture settings, in one call because
    /// they are read out of one `Settings`.
    pub fn set_picture(self, renderer: RendererSetting, filter: FilterSetting, smoothing: bool) {
        if let Some(emulator) = running() {
            emulator
                .borrow_mut()
                .set_picture(renderer, filter, smoothing);
        }
    }

    /// Says what the console's frame is, which is what the picture is scaled from
    /// (M7-12, #145).
    ///
    /// [`Emulator::start`] calls this with the canvas as the markup made it - the
    /// same value it seeds [`Emulator::drawn`] with, so the two cannot disagree
    /// about what is current - and [`Emulator::paint`] calls it whenever that
    /// baseline moves. One method for the one field because `paint` reports a
    /// *change*: a console whose first frame is already the size the markup asked
    /// for announces nothing there, and a frame size that is never announced is a
    /// picture that is never scaled.
    pub fn set_frame(self, size: (u32, u32)) {
        self.frame.set(size);
    }

    /// Sets the running console's volume, `0.0` to `1.0` as the drawer's slider
    /// has it. With no console there is nothing to turn, and the setting is not
    /// lost: a console made later reads [`Settings`] for itself.
    pub fn set_volume(self, volume: f64) {
        if let Some(emulator) = running() {
            emulator.borrow_mut().set_volume(volume as f32);
        }
    }

    /// Mutes or unmutes the running console, leaving its volume where it was.
    pub fn set_muted(self, muted: bool) {
        if let Some(emulator) = running() {
            emulator.borrow_mut().set_muted(muted);
        }
    }

    /// Lights the "Saved" chip, for a second and a half.
    ///
    /// Called by the write itself, not by the flush decision, so a cart whose
    /// write was refused shows nothing at all rather than a lie that faded.
    pub fn mark_saved(self) {
        self.saved.set(true);
        self.hide_saved_after_chip();
    }

    /// The timer that takes the chip off again.
    #[cfg(target_arch = "wasm32")]
    fn hide_saved_after_chip(self) {
        let callback = Closure::wrap(Box::new(move || self.saved.set(false)) as Box<dyn FnMut()>);
        if let Some(window) = web_sys::window() {
            let on_fire: &js_sys::Function = callback.as_js_value().unchecked_ref();
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(on_fire, SAVED_CHIP_MS);
        }
        // The timer outlives this scope, so the closure goes to JS and the GC
        // frees it - the same arrangement a toast's dismiss timer uses (#135).
        callback.into_js_value();
    }

    /// Natively there is no timer to arm, as with a toast's.
    #[cfg(not(target_arch = "wasm32"))]
    fn hide_saved_after_chip(self) {}
}

/// Runs the cartridge in [`LoadedRom`] context on `canvas`, for as long as the
/// component that calls this is mounted.
///
/// Mount it once, next to the two canvases. Starting a console is a side effect on
/// the browser, so it happens in an effect, and the effect reads the canvases and
/// the cartridge because all three are unmounted and mounted together by a `Show`
/// that does not promise which one arrives first.
///
/// `canvas` is the 2D element (`#screen`) and `canvas_gl` the WebGL one. The
/// console needs both because a canvas belongs to one context type for its whole
/// life: the path the console is not drawing with has to stay available for the
/// moment it wants it, which is the whole of the WebGL fallback story (M7-13,
/// #146).
pub fn use_cartridge(canvas: NodeRef<html::Canvas>, canvas_gl: NodeRef<html::Canvas>) -> Controls {
    let roms = expect_context::<RwSignal<Option<LoadedRom>>>();
    let toasts = expect_context::<Toasts>();
    let settings = expect_context::<RwSignal<Settings>>();
    let paused = RwSignal::new(false);
    let console = Controls {
        paused,
        sound_on: RwSignal::new(false),
        frame: RwSignal::new((0, 0)),
        // The 2D path until a console says otherwise - which is also the path
        // that is on screen before a console exists at all.
        renderer: RwSignal::new(RendererKind::Canvas),
        saved: RwSignal::new(false),
    };

    Effect::new(move |started: Option<Option<String>>| {
        let started = started.flatten();
        // The canvas first, and untracked reads of nothing: if it is not there,
        // there is nowhere to draw and whatever was running goes with it.
        let (Some(canvas), Some(canvas_gl)) = (canvas.get(), canvas_gl.get()) else {
            stop();
            return None;
        };
        // The cartridge, taken out of the signal as an owned value: `start` waits
        // on IndexedDB before it will make a console, and a borrow of a signal is
        // not something to be holding across an `.await` (#387).
        let Some(rom) = roms.with(|rom| rom.clone()) else {
            stop();
            return None;
        };
        if started.as_deref() == Some(rom.id.as_str()) {
            // Same cartridge, and a canvas that has not changed hands.
            return started;
        }
        start(&rom, &canvas, &canvas_gl, console, settings, toasts);
        paused.set(false);
        Some(rom.id)
    });

    // The drawer and the top bar write the volume and the mute; the running
    // console is told here. A console started later does not need these to have
    // run first - it reads the same settings as it comes up.
    Effect::new(move |_| console.set_volume(settings.get().volume));
    Effect::new(move |_| console.set_muted(settings.get().muted));
    // ... and so are the three picture settings: which path to draw with, whether
    // it filters, and whether the sampler smooths (M7-13, #146).
    Effect::new(move |_| {
        console.set_picture(
            settings.get().renderer,
            settings.get().filter,
            settings.get().smoothing,
        );
    });

    // The two flushes that do not come from the frame loop: a page going away, or
    // going into a background tab (M7-14, #147).
    use_lifecycle_flush();

    on_cleanup(stop);

    console
}

/// One running console: the machine, the path it is drawn with, and the
/// animation frame that drives it.
struct Emulator {
    /// The machine. Never in a signal, and cloned by nothing: see the
    /// [module docs](self).
    genesis: Genesis,
    /// Where this console's sound goes, `None` from the first frame until a
    /// gesture makes it possible - a context the browser will not start has no
    /// reason to exist, and one it refused has to be tried again later
    /// ([`Emulator::enable_audio`], M7-05, #138).
    audio: Option<AudioOut>,
    /// The volume and the mute, so a console started - or an output remade -
    /// part-way through a session comes up at the level the drawer shows.
    settings: RwSignal<Settings>,
    /// The shell's handle, so the loop can report the audio state back through
    /// [`Controls::sound_on`] without a second owner for the signals.
    console: Controls,
    /// The cart's RAM, if it has any and if this browser can keep it: see [`Save`].
    save: Option<Save>,
    /// When the loop last asked [`SramWatcher`] about this cart's RAM, on the
    /// animation frame's clock - the once a second of [`SRAM_CHECK_INTERVAL_MS`].
    sram_checked: f64,
    /// Where a storage failure is reported, there being no caller to hand it to:
    /// the frame loop and a spawned write are both answerable only to a toast.
    toasts: Toasts,
    /// The block being drained, reused every frame like [`Self::rgba`].
    samples: Vec<i16>,
    /// One console frame in seconds: `1 / target_hz`, which is what
    /// [`frames_to_run`] turns an audio lead into a number of frames with.
    frame_secs: f64,
    /// The 2D canvas (`#screen`): what the 2D path draws on, and what this
    /// console falls back to when WebGL is refused or lost. Held because a canvas
    /// belongs to one context type for its whole life, so this element is the one
    /// that must never have been asked for a WebGL context (M7-13, #146).
    canvas_2d: HtmlCanvasElement,
    /// The WebGL canvas (`#screen-webgl`), kept for the same reason in the other
    /// direction: once it has a context it can never be drawn on with 2D.
    canvas_gl: HtmlCanvasElement,
    /// Which of the two canvases above is the live one, mirrored to
    /// [`Controls::renderer`] so the shell can show it.
    kind: RendererKind,
    /// The drawing path: `Canvas2dRenderer` or `WebGlRenderer`, whichever
    /// [`choose_renderer`] and the browser agreed on (M7-13, #146).
    renderer: Box<dyn Renderer>,
    /// The filter and the smoothing the drawer asks for, kept so a renderer built
    /// later - on a setting change, or after a lost context - is built the way the
    /// player has it rather than the way the default is.
    picture: (FilterSetting, bool),
    /// The RGBA bytes of the frame being drawn, reused every frame so the draw
    /// path allocates nothing (see [`frame_to_rgba_bytes`]).
    rgba: Vec<u8>,
    /// The video-side pacer: the whole policy while there is no audio clock, and
    /// kept warm - but not listened to - while there is.
    pacer: FramePacer,
    paused: bool,
    /// The frame size the last paint drew, which is what `Controls::frame` is
    /// told about and what [`Emulator::redraw`] draws: the renderer keeps its own
    /// idea of the drawing buffer, and the two are only the same number on the 2D
    /// path by coincidence (#145, #146).
    drawn: (u32, u32),
    /// The animation frame that is currently requested, `None` once the browser
    /// stops asking or [`Emulator::stop`] has run.
    frame: Option<i32>,
    /// The callback JS is holding. It has to stay alive at least until the
    /// requested frame fires, so it belongs to the console rather than to the
    /// stack that requested it.
    tick: Option<Closure<dyn FnMut(f64)>>,
    /// The [`WAKE_EVENTS`] listeners, in the order [`WAKE_EVENTS`] lists them.
    /// On `window`, which outlives this console, so they have to be taken off it
    /// before their closures are dropped.
    wake: Vec<Closure<dyn FnMut()>>,
}

impl Emulator {
    /// The console `rom` runs on, painting to `canvas` (2D) or `canvas_gl`
    /// (WebGL), with the loop already running.
    ///
    /// The animation frame is requested before the callback is filed away, so
    /// that there is no moment in which a tick could arrive for a console that
    /// cannot answer it. The callback holds a `Weak` for the same reason
    /// [`stop`] can free the machine: a loop must not be what keeps a retired
    /// console alive.
    fn start(
        rom: &LoadedRom,
        canvas: &HtmlCanvasElement,
        canvas_gl: &HtmlCanvasElement,
        console: Controls,
        settings: RwSignal<Settings>,
        saved: Option<SavedSram>,
        toasts: Toasts,
    ) -> Result<Rc<RefCell<Self>>, StartError> {
        // The drawing path first, because everything after this point has a
        // browser resource attached to it that a failed start would have to give
        // back - and because a console with nothing to draw with is not a console.
        let (renderer, filter, smoothing) = settings
            .with_untracked(|settings| (settings.renderer, settings.filter, settings.smoothing));
        let (kind, renderer) = make_renderer(
            choose_renderer_latched(webgl2_on_offer(renderer), gl_lost(), renderer),
            canvas,
            canvas_gl,
            filter,
            smoothing,
        )
        .ok_or(StartError::NoCanvas)?;
        console.renderer.set(kind);

        // The audio output next, and for one reason beyond the obvious: it is the
        // only place the sample rate the browser will really use is known, and
        // `Genesis` has to be mixed to that rather than to the 48 kHz of
        // `Config::default`. A browser that will not give up a context is not a
        // reason not to run the game, so `None` is a normal answer here and the
        // loop paces on frames until a gesture makes one possible.
        let mut audio = AudioOut::new();
        // The level the drawer shows is applied here as well as in `enable_audio`,
        // because a console made after boot is made with an output that has never
        // heard of the store: the volume effect only runs on a *change*, and a
        // freshly started console is not one. It stayed invisible while the
        // output's own default and the shell's were both full (M7-05, #138); with
        // the store's 0.8 (M7-10, #143) a console that ignored it was audible.
        // Untracked for the same reason as [`Emulator::enable_audio`].
        if let Some(audio) = audio.as_mut() {
            let volume = settings.with_untracked(|settings| settings.volume);
            let muted = settings.with_untracked(|settings| settings.muted);
            audio.set_volume(volume as f32);
            audio.set_muted(muted);
        }
        let mut config = Config::default();
        if let Some(audio) = audio.as_ref() {
            config.sample_rate = audio.sample_rate();
        }
        // A console that will not start takes the audio output back with it: an
        // `AudioOut` dropped is a context closed, which is the only way the
        // browser stops counting it.
        let mut genesis =
            Genesis::new(&rom.bytes, config.clone()).map_err(|_| StartError::NotARom)?;
        // The saved RAM goes in here, before the loop is armed: `install_save` is
        // the one place a cart is given its save, and it is the reason `start` is
        // called only once storage has answered (M7-14, #147).
        let save = install_save(&mut genesis, saved, &rom.id);

        let target_hz = target_hz(&config, &rom.bytes);
        // One read of the canvas, and both the baseline `paint` compares against
        // and the size the shell is told come from it (#145).
        let seeded = (canvas.width(), canvas.height());
        console.set_frame(seeded);
        let emulator = Rc::new(RefCell::new(Self {
            genesis,
            audio,
            settings,
            console,
            save,
            sram_checked: now_ms(),
            toasts,
            samples: Vec::new(),
            frame_secs: 1.0 / target_hz,
            canvas_2d: canvas.clone(),
            canvas_gl: canvas_gl.clone(),
            kind,
            renderer,
            picture: (filter, smoothing),
            rgba: Vec::new(),
            pacer: FramePacer::new(target_hz),
            paused: false,
            // The canvas as the markup made it, so the first frame that does not
            // match it is seen as the change it is.
            drawn: seeded,
            frame: None,
            tick: None,
            wake: Vec::new(),
        }));

        let weak = Rc::downgrade(&emulator);
        // Built before the tick closure, which is what takes the only move of
        // `weak`: the listeners need the same handle, and they only read it.
        let wake = wake_listeners(&weak);
        let callback = Closure::wrap(Box::new(move |now_ms: f64| {
            // The gamepads first, and outside the borrow below. The Gamepad API
            // is poll based, so this is the only place a pad's buttons change
            // (M7-08, #141); a reading can also ask for a hotkey, and a hotkey
            // can reach the console - which is a second `borrow_mut` inside the
            // one just below.
            // The frame's own clock travels with it: a direction held on a pad
            // while a menu is open walks the focus on a timer, and this is the
            // only clock in the page that a pad can be timed against (M7-11, #144).
            crate::gamepad_events::poll_gamepads(now_ms);
            if let Some(emulator) = weak.upgrade() {
                emulator.borrow_mut().tick(now_ms);
            }
        }) as Box<dyn FnMut(f64)>);
        let window = web_sys::window().ok_or(StartError::NoAnimationFrames)?;
        let frame = window
            .request_animation_frame(callback.as_js_value().unchecked_ref())
            .map_err(|_| StartError::NoAnimationFrames)?;

        let mut this = emulator.borrow_mut();
        this.tick = Some(callback);
        this.frame = Some(frame);
        this.wake = wake;
        this.sync_sound();
        drop(this);

        Ok(emulator)
    }

    /// One animation frame: run the console for the time that has passed since
    /// the last tick, draw what it produced, and ask for the next tick.
    fn tick(&mut self, now_ms: f64) {
        let due = self.frames_due(now_ms);
        if !self.paused {
            for _ in 0..due {
                self.sync_pads();
                self.genesis.run_frame();
                self.pump_audio();
            }
            self.paint();
        }
        // Once a second, offer the cart's RAM to storage. The watcher does the
        // deciding, and it is the watcher's window that makes a game which dirties
        // its save every frame one write every two seconds rather than a write per
        // frame (M7-14, #147).
        if now_ms - self.sram_checked >= SRAM_CHECK_INTERVAL_MS {
            self.sram_checked = now_ms;
            self.flush_sram(now_ms, false);
        }
        self.sync_sound();
        self.arm();
    }

    /// Offers this cart's RAM to storage if [`SramWatcher`] says it has moved.
    ///
    /// Three things about this are deliberate, and they are all about the frame
    /// loop it runs inside (#387):
    ///
    /// * the cart's RAM is copied to an owned `Vec` first, so no borrow of the
    ///   console outlives this function and nothing is borrowed across the
    ///   `.await` the write does;
    /// * the write is *spawned*, never awaited - an IndexedDB round trip has no
    ///   business stopping a frame, and a frame that stops is a stutter;
    /// * `spawn_local` only queues a task, so the write is first polled after the
    ///   borrow below has been given back.
    ///
    /// `force` is the last chance of a cart being swapped out or a page closing:
    /// it skips the watcher's window, never its checks.
    fn flush_sram(&mut self, now_ms: f64, force: bool) {
        let Some(save) = self.save.as_mut() else {
            return;
        };
        // The snapshot, and the last moment the console itself is read.
        let Some(sram) = self.genesis.sram() else {
            return;
        };
        let bytes = sram.to_vec();
        if force {
            save.watcher.force();
        }
        if !save.watcher.should_flush(&bytes, now_ms) {
            return;
        }
        let (db, rom_id) = (save.db.clone(), save.rom_id.clone());
        spawn_local(write_saved_sram(
            db,
            rom_id,
            bytes,
            self.console,
            self.toasts,
        ));
    }

    /// Reports the page's merged pad state to the console, once per frame.
    ///
    /// Before [`genesis::Genesis::run_frame`], because that call reads the pads
    /// as they stand at the start of the frame: a press that lands while a frame
    /// is running is then current for the next one (M7-07, #140). The state is
    /// the page's, not the console's - see [`crate::input::with_state`] for why.
    ///
    /// The gamepads are read a little before this, by
    /// [`crate::gamepad_events::poll_gamepads`] in the animation frame, rather
    /// than here: it is the same per-frame moment, but outside the borrow this
    /// runs under.
    fn sync_pads(&mut self) {
        for port in 0..crate::input::PORTS {
            let buttons = crate::input::with_state(|input| input.buttons(port));
            self.genesis.set_pad(port, buttons);
        }
    }

    /// How many frames this tick owes: from the audio clock when there is one,
    /// from [`FramePacer`] when there is not.
    ///
    /// The pacer is advanced while it is not deciding too, so that losing the
    /// audio clock mid-session hands over to a pacer that is not about to run
    /// four frames of catch-up for a gap it never saw - and, as before #138, so
    /// that pausing spends time rather than borrowing it.
    fn frames_due(&mut self, now_ms: f64) -> u32 {
        let buffered = self
            .audio
            .as_ref()
            .filter(|audio| audio.is_running())
            .map(AudioOut::buffered_secs);
        let Some(buffered) = buffered else {
            return self.pacer.frames_due(now_ms);
        };
        let _ = self.pacer.frames_due(now_ms);
        frames_to_run(
            buffered,
            TARGET_AHEAD_SECS,
            self.frame_secs,
            MAX_FRAMES_PER_TICK,
        )
    }

    /// Hands the frame that has just run to the speakers.
    ///
    /// Only ever called straight after `run_frame`, which is what makes the
    /// schedule line up with the picture: the sound of frame N is appended while
    /// frame N is the newest thing the console has made.
    fn pump_audio(&mut self) {
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        if !audio.is_running() {
            // Nothing to schedule into, and nothing is lost by not draining: the
            // mixer drops what it has not been asked for at the start of the
            // next frame.
            return;
        }
        self.samples.clear();
        self.genesis.drain_audio(&mut self.samples);
        if !audio.push(&self.samples) {
            // The context would not take this block, so it will not take the next
            // one either: fall back to the frame pacer, and let a later gesture
            // make a fresh output.
            self.audio = None;
            self.sync_sound();
        }
    }

    /// The gesture handler: take this console's sound off autoplay.
    ///
    /// Also what remakes an output the browser refused at [`Self::start`], or one
    /// that gave up mid-session, because a call from inside a gesture is the only
    /// place a context has a chance of coming up running.
    fn enable_audio(&mut self) {
        if self.audio.is_none() {
            self.audio = AudioOut::new();
            if let Some(audio) = self.audio.as_mut() {
                // Untracked: this runs on the animation frame and on a gesture,
                // and the store is one struct now - a tracked read here would
                // subscribe the console's own start-up to every setting there is.
                let volume = self.settings.with_untracked(|settings| settings.volume);
                let muted = self.settings.with_untracked(|settings| settings.muted);
                audio.set_volume(volume as f32);
                audio.set_muted(muted);
            }
        }
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        // Enabling sound while the console is held must not let a held console
        // play, so a held one asks for the context and then holds it again.
        if self.paused {
            audio.suspend();
        } else {
            audio.resume();
        }
        self.sync_sound();
    }

    /// Holds the console or lets it go, and takes the audio with it: a paused
    /// console is silent, and a released one starts its schedule from wherever
    /// the play head has got to while the rest of the tab kept moving.
    fn set_paused(&mut self, held: bool) {
        self.paused = held;
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        if held {
            audio.suspend();
        } else {
            audio.resume();
        }
    }

    fn set_volume(&mut self, volume: f32) {
        if let Some(audio) = self.audio.as_mut() {
            audio.set_volume(volume);
        }
    }

    fn set_muted(&mut self, muted: bool) {
        if let Some(audio) = self.audio.as_mut() {
            audio.set_muted(muted);
        }
    }

    /// Reports the audio state to the shell, which shows the "Click to enable
    /// sound" chip while it is `false`.
    ///
    /// Polled from the tick rather than driven by `onstatechange` because the
    /// context's state is only readable in the first place from this side of the
    /// boundary, and because the loop is already running sixty times a second.
    /// Written only when it changes: a signal write per frame would re-render the
    /// top bar for nothing.
    fn sync_sound(&self) {
        let running = self.audio.as_ref().is_some_and(|audio| audio.is_running());
        if self.console.sound_on.get() != running {
            self.console.sound_on.set(running);
        }
    }

    /// The last frame the console drew, to the canvas.
    ///
    /// Called on the tick and on a reset, which is the one other moment the
    /// picture changes while nobody is watching it.
    fn paint(&mut self) {
        let (width, height) = {
            let frame = self.genesis.frame();
            frame_to_rgba_bytes(frame, &mut self.rgba);
            (frame.width as u32, frame.height as u32)
        };

        // H32 is 256 wide rather than 320, a 240-line mode is 240 rather than 224,
        // and interlace mode 2 is 448 or 480, so this does change while a game
        // runs. What the renderer does about it is its own business - the 2D path
        // resizes its buffer, WebGL picks a different sub-rect of the texture -
        // but the *frame* is the quantity the shell scales from, so it is the
        // frame changing size that has to be announced (#145).
        if self.drawn != (width, height) {
            self.drawn = (width, height);
            self.console.set_frame((width, height));
        }

        // A context the driver took away is the player's `Canvas` switch arriving
        // from the other side, so it takes the same route: the 2D path, before the
        // frame that would otherwise have been drawn on the lost one (#146).
        if self.renderer.context_lost() {
            self.use_renderer(RendererKind::Canvas);
        }
        self.renderer.draw(&self.rgba, width, height);
    }

    /// Switches the console to `kind`, and leaves the console on the path it was
    /// on if the browser will not have the new one.
    ///
    /// Not a failure worth a toast either way: `Canvas` is an off switch, and a
    /// WebGL that cannot be had is a line in the browser console from
    /// [`make_renderer`], which is where the reason is known.
    fn use_renderer(&mut self, kind: RendererKind) {
        let (filter, smoothing) = self.picture;
        let built = make_renderer(kind, &self.canvas_2d, &self.canvas_gl, filter, smoothing);
        // Assigned only once the new path exists, so a failed switch cannot leave
        // the console with a renderer it has dropped.
        if let Some((kind, renderer)) = built {
            self.kind = kind;
            self.renderer = renderer;
            self.console.renderer.set(kind);
        }
    }

    /// The drawer's picture settings (M7-13, #146).
    ///
    /// A path change and a filter change are the same operation here, and both
    /// end with a redraw: a canvas is transparent until something draws on it, so
    /// a player who switches paths - or who is held and will not draw another
    /// frame of their own - would otherwise be looking at nothing.
    fn set_picture(&mut self, renderer: RendererSetting, filter: FilterSetting, smoothing: bool) {
        self.picture = (filter, smoothing);
        let wanted = choose_renderer_latched(webgl2_on_offer(renderer), gl_lost(), renderer);
        if wanted != self.kind {
            self.use_renderer(wanted);
        }
        self.renderer.set_filter(filter);
        self.renderer.set_smoothing(smoothing);
        self.redraw();
    }

    /// The picture's box moved, so the drawing buffer follows it (#146).
    ///
    /// Resizing a canvas clears it, and a held console draws no frames of its own
    /// to fill it in again, so a held one gets its last frame back here.
    fn resize_surface(&mut self, css_w: f64, css_h: f64, dpr: f64) {
        self.renderer.resize(css_w, css_h, dpr);
        if self.paused {
            self.redraw();
        }
    }

    /// Puts the frame that is already converted back on whatever the renderer is
    /// now. Does nothing before the first frame, when there is no frame to put
    /// back - which is why [`Emulator::start`] does not need to paint.
    fn redraw(&mut self) {
        if !self.rgba.is_empty() {
            let (width, height) = self.drawn;
            self.renderer.draw(&self.rgba, width, height);
        }
    }

    /// Requests the next animation frame, and stops the loop when the browser
    /// says no: a tab that has stopped animating is not going to start again by
    /// being asked repeatedly.
    fn arm(&mut self) {
        let requested = self.tick.as_ref().and_then(|tick| {
            let window = web_sys::window()?;
            window
                .request_animation_frame(tick.as_js_value().unchecked_ref())
                .ok()
        });
        self.frame = requested;
    }

    /// The console's own reset, then a draw: the player should see the game come
    /// back to its title screen even while the loop is held.
    fn reset(&mut self) {
        self.genesis.reset();
        self.paint();
    }

    /// Stops the loop, takes the gesture listeners off `window`, and closes the
    /// audio output.
    ///
    /// The requested frame goes before the callback: dropping a `Closure` that
    /// the browser still intends to call leaves a call to nothing. The same is
    /// true of the wake listeners, which are held by `window` rather than by
    /// anything that stops when this console does - and a browser allows only a
    /// handful of `AudioContext`s per tab, so loading a second ROM into the same
    /// tab needs the first one closed, not merely forgotten.
    fn stop(&mut self) {
        if let Some(frame) = self.frame.take()
            && let Some(window) = web_sys::window()
        {
            let _ = window.cancel_animation_frame(frame);
        }
        self.tick = None;
        if let Some(window) = web_sys::window() {
            for (event, listener) in WAKE_EVENTS.iter().zip(self.wake.iter()) {
                let _ = window
                    .remove_event_listener_with_callback(event, listener.as_ref().unchecked_ref());
            }
        }
        self.wake.clear();
        // Letting the output go is what closes its context: see `Drop for AudioOut`.
        self.audio = None;
    }
}

/// Whether this browser offers WebGL2 to this player, or `false` when they have
/// already said they do not want it.
///
/// The setting is consulted first because the answer costs a context: probing is
/// asking a canvas of one's own for a WebGL2 context (see
/// [`crate::render::webgl::webgl2_supported`]), and a player on `Canvas` should
/// not pay for one they have refused (M7-13, #146).
///
/// A context lost earlier is *not* folded in here - that is the `gl_lost`
/// argument of [`crate::render::choose_renderer_latched`], so the two reasons for
/// not using WebGL stay distinguishable.
fn webgl2_on_offer(setting: RendererSetting) -> bool {
    setting != RendererSetting::Canvas && webgl2_supported()
}

/// Builds the drawing path `kind` asks for, or `None` if neither canvas can be
/// drawn on at all.
///
/// WebGL is tried first when that is the path wanted, and every WebGL failure -
/// no context, a shader that will not compile, a driver that refuses the texture -
/// becomes the 2D path: a cartridge worth loading is worth playing without a
/// shader, and the reason is already in the browser console by the time this
/// returns.
///
/// The two canvases are separate because a canvas belongs to one context type for
/// its whole life. A WebGL failure must not have cost the 2D element its 2D
/// context, and vice versa (M7-13, #146).
fn make_renderer(
    kind: RendererKind,
    canvas_2d: &HtmlCanvasElement,
    canvas_gl: &HtmlCanvasElement,
    filter: FilterSetting,
    smoothing: bool,
) -> Option<(RendererKind, Box<dyn Renderer>)> {
    if kind == RendererKind::Webgl
        && let Some(renderer) = WebGlRenderer::new(canvas_gl, filter, smoothing)
    {
        return Some((RendererKind::Webgl, Box::new(renderer)));
    }
    let renderer = Canvas2dRenderer::new(canvas_2d)?;
    Some((RendererKind::Canvas, Box::new(renderer)))
}

/// Attaches the [`WAKE_EVENTS`] listeners that take a context off autoplay, and
/// returns the closures the browser is now holding.
///
/// They are left attached for the life of the console rather than removed on the
/// first success: `AudioContext::resume` on a running context asks for nothing,
/// and a context can be taken back by the browser - a tab that goes to the
/// background, a device change - so a later gesture has to be worth listening for
/// again.
fn wake_listeners(weak: &Weak<RefCell<Emulator>>) -> Vec<Closure<dyn FnMut()>> {
    WAKE_EVENTS
        .iter()
        .filter_map(|event| {
            let weak = weak.clone();
            let listener = Closure::wrap(Box::new(move || {
                if let Some(emulator) = weak.upgrade() {
                    emulator.borrow_mut().enable_audio();
                }
            }) as Box<dyn FnMut()>);
            let attached = web_sys::window()
                .and_then(|window| {
                    window
                        .add_event_listener_with_callback(event, listener.as_ref().unchecked_ref())
                        .ok()
                })
                .is_some();
            // A listener the browser refused to take must not be held: it will
            // never be called and never be removed.
            attached.then_some(listener)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 512 byte image with just enough of a header for [`target_hz`]: the
    /// system string that says this is a Mega Drive ROM, and the region
    /// field at 0x1F0.
    fn header(region: &str) -> Vec<u8> {
        let mut rom = vec![0u8; 0x200];
        rom[0x100..0x200].fill(b' ');
        rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");
        rom[0x1F0..0x1F0 + region.len()].copy_from_slice(region.as_bytes());
        rom
    }

    /// `0xFF0000FF` and `0xFFFF0000` are red and blue as the VDP writes them.
    const RED: u32 = 0xFF00_00FF;
    const BLUE: u32 = 0xFFFF_0000;

    #[test]
    fn a_two_by_one_frame_is_eight_bytes_of_red_then_blue() {
        let frame = FrameBuffer {
            width: 2,
            height: 1,
            pixels: vec![RED, BLUE],
        };
        let mut out = Vec::new();
        frame_to_rgba_bytes(&frame, &mut out);
        assert_eq!(out, vec![255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(out.len(), frame.width * frame.height * 4);
    }

    /// `FrameBuffer::pixels` is 320 * 480 whatever the display mode, so the
    /// conversion has to stop at `width * height` rather than turn the rest of
    /// the buffer into image bytes.
    #[test]
    fn only_the_valid_pixels_of_a_full_frame_buffer_are_converted() {
        let mut frame = FrameBuffer::new();
        frame.width = 1;
        frame.height = 1;
        frame.pixels[0] = 0x1122_3344;
        frame.pixels[1] = 0xDEAD_BEEF;

        let mut out = Vec::new();
        frame_to_rgba_bytes(&frame, &mut out);
        assert_eq!(out, vec![0x44, 0x33, 0x22, 0x11]);
    }

    /// The reuse contract: a second call replaces the buffer instead of appending
    /// to it, and does not hand back a new allocation. This is what lets the draw
    /// path hold one buffer for the life of the loop.
    #[test]
    fn a_second_call_replaces_the_buffer_rather_than_appending_to_it() {
        let frame = FrameBuffer {
            width: 2,
            height: 1,
            pixels: vec![RED, BLUE],
        };
        let mut out = Vec::new();
        frame_to_rgba_bytes(&frame, &mut out);
        let capacity = out.capacity();

        let small = FrameBuffer {
            width: 1,
            height: 1,
            pixels: vec![BLUE],
        };
        frame_to_rgba_bytes(&small, &mut out);
        assert_eq!(out, vec![0, 0, 255, 255]);
        assert_eq!(
            out.capacity(),
            capacity,
            "the buffer is reused, not rebuilt"
        );
    }

    /// A PAL-only cart is the only one that runs at 50 Hz, and the region comes
    /// from the same [`Config::resolve_region`] the console itself uses.
    #[test]
    fn a_cart_that_claims_only_europe_paces_at_50_hz() {
        assert_eq!(target_hz(&Config::default(), &header("E")), PAL_HZ);
    }

    /// `JUE` - the common all-region header - is the NTSC machine, because
    /// [`Config::default`] prefers USA and that is what the console will run it
    /// as. Keying off the *claimed* regions instead would call this PAL and play
    /// every exported game 4 % slow.
    #[test]
    fn an_all_region_cart_paces_at_60_hz_because_the_console_prefers_ntsc() {
        assert_eq!(target_hz(&Config::default(), &header("JUE")), NTSC_HZ);
        assert_eq!(target_hz(&Config::default(), &header("UE")), NTSC_HZ);
        assert_eq!(target_hz(&Config::default(), &header("J")), NTSC_HZ);
    }

    /// Homebrew and dumps with nothing in the region field: the console gives
    /// them the NTSC export machine, so 60 Hz here too.
    #[test]
    fn a_cart_that_claims_nothing_paces_at_60_hz() {
        assert_eq!(target_hz(&Config::default(), &header("")), NTSC_HZ);
    }

    /// A `Config` that prefers Europe is a PAL machine for an all-region cart,
    /// which is the whole reason `target_hz` takes the config rather than reading
    /// the header on its own.
    #[test]
    fn a_europe_preferring_host_paces_an_all_region_cart_at_50_hz() {
        let config = Config {
            prefer: [
                genesis::Region::Europe,
                genesis::Region::Usa,
                genesis::Region::Japan,
            ],
            ..Config::default()
        };
        assert_eq!(target_hz(&config, &header("JUE")), PAL_HZ);
    }

    #[test]
    fn a_failure_says_what_to_tell_the_player() {
        assert_eq!(
            StartError::NotARom.to_string(),
            "The console could not start this ROM"
        );
        assert_eq!(
            StartError::NoCanvas.to_string(),
            "This browser will not draw to the screen"
        );
        assert_eq!(
            StartError::NoAnimationFrames.to_string(),
            "This browser stopped giving the screen animation frames"
        );
    }

    /// The top bar's buttons are live before a cartridge is loaded, so the
    /// console actions have to be inert rather than wrong: no console, no paused
    /// state invented for a game that is not running, and no panic on reset, on
    /// mute, on volume, or on the click that asks for sound.
    #[test]
    fn pause_and_reset_and_the_audio_controls_do_nothing_without_a_console() {
        let owner = Owner::new();
        owner.set();
        let paused = RwSignal::new(false);
        let controls = Controls {
            paused,
            sound_on: RwSignal::new(false),
            frame: RwSignal::new((0, 0)),
            renderer: RwSignal::new(RendererKind::Canvas),
            saved: RwSignal::new(false),
        };

        // No console, so the frame size is the one `use_cartridge` started with -
        // and it has exactly one door, which is what lets `start` and `paint`
        // share it without either reading the other (#145).
        assert_eq!(controls.frame.get_untracked(), (0, 0));
        controls.set_frame((256, 224));
        assert_eq!(controls.frame.get_untracked(), (256, 224));

        controls.reset();
        controls.enable_audio();
        controls.set_volume(0.5);
        controls.set_muted(true);
        controls.toggle();
        assert!(!paused.get());
        assert!(!controls.sound_on.get());

        // The picture controls of #146 are the same kind of no-op: a resize has
        // no buffer to move, and a new path has no console to move.
        controls.resize(640.0, 480.0, 2.0);
        controls.set_picture(RendererSetting::Webgl, FilterSetting::Crt, true);
        assert_eq!(controls.renderer.get_untracked(), RendererKind::Canvas);

        // The "Saved" chip is the one part of `Controls` that is not a message to
        // a console: it reports something that has already happened, so a write
        // that landed lights it with no console running at all. The 1.5 s timer
        // that takes it off is a browser timer, and natively there is none to
        // arm - so it stays lit, which is what `mark_saved` promises.
        assert!(!controls.saved.get_untracked());
        controls.mark_saved();
        assert!(controls.saved.get_untracked());
    }
}
