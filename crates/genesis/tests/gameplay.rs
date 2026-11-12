//! Gameplay regression tests: play a section of a real game with a recorded
//! input movie and golden the frames it leaves behind (M6-10, #130).
//!
//! `tests/commercial.rs` boots a game and stops at its title screen, a picture
//! no scrolling, no sprite motion and almost no DMA ever has to produce. This
//! suite runs a movie - the `.rsm` input recording of `genesis::movie`
//! (`docs/movie-format.md`) - that gets past the menus, holds Right into the
//! first level and jumps now and then, for about twenty seconds of play. What
//! that exercises is a running game: camera scrolling, sprite animation and
//! object pools, level DMA, and the audio of somebody actually playing.
//!
//! # What a golden here means
//!
//! Four values per game in `tests/golden/gameplay.toml`: the picture at three
//! checkpoints inside the play section (`<key>_<frame>`) and the sound of the
//! whole run (`<key>_audio`, FNV-1a 64 over the same PCM bytes
//! `headless --audio-hash` prints, M6-02 #121). As in `tests/commercial.rs`
//! these are a regression baseline for this emulator, not a claim that the game
//! is emulated correctly.
//!
//! The 15 checkpoint pictures were checked before blessing by automated image
//! classification rather than by a human eye, and every one of them was named
//! as in-level gameplay of the right game. That is a weaker guarantee than
//! looking: if a golden here ever fails in a way that suggests the emulator is
//! wrong, a person should look at the PNG before anyone believes or reblesses
//! it.
//!
//! Every checkpoint is written to `target/test-output/gameplay/` before it is
//! compared, and three things are asserted about it besides its golden: the
//! frame is not blank, the checkpoints differ from each other (the level
//! scrolled), and the frame is not what the same cartridge draws with nobody
//! touching a pad - it is gameplay, not a title screen that survived the movie.
//! Those three are what make a value worth blessing; a golden blessed from a
//! stuck frame would pass every comparison and prove nothing.
//!
//! # Movies
//!
//! They live in `tests/movies`, one per game: text, one line per frame on which
//! the input changed, no cartridge data, so they are committed. Each was
//! recorded with `headless run --pad1 ... --record` and trimmed by hand, which
//! the format is designed for, and each header names its cartridge and region,
//! so a movie cannot be played on the wrong game ([`movie::Movie::check_rom`],
//! `docs/movie-format.md`). `gameplay_movies_are_committed_and_parse` is
//! the part CI can check, CI having no cartridges.
//!
//! `X`, `Y`, `Z` and `Mode` are deliberately absent from every movie: replay
//! goes through [`Genesis::set_pad`], which attaches a 3-button pad, so those
//! four round-trip through the file but never reach a game (#364).
//!
//! # Running it
//!
//! It needs cartridges, so CI skips it; a ROM that is not on disk is a skip,
//! never a failure ([`common::read_rom`], `docs/TESTING.md`, "External test
//! data").
//!
//! ```sh
//! nix develop -c cargo nextest run -p genesis --run-ignored only gameplay
//! ```

mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use genesis::movie::{self, Movie};
use genesis::{Config, Genesis};

use common::{golden_path, golden_png, panic_text, read_rom};

/// Golden file for the whole suite, in `crates/genesis/tests/golden`.
const GOLDEN_FILE: &str = "gameplay.toml";

/// Movies, the directory under `crates/genesis/tests` that holds them.
const MOVIE_DIR: &str = "movies";

/// A frame with this many distinct colours or fewer is a blank screen: the same
/// limit and the same reasoning as `tests/commercial.rs`.
const BLANK_COLOUR_LIMIT: usize = 4;

/// A frame that is this much one colour is a blank screen as well, which the
/// colour count alone does not catch: a card - black with a handful of letters,
/// which is what one of these games was showing when its first checkpoint was
/// first chosen - holds eight distinct colours and says nothing is happening.
const BLANK_SHARE_PERCENT: usize = 60;

/// The acceptance floor of the ticket: below this the suite is one game, not a
/// gameplay regression suite.
const MIN_GAMES: usize = 5;

/// One game section: the cartridge, the movie played on it, how long the run is
/// and the frames its pictures are taken at.
struct Section {
    /// Golden key prefix and PNG name: `"sonic1_green_hill"`.
    key: &'static str,
    /// Exact cartridge file name under `$RUSTSEGA_ROM_DIR`, without its
    /// extension - exact, not a prefix, for the reason `commercial_list.toml`
    /// gives.
    prefix: &'static str,
    /// Movie file name under `tests/movies`.
    movie: &'static str,
    /// Frames to run: the movie plays the whole way and its audio is the audio
    /// golden.
    frames: u32,
    /// Frames the picture goldens are taken at, ascending, the last of them
    /// `frames`. Naming the frame rather than spacing it keeps a checkpoint
    /// honest: lengthening the run cannot quietly re-point a golden at a
    /// different moment.
    checkpoints: &'static [u32],
}

/// The games the ticket names, in the order it names them.
///
/// Green Hill and Emerald Hill are the two Sonic levels. Emerald Hill's movie
/// is the suite's two-pad movie: pad 1 is Sonic and pad 2 steers the partner,
/// and a second player character is visible on screen at its 2100 and 2600
/// checkpoints. Streets of Rage 2 and Gunstar Heroes are the two that need
/// several presses to get through their menus. Thunder Force IV is a vertical
/// shooter: holding Right pins the ship to the right of the screen and the
/// level scrolls at its own pace, which is a different load on the VDP from a
/// side-scroller chasing a character.
const SECTIONS: &[Section] = &[
    Section {
        key: "sonic1_green_hill",
        prefix: "Sonic The Hedgehog (USA, Europe)",
        movie: "sonic1_green_hill.rsm",
        frames: 2200,
        checkpoints: &[1200, 1700, 2200],
    },
    Section {
        key: "sonic2_emerald_hill",
        prefix: "Sonic The Hedgehog 2 (World)",
        movie: "sonic2_emerald_hill.rsm",
        frames: 2600,
        checkpoints: &[1600, 2100, 2600],
    },
    Section {
        key: "sor2_mission_1",
        prefix: "Streets of Rage 2 (USA)",
        movie: "sor2_mission_1.rsm",
        frames: 3400,
        checkpoints: &[2600, 3000, 3400],
    },
    Section {
        key: "gunstar_heroes_mission_1",
        prefix: "Gunstar Heroes (Europe)",
        movie: "gunstar_heroes_mission_1.rsm",
        frames: 4000,
        checkpoints: &[3000, 3500, 4000],
    },
    Section {
        key: "thf4_stage_1",
        prefix: "Thunder Force IV (Europe)",
        movie: "thf4_stage_1.rsm",
        frames: 3200,
        checkpoints: &[2200, 2700, 3200],
    },
];

/// Path of one section's movie file.
fn movie_path(section: &Section) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(MOVIE_DIR)
        .join(section.movie)
}

/// The region [`Genesis::new`] runs `rom` as, which is what a movie's `region`
/// line has to name: the same three steps `headless run --movie` walks
/// (`crates/headless/src/run.rs`).
fn resolved_region(rom: &[u8]) -> genesis::Region {
    let header = genesis::cart::parse_header(&genesis::cart::normalize_rom(rom))
        .expect("every game in the list has a usable header");
    Config::default().resolve_region(&header)
}

/// The movie of `section`: read, parsed, and checked against `rom` by hash and
/// by region.
///
/// Panics rather than skipping: a movie that does not fit the game it is
/// committed next to is a broken test, not a machine without a cartridge.
fn read_movie(section: &Section, rom: &[u8]) -> Movie {
    let path = movie_path(section);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: cannot read the movie: {e}", path.display()));
    let movie = movie::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    movie
        .check_rom(rom)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        movie.region(),
        resolved_region(rom),
        "{} is a {} movie played on a {} cartridge",
        section.movie,
        movie.region().name(),
        resolved_region(rom).name()
    );
    movie
}

/// The movie's frame golden key: `<key>_<frame>`.
fn frame_key(section: &Section, frame: u32) -> String {
    format!("{}_{}", section.key, frame)
}

/// The machine `rom` on the frame `movie` says to be on: `set_pads` then
/// `run_frame`, which is all a replay is (`docs/movie-format.md`, "Replaying").
///
/// Every frame drains the audio - the run's sound is one of the two things
/// being goldened here - and stops to record the picture at each of
/// `section.checkpoints`, writing its PNG first so that whatever is asserted
/// next leaves a frame behind to look at (`docs/TESTING.md`, "Golden files").
///
/// Returns the checkpoint hashes, in `section.checkpoints` order, and the audio
/// hash of the whole run.
fn play(section: &Section, rom: &[u8], movie: &Movie) -> (Vec<u64>, u64) {
    let mut machine = Genesis::new(rom, Config::default())
        .unwrap_or_else(|e| panic!("{}: cannot load ROM image: {e:?}", section.prefix));

    let mut hashes = Vec::new();
    let mut audio: Vec<i16> = Vec::new();
    for frame in 0..section.frames {
        movie.set_pads(&mut machine, frame);
        machine.run_frame();
        // After the frame, as `headless run` does: the buffer then holds the
        // raster the machine just ran through (`crates/headless/src/run.rs`).
        machine.drain_audio(&mut audio);

        let played = frame + 1;
        if !section.checkpoints.contains(&played) {
            continue;
        }
        let shot = machine.frame();
        let pixels = shot.pixels[..shot.width * shot.height].to_vec();
        testkit::dump_png(
            &golden_png(GOLDEN_FILE, &frame_key(section, played)),
            &pixels,
            shot.width,
            shot.height,
        );
        assert_shows_a_level(section, played, &pixels);
        hashes.push(testkit::frame_hash(&pixels, shot.width, shot.height));
    }
    (hashes, audio_hash(&audio))
}

/// Assert that `pixels` is a picture of something happening: enough distinct
/// colours to be a drawn scene ([`BLANK_COLOUR_LIMIT`], the limit and the
/// reasoning `tests/commercial.rs` shares), and no one colour over
/// [`BLANK_SHARE_PERCENT`] of the frame, which is a card rather than a level -
/// the shape a "PRESS START" or a fade-out leaves behind, and the reason this
/// check is here rather than just the colour count.
fn assert_shows_a_level(section: &Section, played: u32, pixels: &[u32]) {
    let mut counted: HashMap<u32, usize> = HashMap::new();
    for pixel in pixels {
        *counted.entry(*pixel).or_insert(0) += 1;
    }
    let colours = counted.len();
    let busiest = counted.values().copied().max().unwrap_or(0);
    let share = busiest * 100 / pixels.len();
    assert!(
        colours > BLANK_COLOUR_LIMIT && share < BLANK_SHARE_PERCENT,
        "{} frame {played} is {colours} colours with the busiest one covering {share}% of the \
         frame, which is a blank screen or a card rather than a level",
        section.key,
    );
}

/// The audio golden's value: [`testkit::fnv1a_64`] over the run's PCM bytes, so
/// a value here and `headless --audio-hash` cannot disagree (M6-02, #121).
fn audio_hash(samples: &[i16]) -> u64 {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    testkit::fnv1a_64(&bytes)
}

/// The picture `rom` leaves after `frames` frames with nobody touching a pad:
/// the title or attract screen every movie here has to have got past.
fn idle_frame_hash(rom: &[u8], frames: u32) -> u64 {
    let mut machine = Genesis::new(rom, Config::default()).expect("the caller loaded this ROM");
    for _ in 0..frames {
        machine.run_frame();
    }
    let shot = machine.frame();
    let pixels = &shot.pixels[..shot.width * shot.height];
    testkit::frame_hash(pixels, shot.width, shot.height)
}

/// One game: play its movie, hold the result to the three properties that say
/// "a game was played", then compare every value with its golden.
fn check(section: &Section, rom: &Path) {
    let bytes = read_rom(rom).expect("the caller checked this ROM is there");
    let movie = read_movie(section, &bytes);
    let (hashes, audio) = play(section, &bytes, &movie);

    assert_eq!(
        hashes.len(),
        section.checkpoints.len(),
        "{} took {} pictures of {} checkpoints",
        section.key,
        hashes.len(),
        section.checkpoints.len()
    );
    let distinct: HashSet<u64> = hashes.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        hashes.len(),
        "{} checkpoints {:?} are {hashes:x?}: the same picture twice means nothing scrolled",
        section.key,
        section.checkpoints,
    );

    let untouched = idle_frame_hash(&bytes, section.frames);
    for (frame, hash) in section.checkpoints.iter().zip(&hashes) {
        assert_ne!(
            *hash, untouched,
            "{} frame {frame} is what {} draws with nobody touching a pad",
            section.key, section.prefix
        );
    }

    for (frame, hash) in section.checkpoints.iter().zip(&hashes) {
        testkit::assert_golden(
            &golden_path(GOLDEN_FILE),
            &frame_key(section, *frame),
            *hash,
        );
    }
    testkit::assert_golden(
        &golden_path(GOLDEN_FILE),
        &format!("{}_audio", section.key),
        audio,
    );
}

/// Every section of [`SECTIONS`], one at a time, every result reported.
///
/// The reason is the one in `tests/commercial.rs`: "which of these games broke"
/// is the question, and a panic on the first one hides every game behind it.
/// One test rather than one per game because nextest would run the games in
/// parallel into one golden file.
#[test]
#[ignore = "needs commercial ROMs; run with `cargo nextest run -p genesis --run-ignored only gameplay`"]
fn gameplay_movies_play_into_the_first_level() {
    let mut passed = Vec::new();
    let mut skipped = Vec::new();
    let mut failures = Vec::new();

    for section in SECTIONS {
        let Some(rom) = testkit::commercial_rom(&format!("{}.md", section.prefix)) else {
            eprintln!("SKIPPED: no ROM named {}.md", section.prefix);
            skipped.push(section.key);
            continue;
        };
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(section, &rom)));
        match outcome {
            Ok(()) => {
                eprintln!("ok: {}", section.key);
                passed.push(section.key);
            }
            Err(payload) => failures.push(format!(
                "{}: {}",
                section.key,
                panic_text(payload.as_ref()).trim()
            )),
        }
    }

    let total = passed.len() + skipped.len() + failures.len();
    eprintln!(
        "gameplay: {} passed, {} skipped, {} failed of {}",
        passed.len(),
        skipped.len(),
        failures.len(),
        total
    );
    if !failures.is_empty() {
        panic!(
            "{n} of {total} gameplay sections failed:\n{list}",
            n = failures.len(),
            list = failures
                .iter()
                .map(|failure| format!("  - {failure}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// The floor of the ticket, checkable without a cartridge: five sections.
#[test]
fn gameplay_list_has_at_least_five_games() {
    assert!(
        SECTIONS.len() >= MIN_GAMES,
        "{} sections, and the ticket asks for at least {MIN_GAMES}",
        SECTIONS.len()
    );
}

/// Every section's movie is on disk, parses, holds input, and ends before the
/// run does - the part of this suite CI can judge, CI having no cartridges.
#[test]
fn gameplay_movies_are_committed_and_parse() {
    for section in SECTIONS {
        let path = movie_path(section);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: cannot read the movie: {e}", path.display()));
        let movie = movie::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let changes = movie.changes();
        let last = changes
            .keys()
            .next_back()
            .copied()
            .unwrap_or_else(|| panic!("{} lists no input at all", path.display()));
        assert!(
            last < section.frames,
            "{}: its last input is frame {last}, and the run is {} frames",
            path.display(),
            section.frames
        );
        for (frame, row) in changes {
            assert_eq!(
                row.len(),
                movie.pads(),
                "{}, frame {frame}: the header says {} pads",
                path.display(),
                movie.pads()
            );
        }
    }
}

/// The goldens cover the list, so a section added without a blessed baseline is
/// a failure here and not a surprise for whoever runs the suite next.
#[test]
fn gameplay_list_has_a_golden_baseline() {
    let golden = testkit::Golden::load(&golden_path(GOLDEN_FILE));
    let missing: Vec<String> = SECTIONS
        .iter()
        .flat_map(|section| {
            section
                .checkpoints
                .iter()
                .map(move |frame| frame_key(section, *frame))
                .chain(std::iter::once(format!("{}_audio", section.key)))
        })
        .filter(|key| golden.get(key).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "{} has no golden key for {missing:?}",
        golden_path(GOLDEN_FILE).display()
    );
}
