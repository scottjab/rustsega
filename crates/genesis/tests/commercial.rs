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
use std::path::Path;

use genesis::Buttons;

use common::{
    Game, golden_path, golden_png, list_path, load_games, panic_text, parse_games, run_frames,
};

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

/// The golden key of a game: `<key>_<frames>`, e.g. `"sonic1_600"`.
///
/// The frame count is part of it so changing how long a game runs cannot quietly
/// compare against a picture taken at a different moment.
fn golden_key(game: &Game) -> String {
    format!("{}_{}", game.key, game.frames)
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
