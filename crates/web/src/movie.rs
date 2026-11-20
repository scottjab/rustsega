//! Recording and replaying an input movie in the browser (M7-18, #365).
//!
//! A movie is `genesis::movie`'s `.rsm`: the frames on which the pads changed,
//! small enough to read, exact enough to replay (`docs/movie-format.md`). The
//! console half - `Movie::record`, `Movie::set_pads`, the format itself - is
//! proven in the core crate; what lives here is the browser session around it:
//!
//! * [`MovieSession`], the one state the frame loop consults: nothing, a
//!   recording, or a replay. While a recording is on, the pads the page's
//!   [`crate::input::InputState`] reports go to the machine unchanged and
//!   `Movie::record` notes them; while a replay is on, `Movie::set_pads`
//!   drives both pads from the movie and the page's live input is not read at
//!   all - a movie owns the pads until it is stopped, which is what makes the
//!   frames it plays the frames it recorded.
//! * [`prepare_replay`], the admission check: the text has to parse, to name
//!   this cartridge (`Movie::check_rom`'s comparison, against the hash the
//!   running console was started with) and to be of the region this run
//!   resolved to. Every refusal answers in the words of [`ReplayError`],
//!   which is what the toast shows.
//! * The `.rsm` download ([`save_to_disk`], [`MovieFileInput`]) - the same
//!   Blob-and-anchor pattern `crate::srm` uses for a save, and a paste box
//!   beside it, because a movie is text and half of them are written by hand.
//!
//! The drawer's section in [`crate::app`] mounts the buttons; the top bar's
//! chip says a session is on while the drawer is closed. Both read
//! [`Controls::movie`] (the [`MovieState`]) rather than the session itself,
//! which is owned by the running console and dies with it.
//!
//! A recording or a replay (re)starts the run: the console is reset and the
//! frame count goes back to 0, because a movie's frame 0 is the first frame
//! of a run and a movie begun mid-game is a movie of nothing in particular.
//! A movie that holds `X`, `Y`, `Z` or `Mode` replays on a 6-button pad by
//! itself (`Movie::set_pads`, #364); the front end records whatever the page
//! holds and lets the file decide.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use genesis::movie::{self, Movie, MovieError};
use genesis::{Buttons, Config, Genesis, Region};

use crate::emulator::Controls;
use crate::input::PORTS;
use crate::ui::toast::{ToastKind, Toasts};

/// The extension a movie is downloaded and expected back under.
pub const MOVIE_SUFFIX: &str = ".rsm";

/// The MIME of a `.rsm`: it is text, and `text/plain` is what the format doc
/// says it is - a file a person can open and edit.
pub const MOVIE_MIME: &str = "text/plain";

/// The id of the hidden input [`MovieFileInput`] mounts and
/// [`open_movie_picker`] clicks, exactly as the ROM picker and the `.srm`
/// input do it (#136, #398).
pub const MOVIE_INPUT_ID: &str = "movie-input";

/// The pads a movie here covers: the page's two ports ([`PORTS`]).
///
/// A recording is of a game somebody is playing, so it records both players
/// whether or not the second one ever touches a pad - which is why the empty
/// second column is in every line of what a recording writes, and why a
/// one-pad movie from elsewhere replays with its single column on port 0.
pub const MOVIE_PADS: usize = PORTS;

/// The movie identity of a cartridge image: [`movie::rom_hash`], the value a
/// `.rsm`'s `rom-hash` line carries. Re-exported because it is what
/// [`prepare_replay`] is checked against and what a recording starts from.
pub use movie::rom_hash;

/// The name a recording is downloaded under: the ROM's SHA-1, which is the id
/// SRAM and save states already use. The id is the identity the movie belongs
/// to and survives a renamed ROM; the movie's own `rom-hash` line says the
/// rest, and a replay never reads the name anyway.
pub fn file_name(rom_id: &str) -> String {
    format!("{rom_id}{MOVIE_SUFFIX}")
}

/// The region the console runs `rom` as: the same three steps
/// `Genesis::new` walks - normalize, header, [`Config::resolve_region`] under
/// the page's default configuration - for the caller that wants the region
/// and not the machine.
///
/// `None` for an image with no usable header, which `Genesis::new` would have
/// refused anyway; the caller only ever asks about a cartridge that is
/// already running.
pub fn run_region(rom: &[u8]) -> Option<Region> {
    let rom = genesis::cart::normalize_rom(rom);
    let header = genesis::cart::parse_header(&rom).ok()?;
    Some(Config::default().resolve_region(&header))
}

/// What the movie session of the running console is doing, as the shell sees
/// it: the chip in the top bar, the labels of the drawer's buttons, and the
/// "stop" that goes with whichever one is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MovieState {
    /// Nothing is rolling.
    #[default]
    Idle,
    /// A recording is on: the pads go to the machine as the page reports
    /// them, and stopping writes the run out as a `.rsm`.
    Recording,
    /// A replay is on: the movie owns the pads and the page's own input is
    /// ignored, until it is stopped.
    Replaying,
}

impl MovieState {
    /// The chip's word for this state; empty when there is nothing to say.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::Recording => "Recording input",
            Self::Replaying => "Replaying movie",
        }
    }
}

/// Why a replay was refused, in the one sentence a toast shows.
///
/// The parse half of this is [`MovieError`], which already names the line and
/// the letter (the point of a text format is that a person fixes it by hand),
/// so [`Display`](core::fmt::Display) here passes those words through. This
/// adds its own only for the two checks the console makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// `movie::parse` refused the text.
    Parse(MovieError),
    /// The movie is for another cartridge: its `rom-hash` is not the hash the
    /// running console was started with - the same comparison
    /// `Movie::check_rom` makes against the image, and the same refusal
    /// headless gives a `--movie` for the wrong game.
    WrongRom,
    /// The movie is of the right cartridge in the wrong region: a PAL replay
    /// of an NTSC recording is a different run rather than a broken file, so
    /// it is refused with both regions named.
    RegionMismatch {
        /// The region the movie was recorded on.
        movie: Region,
        /// The region this run resolved to.
        run: Region,
    },
}

impl core::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "{error}"),
            Self::WrongRom => write!(f, "That movie was recorded for a different cartridge"),
            Self::RegionMismatch { movie, run } => write!(
                f,
                "That movie is for the {} region, and this cartridge runs as {}",
                movie.name(),
                run.name()
            ),
        }
    }
}

impl std::error::Error for ReplayError {}

/// Admits `text` as this run's replay: it has to parse, to name the cartridge
/// `rom_hash` was taken from, and to be of the region the run resolved to.
///
/// The two checks are the console's rather than `genesis::movie`'s - the
/// machine has no region of its own to compare against and no image to hash
/// once it is running - which is why they sit here, exactly where headless
/// keeps its pair (`run.rs`'s `read_movie`). The hash comparison is
/// `Movie::check_rom`'s own predicate against the value
/// [`rom_hash`] of the image produced when the console started.
pub fn prepare_replay(text: &str, rom_hash: u64, region: Region) -> Result<Movie, ReplayError> {
    let parsed = movie::parse(text).map_err(ReplayError::Parse)?;
    if parsed.rom_hash() != rom_hash {
        return Err(ReplayError::WrongRom);
    }
    if parsed.region() != region {
        return Err(ReplayError::RegionMismatch {
            movie: parsed.region(),
            run: region,
        });
    }
    Ok(parsed)
}

/// The movie state the frame loop consults, once per frame before
/// [`Genesis::run_frame`]: where this frame's pads come from, and whether
/// they are being written down.
#[derive(Debug, Default)]
pub enum MovieSession {
    /// Nothing is on: the pads are the page's live input, as ever.
    #[default]
    Idle,
    /// A recording is on. The pads still go to the machine as the page
    /// reports them - a recording is of a game somebody is playing - and
    /// [`Movie::record`] notes what was held, so the file lists only the
    /// frames where the input changed.
    Recording(Movie),
    /// A replay is on. [`Movie::set_pads`] drives both pads from the movie -
    /// attaching a 6-button pad for a movie that holds `X`, `Y`, `Z` or
    /// `Mode` by itself (#364) - and the page's live input is not read at
    /// all: the movie owns the pads until it is stopped.
    Replaying(Movie),
}

impl MovieSession {
    /// What this session is, for the chip and the buttons.
    pub fn state(&self) -> MovieState {
        match self {
            Self::Idle => MovieState::Idle,
            Self::Recording(_) => MovieState::Recording,
            Self::Replaying(_) => MovieState::Replaying,
        }
    }

    /// Whether a replay is on, which is the one state in which the frame
    /// loop's pads come from somewhere other than the page.
    pub fn drives_pads(&self) -> bool {
        matches!(self, Self::Replaying(_))
    }

    /// Starts a recording: both pads of the cartridge `rom_hash` names, on
    /// `region`, with nothing held from frame 0. Anything already on is
    /// replaced - a discarded recording is gone, there being no second file
    /// to keep it in.
    pub fn start_recording(&mut self, rom_hash: u64, region: Region) {
        *self = Self::Recording(Movie::new(rom_hash, region, MOVIE_PADS));
    }

    /// Puts `movie` on the pads. Anything already on is replaced, as
    /// [`MovieSession::start_recording`] is.
    pub fn start_replay(&mut self, movie: Movie) {
        *self = Self::Replaying(movie);
    }

    /// Stops whatever is on, handing back the recording if one was rolling -
    /// `None` for a replay, which has nothing to save, and for a session
    /// that was never on.
    pub fn stop(&mut self) -> Option<Movie> {
        match std::mem::take(self) {
            Self::Recording(movie) => Some(movie),
            Self::Replaying(_) | Self::Idle => None,
        }
    }

    /// The pads of the frame `frame` about to run: the movie's, with `live`
    /// unread, while a replay is on; otherwise `live`, with a rolling
    /// recorder noting them.
    ///
    /// Before [`Genesis::run_frame`], because that call reads the pads as
    /// they stand at the start of the frame (M7-07, #140) - the same moment
    /// the page's input has always been applied at.
    pub fn sync_pads(&mut self, machine: &mut Genesis, frame: u32, live: &[Buttons; MOVIE_PADS]) {
        if let Self::Replaying(movie) = &*self {
            movie.set_pads(machine, frame);
            return;
        }
        for (port, buttons) in live.iter().enumerate() {
            machine.set_pad(port, *buttons);
        }
        if let Self::Recording(movie) = self {
            movie.record(frame, live);
        }
    }
}

/// The [`web_sys::Blob`] a `.rsm` download is made of.
///
/// Split out from [`save_to_disk`] so that the MIME a player's browser is
/// handed is assertable rather than believed, as `crate::srm`'s is.
pub fn blob(bytes: &[u8]) -> web_sys::Blob {
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(MOVIE_MIME);
    web_sys::Blob::new_with_u8_array_sequence_and_options(&parts.into(), &options)
        .expect("a Blob over bytes already in memory cannot fail")
}

/// The `<a download>` that hands `bytes` to the browser under `name`, appended
/// to the page and ready to click. Kept apart from the click so both are
/// testable, as `crate::srm`'s is.
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

/// Hand `bytes` to the browser as a file called `name`: an anchor, a click,
/// and both taken away again. The URL is revoked after a tick, as
/// [`crate::srm::save_to_disk`] revokes its own - a URL revoked before the
/// navigation is queued is a download of nothing.
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

/// Opens the browser's `.rsm` picker.
pub fn open_movie_picker() {
    use wasm_bindgen::JsCast;
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(element) = document.get_element_by_id(MOVIE_INPUT_ID) else {
        return;
    };
    element
        .dyn_into::<web_sys::HtmlInputElement>()
        .expect("`#movie-input` is the file input `MovieFileInput` mounts")
        .click();
}

/// The hidden `<input type="file">` a replay reads a `.rsm` from, mounted once
/// by [`crate::app`] beside the button that opens it.
///
/// A second input rather than reusing the ROM picker's or the `.srm` one's,
/// for the reason `SrmFileInput` gives: each routes what it is given to a
/// different reader. `accept` is the extension only - `text/plain` would let
/// anything through, and no MIME is registered for `.rsm` anyway.
#[component]
pub fn MovieFileInput() -> impl IntoView {
    let input_ref = NodeRef::<html::Input>::new();
    let console = expect_context::<Controls>();
    let toasts = expect_context::<Toasts>();

    view! {
        <input
            id=MOVIE_INPUT_ID
            class="file-input"
            type="file"
            accept=".rsm"
            // Out of the tab order, exactly as the ROM and `.srm` inputs are:
            // the control is invisible, so a keyboard user reaches the replay
            // button instead.
            tabindex="-1"
            node_ref=input_ref
            on:change=move |_| {
                let Some(input) = input_ref.get() else { return };
                let file = input.files().and_then(|files| files.item(0));
                // Cleared first, so replaying the same file twice - after
                // refusing the wrong one, say - is a second change rather
                // than nothing at all.
                input.set_value("");
                let Some(file) = file else { return };
                let toasts = toasts;
                spawn_local(read_and_replay(file, console, toasts));
            }
        />
    }
}

/// One picked file, into bytes, into text, into [`Controls::replay_movie`].
///
/// A `.rsm` is text, so a pick that is not is refused here rather than handed
/// to the parser as replacement characters; the parse and the checks are the
/// replay's own answer, and they say so in the toast.
async fn read_and_replay(file: web_sys::File, console: Controls, toasts: Toasts) {
    let name = file.name();
    let Ok(bytes) = crate::rom_picker::read_file(&file).await else {
        toasts.push(ToastKind::Danger, format!("{name} could not be read"));
        return;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        toasts.push(
            ToastKind::Danger,
            format!("{name} is not a .rsm movie, which is text"),
        );
        return;
    };
    console.replay_movie(text, toasts);
}
