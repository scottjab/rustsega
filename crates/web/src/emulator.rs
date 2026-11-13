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

use std::cell::RefCell;
use std::rc::Rc;
use std::rc::Weak;

use genesis::Config;
use genesis::Genesis;
use leptos::html;
use leptos::prelude::*;
use vdp::FrameBuffer;
use wasm_bindgen::Clamped;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::CanvasRenderingContext2d;
use web_sys::HtmlCanvasElement;
use web_sys::ImageData;

use crate::audio::AudioOut;
use crate::audio_pacing::MAX_FRAMES_PER_TICK;
use crate::audio_pacing::TARGET_AHEAD_SECS;
use crate::audio_pacing::frames_to_run;
use crate::frame_pacer::FramePacer;
use crate::rom::LoadedRom;
use crate::settings::Settings;
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
}

/// The running console, if there is one.
fn running() -> Option<Rc<RefCell<Emulator>>> {
    RUNNING.with(|running| running.borrow().clone())
}

/// Stops the running console, if any: cancels its animation frame and lets it go.
fn stop() {
    // Taking it out of the slot before stopping it is what frees the machine -
    // the loop itself only holds a `Weak` (see [`Emulator::start`]).
    let emulator = RUNNING.with(|running| running.borrow_mut().take());
    if let Some(emulator) = emulator {
        emulator.borrow_mut().stop();
    }
}

/// Starts `rom` on `canvas`, replacing whatever was running.
///
/// Replacing, not adding: a second console drawing into one canvas is the failure
/// mode of "load another ROM without reloading the page", and it also costs
/// another megabyte of machine.
fn start(
    rom: &LoadedRom,
    canvas: &HtmlCanvasElement,
    console: Controls,
    settings: RwSignal<Settings>,
) -> Result<(), StartError> {
    // Before the new console asks for a frame of its own: the old one's request
    // has to be cancelled, not merely dropped along with its callback, or the
    // browser calls back into a closure that has gone.
    stop();
    let emulator = Emulator::start(rom, canvas, console, settings)?;
    RUNNING.with(|running| *running.borrow_mut() = Some(emulator));
    Ok(())
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
}

/// Runs the cartridge in [`LoadedRom`] context on `canvas`, for as long as the
/// component that calls this is mounted.
///
/// Mount it once, next to the canvas. Starting a console is a side effect on the
/// browser, so it happens in an effect, and the effect reads both the canvas and
/// the cartridge because the two are unmounted and mounted together by a `Show`
/// that does not promise which one arrives first.
pub fn use_cartridge(canvas: NodeRef<html::Canvas>) -> Controls {
    let roms = expect_context::<RwSignal<Option<LoadedRom>>>();
    let toasts = expect_context::<Toasts>();
    let settings = expect_context::<RwSignal<Settings>>();
    let paused = RwSignal::new(false);
    let console = Controls {
        paused,
        sound_on: RwSignal::new(false),
    };

    Effect::new(move |started: Option<Option<String>>| {
        let started = started.flatten();
        // The canvas first, and untracked reads of nothing: if it is not there,
        // there is nowhere to draw and whatever was running goes with it.
        let Some(canvas) = canvas.get() else {
            stop();
            return None;
        };
        let Some(id) = roms.with(|rom| rom.as_ref().map(|rom| rom.id.clone())) else {
            stop();
            return None;
        };
        if started.as_deref() == Some(id.as_str()) {
            // Same cartridge, and a canvas that has not changed hands.
            return started;
        }
        // The image is read with the signal and put in the console before
        // anything else runs: `Genesis::new` is the only thing that takes it, and
        // it does not look at the signal.
        roms.with(|rom| {
            if let Some(rom) = rom.as_ref()
                && let Err(error) = start(rom, &canvas, console, settings)
            {
                toasts.push(ToastKind::Danger, error.to_string());
            }
        });
        paused.set(false);
        Some(id)
    });

    // The drawer and the top bar write the volume and the mute; the running
    // console is told here. A console started later does not need these to have
    // run first - it reads the same settings as it comes up.
    Effect::new(move |_| console.set_volume(settings.get().volume));
    Effect::new(move |_| console.set_muted(settings.get().muted));

    on_cleanup(stop);

    console
}

/// One running console: the machine, the canvas it is drawn to, and the
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
    /// The block being drained, reused every frame like [`Self::rgba`].
    samples: Vec<i16>,
    /// One console frame in seconds: `1 / target_hz`, which is what
    /// [`frames_to_run`] turns an audio lead into a number of frames with.
    frame_secs: f64,
    /// The canvas whose context `ctx` draws into. Kept because its `width` and
    /// `height` attributes are the canvas's own resolution, which is what has to
    /// follow the VDP's display mode.
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    /// The RGBA bytes of the frame being drawn, reused every frame so the draw
    /// path allocates nothing (see [`frame_to_rgba_bytes`]).
    rgba: Vec<u8>,
    /// The video-side pacer: the whole policy while there is no audio clock, and
    /// kept warm - but not listened to - while there is.
    pacer: FramePacer,
    paused: bool,
    /// The canvas size the last frame was drawn at, so the attributes are written
    /// only when the picture really changes size.
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
    /// The console `rom` runs on, painting to `canvas`, with the loop already
    /// running.
    ///
    /// The animation frame is requested before the callback is filed away, so
    /// that there is no moment in which a tick could arrive for a console that
    /// cannot answer it. The callback holds a `Weak` for the same reason
    /// [`stop`] can free the machine: a loop must not be what keeps a retired
    /// console alive.
    fn start(
        rom: &LoadedRom,
        canvas: &HtmlCanvasElement,
        console: Controls,
        settings: RwSignal<Settings>,
    ) -> Result<Rc<RefCell<Self>>, StartError> {
        // The canvas first, because everything after this point has a browser
        // resource attached to it that a failed start would have to give back.
        let ctx = canvas
            .get_context("2d")
            .ok()
            .flatten()
            .and_then(|context| context.dyn_into::<CanvasRenderingContext2d>().ok())
            .ok_or(StartError::NoCanvas)?;

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
        let genesis = Genesis::new(&rom.bytes, config.clone()).map_err(|_| StartError::NotARom)?;

        let target_hz = target_hz(&config, &rom.bytes);
        let emulator = Rc::new(RefCell::new(Self {
            genesis,
            audio,
            settings,
            console,
            samples: Vec::new(),
            frame_secs: 1.0 / target_hz,
            canvas: canvas.clone(),
            ctx,
            rgba: Vec::new(),
            pacer: FramePacer::new(target_hz),
            paused: false,
            // The canvas as the markup made it, so the first frame that does not
            // match it is seen as the change it is.
            drawn: (canvas.width(), canvas.height()),
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
            crate::gamepad_events::poll_gamepads();
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
        self.sync_sound();
        self.arm();
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
        // runs. Writing either attribute clears a canvas, so it is written when it
        // has to be and not once per frame.
        if self.drawn != (width, height) {
            self.canvas.set_width(width);
            self.canvas.set_height(height);
            self.drawn = (width, height);
        }

        let image = ImageData::new_with_u8_clamped_array_and_sh(Clamped(&self.rgba), width, height);
        if let Ok(image) = image {
            let _ = self.ctx.put_image_data(&image, 0.0, 0.0);
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
        };

        controls.reset();
        controls.enable_audio();
        controls.set_volume(0.5);
        controls.set_muted(true);
        controls.toggle();
        assert!(!paused.get());
        assert!(!controls.sound_on.get());
    }
}
