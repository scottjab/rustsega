//! The ROM harness, shared by every test file in `crates/genesis/tests`
//! (M6-03, #122).
//!
//! One regression is three lines: which ROM, how many frames with what input, and
//! which golden key to compare the picture against.
//!
//! ```text
//! run_and_check(RomRun {
//!     rom: PathBuf::from("Sonic The Hedgehog (USA, Europe).md"),
//!     frames: 300,
//!     inputs: &[(240, Buttons::START)],
//!     golden_file: "sonic.toml",
//!     key: "title_screen",
//! });
//! ```
//!
//! [`run_and_check`] is the whole thing: it loads the ROM, runs it, writes the
//! last frame to `target/test-output/` as a PNG **before** it asserts anything,
//! then compares the frame hash with `crates/genesis/tests/golden/<file>` through
//! [`testkit::assert_golden`]. A run that fails therefore always leaves a picture
//! behind, which is how a golden is reviewable at all (`docs/TESTING.md`, "Golden
//! files": "Dump a PNG with `testkit::dump_png` and look at it before accepting a
//! new hash").
//!
//! [`run_frames`] is the same run on its own, for a test that wants to look
//! inside the machine before deciding what the golden is allowed to be.
//!
//! A ROM that is not on disk is a skip, never a failure - CI has no commercial
//! cartridges - so a test file full of ROM regressions stays green there and says
//! `SKIPPED:` on the way past each one (see [`read_rom`]).
//!
//! Which games to run is `commercial_list.toml`'s business, read by [`load_games`],
//! so every commercial suite starts from the same cartridges with the same START
//! presses.
//!
//! # Reuse
//!
//! Each integration-test binary compiles its own copy of this module and only
//! sees the items it uses, so an item written for a later ticket's test file
//! looks unused here. That is the point of a shared harness, so the lint is off
//! for the whole module rather than sprinkled through it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use genesis::{Buttons, Config, Genesis};

/// The game list: the configuration shared by the commercial suites, sitting next
/// to the test files in `crates/genesis/tests` (see [`load_games`]).
pub const LIST_FILE: &str = "commercial_list.toml";

/// The message of a caught panic, for a suite that runs the list game by game and
/// reports every game rather than aborting on the first one.
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "panicked with a non-string payload".to_string())
    }
}

/// A scratch directory that deletes itself, so a test can put a ROM where a ROM
/// goes without littering `/tmp`.
///
/// `testkit`'s own `TempDir` is test-only inside `testkit` and so unreachable
/// from here; `crates/headless/tests/cli.rs` grew the same three lines for the
/// same reason.
pub struct TempDir(PathBuf);

impl TempDir {
    /// `tag` only shows up in the directory name, to make a failure easy to trace
    /// back to the test that made it.
    pub fn new(tag: &str) -> TempDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rustsega-genesis-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    /// A path inside this directory.
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The directory itself.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// This crate's directory, so that nothing here depends on the cwd a test runs in
/// (`cargo` sets it to the crate root, but the golden files and the PNG output
/// are both found from here rather than by guessing).
fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Where golden files live: `crates/genesis/tests/golden/<file>`.
pub fn golden_path(golden_file: &str) -> PathBuf {
    crate_dir().join("tests/golden").join(golden_file)
}

/// Where a run's picture goes for a human to look at:
/// `target/test-output/<suite>/<key>.png`, under the workspace `target` so it
/// never lands in the repo.
///
/// Grouped by golden file, so a suite's pictures stay together and two suites
/// that use the same key name do not overwrite each other.
pub fn golden_png(golden_file: &str, key: &str) -> PathBuf {
    let stem = Path::new(golden_file)
        .file_stem()
        .map(|stem| stem.to_os_string())
        .unwrap_or_else(|| golden_file.into());
    crate_dir()
        .join("../../target/test-output")
        .join(stem)
        .join(format!("{key}.png"))
}

/// The ROM's bytes, or `None` - after printing `SKIPPED:` - when the file is not
/// there.
///
/// Missing is the one thing a ROM lookup may answer with: CI has no commercial
/// cartridges, and `docs/TESTING.md` is explicit that absent data is a skip
/// rather than a failure. A file that exists but cannot be read is a different
/// matter and panics, because calling a broken cartridge a skip is how a real
/// failure goes unnoticed.
pub fn read_rom(rom: &Path) -> Option<Vec<u8>> {
    match std::fs::read(rom) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIPPED: no ROM at {}", rom.display());
            None
        }
        Err(err) => panic!("cannot read ROM {}: {err}", rom.display()),
    }
}

/// Load `rom` and run it for `frames` frames with `inputs` on port 1.
///
/// Frame numbers in `inputs` are 1-based and a frame with no entry is run with the
/// pad released, which is how `headless --pad1` spells a script
/// (`crates/headless/src/run.rs`) so the two cannot mean different things by the
/// same text. The pad is set before the frame runs, i.e. a button is held
/// *during* the frame it names.
///
/// Panics on a ROM that will not load, because a caller that got as far as
/// running it means to: [`run_and_check`] is what turns a missing ROM into a skip.
pub fn run_frames(rom: &Path, frames: u32, inputs: &[(u32, Buttons)]) -> Genesis {
    let bytes =
        std::fs::read(rom).unwrap_or_else(|e| panic!("cannot read ROM {}: {e}", rom.display()));
    run_bytes(&bytes, frames, inputs)
}

/// The run behind [`run_frames`] and [`run_and_check`], for a caller that already
/// has the image - which is what makes a skip and a run share one path.
pub fn run_bytes(bytes: &[u8], frames: u32, inputs: &[(u32, Buttons)]) -> Genesis {
    let mut machine = Genesis::new(bytes, Config::default())
        .unwrap_or_else(|e| panic!("cannot load ROM image: {e:?}"));
    for frame in 1..=frames {
        let buttons = inputs
            .iter()
            .find(|(number, _)| *number == frame)
            .map_or_else(Buttons::empty, |(_, buttons)| *buttons);
        machine.set_pad(0, buttons);
        machine.run_frame();
    }
    machine
}

/// One ROM regression: which ROM, how many frames with what input, and which
/// golden key the last frame is compared against.
///
/// `golden_file` is a file name in `crates/genesis/tests/golden`, and `key` is the
/// key inside it, so a suite of regressions is one TOML file (see
/// [`golden_path`] and [`golden_png`] for where each half lands on disk).
pub struct RomRun<'a> {
    /// The cartridge image to load. Absent means the run is skipped.
    pub rom: PathBuf,
    /// Frames to run; the golden is the picture the last of them leaves behind.
    pub frames: u32,
    /// Port 1, as `(frame, buttons)`, 1-based - see [`run_frames`].
    pub inputs: &'a [(u32, Buttons)],
    /// Golden file name under `crates/genesis/tests/golden`, `"sonic.toml"`.
    pub golden_file: &'a str,
    /// Key within it, `"title_screen_300"`.
    pub key: &'a str,
}

/// Run `run` and compare its last frame with the golden file.
///
/// A missing ROM is a skip: [`read_rom`] prints `SKIPPED:` and this returns
/// without touching the golden, so an absent cartridge neither fails CI nor
/// records a value.
///
/// The PNG is written before [`testkit::assert_golden`] runs, so the frame a
/// mismatch complains about is on disk to be looked at.
pub fn run_and_check(run: RomRun<'_>) {
    let Some(bytes) = read_rom(&run.rom) else {
        return;
    };
    let machine = run_bytes(&bytes, run.frames, run.inputs);

    let frame = machine.frame();
    let pixels = &frame.pixels[..frame.width * frame.height];
    testkit::dump_png(
        &golden_png(run.golden_file, run.key),
        pixels,
        frame.width,
        frame.height,
    );

    let hash = testkit::frame_hash(pixels, frame.width, frame.height);
    testkit::assert_golden(&golden_path(run.golden_file), run.key, hash);
}

// ---------------------------------------------------------------------------\n
// The game list of `commercial_list.toml`.\n
//\n
// It lives here rather than in `commercial.rs` because it is the configuration\n
// of more than one suite: the smoke test of `commercial.rs` and the save-state\n
// determinism test of `save_state.rs` (M6-08, #128) both run the same games with\n
// the same START presses, and two parsers for one file is two ways for the two\n
// suites to disagree about what the list said.\n
// ---------------------------------------------------------------------------\n
/// One `[[game]]` block of [`LIST_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Game {
    /// The exact file name of the cartridge under `$RUSTSEGA_ROM_DIR`, without
    /// its extension. Exact, not a prefix: [`testkit::commercial_rom`] takes a
    /// whole name, and [`testkit::find_commercial_rom`] - which does prefix
    /// matching - answers with a Beta build under the same stem.
    pub prefix: String,
    /// Short name for the golden key and the PNG: `"sonic1"`.
    pub key: String,
    /// Frames to run; the golden is the last of them.
    pub frames: u32,
    /// 1-based frames on which port 1 holds START - see [`run_frames`].
    pub start_press: Vec<u32>,
}

impl Game {
    /// A field this entry still needs, if any: `Some("frames")` for a block that
    /// did not say how long to run. Empty values stand for "absent", so
    /// `frames = 0` is refused by the same arm.
    pub fn missing_field(&self) -> Option<&'static str> {
        if self.key.is_empty() {
            Some("key")
        } else if self.prefix.is_empty() {
            Some("prefix")
        } else if self.frames == 0 {
            Some("frames")
        } else {
            None
        }
    }
}

/// Path of [`LIST_FILE`].
pub fn list_path() -> PathBuf {
    crate_dir().join("tests").join(LIST_FILE)
}

/// The three shapes a value of a `[[game]]` block takes in [`LIST_FILE`].
enum Field {
    Text(String),
    Number(u32),
    Numbers(Vec<u32>),
}

/// `value` as an integer, or an error naming the line and the field.
fn parse_number(name: &str, value: &str, line: usize) -> Result<u32, String> {
    value.parse::<u32>().map_err(|e| {
        format!("line {line}: {name} = {value}: expected a quoted string or a number: {e}")
    })
}

/// A `name = value` right-hand side: a quoted string, an integer, or an inline
/// array of integers.
///
/// Anything else is an error naming the line. A value this test cannot read has
/// to be loud: a game dropped from the list is a smaller suite that still
/// reports green.
fn parse_value(name: &str, value: &str, line: usize) -> Result<Field, String> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('"') {
        let text = rest.strip_suffix('"').ok_or_else(|| {
            format!("line {line}: {name} = {value}: expected a quoted string ending in `\"`")
        })?;
        if text.contains('"') {
            return Err(format!(
                "line {line}: {name} = {value}: no escaped quotes in the list, please"
            ));
        }
        return Ok(Field::Text(text.to_string()));
    }
    if let Some(rest) = value.strip_prefix('[') {
        let inner = rest
            .strip_suffix(']')
            .ok_or_else(|| format!("line {line}: {name} = {value}: expected `]`"))?;
        let mut numbers = Vec::new();
        for item in inner
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            numbers.push(parse_number(name, item, line)?);
        }
        return Ok(Field::Numbers(numbers));
    }
    Ok(Field::Number(parse_number(name, value, line)?))
}

/// One line of a `[[game]]` block, applied to `current`.
fn apply_line(line: usize, text: &str, current: &mut Game) -> Result<(), String> {
    let (name, value) = text.split_once('=').ok_or_else(|| {
        format!("line {line}: expected `name = value` or `[[game]]`, got `{text}`")
    })?;
    let name = name.trim();
    match parse_value(name, value, line)? {
        Field::Text(text) => match name {
            "key" => current.key = text,
            "prefix" => current.prefix = text,
            _ => return Err(format!("line {line}: unknown list field `{name}`")),
        },
        Field::Number(number) => match name {
            "frames" => current.frames = number,
            _ => return Err(format!("line {line}: unknown list field `{name}`")),
        },
        Field::Numbers(frames) => match name {
            "start_press" => current.start_press = frames,
            _ => return Err(format!("line {line}: unknown list field `{name}`")),
        },
    }
    Ok(())
}

/// Parse the `[[game]]` blocks of [`LIST_FILE`].
///
/// `genesis` depends on its chips and on nothing else, tests included
/// (`crates/genesis/Cargo.toml`), so these lines of TOML reading stand in for
/// adding the `toml` crate to it. The accepted shape is exactly what the file
/// uses, and `parse_games_rejects_what_it_cannot_read` covers the ways a
/// hand-edited list can be wrong.
pub fn parse_games(text: &str) -> Result<Vec<Game>, String> {
    let mut games: Vec<Game> = Vec::new();
    let mut current = Game::default();
    let mut open = false;

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if text == "[[game]]" {
            if open {
                if let Some(field) = current.missing_field() {
                    return Err(format!("line {line}: [[game]] block has no `{field}`"));
                }
                games.push(current.clone());
            }
            current = Game::default();
            open = true;
            continue;
        }
        if !open {
            return Err(format!(
                "line {line}: `{name}` before any `[[game]]`",
                name = text
            ));
        }
        apply_line(line, text, &mut current)?;
    }

    if open {
        if let Some(field) = current.missing_field() {
            return Err(format!("last [[game]] block has no `{field}`"));
        }
        games.push(current);
    }
    Ok(games)
}

/// The list as committed, parsed and checked.
///
/// Panics with the file and the line on the table when it is wrong: a broken list
/// is a broken suite, not a list of skips.
pub fn load_games() -> Vec<Game> {
    let path = list_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: cannot read the game list: {e}", path.display()));
    parse_games(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
