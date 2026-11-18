//! Run a cartridge with no input and write out the memories of a named frame:
//! 68000 work RAM, VRAM or CRAM. `cargo run -p genesis --release --example
//! ram_dump -- ROM --frames N --out PATH` writes the work RAM one; `--vram` and
//! `--cram` (#534) write the VDP's two.
//!
//! The reference oracle of #525 (`scripts/ref/`, its README has the whole
//! story) compares this machine against a second emulator by work RAM state at a
//! named frame, and this is our half of that: read a ROM, run it N frames with
//! no `set_pad` call at all, write [`genesis::Bus::ram`] out. Work RAM is the
//! 64 KiB at `$FF0000` mirrored over itself, so `Bus::ram` *is* all of it and the
//! image needs no address and no length - which is what makes a byte
//! comparison, rather than a schema, enough to compare two emulators.
//!
//! ```text
//! cargo run -p genesis --release --example ram_dump -- \
//!     "$RUSTSEGA_ROM_DIR/Sonic & Knuckles (World).md" --frames 600 --out /tmp/r00600.bin
//! ```
//!
//! Why this lives here and not in `headless`: `headless run` reports a frame
//! (`--hash`, `--dump-png`, `--colours`) and has no way to report memory, so the
//! oracle needed one and a `cargo` example is the smallest place that can reach
//! [`genesis::Bus::ram`] without touching the shipped binary - an example is not
//! built by `cargo build --release` and not linked into anything. The right long
//! home is `headless run --dump-ram PATH`, which would let `scripts/ref/` stop
//! shelling into `cargo run --example`; that needs its own tests and docs and is
//! deliberately not done here.
//!
//! Nothing about the reference appears in this file, on purpose: it knows about a
//! ROM, a region and some RAM, and about no second emulator.

use std::path::{Path, PathBuf};

use genesis::{Config, Genesis, Region, RegionSetting};

/// What the command line said, after parsing.
///
/// `out`, `vram` and `cram` are each optional and at least one is required: the
/// work-RAM comparison of #525 wants the 68000's memory and the VDP comparison of
/// #534 wants the chip's, and both are the same run of the same machine up to the
/// point where the images get written. Naming them from one invocation is the
/// point - at a `--stop-at-pc` anchor the images are meant to be one moment, and
/// three runs that each stopped at their own arrival would not be.
struct Args {
    rom: PathBuf,
    frames: u32,
    /// Where the 64 KiB work-RAM image goes, if it is wanted.
    out: Option<PathBuf>,
    /// Where the 64 KiB VRAM image goes, if it is wanted (#534).
    vram: Option<PathBuf>,
    /// Where the 128 B CRAM image goes, if it is wanted (#534).
    cram: Option<PathBuf>,
    region: RegionSetting,
    tmss: bool,
    /// Stop at this address instead of at the end of a frame: see [`USAGE`].
    stop_at_pc: Option<u32>,
}

/// One memory this run was asked to write out.
struct Image {
    /// What the report calls it: `ram`, `vram`, `cram`.
    name: &'static str,
    path: PathBuf,
    bytes: usize,
}

/// The longest a frame can be, in scanlines: a PAL raster. It bounds one leg of
/// [`genesis::Genesis::run_until_pc`] below, and [`genesis::Scheduler`] is what
/// says 313 is the most any region's raster holds.
const LINES_PER_FRAME: u32 = 313;

/// How many frames of slack each leg gets on top of the frames still to run.
///
/// A cart can be a long time getting to its own vertical interrupt: `Golden Axe
/// II (World)` first reaches the handler of the long at `$78` around frame 56,
/// and one that waits for a RAM test could wait longer. The slack is not free - a
/// mistyped address costs this many frames of emulation before it is reported -
/// and it is cheap, because only the leg that fails pays for it.
const FIRST_ARRIVAL_SLACK: u32 = 600;

fn main() {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("ram_dump: {message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let rom = match std::fs::read(&args.rom) {
        Ok(rom) => rom,
        Err(e) => fail(&format!("cannot read {}: {e}", args.rom.display())),
    };
    // `tmss` is here because the two sides of a comparison have to be able to
    // agree on the machine: `Config::default()` is TMSS off, and a reference that
    // defaults to the security check on has to be met in the middle (#525).
    let config = Config {
        region: args.region,
        tmss: args.tmss,
        ..Config::default()
    };
    // The region is resolved before `Genesis::new` takes the config, so the line
    // this prints says what the machine is running as rather than what it was
    // asked for - the same rule `headless run` prints by (`region=usa`).
    let region = described_region(&rom, &config);

    let mut machine = match Genesis::new(&rom, config) {
        Ok(machine) => machine,
        Err(e) => fail(&format!("cannot load {}: {e}", args.rom.display())),
    };
    // Two ways to reach the named frame. Without `--stop-at-pc` it is the frame
    // loop the whole emulator is driven by. With it, a frame is one arrival at an
    // address the cart reaches once a frame - its vertical-blank entry - which is
    // the same point in the instruction stream on any machine, and is what #526
    // needs the reference and us to agree on.
    //
    // The budget of each leg is the whole run: a cart that enables its VBlank
    // interrupt late should still get to its first arrival, and an address that is
    // never reached should cost one run before it is reported rather than hang.
    if let Some(pc) = args.stop_at_pc {
        let budget = (args.frames + FIRST_ARRIVAL_SLACK) * LINES_PER_FRAME;
        for arrival in 1..=args.frames {
            if !machine.run_until_pc(pc, budget) {
                fail(&format!(
                    "arrival {arrival} of {pc:#08X} never happened inside {budget} scanlines"
                ));
            }
        }
    } else {
        for _ in 0..args.frames {
            machine.run_frame();
        }
    }

    // Every image comes off the same stopped machine, in the order the caller
    // named them, so a VRAM dump and a work-RAM dump of "frame 600" are the same
    // instant rather than two runs that hoped to agree.
    let bus = machine.bus();
    let mut written: Vec<Image> = Vec::new();
    if let Some(path) = &args.out {
        written.push(write_image("ram", path.as_path(), &bus.ram[..]));
    }
    if let Some(path) = &args.vram {
        // VRAM is 64 KiB addressed as the chip addresses it, which is the one
        // representation in which two emulators' pattern tables can be compared
        // byte for byte (#534).
        written.push(write_image("vram", path.as_path(), &bus.vdp.vram));
    }
    if let Some(path) = &args.cram {
        written.push(write_image(
            "cram",
            path.as_path(),
            &cram_image(&bus.vdp.cram),
        ));
    }

    let images = written
        .iter()
        .map(|image| format!("{}={:?} bytes={}", image.name, image.path, image.bytes))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "region={} frames={} {}{}",
        region.map(Region::name).unwrap_or("unknown"),
        args.frames,
        match args.stop_at_pc {
            Some(pc) => format!("stop={pc:#08X} "),
            None => String::new(),
        },
        images
    );
}

/// Write one image out and describe it, failing the run if the disk said no.
fn write_image(name: &'static str, path: &Path, bytes: &[u8]) -> Image {
    if let Err(e) = std::fs::write(path, bytes) {
        fail(&format!("cannot write {}: {e}", path.display()));
    }
    Image {
        name,
        path: path.to_path_buf(),
        bytes: bytes.len(),
    }
}

/// CRAM as an image: sixty-four colour words, little endian, 128 bytes.
///
/// Little endian because the reference this is compared against writes a word low
/// byte first, and a byte order is the one thing here that has to match a second
/// emulator's choice rather than ours. The colour itself is unchanged: `0000
/// BBB0 GGG0 RRR0` either way (genvdp.txt section 9), so an image of a palette is
/// an image of the palette and not of a convention.
fn cram_image(cram: &[u16]) -> Vec<u8> {
    let mut image = Vec::with_capacity(cram.len() * 2);
    for colour in cram {
        image.extend_from_slice(&colour.to_le_bytes());
    }
    image
}

const USAGE: &str = "\
usage: ram_dump <ROM> --frames N [--out PATH] [--vram PATH] [--cram PATH]
                 [--region auto|usa|japan|europe] [--tmss] [--stop-at-pc ADDRESS]

Run <ROM> for N frames with no input and write out the memories named: 64 KiB of
68000 work RAM (0xFF0000-0xFFFFFF) with --out, 64 KiB of VRAM with --vram, and 64
colour words of CRAM (128 B, little endian) with --cram. At least one of the three
is needed and they are all written from one stopped machine. --region forces a
region instead of reading the cartridge header; --tmss starts with the security
check enabled. Prints `region=... frames=...` and one `name=PATH bytes=N` for
every image written.

--stop-at-pc makes a frame an arrival at ADDRESS instead of a whole raster: the
run ends, and the RAM is written, the Nth time the 68000 is about to execute
there. Give it an address the cart reaches once a frame - the entry of its
vertical-blank routine, which the long at $78 of the cartridge points at - so
that two emulators can be dumped at the same moment rather than at whatever
their own frame loops happen to end on (#526).";

/// Parse the command line. Hand-rolled, and deliberately so: this is four
/// options, `clap` is not a dependency of this crate, and an example is not a
/// reason to add one. Anything it cannot read is an error rather than a default,
/// because an oracle that quietly ran the wrong machine is worse than one that
/// refuses to run.
fn parse<I: Iterator<Item = String>>(mut argv: I) -> Result<Args, String> {
    let mut rom: Option<PathBuf> = None;
    let mut frames: Option<u32> = None;
    let mut out: Option<PathBuf> = None;
    let mut vram: Option<PathBuf> = None;
    let mut cram: Option<PathBuf> = None;
    let mut region = RegionSetting::Auto;
    let mut tmss = false;
    let mut stop_at_pc: Option<u32> = None;

    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--frames" => {
                let value = argv.next().ok_or("--frames wants a frame count")?;
                frames = Some(
                    value
                        .parse()
                        .map_err(|_| format!("{value:?} is not a frame count"))?,
                );
            }
            "--out" => out = Some(PathBuf::from(argv.next().ok_or("--out wants a path")?)),
            "--vram" => {
                vram = Some(PathBuf::from(argv.next().ok_or("--vram wants a path")?));
            }
            "--cram" => {
                cram = Some(PathBuf::from(argv.next().ok_or("--cram wants a path")?));
            }
            "--region" => {
                let value = argv
                    .next()
                    .ok_or("--region wants auto, usa, japan or europe")?;
                region = match value.as_str() {
                    "auto" => RegionSetting::Auto,
                    "usa" => RegionSetting::Force(Region::Usa),
                    "japan" => RegionSetting::Force(Region::Japan),
                    "europe" => RegionSetting::Force(Region::Europe),
                    other => {
                        return Err(format!(
                            "{other:?} is not a region (auto, usa, japan, europe)"
                        ));
                    }
                };
            }
            "--tmss" => tmss = true,
            "--stop-at-pc" => {
                let value = argv.next().ok_or("--stop-at-pc wants an address")?;
                let digits = value.strip_prefix("0x").unwrap_or(&value);
                stop_at_pc = Some(
                    u32::from_str_radix(digits, 16)
                        .map_err(|_| format!("{value:?} is not a hex address"))?,
                );
            }
            other if other.starts_with("--") => return Err(format!("unknown option {other:?}")),
            other => {
                if let Some(first) = &rom {
                    return Err(format!("two ROMs named: {} and {other:?}", first.display()));
                }
                rom = Some(PathBuf::from(other));
            }
        }
    }

    let frames = frames.ok_or("no --frames N: say which frame to dump")?;
    if out.is_none() && vram.is_none() && cram.is_none() {
        return Err("no memory named: give at least one of --out, --vram, --cram".to_string());
    }
    Ok(Args {
        rom: rom.ok_or("no ROM named")?,
        frames,
        out,
        vram,
        cram,
        region,
        tmss,
        stop_at_pc,
    })
}

/// The region this run is about to be, or `None` when the ROM has no header to
/// read it from - which is reported as `unknown` rather than as a failure,
/// because `Genesis::new` gives the same ROM a machine anyway and the dump of a
/// headerless image is still worth having.
///
/// [`Config::resolve_region`] is the rule `Genesis::new` uses, walked on a
/// normalized image, so this line cannot disagree with the machine.
fn described_region(rom: &[u8], config: &Config) -> Option<Region> {
    let rom = genesis::cart::normalize_rom(rom);
    let header = genesis::cart::parse_header(&rom).ok()?;
    Some(config.resolve_region(&header))
}

/// Report a failure on stderr and stop. `main` cannot use `?` and still choose
/// its exit code, and a caller that scripts this tool needs a non-zero one for
/// every failure, not just for a bad command line.
fn fail(message: &str) -> ! {
    eprintln!("ram_dump: {message}");
    std::process::exit(1);
}
