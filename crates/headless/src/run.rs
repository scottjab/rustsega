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
    /// Print `colours=N` for the reported frame on stdout.
    ///
    /// N is the number of distinct pixels the frame holds, which is how
    /// `crates/genesis/tests/commercial.rs` decides a screen is blank; `sweep`
    /// asks for it in order to call a run that drew nothing `Blank` (M6-05, #124).
    #[arg(long)]
    pub colours: bool,
    /// Print `max_colours=N` for the whole run on stdout: the widest frame it made.
    ///
    /// `--colours` answers "what does the frame it stopped on look like" and this
    /// answers "did it ever draw", which is the question a compatibility report
    /// actually wants: a title screen fades in, holds, and fades out again, so the
    /// one sampled frame can be mid-fade while frame 700 held thirty colours
    /// (#373). `sweep` asks for both.
    #[arg(long)]
    pub max_colours: bool,
    /// Which frame `--hash`, `--colours` and `--dump-png` report: 1-based.
    ///
    /// The last frame of the run when the caller does not say. It exists so a
    /// caller can run past the frame it reports: `sweep` runs each ROM for longer
    /// than 600 frames to watch for a picture (#373) while its `distinct_colors`
    /// and `frame_hash` columns stay the frame-600 ones of the committed report.
    #[arg(long, value_name = "N")]
    pub sample_frame: Option<u32>,
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
    /// no entry is run with the pad released. The pad's names: UP DOWN LEFT
    /// RIGHT A B C START, plus the 6-button pad's X Y Z MODE. A script that
    /// names any of those four runs port 1 as a 6-button pad for the whole run
    /// (#364); one that does not keeps the 3-button pad, so every script
    /// written before #364 runs unchanged.
    #[arg(long = "pad1", value_name = "SCRIPT")]
    pub pad1: Option<String>,
    /// Replay the pad input of a movie instead of holding `--pad1`.
    ///
    /// A `.rsm` movie of `docs/movie-format.md` (M6-09, #129), whose ROM hash and
    /// region have to match this run; or a TASVideos movie (#673): a BizHawk
    /// `.bk2`, a Gens `.gmv`, or the `.zip` TASVideos serves either in, which is
    /// played on this ROM whatever it was made for (the game it names is
    /// printed). The pads then follow the movie frame by frame. Cannot be
    /// combined with `--pad1`.
    #[arg(long, value_name = "PATH")]
    pub movie: Option<PathBuf>,
    /// Write the run's port 1 input to `PATH` as a `.rsm` movie.
    ///
    /// What gets recorded is what the run actually held - a `--pad1` script, or
    /// nothing held at all - and replaying the file with `--movie` gives the same
    /// frames back.
    #[arg(long, value_name = "PATH")]
    pub record: Option<PathBuf>,
    /// Print the 68000's instruction trail when it next runs at `ADDR`, and stop.
    ///
    /// #436: the black carts that park on a `BRA self` or a `STOP` in their own
    /// boot code are indistinguishable from a running one once they stop
    /// branching, so the question - which address branched into the halt - can
    /// only be answered by a recorder that was running beforehand. The trail is
    /// the last [`m68k::TRAIL_LEN`] executed addresses, oldest first, each
    /// disassembled from the cartridge image when it lands there.
    ///
    /// The run stops on the frame that reaches `ADDR`, because the trail froze
    /// there and the frames after it can add nothing. If `ADDR` is never reached
    /// the trail of the whole run - the last instructions it ran - is printed
    /// anyway, under a line saying so.
    #[arg(long, value_name = "ADDR", value_parser = parse_hex_address)]
    pub trace_to: Option<u32>,
}

/// `--trace-to` takes a 68000 address in hex, with or without the `0x`.
///
/// Hex and nothing else: an address is read out of a disassembly, and accepting
/// `200` as decimal would make the same spelling mean two things.
fn parse_hex_address(value: &str) -> Result<u32, String> {
    let text = value.trim();
    let digits = text.strip_prefix("0x").unwrap_or(text);
    u32::from_str_radix(digits, 16).map_err(|_| format!("{value:?} is not a hex address"))
}

/// Run the machine `args` describes and report it.
///
/// Errors go back to `main`, which prints them and exits non-zero; nothing here
/// panics on a bad file name or a missing ROM, because that is the normal case
/// for a command line tool.
pub fn run(args: RunArgs) -> Result<()> {
    let rom = read_rom(&args.rom)?;
    // `Config::default()` is TMSS off, 48 kHz and the discrete YM2612 of a
    // Model 1 with its board low-pass: the 48 kHz is the rate `--dump-wav` and
    // `--audio-hash` report at, and nothing else here is a host choice.
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
    warn_unreachable_pad_frames(&pads, args.frames);
    // A script that names one of X, Y, Z or MODE anywhere needs the 6-button
    // pad (#364), and it needs it for the whole run: the pad kind is a device
    // the game can detect, not something that may change with the frame.
    let six_button = wants_six_button_pad(&pads);
    // The recorder records what the machine is handed, frame by frame; it starts
    // empty and `Movie::record` keeps only the frames that change.
    let mut recorder = args
        .record
        .as_ref()
        .map(|_| Movie::new(movie::rom_hash(&rom), region, RECORDED_PADS));

    let sample_rate = config.sample_rate;
    let mut machine = Genesis::new(&rom, config)
        .with_context(|| format!("cannot run ROM {}", quote(&args.rom)))?;
    // Armed after the load, because [`Genesis::new`] resets and a trail armed
    // before it would have recorded a machine that no longer exists.
    if let Some(addr) = args.trace_to {
        machine.arm_cpu_trail(Some(addr));
    }

    // Audio is only collected when someone asked for it: `drain_audio` mixes and
    // resamples every call, and a `--hash`-only run has nowhere to put the
    // answer (`Genesis::drain_audio` hands a frame's sound out once).
    let want_audio = args.dump_wav.is_some() || args.audio_hash;
    let mut audio: Vec<i16> = Vec::new();

    // The frame the report describes, and so the only frame worth counting pixels
    // on unless `--max-colours` asked for every one of them.
    let sample = args
        .sample_frame
        .unwrap_or(args.frames)
        .clamp(1, args.frames);
    let want_peak = args.max_colours;
    let mut peak_colours = 0;
    // The reported frame, kept as bytes rather than as a borrow of `machine`, so
    // that a run of 1800 frames can report the picture of frame 600.
    let mut reported: Option<(usize, usize, Vec<u32>)> = None;

    let started = Instant::now();
    // Frames actually run, which is `args.frames` unless `--trace-to` stopped
    // early on the frame that froze its trail.
    let mut ran = 0_u32;
    // 1-based, so that a script written as `START@1` means the first frame. The
    // pad is set before the frame runs: a button is held *during* a frame.
    for frame in 1..=args.frames {
        ran = frame;
        let held = pads.get(&frame).copied().unwrap_or_else(Buttons::empty);
        if let Some(movie) = &movie {
            // Movie frames are 0-based and this loop is 1-based: `--pad1 A@1` and
            // a movie's frame 0 are the same, first, frame.
            movie.set_pads(&mut machine, frame - 1);
        } else if six_button {
            machine.set_pad6(0, held);
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
        if machine.cpu().trail_caught() {
            // The trail is frozen, so every frame from here records nothing the
            // caller asked for and costs the whole of the run budget.
            break;
        }
        if want_peak || frame == sample {
            let picture = machine.frame();
            let pixels = &picture.pixels[..picture.width * picture.height];
            if want_peak {
                peak_colours = peak_colours.max(distinct_colours(pixels));
            }
            if frame == sample {
                reported = Some((picture.width, picture.height, pixels.to_vec()));
            }
        }
    }
    let elapsed = started.elapsed();
    if let Some(path) = &args.record {
        let recorder = recorder.expect("--record is what makes a recorder");
        fs::write(path, recorder.format())
            .with_context(|| format!("cannot write movie {}", quote(path)))?;
    }

    let (width, height, captured) = reported.unwrap_or_else(|| {
        let frame = machine.frame();
        let pixels = &frame.pixels[..frame.width * frame.height];
        (frame.width, frame.height, pixels.to_vec())
    });
    let pixels = captured.as_slice();
    if let Some(path) = &args.dump_png {
        if path.is_dir() {
            // Worth saying here rather than leaving it to the `File::create` inside
            // `testkit::dump_png`: "is a directory" reads better than an OS error,
            // and this is the one failure worth naming for the flag.
            bail!("--dump-png: {} is a directory", quote(path));
        }
        // Everything else that can stop a PNG being written - a read-only
        // filesystem, a parent that cannot hold one, the disk full - comes back
        // from `dump_png` now instead of panicking inside it (#345).
        testkit::dump_png(path, pixels, width, height)
            .with_context(|| format!("--dump-png: cannot write {}", quote(path)))?;
    }
    if args.hash {
        let hash = testkit::frame_hash(pixels, width, height);
        println!("frame_hash=0x{hash:016x}");
    }
    if args.colours {
        println!("colours={}", distinct_colours(pixels));
    }
    if args.max_colours {
        println!("max_colours={peak_colours}");
    }
    if let Some(path) = &args.dump_wav {
        wav::write_wav(path, sample_rate, &audio)
            .with_context(|| format!("cannot write WAV {}", quote(path)))?;
    }
    if args.audio_hash {
        let hash = audio_hash(&audio);
        println!("audio_hash=0x{hash:016x}");
    }

    if let Some(addr) = args.trace_to {
        let trail = machine.cpu().trail().expect("--trace-to arms a trail");
        if trail.caught() {
            println!(
                "trace_to=0x{addr:06x} caught frame {ran} entries={}",
                trail.entries().len()
            );
        } else {
            println!(
                "trace_to=0x{addr:06x} not reached in {ran} frames, showing the end of the run:"
            );
        }
        for entry in trail.entries() {
            println!("{}", trail_line(machine.bus().rom.as_slice(), entry));
        }
    }

    eprintln!("{summary} frames={ran} time={:.3}s", elapsed.as_secs_f64());
    Ok(())
}

/// One line of a `--trace-to` trail: the address, and the instruction there when
/// the address is inside the cartridge image.
///
/// The image is only mapped at 0, so a trail entry in work RAM or in I/O space
/// prints as the bare address - which is itself information: the cart was not
/// running cartridge code at that moment.
fn trail_line(rom: &[u8], addr: u32) -> String {
    let start = addr as usize;
    if start + 2 > rom.len() {
        return format!("0x{addr:06x}");
    }
    let end = rom.len().min(start + 8);
    let words: Vec<u16> = rom[start..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .collect();
    let (text, _len) = m68k::disassemble(&words, addr);
    format!("0x{addr:06x}  {text}")
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
    let bytes = fs::read(path).with_context(|| format!("cannot read movie {name}"))?;
    let movie = if tas::looks_like_tas(&bytes) {
        // A TASVideos movie: BizHawk .bk2, Gens .gmv, or the .zip around
        // either (#673). It names no rustsega ROM hash, so it is a movie of
        // the ROM it is played on, and what it says it was made for is
        // printed for the user to compare.
        let imported = tas::import(&bytes, movie::rom_hash(rom), region)
            .with_context(|| format!("cannot read movie {name}"))?;
        eprintln!("{}", describe_tas(&imported));
        imported.movie
    } else {
        let text = String::from_utf8(bytes)
            .with_context(|| format!("cannot read movie {name}: not text, and not a TAS movie"))?;
        let movie = movie::parse(&text).with_context(|| format!("cannot read movie {name}"))?;
        movie
            .check_rom(rom)
            .with_context(|| format!("movie {name}"))?;
        movie
    };
    if movie.region() != region {
        bail!(
            "movie {name} is for region {}, and this run is {}",
            movie.region().name(),
            region.name()
        );
    }
    Ok(Some(movie))
}

/// The line `headless` prints about a TAS movie it is about to play: what it
/// is, how long, and the game it says it was made for, since nothing checks
/// that it is this ROM.
fn describe_tas(imported: &tas::Imported) -> String {
    let mut line = format!(
        "movie: {} of {} frames",
        imported.format.name(),
        imported.frames
    );
    if let Some(game) = &imported.game_name {
        line.push_str(&format!(", made for \"{game}\""));
    }
    if let Some(digest) = &imported.rom_digest {
        line.push_str(&format!(" (ROM digest {digest})"));
    }
    if let Some(rerecords) = imported.rerecords {
        line.push_str(&format!(", {rerecords} rerecords"));
    }
    line
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

/// Distinct pixels of a frame, as `--colours` and `--max-colours` print them.
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

/// Say so when `--pad1` presses a button on a frame the run never reaches (#345,
/// #464).
///
/// The loop that hands the pad to the machine runs `1..=frames`, so an entry past
/// the end is looked up by nothing: `--pad1 START@1200 --frames 300` held nothing,
/// ran clean, and never mentioned it. Frame 0 has the same shape off the front of
/// the run (#464): it is never looked up either, and it usually means the script
/// was written 0-based. A warning on stderr rather than an error, because the
/// frames the entry *does* name are still worth running.
fn warn_unreachable_pad_frames(pads: &BTreeMap<u32, Buttons>, frames: u32) {
    let zero = pads.keys().filter(|frame| **frame == 0).count();
    let past: Vec<String> = pads
        .keys()
        .filter(|frame| **frame > frames)
        .map(|frame| frame.to_string())
        .collect();
    if zero > 0 {
        eprintln!(
            "warning: --pad1 holds nothing on frame 0: frames are 1-based, so the first frame is --pad1 ...@1"
        );
    }
    if past.is_empty() {
        return;
    }
    eprintln!(
        "warning: --pad1 holds nothing on frame{} {}, past --frames {frames}",
        if past.len() == 1 { "" } else { "s" },
        past.join(", ")
    );
}

/// The buttons `--pad1` can hold, by the name it spells them with: the twelve
/// of a 6-button pad (plutiedev, "Controllers", "Button layout"). The last four
/// are the ones the 3-button pad cannot report; a script that names any of them
/// runs port 1 as a 6-button pad (`wants_six_button_pad`).
const BUTTONS: [(&str, Buttons); 12] = [
    ("UP", Buttons::UP),
    ("DOWN", Buttons::DOWN),
    ("LEFT", Buttons::LEFT),
    ("RIGHT", Buttons::RIGHT),
    ("A", Buttons::A),
    ("B", Buttons::B),
    ("C", Buttons::C),
    ("START", Buttons::START),
    ("X", Buttons::X),
    ("Y", Buttons::Y),
    ("Z", Buttons::Z),
    ("MODE", Buttons::MODE),
];

/// One button name, as `--pad1` spells it: upper case, all twelve of the pad.
/// Before #364 the four 6-button letters were refused here, because the CLI
/// could only attach a 3-button pad; now a script that names one of them is
/// what makes the run a 6-button one.
fn parse_button(name: &str) -> Result<Buttons> {
    let wanted = name.trim().to_ascii_uppercase();
    if let Some((_, buttons)) = BUTTONS.iter().find(|(button, _)| *button == wanted) {
        return Ok(*buttons);
    }
    let names = BUTTONS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" ");
    bail!("--pad1: {name:?} is not a button; use {names}")
}

/// Does this run's port 1 pad have to be the 6-button one? A script that names
/// `X`, `Y`, `Z` or `MODE` anywhere does - those four are what a 3-button pad
/// cannot report ([`Buttons::SIX_BUTTON_ONLY`]) - and the question is answered
/// once for the whole run rather than per frame: the pad kind is a device the
/// game can detect, not something that may change with the frame (#364). A
/// 6-button pad answers the 3-button rows exactly as the 3-button pad does, so
/// the frames of a script that never names the four are the frames it always
/// made.
fn wants_six_button_pad(pads: &BTreeMap<u32, Buttons>) -> bool {
    pads.values()
        .any(|buttons| buttons.intersects(Buttons::SIX_BUTTON_ONLY))
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

    /// #364: the four letters a 6-button pad adds are scriptable now, by the
    /// same names the movie format spells them.
    #[test]
    fn pad_script_accepts_the_six_button_letters() {
        let pads = script("X@3,Y@4,Z@5,MODE@6");
        assert_eq!(pads.get(&3), Some(&Buttons::X));
        assert_eq!(pads.get(&4), Some(&Buttons::Y));
        assert_eq!(pads.get(&5), Some(&Buttons::Z));
        assert_eq!(pads.get(&6), Some(&Buttons::MODE));
    }

    /// A script that names any of the four anywhere makes this a 6-button run;
    /// one that does not keeps the 3-button pad, so existing scripts run
    /// unchanged.
    #[test]
    fn a_script_naming_a_six_button_letter_needs_the_six_button_pad() {
        assert!(wants_six_button_pad(&script("A@1,Z@2")));
        assert!(wants_six_button_pad(&script("MODE@10")));
        assert!(!wants_six_button_pad(&script("START@1,LEFT+A@2")));
        assert!(!wants_six_button_pad(&BTreeMap::new()));
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
