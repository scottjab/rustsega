//! The `run` subcommand: load a ROM, run it for a number of frames, and report
//! what the machine produced (M6-01, #120).
//!
//! The reporting is the point. `--hash` prints the same FNV-1a value a golden
//! file records (`docs/TESTING.md`, "Golden files", via [`testkit::frame_hash`])
//! and `--dump-png` writes the same bytes [`testkit::dump_png`] writes, so what
//! this command says about a frame and what the test suite says about it cannot
//! disagree. `--dump-wav` and `--audio-hash` do the same for the sound of the
//! whole run: the WAV is [`crate::wav::write_wav`] over what
//! [`genesis::Genesis::drain_audio`] hands out, and the hash is over the same
//! PCM bytes the `data` chunk holds (M6-02, #121).
//!
//! `--pad1` holds buttons for the run, `--movie` plays a recorded movie instead
//! of holding a script, and `--record` writes what the run held out to one: the
//! `.rsm` format of [`genesis::movie`], documented in `docs/movie-format.md`
//! (M6-09, #129).
//! Reading and writing the file is this command's job - the format's own parser
//! never sees a path.
//!
//! Everything is `Genesis`' own public API: [`genesis::Genesis::new`], then one
//! [`genesis::Genesis::run_frame`] per frame, then [`genesis::Genesis::frame`]
//! for the picture. Nothing here reaches into the machine, which is the check
//! that the public API is enough to drive the console from outside.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, ValueEnum};
use genesis::movie::{self, Movie};
use genesis::{Buttons, Config, Genesis, Region, RegionSetting};

use crate::wav;

/// Frames for `--frames` when the caller does not say: one second of NTSC, the
/// shortest run that gets a real game past its reset and into something on screen.
const DEFAULT_FRAMES: u32 = 60;

/// How many pads `--record` writes: one, because `--pad1` is the only pad the CLI
/// can hold. A two-pad movie is written by hand or by a front end with two live
/// pads to take input from (M6-09, #129).
const RECORDED_PADS: usize = 1;

/// The region the caller asked for: the same three choices [`genesis::Region`]
/// has, plus `auto` for "read it off the cartridge".
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RegionArg {
    /// Read the region off the cartridge header at 0x1F0.
    Auto,
    /// Run as a Mega Drive / Genesis from the Americas whatever the header says.
    Usa,
    /// Run as a domestic Japanese Mega Drive.
    Japan,
    /// Run as a PAL Mega Drive.
    Europe,
}

impl RegionArg {
    /// The [`RegionSetting`] this is, i.e. the choice `Config::region` wants.
    fn setting(self) -> RegionSetting {
        match self {
            Self::Auto => RegionSetting::Auto,
            Self::Usa => RegionSetting::Force(Region::Usa),
            Self::Japan => RegionSetting::Force(Region::Japan),
            Self::Europe => RegionSetting::Force(Region::Europe),
        }
    }
}

/// The `run` arguments, as clap sees them.
#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// ROM image to run. Raw `.bin`/`.md` and interleaved `.smd` are both fine:
    /// `Genesis::new` deinterleaves what needs it.
    #[arg(value_name = "ROM")]
    pub rom: PathBuf,
    /// Frames to run before stopping.
    #[arg(long, default_value_t = DEFAULT_FRAMES, value_name = "N")]
    pub frames: u32,
    /// Write the last frame to `PATH` as an RGBA PNG.
    #[arg(long, value_name = "PATH")]
    pub dump_png: Option<PathBuf>,
    /// Write the run's audio to `PATH` as a 16-bit stereo PCM WAV.
    ///
    /// The audio of the whole run, from the first frame to the last, at
    /// `Config::sample_rate` (48 kHz unless the caller changed it in code).
    /// M6-02, #121.
    #[arg(long, value_name = "PATH")]
    pub dump_wav: Option<PathBuf>,
    /// Print `frame_hash=0x...` for the last frame on stdout.
    #[arg(long)]
    pub hash: bool,
    /// Print `colours=N` for the last frame on stdout.
    ///
    /// N is the number of distinct pixels the frame holds, which is how
    /// `crates/genesis/tests/commercial.rs` decides a screen is blank; `sweep`
    /// asks for it in order to call a run that drew nothing `Blank` (M6-05, #124).
    #[arg(long)]
    pub colours: bool,
    /// Print `audio_hash=0x...` for the run's audio on stdout.
    ///
    /// FNV-1a 64 over the same PCM bytes `--dump-wav` writes as the WAV's
    /// `data` chunk, so a hash and a file describe the same audio.
    #[arg(long)]
    pub audio_hash: bool,
    /// Force a region instead of reading the header.
    #[arg(long, value_enum, default_value_t = RegionArg::Auto)]
    pub region: RegionArg,
    /// Hold buttons on port 1 at given frames: `START@120,LEFT+A@121`.
    ///
    /// Frame numbers are 1-based - `A@1` is held during the first frame this
    /// command runs - and one entry's buttons are joined with `+`. A frame with
    /// no entry is run with the pad released. The 3-button pad's names only: UP
    /// DOWN LEFT RIGHT A B C START. `X`, `Y`, `Z` and `Mode` belong to a 6-button
    /// pad, which this command cannot attach, so they are refused rather than
    /// silently dropped - a movie holds them fine (`--movie`, `--record`).
    #[arg(long = "pad1", value_name = "SCRIPT")]
    pub pad1: Option<String>,
    /// Replay the pad input of a `.rsm` movie instead of holding `--pad1`.
    ///
    /// The movie of `docs/movie-format.md` (M6-09, #129). Its ROM hash and its
    /// region have to match this run, and the pads then follow the movie frame by
    /// frame instead of the script. Cannot be combined with `--pad1`.
    #[arg(long, value_name = "PATH")]
    pub movie: Option<PathBuf>,
    /// Write the run's port 1 input to `PATH` as a `.rsm` movie.
    ///
    /// What gets recorded is what the run actually held - a `--pad1` script, or
    /// nothing held at all - and replaying the file with `--movie` gives the same
    /// frames back.
    #[arg(long, value_name = "PATH")]
    pub record: Option<PathBuf>,
}

/// Run the machine `args` describes and report it.
///
/// Errors go back to `main`, which prints them and exits non-zero; nothing here
/// panics on a bad file name or a missing ROM, because that is the normal case
/// for a command line tool.
pub fn run(args: RunArgs) -> Result<()> {
    let rom = read_rom(&args.rom)?;
    // `Config::default()` is TMSS off, 48 kHz and the Model 1 low-pass: the
    // 48 kHz is the rate `--dump-wav` and `--audio-hash` report at, and nothing
    // else here is a host choice.
    let config = Config {
        region: args.region.setting(),
        ..Config::default()
    };
    let summary = describe(&rom, &config)?;
    if args.movie.is_some() && args.pad1.is_some() {
        bail!(
            "--movie is this run's input; drop --pad1, or use --record to turn a script into a movie"
        );
    }
    if args.movie.is_some() && args.record.is_some() {
        bail!("--movie replays and --record writes: naming both asks which one to do to one file");
    }
    let region = resolved_region(&rom, &config)?;
    let movie = read_movie(args.movie.as_deref(), &rom, region)?;
    let pads = parse_pad_script(args.pad1.as_deref())?;
    // The recorder records what the machine is handed, frame by frame; it starts
    // empty and `Movie::record` keeps only the frames that change.
    let mut recorder = args
        .record
        .as_ref()
        .map(|_| Movie::new(movie::rom_hash(&rom), region, RECORDED_PADS));

    let sample_rate = config.sample_rate;
    let mut machine = Genesis::new(&rom, config)
        .with_context(|| format!("cannot run ROM {}", quote(&args.rom)))?;

    // Audio is only collected when someone asked for it: `drain_audio` mixes and
    // resamples every call, and a `--hash`-only run has nowhere to put the
    // answer (`Genesis::drain_audio` hands a frame's sound out once).
    let want_audio = args.dump_wav.is_some() || args.audio_hash;
    let mut audio: Vec<i16> = Vec::new();

    let started = Instant::now();
    // 1-based, so that a script written as `START@1` means the first frame. The
    // pad is set before the frame runs: a button is held *during* a frame.
    for frame in 1..=args.frames {
        let held = pads.get(&frame).copied().unwrap_or_else(Buttons::empty);
        if let Some(movie) = &movie {
            // Movie frames are 0-based and this loop is 1-based: `--pad1 A@1` and
            // a movie's frame 0 are the same, first, frame.
            movie.set_pads(&mut machine, frame - 1);
        } else {
            machine.set_pad(0, held);
        }
        if let Some(recorder) = &mut recorder {
            recorder.record(frame - 1, &[held]);
        }
        machine.run_frame();
        if want_audio {
            // After the frame, so the buffer holds the raster the machine just
            // ran through rather than the one before it.
            machine.drain_audio(&mut audio);
        }
    }
    let elapsed = started.elapsed();
    if let Some(path) = &args.record {
        let recorder = recorder.expect("--record is what makes a recorder");
        fs::write(path, recorder.format())
            .with_context(|| format!("cannot write movie {}", quote(path)))?;
    }

    let frame = machine.frame();
    let pixels = &frame.pixels[..frame.width * frame.height];
    if let Some(path) = &args.dump_png {
        if path.is_dir() {
            // Worth saying here rather than letting `testkit::dump_png` panic on
            // the `File::create` inside it: a panic is not how a command line
            // tool should answer `--dump-png /tmp`.
            bail!("--dump-png: {} is a directory", quote(path));
        }
        testkit::dump_png(path, pixels, frame.width, frame.height);
    }
    if args.hash {
        let hash = testkit::frame_hash(pixels, frame.width, frame.height);
        println!("frame_hash=0x{hash:016x}");
    }
    if args.colours {
        println!("colours={}", distinct_colours(pixels));
    }
    if let Some(path) = &args.dump_wav {
        wav::write_wav(path, sample_rate, &audio)
            .with_context(|| format!("cannot write WAV {}", quote(path)))?;
    }
    if args.audio_hash {
        let hash = audio_hash(&audio);
        println!("audio_hash=0x{hash:016x}");
    }

    eprintln!(
        "{summary} frames={} time={:.3}s",
        args.frames,
        elapsed.as_secs_f64()
    );
    Ok(())
}

/// Read a ROM image off disk, naming the path in every failure: "No such file
/// or directory" on its own says nothing about which of the arguments it means.
fn read_rom(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("cannot read ROM {}", quote(path)))
}

/// The movie to replay: read from `path`, its ROM hash checked against `rom` and
/// its region against the region this run resolved to.
///
/// The two checks are this command's rather than `genesis::movie`'s: the machine
/// on the other end of the replay is the one [`run`] is about to build, and a
/// movie for another cartridge or another region is a different run rather than a
/// broken file (`docs/movie-format.md`).
fn read_movie(path: Option<&Path>, rom: &[u8], region: Region) -> Result<Option<Movie>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let name = quote(path);
    let text = fs::read_to_string(path).with_context(|| format!("cannot read movie {name}"))?;
    let movie = movie::parse(&text).with_context(|| format!("cannot read movie {name}"))?;
    movie
        .check_rom(rom)
        .with_context(|| format!("movie {name}"))?;
    if movie.region() != region {
        bail!(
            "movie {name} is for region {}, and this run is {}",
            movie.region().name(),
            region.name()
        );
    }
    Ok(Some(movie))
}

/// The region [`genesis::Genesis::new`] runs `rom` as under `config`: the same
/// three steps [`describe`] walks, for the caller that wants the region and not
/// the summary line.
fn resolved_region(rom: &[u8], config: &Config) -> Result<Region> {
    let rom = genesis::cart::normalize_rom(rom);
    let header = genesis::cart::parse_header(&rom).context("ROM has no usable header")?;
    Ok(config.resolve_region(&header))
}

/// `--audio-hash`: [`testkit::fnv1a_64`] over the run's audio as PCM bytes,
/// i.e. over exactly the bytes `--dump-wav` writes into the `data` chunk, so
/// `audio_hash` and `out.wav` cannot disagree about what was heard
/// (M6-02, #121).
fn audio_hash(samples: &[i16]) -> u64 {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    testkit::fnv1a_64(&bytes)
}

/// The first half of the summary line: `title="SONIC THE HEDGEHOG" region=usa`.
///
/// The region is `Config::resolve_region`'s answer, i.e. what the console is
/// actually running as, and `describe` walks the same three steps
/// [`genesis::Genesis::new`] does - normalize, header, region - so the line
/// cannot disagree with the machine (docs/ARCHITECTURE.md, "genesis").
fn describe(rom: &[u8], config: &Config) -> Result<String> {
    let rom = genesis::cart::normalize_rom(rom);
    let header = genesis::cart::parse_header(&rom).context("ROM has no usable header")?;
    let title = title_of(&header).unwrap_or_else(|| "unknown".to_string());
    let region = config.resolve_region(&header);
    Ok(format!("title={title:?} region={}", region.name()))
}

/// Distinct pixels of a frame, as `--colours` prints them.
///
/// The same count `crates/genesis/tests/commercial.rs` takes to decide a screen
/// is not blank, over the `0xAABBGGRR` pixels the machine holds.
fn distinct_colours(pixels: &[u32]) -> usize {
    let mut seen = std::collections::HashSet::new();
    for pixel in pixels {
        seen.insert(*pixel);
    }
    seen.len()
}

/// The cartridge's title for the summary line, or `None` when neither title
/// field says anything.
///
/// The export field at 0x150 first and the domestic one at 0x120 as the
/// fallback, because an English-language ROM usually fills only one of the two
/// (plutiedev, "Game title"). Runs of padding collapse to one space:
/// `cart::header::text` trims a field's ends but Sega mandated a fixed-width
/// one, and a publisher may fill it a 16-byte block at a time, which is how
/// `SONIC THE HEDGEHOG` reaches us as `SONIC THE<15 spaces>HEDGEHOG`. The
/// summary is for a person to read, so it is shown the way it was meant.
pub(crate) fn title_of(header: &genesis::cart::RomHeader) -> Option<String> {
    let readable = |field: &str| {
        let collapsed = field.split_whitespace().collect::<Vec<_>>().join(" ");
        (!collapsed.is_empty()).then_some(collapsed)
    };
    readable(&header.overseas_title).or_else(|| readable(&header.domestic_title))
}

/// A path in quotes, so a name with spaces in it still reads as one item.
fn quote(path: &Path) -> String {
    format!("{:?}", path)
}

/// `--pad1`: the script as a map from frame number to the buttons held on it.
///
/// `START@120,START@121` holds START on frames 120 and 121; `A+B@10` holds A and
/// B together on frame 10. Frame numbers are 1-based, see [`RunArgs::pad1`].
fn parse_pad_script(script: Option<&str>) -> Result<BTreeMap<u32, Buttons>> {
    let Some(script) = script else {
        return Ok(BTreeMap::new());
    };
    let mut pads = BTreeMap::new();
    for entry in script.split(',') {
        let (buttons, frame) = entry
            .rsplit_once('@')
            .ok_or_else(|| anyhow!("--pad1: entry {entry:?} needs a frame, as in START@120"))?;
        let frame: u32 = frame
            .trim()
            .parse()
            .with_context(|| format!("--pad1: {entry:?} does not name a frame number"))?;
        let held = pads.entry(frame).or_insert_with(Buttons::empty);
        for name in buttons.split('+') {
            *held |= parse_button(name)?;
        }
    }
    Ok(pads)
}

/// The buttons `--pad1` can hold, by the name it spells them with: the eight of
/// a 3-button pad (plutiedev, "Controllers", "Button layout").
const BUTTONS: [(&str, Buttons); 8] = [
    ("UP", Buttons::UP),
    ("DOWN", Buttons::DOWN),
    ("LEFT", Buttons::LEFT),
    ("RIGHT", Buttons::RIGHT),
    ("A", Buttons::A),
    ("B", Buttons::B),
    ("C", Buttons::C),
    ("START", Buttons::START),
];

/// One button name, as `--pad1` spells it: upper case, and the four a 6-button
/// pad adds are refused rather than quietly dropped.
///
/// `Genesis::set_pad` attaches a 3-button pad, which cannot report `X`, `Y`, `Z`
/// or `MODE`, so a script that asked for one would be holding a button no game
/// can see (plutiedev, "Controllers", "6-button controller"). A movie records and
/// replays those letters all the same; what it cannot do until the console grows a
/// 6-button `set_pad` is show them to the game (`docs/movie-format.md`).
fn parse_button(name: &str) -> Result<Buttons> {
    const SIX_BUTTON_ONLY: [&str; 4] = ["X", "Y", "Z", "MODE"];
    let wanted = name.trim().to_ascii_uppercase();
    if let Some((_, buttons)) = BUTTONS.iter().find(|(button, _)| *button == wanted) {
        return Ok(*buttons);
    }
    if SIX_BUTTON_ONLY.contains(&wanted.as_str()) {
        bail!(
            "--pad1: {wanted} is a 6-button pad button and the CLI attaches a 3-button \
             pad; 6-button input comes with the movie format (#129)"
        );
    }
    let names = BUTTONS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" ");
    bail!("--pad1: {name:?} is not a button; use {names}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame keys of a script, so a test can say what the map holds without
    /// spelling out `Buttons`, whose bits are private.
    fn script(text: &str) -> BTreeMap<u32, Buttons> {
        parse_pad_script(Some(text)).expect("script parses")
    }

    #[test]
    fn pad_script_maps_frames_to_buttons() {
        let pads = script("START@120,START@121");
        assert_eq!(pads.len(), 2);
        assert_eq!(pads.get(&120), Some(&Buttons::START));
        assert_eq!(pads.get(&121), Some(&Buttons::START));
        assert_eq!(pads.get(&119), None);
    }

    #[test]
    fn pad_script_ands_two_buttons_on_one_frame() {
        let pads = script("LEFT+A@10");
        let held = pads[&10];
        assert!(held.contains(Buttons::LEFT) && held.contains(Buttons::A));
    }

    #[test]
    fn pad_script_rejects_a_missing_frame() {
        let err = parse_pad_script(Some("START")).expect_err("START without @ is an error");
        assert!(
            err.to_string().contains("needs a frame"),
            "unhelpful error: {err}"
        );
    }

    #[test]
    fn pad_script_rejects_an_unknown_button() {
        let err = parse_pad_script(Some("TRIGGER@3")).expect_err("TRIGGER is not a button");
        assert!(err.to_string().contains("not a button"), "unhelpful: {err}");
    }

    #[test]
    fn pad_script_refuses_a_button_the_cli_cannot_hold() {
        // Not silently dropped: a script that asked for Z would test nothing,
        // because `Genesis::set_pad` attaches a 3-button pad.
        let err =
            parse_pad_script(Some("Z@3")).expect_err("Z is not reachable through the CLI's pad");
        assert!(err.to_string().contains("6-button"), "unhelpful: {err}");
    }

    /// `--audio-hash` hashes the run's audio as little-endian PCM bytes: the
    /// expected values are FNV-1a 64 over `01 00 FF 7F` and over nothing,
    /// computed independently in Python from the FNV definition, so this pins
    /// the byte order and the offset basis rather than restating the code.
    #[test]
    fn audio_hash_is_fnv_over_the_pcm_bytes() {
        assert_eq!(audio_hash(&[1, -1]), 0xab59_b177_460d_9ada);
        assert_eq!(audio_hash(&[]), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn no_pad_script_is_no_pad() {
        assert!(parse_pad_script(None).expect("no script").is_empty());
    }

    #[test]
    fn region_arg_maps_to_the_setting() {
        assert_eq!(RegionArg::Auto.setting(), RegionSetting::Auto);
        assert_eq!(RegionArg::Usa.setting(), RegionSetting::Force(Region::Usa));
        assert_eq!(
            RegionArg::Japan.setting(),
            RegionSetting::Force(Region::Japan)
        );
        assert_eq!(
            RegionArg::Europe.setting(),
            RegionSetting::Force(Region::Europe)
        );
    }

    /// A ROM image with a header naming itself `TITLE` and claiming nothing.
    fn rom_with(title: &str) -> Vec<u8> {
        let mut rom = vec![0_u8; 0x200];
        rom[0x150..0x160].copy_from_slice(format!("{title:<16}").as_bytes());
        rom
    }

    #[test]
    fn summary_names_the_title_and_the_auto_region() {
        let text = describe(&rom_with("FAKE ROM"), &Config::default()).expect("header parses");
        // A header that claims no region runs as the NTSC export machine
        // (`Config::resolve_region`), which is what the line has to report.
        assert_eq!(text, "title=\"FAKE ROM\" region=usa");
    }

    #[test]
    fn summary_says_a_forced_region() {
        let config = Config {
            region: RegionSetting::Force(Region::Europe),
            ..Config::default()
        };
        let text = describe(&rom_with("FAKE ROM"), &config).expect("header parses");
        assert_eq!(text, "title=\"FAKE ROM\" region=europe");
    }

    #[test]
    fn summary_survives_a_header_with_no_title() {
        let text = describe(&rom_with(""), &Config::default()).expect("header parses");
        assert_eq!(text, "title=\"unknown\" region=usa");
    }

    #[test]
    fn summary_falls_back_to_the_domestic_title() {
        let mut rom = rom_with("");
        rom[0x120..0x12A].copy_from_slice(b"GREAT GAME");
        let text = describe(&rom, &Config::default()).expect("header parses");
        assert_eq!(text, "title=\"GREAT GAME\" region=usa");
    }

    #[test]
    fn summary_collapses_a_title_written_in_16_byte_blocks() {
        // The shape a shipped Sonic has: the export field is space-padded and
        // the publisher wrote the second word at the next 16-byte boundary.
        let mut rom = rom_with("");
        rom[0x150..0x159].copy_from_slice(b"SONIC THE");
        rom[0x160..0x168].copy_from_slice(b"HEDGEHOG");
        let text = describe(&rom, &Config::default()).expect("header parses");
        assert_eq!(text, "title=\"SONIC THE HEDGEHOG\" region=usa");
    }

    #[test]
    fn describe_rejects_an_image_with_no_header() {
        let err = describe(&[0_u8; 0x10], &Config::default()).expect_err("0x10 bytes is not a ROM");
        assert!(err.to_string().contains("no usable header"), "{err}");
    }
}
