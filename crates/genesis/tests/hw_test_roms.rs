//! Hardware test ROM goldens (M6-06, #125).
//!
//! Every ROM that `scripts/fetch-test-data.sh` can fetch under
//! `test-roms/genesis/` is run here and its picture pinned, so accuracy is
//! measured rather than guessed. Nothing in the pack is self-checking: these
//! ROMs draw a pattern or print a report and expect a human to judge it
//! (`docs/test-roms.md`, "Reporting"). So a test here is
//! [`run_and_check`]: run the ROM, dump the frame to `target/test-output/`,
//! compare its hash with `golden/hw-test-roms.toml`.
//!
//! # What each golden is supposed to look like
//!
//! Each test says below what its screen shows and how that was checked. The
//! screens are not guessed at: the pack's text is the ROMs' own font, and the
//! dumps were read back tile by tile (8x8 cells compared against the tile data
//! in the ROM image) to get the strings the ROM printed. That is how
//! `memtest_68k`'s report and `vctest`'s menu below were transcribed, and it is
//! what makes a golden here mean something rather than record whatever the
//! emulator happened to do.
//!
//! Two of the numbers quoted below are the point of the pack rather than
//! decoration: `MD1536` and `960PAL` leave **more than the 512 colours a single
//! VDP palette can hold** in one frame (1407 and 872 distinct pixels
//! respectively), which is only possible if the emulator honours the palette
//! writes the ROM makes in the middle of the frame. That is the accuracy this
//! suite exists to keep.
//!
//! # Frame counts
//!
//! Sixty frames: every ROM whose output settles has settled well before then
//! (`512PAL.BIN` is identical at 30, 120 and 300 frames, `memtest_68k` at 3 and
//! 300), and sixty frames of any of them is under the two-second budget
//! `docs/TESTING.md` gives a test that `just check` runs. The four ROMs that
//! animate - `MD1536`, `mdtrans`, `STETEST2`, `wbug` - have no settled screen,
//! so their golden pins one fixed frame of the animation and its key says so.
//!
//! A missing ROM is a skip, never a failure: see `common::read_rom`.

mod common;

use std::collections::HashSet;
use std::path::PathBuf;

use genesis::{Buttons, Genesis};
use testkit::require_data;

use common::{RomRun, run_and_check, run_frames};

/// The golden file every test in this file writes its key into.
const GOLDEN_FILE: &str = "hw-test-roms.toml";

/// Frames per test: see the module comment for why sixty.
const FRAMES: u32 = 60;

/// A `md-test-pack` ROM by file name, or a skip when the pack was not fetched.
///
/// Spelled as a macro because [`require_data`] returns from the function it is
/// written in, and the test is the function that has to return.
macro_rules! pack {
    ($name:literal) => {
        require_data!(testkit::test_rom(concat!("genesis/md-test-pack/", $name)))
    };
}

/// Run one pack ROM and pin its last frame under `key`.
fn check(key: &str, rom: PathBuf, frames: u32, inputs: &[(u32, Buttons)]) {
    run_and_check(RomRun {
        rom,
        frames,
        inputs,
        golden_file: GOLDEN_FILE,
        key,
    });
}

/// The active pixels of a machine's last frame, as [`testkit::frame_hash`]
/// wants them.
fn pixels(machine: &Genesis) -> Vec<u32> {
    let frame = machine.frame();
    frame.pixels[..frame.width * frame.height].to_vec()
}

/// How many different pixels a frame holds. One means nothing was drawn.
fn distinct_colours(pixels: &[u32]) -> usize {
    pixels.iter().collect::<HashSet<_>>().len()
}

/// The hash [`run_and_check`] would pin for a machine's last frame, for a test
/// that has to compare two runs of its own before pinning either.
fn hash_of(machine: &Genesis) -> u64 {
    let frame = machine.frame();
    testkit::frame_hash(&pixels(machine), frame.width, frame.height)
}

/// `512PAL.BIN`: TmEE's 512-colour test, PAL only ("512 color test -
/// www.hot.ee/tmeeco 50Hz only", and its no-modem comment says "Works correctly
/// only on 50Hz machines !"). It walks the whole 9-bit palette across the frame
/// by writing CRAM while the beam is still drawing, so the screen is a dense
/// grid of colour cells of 449 distinct colours over the backdrop.
#[test]
fn md_test_pack_512pal_paints_the_whole_palette() {
    check("md_test_pack_512pal", pack!("512PAL.BIN"), FRAMES, &[]);
}

/// `960PAL.bin`: TmEE's follow-up ("960 color test - ... Is it possible to get
/// more colors ???"). The console has 512 colours, so the ROM has to change the
/// palette mid-frame to show more; the dumped frame holds 872 distinct pixels,
/// 360 more than one palette can produce. Geometry is 256x240: H32, and 240
/// lines because the ROM asks for them.
#[test]
fn md_test_pack_960pal_shows_more_colours_than_one_palette_holds() {
    check("md_test_pack_960pal", pack!("960PAL.bin"), FRAMES, &[]);
}

/// `PAL512.bin`: a second 512-colour test - its header says nothing but "SAMPLE
/// PROGRAM" (2006.JAN). 256x240, 244 distinct colours in a repeating block
/// grid: each block is a slice of the palette, the same idea as `512PAL.BIN`
/// in H32.
#[test]
fn md_test_pack_pal512_paints_its_colour_grid() {
    check("md_test_pack_pal512", pack!("PAL512.bin"), FRAMES, &[]);
}

/// `MD1536.bin`: walks all 1536 CRAM colours (512 x 3, so three palette pages
/// per frame). The animation never settles, so this pins frame 60 of it; the
/// frame is a full-screen colour sweep holding 1407 distinct pixels.
#[test]
fn md_test_pack_md1536_walks_the_cram_colours() {
    check(
        "md_test_pack_md1536_frame_60",
        pack!("MD1536.bin"),
        FRAMES,
        &[],
    );
}

/// `mdtrans.bin`: transparency and plane priority. Nineteen colours over a
/// scrolling field of plane A/B cells with the sprite and shadow priority
/// stacked on top; it scrolls, so frame 60 is pinned.
#[test]
fn md_test_pack_mdtrans_draws_its_priority_field() {
    check(
        "md_test_pack_mdtrans_frame_60",
        pack!("mdtrans.bin"),
        FRAMES,
        &[],
    );
}

/// `memtest_68k.bin`: the 68000 memory test. It prints a report and stops, and
/// the report is identical from frame 3 onwards. The screen, read off the dump:
///
/// ```text
///                     68000 memory test
///  400000-7FFFFF : FFFF FFFF (4E00 4E00)
///  A00000-A0FFFF : F3ED F3F3 (4E00 4E00)
///  A00000-A03FFF : F3ED F3F3 (F3ED F3F3)
///  A04000-A05FFF : 0000 0000 (0000 0000)
///  A06000-A07EFF : FFFF FFFF (FFFF FFFF)
///  A10000-A1001F : A0A0 A0A0 (A0A0 A0A0)
///  A11100        : FEFE FEFF (4F00 4F00)
///  A11100        : FEFE FEFF (4E00 4E00)
///  A11200        : FFFF FFFF (4E00 4E00)
///  C00000-C00003 : 1122 1122 (1122 1122)
///  C00004-C00007 : 3688 3688 (4E88 4E88)
///  C00008-C0000F : C01A C024 (C0?? C0??)
///  C00018-C0001F : FFFF FFFF (4E71 4E71)
///
///  RAM, ROM, mirrored VDP addresses, and
///  locations that lock up the machine are
///  not tested.
/// ```
///
/// So the rows are the places where a write is *not* expected to land -
/// cartridge space, the I/O registers, the write-only VDP ports - with the
/// value written and the value read back. Work RAM ($E00000) is absent, which is
/// the ROM's way of saying it passed. Every read-back matches what a real
/// console returns for those regions: $4E71, the byte pair of `NOP`, is the
/// 68000 bus floating after an instruction fetch.
#[test]
fn md_test_pack_memtest_68k_prints_its_memory_report() {
    check(
        "md_test_pack_memtest_68k",
        pack!("memtest_68k.bin"),
        FRAMES,
        &[],
    );
}

/// `SHLTEST.BIN` / `SHLTEST2.BIN`: TmEE's shadow/highlight test (2010.JAN,
/// "Shadow/Highlight mode test"). Two blocks of 16-colour ramps, each drawn in
/// one of the four shadow/highlight modes, so 17 colours: the ramp plus the
/// half-brightness and double-brightness rows the mode selects. The two
/// variants are the same test with the modes in another order, and their
/// goldens differ, which is the point.
#[test]
fn md_test_pack_shltest_draws_its_shadow_and_highlight_ramps() {
    check("md_test_pack_shltest", pack!("SHLTEST.BIN"), FRAMES, &[]);
}

/// `SHLTEST2.BIN`: the second variant of the same test; see
/// [`md_test_pack_shltest_draws_its_shadow_and_highlight_ramps`].
#[test]
fn md_test_pack_shltest2_draws_its_shadow_and_highlight_ramps() {
    check("md_test_pack_shltest2", pack!("SHLTEST2.BIN"), FRAMES, &[]);
}

/// `stetest.bin` and `STETEST2.bin`: Paul Lee's sprite-table tests (1996,
/// "DEMO PROGRAM 6" and "DEMO PROGRAM 1"). Fields of solid-colour plane cells
/// with the sprite layer drawing over them, so a wrong sprite limit or priority
/// shows as a shape in the wrong colour: `stetest` leaves yellow and cyan sprite
/// shapes over green and blue bands, `STETEST2` text and blocks. Both are
/// 256x224; `stetest` settles after a few frames and `STETEST2` animates, so
/// only the second pins a numbered frame.
#[test]
fn md_test_pack_stetest_draws_its_sprite_field() {
    check("md_test_pack_stetest", pack!("stetest.bin"), FRAMES, &[]);
}

/// `STETEST2.bin`: the other sprite-table test, on its 240-frame animation
/// cycle; see [`md_test_pack_stetest_draws_its_sprite_field`].
#[test]
fn md_test_pack_stetest2_draws_its_sprite_field() {
    check(
        "md_test_pack_stetest2_frame_60",
        pack!("STETEST2.bin"),
        FRAMES,
        &[],
    );
}

/// `vctest.bin`: Charles MacDonald's V counter test program (2000.DEC). It
/// boots to a menu, and the menu text in the dump is the ROM's own, verbatim:
///
/// ```text
///   V counter test program
///   by Charles MacDonald
///   E-mail: cgfm2@hotmail.com
///
///   0. Mode 4 (256x192)
///   1. Mode 4 (256x192, interlace 1)
///   2. Mode 4 (256x192, interlace 2)
///   3. Mode 5 (320x224)
///   4. Mode 5 (320x448, interlace 1)
///   5. Mode 5 (320x448, interlace 2)
///   6. Mode 5 (320x240, PAL)
///   7. Mode 5 (320x480, interlace 1+PAL)
///   8. Mode 5 (320x480, interlace 2+PAL)
///
///   Press START to run a test.
///   Press A to toggle scanning 262 or
///   312 lines.
///
///   Lines to scan: 262
/// ```
///
/// `docs/test-roms.md` called this ROM a "VDP colour test"; it is not, and the
/// table now says so.
#[test]
fn md_test_pack_vctest_prints_its_mode_menu() {
    check("md_test_pack_vctest_menu", pack!("vctest.bin"), FRAMES, &[]);
}

/// The same ROM one button press later. `vctest` runs a test when START is
/// pressed and then prints, per display mode, the V counter values it measured:
///
/// ```text
///   Press START to exit.
///   Press A to format for IM2 values,
///   Press B for the previous page,
///   Press C for the next page.
///   Page: 0/2  Reg: 8C81 8144  Format:0
///
///   0000 0010 0020 0030 0040 0050 0060 0070
///   ...
/// ```
///
/// START is held for one frame and released on the next, which is how the
/// harness spells a press ([`run_frames`]: "a frame with no entry is run with
/// the pad released"). The result screen is stable from frame 70 on.
#[test]
fn md_test_pack_vctest_start_press_runs_a_test() {
    let rom = pack!("vctest.bin");
    let inputs = &[(FRAMES, Buttons::START)];
    let pressed = hash_of(&run_frames(&rom, FRAMES + 30, inputs));
    let idle = hash_of(&run_frames(&rom, FRAMES + 30, &[]));
    assert_ne!(
        pressed, idle,
        "vctest's menu says \"Press START to run a test\", so START has to change the screen"
    );

    check("md_test_pack_vctest_after_start", rom, FRAMES + 30, inputs);
}

/// `wtest.bin`, `wtest_i1.bin`, `wtest_i2.bin`: the VDP write-timing tests.
/// Each draws a field of colour blocks and prints the position it is writing
/// at ("X:04 Y:08" over the pattern); `wtest_i1` and `wtest_i2` are the same
/// test with interrupts enabled, and `wtest_i2` runs the display in its
/// interlaced 320x448 mode, which is why its golden is 192 lines taller.
#[test]
fn md_test_pack_wtest_draws_its_write_pattern() {
    check("md_test_pack_wtest", pack!("wtest.bin"), FRAMES, &[]);
}

/// `wtest_i1.bin`: the write-timing test with interrupts on; see
/// [`md_test_pack_wtest_draws_its_write_pattern`].
#[test]
fn md_test_pack_wtest_i1_draws_its_write_pattern() {
    check("md_test_pack_wtest_i1", pack!("wtest_i1.bin"), FRAMES, &[]);
}

/// `wtest_i2.bin`: the write-timing test with interrupts on, 320x448; see
/// [`md_test_pack_wtest_draws_its_write_pattern`].
#[test]
fn md_test_pack_wtest_i2_draws_its_write_pattern() {
    check("md_test_pack_wtest_i2", pack!("wtest_i2.bin"), FRAMES, &[]);
}

/// `wbug.bin`: the write-timing bug repro. Its screen is the backdrop colour
/// with a block of single-pixel marks near the top-left - one per VDP write the
/// ROM attempted, two lines of them - and the whole block walks sideways as the
/// ROM loops: it is four pixels further right at frame 120 than at 60, and back
/// where it started by frame 300. Frame 60 is pinned.
#[test]
fn md_test_pack_wbug_walks_its_pattern() {
    check("md_test_pack_wbug_frame_60", pack!("wbug.bin"), FRAMES, &[]);
}

/// `titan-overdrive2.bin` (TiTAN, 2017): a stress case, not a pass/fail judge,
/// so `docs/test-roms.md` is explicit that it gets no golden - its output
/// legitimately differs between VDP revisions. What it does prove is that the
/// machine does not crash, hang, or drift, so this asks those three questions:
/// does 60 frames finish, is the picture non-blank, and does the same input give
/// the same frame hash twice? Two runs of the hardest ROM in the collection is
/// ~1.5 s at the `[profile.test]` opt-level, so it stays inside the budget and
/// runs on every pull request.
#[test]
fn overdrive2_runs_without_crashing_and_is_deterministic() {
    let rom = require_data!(testkit::test_rom("genesis/overdrive2/titan-overdrive2.bin"));

    let first = hash_of(&run_frames(&rom, FRAMES, &[]));
    let second = run_frames(&rom, FRAMES, &[]);
    assert_eq!(
        first,
        hash_of(&second),
        "the same ROM and the same (empty) input has to give the same picture"
    );

    let colours = distinct_colours(&pixels(&second));
    assert!(
        colours > 1,
        "Overdrive 2 left a blank screen after {FRAMES} frames"
    );
    testkit::dump_png(
        &common::golden_png("hw-test-roms", "overdrive2_frame_60"),
        &pixels(&second),
        second.frame().width,
        second.frame().height,
    );
}
