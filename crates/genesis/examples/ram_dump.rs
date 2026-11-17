//! Run a cartridge with no input and write the 68000 work RAM of a named frame
//! to a file: `cargo run -p genesis --release --example ram_dump`.
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

use std::path::PathBuf;

use genesis::{Config, Genesis, Region, RegionSetting};

/// What the command line said, after parsing.
struct Args {
    rom: PathBuf,
    frames: u32,
    out: PathBuf,
    region: RegionSetting,
    tmss: bool,
}

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
    for _ in 0..args.frames {
        machine.run_frame();
    }

    let ram: &[u8] = &machine.bus().ram[..];
    if let Err(e) = std::fs::write(&args.out, ram) {
        fail(&format!("cannot write {}: {e}", args.out.display()));
    }
    println!(
        "region={} frames={} ram={:?} bytes={}",
        region.map(Region::name).unwrap_or("unknown"),
        args.frames,
        args.out,
        ram.len()
    );
}

const USAGE: &str = "\
usage: ram_dump <ROM> --frames N --out PATH [--region auto|usa|japan|europe] [--tmss]

Run <ROM> for N frames with no input and write its 64 KiB of 68000 work RAM
(0xFF0000-0xFFFFFF) to PATH. --region forces a region instead of reading the
cartridge header; --tmss starts with the security check enabled. Prints
`region=... frames=... ram=... bytes=...`.";

/// Parse the command line. Hand-rolled, and deliberately so: this is four
/// options, `clap` is not a dependency of this crate, and an example is not a
/// reason to add one. Anything it cannot read is an error rather than a default,
/// because an oracle that quietly ran the wrong machine is worse than one that
/// refuses to run.
fn parse<I: Iterator<Item = String>>(mut argv: I) -> Result<Args, String> {
    let mut rom: Option<PathBuf> = None;
    let mut frames: Option<u32> = None;
    let mut out: Option<PathBuf> = None;
    let mut region = RegionSetting::Auto;
    let mut tmss = false;

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
    Ok(Args {
        rom: rom.ok_or("no ROM named")?,
        frames,
        out: out.ok_or("no --out PATH: say where the RAM image goes")?,
        region,
        tmss,
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
