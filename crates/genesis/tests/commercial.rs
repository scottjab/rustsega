//! Commercial smoke tests: boot a list of well-known games, run them for ten
//! seconds of emulated time, and hold the machine to three things - it did not
//! panic, it drew a picture, and that picture is the picture it drew last time
//! (M6-04, #123).
//!
//! The games live in `commercial_list.toml` next to this file, and that file is
//! the whole configuration: ROM name, frames to run, when to press START. Adding
//! a game is one more `[[game]]` block, and
//! `commercial_list_lists_at_least_fifteen_games` is the check in `just check`
//! that it stayed a list of games.
//!
//! # What a golden here means
//!
//! `tests/golden/commercial.toml` is a regression baseline, not a claim that a
//! game is emulated correctly: it records what this emulator produced at frame
//! 600, so the next change that moves a pixel is visible. Each game's picture
//! is written to `target/test-output/commercial/` before its value is compared,
//! the per-game table of what is in the list and what is held out of it is
//! `docs/compat-commercial.md`, and blessing follows `docs/TESTING.md`
//! ("Golden files").
//!
//! # Why it is one test, and why it collects
//!
//! The list is long and a run is slow, so this is one `#[ignore]`d test rather
//! than one per game: nextest runs tests in parallel, and twenty-odd writers of
//! one golden file is a race, not a suite. So every game's result is collected
//! instead of asserted as it goes, because "which of these games broke" is the
//! question and the first answer alone does not settle it. A panic is caught per
//! game for the same reason - "no panics" is one of the three things under test
//! here, and an abort on the first one would hide every game behind it.
//!
//! # Running it
//!
//! It needs cartridges, so CI skips it and `just test-commercial` runs it:
//!
//! ```sh
//! nix develop -c just test-commercial
//! ```
//!
//! A ROM that is not on disk is a skip, never a failure (`docs/TESTING.md`,
//! "External test data"), so the run prints `SKIPPED:` for each name it cannot
//! find and judges only what it has.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use genesis::Buttons;

use common::{golden_path, golden_png, run_frames};

/// The game list: the configuration of this test, in the directory of this test.
const LIST_FILE: &str = "commercial_list.toml";

/// Golden file for the whole list, in `crates/genesis/tests/golden`.
const GOLDEN_FILE: &str = "commercial.toml";

/// A frame with this many distinct colours or fewer is a blank screen: the
/// compare is `>`, so a game has to clear this to count as drawing.
///
/// Four rather than one, because a game that has got as far as a title card is
/// showing tens or hundreds of colours, while a backdrop and a border - what the
/// chip's reset state and a renderer that draws nothing leave behind - sit well
/// under it. The thinnest picture in the list at its checkpoint is five
/// (`nbajam_600`, an attract screen); the fullest is ninety-six.
const BLANK_COLOUR_LIMIT: usize = 4;

/// The acceptance floor of the ticket: below this the suite is testing one
/// cartridge, not the emulator.
const MIN_GAMES: usize = 15;

/// One `[[game]]` block of [`LIST_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Game {
    /// The exact file name of the cartridge under `$RUSTSEGA_ROM_DIR`, without
    /// its extension. Exact, not a prefix: [`testkit::commercial_rom`] takes a
    /// whole name, and [`testkit::find_commercial_rom`] - which does prefix
    /// matching - answers with a Beta build under the same stem.
    prefix: String,
    /// Short name for the golden key and the PNG: `"sonic1"`.
    key: String,
    /// Frames to run; the golden is the last of them.
    frames: u32,
    /// 1-based frames on which port 1 holds START - see [`run_frames`].
    start_press: Vec<u32>,
}

impl Game {
    /// A field this entry still needs, if any: `Some("frames")` for a block that
    /// did not say how long to run. Empty values stand for "absent", so
    /// `frames = 0` is refused by the same arm.
    fn missing_field(&self) -> Option<&'static str> {
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

/// This crate's directory, so neither the list nor the goldens depend on a cwd.
fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Path of [`LIST_FILE`].
fn list_path() -> PathBuf {
    crate_dir().join("tests").join(LIST_FILE)
}

/// The golden key of a game: `<key>_<frames>`, e.g. `"sonic1_600"`.
///
/// The frame count is part of it so changing how long a game runs cannot quietly
/// compare against a picture taken at a different moment.
fn golden_key(game: &Game) -> String {
    format!("{}_{}", game.key, game.frames)
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
fn parse_games(text: &str) -> Result<Vec<Game>, String> {
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
fn load_games() -> Vec<Game> {
    let path = list_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: cannot read the game list: {e}", path.display()));
    parse_games(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// One game, from the ROM at `rom`: run it, write its picture, then compare that
/// picture with its golden.
///
/// The PNG comes out before any assertion, so a failure leaves the frame that
/// caused it behind to be looked at - the rule `docs/TESTING.md` states for every
/// golden in the repo, and the reason [`common`] writes its own the same way.
fn smoke(game: &Game, rom: &Path) {
    let inputs: Vec<(u32, Buttons)> = game
        .start_press
        .iter()
        .map(|&frame| (frame, Buttons::START))
        .collect();

    let machine = run_frames(rom, game.frames, &inputs);
    let frame = machine.frame();
    let pixels = &frame.pixels[..frame.width * frame.height];
    let key = golden_key(game);

    testkit::dump_png(
        &golden_png(GOLDEN_FILE, &key),
        pixels,
        frame.width,
        frame.height,
    );

    let colours: HashSet<u32> = pixels.iter().copied().collect();
    assert!(
        colours.len() > BLANK_COLOUR_LIMIT,
        "frame {} is {} distinct colours, which is a blank screen",
        game.frames,
        colours.len()
    );

    testkit::assert_golden(
        &golden_path(GOLDEN_FILE),
        &key,
        testkit::frame_hash(pixels, frame.width, frame.height),
    );
}

/// The message of a caught panic, for the failure list of
/// [`commercial_games_boot_and_draw`].
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "panicked with a non-string payload".to_string())
    }
}

/// Every game of the list, one at a time, every result reported.
///
/// A missing ROM is a skip on the way past (CI has no cartridges), so the test
/// passes there having done nothing; `just test-commercial` is how it runs.
#[test]
#[ignore = "needs commercial ROMs; run with `just test-commercial`"]
fn commercial_games_boot_and_draw() {
    let mut passed = Vec::new();
    let mut skipped = Vec::new();
    let mut failures = Vec::new();

    for game in load_games() {
        let Some(rom) = testkit::commercial_rom(&format!("{}.md", game.prefix)) else {
            eprintln!("SKIPPED: no ROM named {}.md", game.prefix);
            skipped.push(game.key);
            continue;
        };

        // A panic is a result, not an end to the run: see the module docs.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| smoke(&game, &rom))) {
            Ok(()) => {
                eprintln!("ok: {}", game.key);
                passed.push(game.key);
            }
            Err(payload) => {
                let reason = panic_text(payload.as_ref());
                failures.push(format!("{}: {}", game.key, reason.trim()));
            }
        }
    }

    let total = passed.len() + skipped.len() + failures.len();
    eprintln!(
        "commercial: {} passed, {} skipped, {} failed of {}",
        passed.len(),
        skipped.len(),
        failures.len(),
        total
    );
    if !failures.is_empty() {
        panic!(
            "{filled} of {total} commercial games failed:\n{list}",
            filled = failures.len(),
            list = failures
                .iter()
                .map(|failure| format!("  - {failure}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// The list on its own, with no ROM in sight: enough of it that `just check`
/// notices a list that stopped describing games.
#[test]
fn commercial_list_lists_at_least_fifteen_games() {
    let games = load_games();
    assert!(
        games.len() >= MIN_GAMES,
        "{} game(s) in {}, and the ticket asks for at least {MIN_GAMES}",
        games.len(),
        list_path().display()
    );
}

/// Every entry says what it is, and says it once.
///
/// A repeated key would be two games sharing one golden key, so the second
/// overwrites the first in `commercial.toml`; a repeated ROM would be one
/// cartridge tested twice while another goes untested.
#[test]
fn commercial_list_entries_are_complete_and_unique() {
    let mut keys: HashMap<String, usize> = HashMap::new();
    let mut prefixes: HashMap<String, usize> = HashMap::new();
    for game in load_games() {
        assert_eq!(game.missing_field(), None, "incomplete entry: {game:?}");
        *keys.entry(game.key).or_default() += 1;
        *prefixes.entry(game.prefix).or_default() += 1;
    }
    for (key, count) in keys {
        assert_eq!(count, 1, "game key {key:?} is used by {count} entries");
    }
    for (prefix, count) in prefixes {
        assert_eq!(count, 1, "ROM {prefix:?} is run by {count} entries");
    }
}

/// The goldens cover the list.
///
/// This is the only part of the suite CI can check, CI having no cartridges: a
/// game added to the list without a blessed baseline is otherwise a failure
/// nobody sees until someone runs `just test-commercial`.
#[test]
fn commercial_list_has_a_golden_baseline() {
    let golden = testkit::Golden::load(&golden_path(GOLDEN_FILE));
    let missing: Vec<String> = load_games()
        .iter()
        .map(golden_key)
        .filter(|key| golden.get(key).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "{} has no golden key for {missing:?}: bless them with `RUSTSEGA_BLESS=1 \
         nix develop -c cargo nextest run -p genesis commercial_games_boot_and_draw`",
        golden_path(GOLDEN_FILE).display()
    );
}

/// The list parser takes the shapes the list uses.
#[test]
fn parse_games_reads_a_list() {
    let text = r#"
# a comment
[[game]]
key = "sonic1"
prefix = "Sonic The Hedgehog (USA, Europe)"
frames = 600
start_press = [300]

[[game]]
key = "no_start"
prefix = "Some Other Game (World)"
frames = 42
start_press = []
"#;
    let games = parse_games(text).expect("the sample parses");
    assert_eq!(
        games,
        vec![
            Game {
                key: "sonic1".into(),
                prefix: "Sonic The Hedgehog (USA, Europe)".into(),
                frames: 600,
                start_press: vec![300],
            },
            Game {
                key: "no_start".into(),
                prefix: "Some Other Game (World)".into(),
                frames: 42,
                start_press: vec![],
            },
        ]
    );
}

/// A `[300, 301]` START press - two frames of the button, which is what some
/// title screens need - is one game with two entries in its list.
#[test]
fn parse_games_reads_a_multi_frame_start_press() {
    let games = parse_games(
        "[[game]]\nkey = \"g\"\nprefix = \"P\"\nframes = 600\nstart_press = [300, 301]\n",
    )
    .expect("the sample parses");
    assert_eq!(games[0].start_press, vec![300, 301]);
}

/// ...and refuses the ways a hand-edited list can go wrong, rather than quietly
/// dropping the entry that went wrong.
#[test]
fn parse_games_rejects_what_it_cannot_read() {
    let good = "[[game]]\nkey = \"g\"\nprefix = \"P\"\nframes = 1\n";

    let cases = [
        ("field before any [[game]]", "key = \"g\"\n".to_string()),
        (
            "missing frames",
            "[[game]]\nkey = \"g\"\nprefix = \"P\"\n".to_string(),
        ),
        (
            "missing prefix",
            "[[game]]\nkey = \"g\"\nframes = 1\n".to_string(),
        ),
        (
            "missing key",
            "[[game]]\nprefix = \"P\"\nframes = 1\n".to_string(),
        ),
        ("unknown field", format!("{good}colour = \"blue\"\n")),
        ("no equals sign", "[[game]]\nframes 600\n".to_string()),
        (
            "unquoted value",
            "[[game]]\nkey = g\nprefix = \"P\"\nframes = 1\n".to_string(),
        ),
        (
            "string where a number belongs",
            format!("{good}frames = \"600\"x\n"),
        ),
        (
            "unterminated string",
            "[[game]]\nkey = \"g\nprefix = \"P\"\nframes = 1\n".to_string(),
        ),
    ];

    for (what, text) in cases {
        let outcome = parse_games(&text);
        assert!(
            outcome.is_err(),
            "{what}: expected an error, got {outcome:?}"
        );
    }
}
