//! `headless sweep` from the outside (M6-05, #124): fake ROMs in a directory and
//! the two files the sweep writes about them.
//!
//! The real collection is not involved: these are the same two-page fake images
//! `tests/cli.rs` uses, so the sweep's plumbing - the child process, the wall
//! clock, the CSV, the markdown - is what is under test, and it runs on CI where
//! there are no cartridges. The classification rules themselves are unit-tested
//! in `src/sweep.rs`, and the full sweep over real games is what the report in
//! `docs/compat.md` is made of.
//!
//! The later tests hand the sweep ROMs that paint ([`paint_program`]), because the
//! two rules that decide a verdict are only reachable through a picture: a cart
//! whose picture arrives after the checkpoint (#373's frame window) and a cart
//! whose picture only arrives when `--pad1` presses a button.

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

// ---------------------------------------------------------------------------
// The hand-assembled programs the painting fixtures are built out of.
//
// These are copies of the helpers in `crates/genesis/tests/boot.rs`, not imports:
// a test binary cannot use another test binary's helpers, and a `headless` test
// should not depend on the `genesis` test suite to build its own cartridges.
// ---------------------------------------------------------------------------

/// The 68k's VDP control and data ports (plutiedev "Memory map").
const VDP_CONTROL: u32 = 0xC0_0004;
const VDP_DATA: u32 = 0xC0_0000;
/// Where a fixture program goes: past the 0x200 bytes a load needs, and under
/// reset vectors asking for the top of work RAM.
const CODE: u32 = 0x1000;
const STACK: u32 = 0x00FF_0000;
/// `bra.b $`: wait in place (M68000UM section 8 "BRA").
const WAIT: u16 = 0x60FE;
/// R1 bit 6: the display enable (genvdp.txt section 17 "$01").
const DISPLAY_ON: u8 = 0x40;
/// Plane A's name table: R2 = `0x38` is VRAM `0xE000` in 16 KiB steps, clear of
/// the pattern below it (genvdp.txt section 17 "$02").
const NAME_TABLE_REG: u8 = 0x38;
const NAME_TABLE: u32 = 0xE000;
/// The one cell the fixture draws, and the tile it names: plane A patterns are
/// read at `tile * 32` bytes into VRAM (`vdp::pattern::tile_addr`), so the art
/// lives at `0x2000` and the entry says `0x100`.
const TILE: u16 = 0x0100;
const TILE_ADDR: u32 = 0x2000;
/// The CRAM words the fixture writes: `----bbb0 ggg0 rrr0` (genvdp.txt section 9)
/// for black, red, green, blue and white. Entry 0 is the backdrop and the other
/// four are the tile's bands, which is what makes a frame of more colours than
/// `src/sweep.rs`'s `BLANK_COLOUR_LIMIT` out of one 8x8 cell.
const COLOURS: [u16; 5] = [0x0000, 0x000E, 0x00E0, 0x0E00, 0x0EEE];

/// `move.w #$value,(addr).L` = `33FC imm32 addr32` (M68000UM section 8 "MOVE"),
/// the same encoding `boot.rs` uses.
fn store_word(addr: u32, value: u16) -> Vec<u16> {
    let value = value.to_be_bytes();
    let addr = addr.to_be_bytes();
    vec![
        0x33FC,
        u16::from_be_bytes([value[0], value[1]]),
        u16::from_be_bytes([addr[0], addr[1]]),
        u16::from_be_bytes([addr[2], addr[3]]),
    ]
}

/// A VDP register command through the control port: `100r rrrr dddd dddd`
/// (genvdp.txt section 7 "Programming VDP registers").
fn vdp_register(reg: u8, value: u8) -> Vec<u16> {
    store_word(
        VDP_CONTROL,
        0x8000 | (u16::from(reg) << 8) | u16::from(value),
    )
}

/// The two control-port words that point the data port at byte `addr` of the
/// memory `code` names - `0x4000` for a VRAM write, `0xC000` for a CRAM write
/// (genvdp.txt section 7 "table of code register settings").
fn vdp_command(code: u16, addr: u32) -> Vec<u16> {
    let mut words = store_word(VDP_CONTROL, code | (addr & 0x3FFF) as u16);
    words.extend(store_word(VDP_CONTROL, ((addr >> 14) & 0x0003) as u16));
    words
}

/// The program at [`CODE`] under reset vectors for [`STACK`], with a header naming
/// the image `FAKE ROM` so the sweep has something to report for it.
fn fake_rom(program: &[u16]) -> Vec<u8> {
    let mut rom = vec![0_u8; CODE as usize + 2 * program.len()];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = CODE as usize + 2 * index;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom[0x150..0x158].copy_from_slice(b"FAKE ROM");
    rom
}

/// Write `program` to `path` as a cartridge image.
fn write_fake_rom(path: &Path, program: &[u16]) {
    fs::write(path, fake_rom(program)).expect("write fake rom");
}

/// The program that paints: the display on, plane A's name table at
/// [`NAME_TABLE`], the [`COLOURS`] into CRAM, one 8x8 tile of four colour bands at
/// [`TILE_ADDR`], and the entry naming it at cell (0, 0).
///
/// The tile's four bands are four pairs of pattern rows, and a pattern row is four
/// bytes of eight colour indices read high nibble first
/// (`vdp::pattern::tile_row`), so band `n` is two words of `0xnnnn`.
///
/// R15 is set to 2 first because every VDP register reads 0 at power-on and that
/// means the data port does not advance on its own: the fixture writes CRAM and
/// the tile as runs of port words (genvdp.txt section 17 "$0F").
fn paint_program() -> Vec<u16> {
    let mut program = Vec::new();
    program.extend(vdp_register(1, DISPLAY_ON));
    program.extend(vdp_register(2, NAME_TABLE_REG));
    program.extend(vdp_register(15, 2));
    program.extend(vdp_command(0xC000, 0));
    program.extend(
        COLOURS
            .iter()
            .flat_map(|&colour| store_word(VDP_DATA, colour)),
    );
    program.extend(vdp_command(0x4000, TILE_ADDR));
    for band in 1..=4_u16 {
        let row = 0x1111 * band;
        program.extend(store_word(VDP_DATA, row));
        program.extend(store_word(VDP_DATA, row));
    }
    program.extend(vdp_command(0x4000, NAME_TABLE));
    program.extend(store_word(VDP_DATA, TILE));
    program
}

/// About a frame of nothing at all: `move.w #$ffff,d0` then `dbf d0,$-2`, which is
/// a 65536-step count of the shortest loop the fixture can assemble (M68000UM
/// sections 8 "MOVE" and "DBcc"). [`late_painter`] repeats this until the frame
/// window rule needs it.
fn busy_wait_program() -> Vec<u16> {
    vec![0x303C, 0xFFFF, 0x51C8, 0xFFFE]
}

/// A cart that paints, but not at once: it sits in [`busy_wait_program`] first, so
/// the checkpoint frame is dark and the picture only arrives later in the run.
fn late_painter(waits: usize) -> Vec<u16> {
    let mut program = Vec::new();
    for _ in 0..waits {
        program.extend(busy_wait_program());
    }
    program.extend(paint_program());
    program.push(WAIT);
    program
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
    assert_eq!(
        booted[6], "1",
        "no frame of the run drew either, so #373's peak says the same: {booted:?}"
    );
    assert_eq!(
        booted[7], "",
        "and holding START did not make it draw, so it is not input-gated: {booted:?}"
    );
    assert!(
        booted[8].starts_with("0x"),
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

/// The frame-window rule of #373 from the outside: a cart that is dark at the
/// checkpoint and paints later in the run is `Boots`, not `Blank`.
///
/// The unit tests in `src/sweep.rs` cover `classify` with numbers; this is the
/// same rule reached through a child process that really did draw after the frame
/// the report is taken at. `--frames 1` is the checkpoint and `--run-frames 30` is
/// the watched run, so the row says "one colour at the checkpoint, the fixture's
/// five colours somewhere in the run", and only the widest frame can say that.
#[test]
fn sweep_calls_a_cart_that_paints_after_the_checkpoint_booting() {
    let roms = TempDir::new("late");
    write_fake_rom(&roms.join("late.md"), &late_painter(1));
    let out = TempDir::new("late-out");
    let csv = out.join("compat.csv");

    let run = headless()
        .args(["sweep", roms.join(".").to_str().unwrap()])
        .args(["--frames", "1", "--run-frames", "30"])
        .args(["--csv", csv.to_str().unwrap()])
        .output()
        .expect("run headless sweep over a late-painting ROM");
    assert!(run.status.success(), "sweep failed: {}", report(&run));

    let rows = read_csv(&csv);
    let painted = &rows[1];
    assert_eq!(
        painted[5], "1",
        "the checkpoint frame is still dark: the fixture spends its first frames \
         in a busy-wait: {painted:?}"
    );
    assert_eq!(
        painted[6], "5",
        "and the run went on to the fixture's picture, which is the whole point of \
         the watched run: {painted:?}"
    );
    assert_eq!(
        painted[4], "Boots",
        "dark at the checkpoint and lit later is a cart that boots: {painted:?}"
    );
    assert_eq!(
        painted[7], "",
        "and it painted without being poked, so no input gate is named: {painted:?}"
    );
}
