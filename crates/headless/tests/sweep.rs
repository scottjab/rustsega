//! `headless sweep` from the outside (M6-05, #124): a directory of two fake ROMs
//! and the two files the sweep writes about them.
//!
//! The real collection is not involved: these are the same two-page fake images
//! `tests/cli.rs` uses, so the sweep's plumbing - the child process, the wall
//! clock, the CSV, the markdown - is what is under test, and it runs on CI where
//! there are no cartridges. The classification rules themselves are unit-tested
//! in `src/sweep.rs`, and the full sweep over real games is what the report in
//! `docs/compat.md` is made of.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The headless binary under test, as cargo built it for this test target.
fn headless() -> Command {
    Command::new(env!("CARGO_BIN_EXE_headless"))
}

/// A scratch directory that cleans up after itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rustsega-sweep-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A cartridge that loads and runs: reset vectors for a `bra.b $=0` at 0x1000 and
/// a header that names itself `FAKE ROM`.
///
/// It draws nothing - an infinite loop never touches the VDP - which is exactly
/// what makes it useful here: the sweep has to call a run that finished with a
/// blank screen `Blank`, not `Boots`. The vector table and the `BRA.B` encoding
/// are the ones `crates/genesis/tests/boot.rs` uses.
fn write_loop_rom(path: &Path) {
    const STACK: u32 = 0x00FF_0000;
    const CODE: u32 = 0x1000;
    let mut rom = vec![0_u8; 0x1002];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    rom[CODE as usize..CODE as usize + 2].copy_from_slice(&0x60FE_u16.to_be_bytes());
    rom[0x150..0x158].copy_from_slice(b"FAKE ROM");
    fs::write(path, rom).expect("write fake rom");
}

/// An image too small to hold a header at 0x100, so the sweep cannot even load it.
fn write_garbage_rom(path: &Path) {
    fs::write(path, [0xAB_u8; 16]).expect("write garbage rom");
}

/// stdout and stderr together, for a failure message.
fn report(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The CSV as rows of fields, without a CSV parser.
///
/// The fixtures below produce no comma and no quote in any field, so splitting on
/// the comma is exact rather than almost right; `src/sweep.rs` unit-tests the
/// quoting itself.
fn read_csv(path: &Path) -> Vec<Vec<String>> {
    let text = fs::read_to_string(path).expect("read the CSV the sweep wrote");
    text.lines()
        .map(|line| line.split(',').map(str::to_string).collect())
        .collect()
}

#[test]
fn sweep_on_temp_dir() {
    let roms = TempDir::new("roms");
    write_loop_rom(&roms.join("loop.md"));
    write_garbage_rom(&roms.join("junk.md"));
    let out = TempDir::new("out");
    let csv = out.join("compat.csv");
    let markdown = out.join("compat.md");

    let run = headless()
        .args(["sweep", roms.join(".").to_str().unwrap()])
        .args(["--frames", "5", "--jobs", "2"])
        .args(["--csv", csv.to_str().unwrap()])
        .args(["--markdown", markdown.to_str().unwrap()])
        .output()
        .expect("run headless sweep");
    assert!(run.status.success(), "sweep failed: {}", report(&run));

    let rows = read_csv(&csv);
    assert_eq!(
        rows[0],
        vec![
            "file",
            "title",
            "serial",
            "region",
            "status",
            "distinct_colors",
            "frame_hash",
            "seconds",
            "notes",
        ],
        "the CSV header"
    );
    assert_eq!(rows.len(), 3, "one row per ROM, plus the header:\n{rows:?}");

    // Sorted by file name, so `junk.md` then `loop.md`.
    let junk = &rows[1];
    assert_eq!(junk[0], "junk.md");
    assert_eq!(junk[4], "LoadError", "16 bytes is not a ROM: {junk:?}");
    assert!(!junk[8].is_empty(), "the row should say why: {junk:?}");

    let booted = &rows[2];
    assert_eq!(booted[0], "loop.md");
    assert_eq!(booted[1], "FAKE ROM");
    assert_eq!(booted[3], "usa");
    assert_eq!(
        booted[4], "Blank",
        "an infinite loop runs, and draws nothing: {booted:?}"
    );
    assert_eq!(booted[5], "1", "the border is the only colour on screen");
    assert!(
        booted[6].starts_with("0x"),
        "a frame that ran has a hash: {booted:?}"
    );

    let text = fs::read_to_string(&markdown).expect("read the markdown the sweep wrote");
    assert!(text.contains("| Blank | 1 |"), "counts table:\n{text}");
    assert!(text.contains("| LoadError | 1 |"), "counts table:\n{text}");
    assert!(
        text.contains("`junk.md`"),
        "the problem should be listed:\n{text}"
    );
}

#[test]
fn sweep_skips_a_sega_cd_image_without_running_it() {
    let roms = TempDir::new("cd");
    let mut image = vec![0_u8; 0x1002];
    image[0..4].copy_from_slice(&0x00FF_0000_u32.to_be_bytes());
    image[4..8].copy_from_slice(&0x1000_u32.to_be_bytes());
    image[0x1000..0x1002].copy_from_slice(&0x60FE_u16.to_be_bytes());
    image[0x100..0x10B].copy_from_slice(b"SEGA CD ROM");
    fs::write(roms.join("bios.md"), &image).expect("write a Sega CD image");

    let out = TempDir::new("cd-out");
    let csv = out.join("compat.csv");
    let run = headless()
        .args(["sweep", roms.join(".").to_str().unwrap()])
        .args(["--frames", "5", "--csv", csv.to_str().unwrap()])
        .output()
        .expect("run headless sweep");
    assert!(run.status.success(), "sweep failed: {}", report(&run));

    let rows = read_csv(&csv);
    assert_eq!(rows[1][0], "bios.md");
    assert_eq!(
        rows[1][4], "Skipped",
        "a Sega CD BIOS is not a cartridge fault: {:?}",
        rows[1]
    );
    assert!(
        rows[1][8].contains("Sega CD"),
        "and it says so: {:?}",
        rows[1]
    );
}
