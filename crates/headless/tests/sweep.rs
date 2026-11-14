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
            "max_colours",
            "input_gate",
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
    assert!(!junk[10].is_empty(), "the row should say why: {junk:?}");

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
        rows[1][10].contains("Sega CD"),
        "and it says so: {:?}",
        rows[1]
    );
}

// ---------------------------------------------------------------------------
// The frame window and the input gate (#373)
// ---------------------------------------------------------------------------

/// Where the fake ROMs below put their program, and the reset stack pointer:
/// `crates/genesis/tests/boot.rs`'s fake ROMs use the same pair.
const CODE: u32 = 0x1000;
const STACK: u32 = 0x00FF_0000;
/// The VDP data and control ports (plutiedev, "Memory map").
const VDP_DATA: u32 = 0xC0_0000;
const VDP_CONTROL: u32 = 0xC0_0004;
/// The HV counter is `$C00008`: a byte read of the even address answers the V
/// counter - `read_byte(0xC00008)` in `crates/genesis/src/bus.rs`'s
/// `hv_counter_read` - and that is how the fakes below count frames.
/// Port 1's data register, TH high, is `$A10003`
/// (`crates/headless/tests/cli.rs`'s pad ROM). Both appear as raw words in
/// `wait_frames` and `write_press_to_draw_rom`.

/// `move.w #$value, (addr).L`: `33FC imm32 addr32` (M68000UM section 8 "MOVE",
/// destination field first), the encoding `crates/genesis/tests/boot.rs` uses.
fn store_word(addr: u32, value: u16) -> Vec<u16> {
    let addr = addr.to_be_bytes();
    vec![
        0x33FC,
        value,
        u16::from_be_bytes([addr[0], addr[1]]),
        u16::from_be_bytes([addr[2], addr[3]]),
    ]
}

/// `op disp16`, a `.W` branch to word index `to` of a program whose next word
/// would be `from_next`. `.W` because the draw blocks are longer than a `.B`
/// displacement reaches.
fn branch(op: u16, from_next: usize, to: usize) -> Vec<u16> {
    let disp = (to as i32 - from_next as i32) * 2;
    vec![op, disp as u16]
}

/// The set-up every fake ROM below runs: display on, the four table bases moved off
/// VRAM's first 64 bytes so that tile 0 is the only thing plane A can show (its
/// name table stays all zeroes, so every cell names tile 0), auto-increment of 2 so
/// one write command and a run of data writes fills a tile, and eight distinct
/// colours in CRAM words 0-7.
///
/// Tile 0 is what turns and clears the picture: with the pattern all zeroes every
/// pixel is colour index 0, one colour, and with the pattern of `LIT_TILE` the
/// pixels are indices 1 to 5 - five distinct colours, which is more than
/// `sweep::BLANK_COLOUR_LIMIT`.
fn init_video() -> Vec<u16> {
    let mut p = Vec::new();
    for command in [
        0x8140, // R1: display enable
        0x8238, // R2: plane A name table at 0xE000
        0x8401, // R4: plane B name table at 0x2000
        0x8550, // R5: sprite attribute table at 0xA000
        0x8D04, // R13: horizontal scroll table at 0x1000
        0x8F02, // R15: auto-increment 2 bytes
    ] {
        p.extend(store_word(VDP_CONTROL, command));
    }
    p.extend(store_word(VDP_CONTROL, 0xC000)); // CRAM write command, word 0
    p.extend(store_word(VDP_CONTROL, 0x0000));
    for colour in [0x000E, 0x00E0, 0x0E00, 0x00EE, 0x0EE0, 0x0E0E, 0x0EEE, 0x0001] {
        p.extend(store_word(VDP_DATA, colour));
    }
    p
}

/// Tile 0's 32 pattern words: four bitplanes of eight rows (planar, which is what
/// `crates/vdp/src/pattern.rs` reads), giving colour indices 1, 2, 3, 4, 4, 5, 5, 5
/// down the tile. `erase` is the same thirty-two writes of zeroes, which leaves
/// every pixel at index 0 and the frame at one colour.
fn write_tile(erase: bool) -> Vec<u16> {
    let planes: [[u16; 8]; 3] = if erase {
        [[0; 8]; 3]
    } else {
        [
            [0xFFFF, 0x0000, 0xFFFF, 0x0000, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF],
            [0x0000, 0xFFFF, 0xFFFF, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000],
            [0x0000, 0x0000, 0x0000, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF],
        ]
    };
    let mut p = Vec::new();
    p.extend(store_word(VDP_CONTROL, 0x4000)); // VRAM write command, address 0
    p.extend(store_word(VDP_CONTROL, 0x0000));
    for plane in planes {
        for row in plane {
            p.extend(store_word(VDP_DATA, row));
        }
    }
    p
}

/// `wait for outer * inner frame boundaries`: `MOVE.B` of the HV counter sets the
/// condition codes from the V counter it read, so `BPL` holds while V is under 128
/// and `BMI` holds while it is not, and the pair crosses line 128 exactly once a
/// frame. The VDP's own VBlank *flag* cannot be used this way because reading it
/// does not clear it (`crates/vdp/src/status.rs`), so it marks no edge.
///
/// `MOVEQ` is the only loader, and it is an eight-bit signed immediate, hence
/// `outer * inner` rather than a frame count on its own.
fn wait_frames(outer: u8, inner: u8) -> Vec<u16> {
    //  word 0: moveq #outer-1, d6    0x7C00 | imm
    //  word 1: moveq #inner-1, d7    0x7E00 | imm
    //  word 2: `here` move.b ($C00008).L, d0        word 5: bpl  `here`   (-8)
    //  word 6: `there` move.b ($C00008).L, d0       word 9: bmi  `there`  (-8)
    //  word 10: dbra d7, `here`  (-20)               word 12: dbra d6, `here` (-24)
    vec![
        0x7C00 | u16::from(outer) - 1,
        0x7E00 | u16::from(inner) - 1,
        0x1039,
        0x00C0,
        0x0008,
        0x6AF8,
        0x1039,
        0x00C0,
        0x0008,
        0x6BF8,
        0x51C7,
        0xFFEC,
        0x51C6,
        0xFFE8,
    ]
}

/// `forever: bra.b forever`, which is how each fake ROM sits out the rest of a run.
fn halt() -> Vec<u16> {
    vec![0x60FE_u16]
}

/// Assemble `program` into a cartridge image with a header that names itself `title`.
fn rom_image(title: &str, program: &[u16]) -> Vec<u8> {
    let mut rom = vec![0_u8; (CODE as usize + 2 * program.len()).max(0x200)];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (word, at) in program.iter().zip(0..) {
        let at = CODE as usize + 2 * at;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom[0x150..0x160].copy_from_slice(format!("{title:<16}").as_bytes());
    rom
}

/// A cart that is dark for the first 700 frames, holds a picture from 700 to 900,
/// and goes dark again: `headless run --frames 600 --colours` sees one colour and
/// `--max-colours` sees five (#373).
fn write_window_rom(path: &Path) {
    let mut p = init_video();
    p.extend(write_tile(true));
    p.extend(wait_frames(7, 100));
    p.extend(write_tile(false));
    p.extend(wait_frames(2, 100));
    p.extend(write_tile(true));
    p.extend(halt());
    fs::write(path, rom_image("WINDOW ROM", &p)).expect("write window rom");
}

/// A cart that draws only while a button is held on port 1: at rest every pin reads
/// high, and all-high is the one thing that keeps tile 0 cleared
/// (`crates/genesis/tests/boot.rs`, `pins`).
fn write_press_to_draw_rom(path: &Path) {
    let mut p = init_video();
    p.extend(write_tile(true));
    // loop: move.b ($A10003).L, d0 ; cmpi.b #$FF, d0 ; beq.w wait
    let loop_at = p.len();
    p.extend([0x1039, 0x00A1, 0x0003, 0x0C00, 0x00FF]);
    p.extend(store_word(0, 0)); // placeholder for `beq.w wait`
    let beq_at = p.len() - 2;
    p.extend(write_tile(false));
    // wait:
    let wait_at = p.len();
    p.extend(wait_frames(1, 1));
    p.extend(branch(0x6100, p.len() + 2, loop_at)); // bra.w loop
    let disp = (wait_at as i32 - (beq_at as i32 + 2)) * 2;
    p[beq_at] = 0x6700;
    p[beq_at + 1] = disp as u16;
    fs::write(path, rom_image("PRESS ROM", &p)).expect("write press rom");
}

/// The row of `file` in a CSV the sweep wrote, by name.
fn row_for(rows: &[Vec<String>], file: &str) -> Vec<String> {
    rows.iter()
        .find(|row| row.first().map_or(false, |name| name == file))
        .unwrap_or_else(|| panic!("no row for {file} in {rows:#?}"))
        .clone()
}

/// The sweep's columns, by name, so a test says `row["status"]` rather than
/// `row[4]` and cannot be quietly wrong about what `sweep` writes.
fn columns(rows: &[Vec<String>]) -> Vec<String> {
    rows.first().cloned().unwrap_or_default()
}

fn field(row: &[String], header: &[String], name: &str) -> String {
    let at = header
        .iter()
        .position(|column| column == name)
        .unwrap_or_else(|| panic!("no {name} column in {header:?}"));
    row[at].clone()
}

/// The sweep runs each ROM past `--frames`, so a cart that only draws between 700
/// and 900 is a working cart even though the 600-frame sample is dark (#373).
#[test]
fn sweep_calls_a_rom_that_drew_outside_the_sampled_frame_boots() {
    let roms = TempDir::new("window");
    write_window_rom(&roms.join("window.md"));
    let out = TempDir::new("window-out");
    let csv = out.join("compat.csv");

    let run = headless()
        .args(["sweep", roms.join(".").to_str().unwrap()])
        .args(["--frames", "600", "--run-frames", "1800", "--input-gate", "none"])
        .args(["--csv", csv.to_str().unwrap()])
        .output()
        .expect("run headless sweep");
    assert!(run.status.success(), "sweep failed: {}", report(&run));

    let rows = read_csv(&csv);
    let header = columns(&rows);
    let row = row_for(&rows, "window.md");
    assert_eq!(
        field(&row, &header, "distinct_colors"),
        "1",
        "the frame at 600 is the dark one: {row:?}"
    );
    assert!(
        field(&row, &header, "max_colours").parse::<usize>().expect("a number") > 4,
        "some frame of the run held a picture, and the CSV has to say so: {row:?}"
    );
    assert_eq!(
        field(&row, &header, "status"),
        "Boots",
        "a cart that drew at 700 and went dark again did draw: {row:?}"
    );
}

/// The other half of #373, and the test that stops "watch every frame" becoming
/// "always boots": a cart that never writes a colour to CRAM stays `Blank`.
#[test]
fn sweep_keeps_a_rom_that_never_draws_blank() {
    let roms = TempDir::new("never");
    write_loop_rom(&roms.join("loop.md"));
    let out = TempDir::new("never-out");
    let csv = out.join("compat.csv");

    let run = headless()
        .args(["sweep", roms.join(".").to_str().unwrap()])
        .args(["--frames", "600", "--run-frames", "1800"])
        .args(["--csv", csv.to_str().unwrap()])
        .output()
        .expect("run headless sweep");
    assert!(run.status.success(), "sweep failed: {}", report(&run));

    let rows = read_csv(&csv);
    let header = columns(&rows);
    let row = row_for(&rows, "loop.md");
    assert_eq!(
        field(&row, &header, "max_colours"),
        "1",
        "an infinite loop never writes a colour: {row:?}"
    );
    assert_eq!(field(&row, &header, "status"), "Blank", "{row:?}");
    assert_eq!(
        field(&row, &header, "input_gate"),
        "",
        "nothing was held, so nothing was gated: {row:?}"
    );
}

/// A cart that only draws once START is held boots - but the row says which gate
/// did it, and its own unperturbed colour counts stay at one, so "never drew" and
/// "only drew when poked" are still two different rows of the CSV (#373).
#[test]
fn sweep_names_the_input_gate_that_made_a_rom_draw() {
    let roms = TempDir::new("press");
    write_press_to_draw_rom(&roms.join("press.md"));
    let out = TempDir::new("press-out");

    let swept = |extra: &[&str]| -> Vec<Vec<String>> {
        let name = format!("{}.csv", if extra.is_empty() { "gated" } else { "raw" });
        let csv = out.join(&name);
        let mut args = vec![
            "sweep".to_string(),
            roms.join(".").to_str().unwrap().to_string(),
            "--frames".to_string(),
            "600".to_string(),
            "--run-frames".to_string(),
            "1200".to_string(),
            "--csv".to_string(),
            csv.to_str().unwrap().to_string(),
        ];
        args.extend(extra.iter().map(|arg| arg.to_string()));
        let run = headless().args(&args).output().expect("run headless sweep");
        assert!(run.status.success(), "sweep failed: {}", report(&run));
        read_csv(&csv)
    };

    let rows = swept(&[]);
    let header = columns(&rows);
    let row = row_for(&rows, "press.md");
    assert_eq!(
        field(&row, &header, "status"),
        "Boots",
        "START at 300 got it to draw: {row:?}"
    );
    assert!(
        !field(&row, &header, "input_gate").is_empty(),
        "and the row names the gate rather than folding it into Boots: {row:?}"
    );
    assert_eq!(
        field(&row, &header, "max_colours"),
        "1",
        "its own unperturbed run stayed dark: {row:?}"
    );

    let rows = swept(&["--input-gate", "none"]);
    let header = columns(&rows);
    let row = row_for(&rows, "press.md");
    assert_eq!(
        field(&row, &header, "status"),
        "Blank",
        "with no input at all this cart does not draw: {row:?}"
    );
    assert!(
        field(&row, &header, "input_gate").is_empty(),
        "and nothing is claimed about a gate: {row:?}"
    );
}

#[test]
fn dump_window_program() {
    let base = init_video();
    let a = [base.as_slice(), halt().as_slice()].concat();
    std::fs::write("/tmp/a.md", rom_image("A", &a)).unwrap();
    let b = [base.as_slice(), write_tile(true).as_slice(), halt().as_slice()].concat();
    std::fs::write("/tmp/b.md", rom_image("B", &b)).unwrap();
    let c = [
        base.as_slice(),
        write_tile(true).as_slice(),
        wait_frames(1, 1).as_slice(),
        halt().as_slice(),
    ]
    .concat();
    std::fs::write("/tmp/c.md", rom_image("C", &c)).unwrap();
    let d = [base.as_slice(), wait_frames(1, 1).as_slice(), halt().as_slice()].concat();
    std::fs::write("/tmp/d.md", rom_image("D", &d)).unwrap();
    for (name, prog): (&str, &[u16]) in [
        ("e", &[0x7C06_u16, 0x7E63][..]),
        ("g", &[0x1039, 0x00C0, 0x0008][..]),
        ("h", &[0x51C7, 0xFFFE][..]),
        ("i", &[0x7C06, 0x1039, 0x00C0, 0x0008, 0x6AF8][..]),
        ("j", &[0x7C06, 0x1039, 0x00C0, 0x0008, 0x6BF8][..]),
        ("k", &[0x1039, 0x00C0, 0x0008, 0x6AF8, 0x51C7, 0xFFF4][..]),
    ] {
        let rom = [base.as_slice(), prog, halt().as_slice()].concat();
        std::fs::write(format!("/tmp/{name}.md"), rom_image(name, &rom)).unwrap();
    }
    let mut p = init_video();
    p.extend(write_tile(true));
    p.extend(wait_frames(7, 100));
    println!(
        "{} words: {:04X?}",
        p.len(),
        &p[..60.min(p.len())]
    );
    let w = wait_frames(7, 100);
    println!("wait: {:04X?}", w);
}
