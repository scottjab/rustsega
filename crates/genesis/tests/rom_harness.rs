//! The ROM harness of `tests/common` (M6-03, #122): a ROM, a frame count, an
//! input script and a golden key, in three lines of test.
//!
//! This file is the harness's own test and so the worked example the next
//! ticket's ROM regression tests copy: [`run_and_check`] is the whole regression,
//! [`run_frames`] is the same run for a test that wants to look inside the
//! machine first. `harness_runs_fake_rom_and_matches_golden` is the golden case
//! and runs on a fake ROM written here, so the harness is covered by `just
//! check` with no cartridge and no network; the real ROMs are M6-04 (#124) and
//! M6-06 (#125).

mod common;

use std::path::{Path, PathBuf};

use genesis::{Buttons, Genesis};

use common::{RomRun, TempDir, golden_png, read_rom, run_and_check, run_frames};

/// How many frames the fake ROM runs for. More than one: a golden taken from the
/// frame a machine shows before its program has run is a golden of the reset
/// state, and one frame is the shortest run that can prove the harness asked for
/// the number it was given.
const FRAMES: u32 = 4;

/// The backdrop palette entry the fake ROM selects with register 7, and the
/// colour it writes there (genvdp.txt section 17 "$07": "Bits 5-0 of this
/// register select a palette entry to be used as the backdrop color").
const BACKDROP_ENTRY: usize = 1;
/// CRAM `----bbb0 ggg0 rrr0` for full red (genvdp.txt section 9).
const BACKDROP_CRAM: u16 = 0x000E;
/// The same colour as a frame-buffer pixel, `0xAABBGGRR`: the DAC levels of
/// `vdp::color` make `0x000E` 255, 0, 0 at normal intensity, and
/// `backdrop_rom_is_a_solid_backdrop_colour` is the test that says the frame
/// really is that pixel.
const BACKDROP_PIXEL: u32 = 0xFF00_00FF;

/// The golden file and key this test owns.
const GOLDEN_FILE: &str = "backdrop.toml";
const GOLDEN_KEY: &str = "backdrop_4_frames";

/// The 68k's VDP control port and data port (plutiedev "Memory map").
const VDP_CONTROL: u32 = 0xC0_0004;
const VDP_DATA: u32 = 0xC0_0000;
/// Where the fake ROMs put their program: past the 0x200 bytes a load needs
/// ([`genesis::cart::parse_header`]).
const CODE: u32 = 0x1000;
/// The reset stack pointer the fake ROMs ask for: the top of work RAM.
const STACK: u32 = 0x00FF_0000;
/// Where the input test's program copies port 1's data register to. Cartridge
/// space is read-only, so it has to be work RAM, and `0xFF0000` is the last
/// mirror of the 64 KiB at `0xE00000` (plutiedev "Memory map").
const PAD_COPY: u32 = 0xFF_0000;
/// Port 1's data register at TH = 0: `D0-D5` are UP, DOWN, LEFT, RIGHT, B, C
/// and every line is active low, so a pad at rest reads all ones
/// (segaretro "Control Pad (Mega Drive)", "Technical information").
const PADS_AT_REST: u8 = 0x7F;

/// The pixels of the active display, i.e. the bytes [`testkit::frame_hash`] and
/// [`testkit::dump_png`] are given.
fn active_pixels(machine: &Genesis) -> &[u32] {
    let frame = machine.frame();
    &frame.pixels[..frame.width * frame.height]
}

/// Write [`backdrop_rom`] to `path` and hand the path back.
fn write_backdrop_rom(path: &Path) -> PathBuf {
    std::fs::write(path, backdrop_rom()).expect("write the fake ROM");
    path.to_path_buf()
}

/// The `move.w #$value, (addr).L` of `boot.rs`: `33FC imm16 addr32`, checked with
/// `m68k::disassemble` (M68000UM section 8 "MOVE").
fn store_word(addr: u32, value: u16) -> Vec<u16> {
    let word = value.to_be_bytes();
    let addr = addr.to_be_bytes();
    vec![
        0x33FC,
        u16::from_be_bytes([word[0], word[1]]),
        u16::from_be_bytes([addr[0], addr[1]]),
        u16::from_be_bytes([addr[2], addr[3]]),
    ]
}

/// A VDP register command through `$C00004`: `100r rrrr dddd dddd`
/// (genvdp.txt section 7 "Programming VDP registers").
fn vdp_register(reg: u8, value: u8) -> Vec<u16> {
    store_word(
        VDP_CONTROL,
        0x8000 | (u16::from(reg) << 8) | u16::from(value),
    )
}

/// `move.b (addr).L, (dest).L` forever: a program that samples an I/O register
/// over and over. `MOVE.B (xxx).L, (yyy).L` is `13F9 src32 dst32` and the
/// `BRA.B` sits ten bytes into the loop, so it steps back twelve
/// (M68000UM section 8 "MOVE" and "BRA").
fn sample_forever(addr: u32, dest: u32) -> Vec<u16> {
    let src = addr.to_be_bytes();
    let dst = dest.to_be_bytes();
    vec![
        0x13F9,
        u16::from_be_bytes([src[0], src[1]]),
        u16::from_be_bytes([src[2], src[3]]),
        u16::from_be_bytes([dst[0], dst[1]]),
        u16::from_be_bytes([dst[2], dst[3]]),
        0x60F4,
    ]
}

/// Lay `program` at [`CODE`] under the reset vectors for [`STACK`] / [`CODE`].
fn rom_with_program(program: &[u16]) -> Vec<u8> {
    let mut rom = vec![0_u8; CODE as usize + 2 * program.len()];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = CODE as usize + 2 * index;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom
}

/// A cartridge image whose program asks the VDP for a solid red backdrop and then
/// waits: register 7 to make CRAM entry [`BACKDROP_ENTRY`] the backdrop,
/// register 1 with the display on, a CRAM write command for that entry, the
/// colour to the data port, and `bra.b $` (M68000UM section 8 "BRA").
fn backdrop_rom() -> Vec<u8> {
    /// R1 bit 6 is the display enable (genvdp.txt section 17 "$01").
    const DISPLAY_ON: u8 = 0x40;

    let mut program = vdp_register(7, BACKDROP_ENTRY as u8);
    program.extend(vdp_register(1, DISPLAY_ON));
    // The CRAM write command for our entry's byte address: code `0011` is CRAM
    // write, with CD1-CD0 in the first half and CD5-CD2 in the second, and the
    // 64 CRAM words live at byte addresses `$00`-`$7E` (genvdp.txt section 7
    // "table of code register settings" and section 9 "CRAM").
    let cram_addr = (BACKDROP_ENTRY * 2) as u16;
    program.extend(store_word(VDP_CONTROL, 0xC000 | (cram_addr & 0x3FFF)));
    program.extend(store_word(VDP_CONTROL, (cram_addr >> 14) & 0x0003));
    program.extend(store_word(VDP_DATA, BACKDROP_CRAM));
    program.push(0x60FE);
    rom_with_program(&program)
}

/// The fake ROM of [`backdrop_rom`] really does fill the screen with one colour,
/// which is what makes the golden below worth having: the harness only asserts a
/// hash, and this is the test that says the hash is of a picture rather than of
/// noise (`docs/TESTING.md`: "A golden that matches garbage is worse than none").
///
/// The colour is the CRAM entry the ROM wrote, so this is not the all-zero frame
/// a machine that never ran its ROM would show.
#[test]
fn backdrop_rom_is_a_solid_backdrop_colour() {
    let dir = TempDir::new("backdrop-solid");
    let machine = run_frames(&write_backdrop_rom(&dir.join("backdrop.md")), FRAMES, &[]);

    let pixels = active_pixels(&machine);
    assert_eq!(
        pixels[0], BACKDROP_PIXEL,
        "the backdrop the ROM asks for is full red, not {:#010x}",
        pixels[0]
    );
    assert!(
        pixels.iter().all(|&p| p == BACKDROP_PIXEL),
        "the fake ROM's {} x {} frame is not one colour",
        machine.frame().width,
        machine.frame().height
    );
}

/// The harness runs a ROM, dumps a PNG for the human and compares the frame hash
/// with the golden file: the three-line regression every later ROM test is built
/// out of.
#[test]
fn harness_runs_fake_rom_and_matches_golden() {
    let dir = TempDir::new("backdrop-golden");
    let rom = write_backdrop_rom(&dir.join("backdrop.md"));

    run_and_check(RomRun {
        rom,
        frames: FRAMES,
        inputs: &[],
        golden_file: GOLDEN_FILE,
        key: GOLDEN_KEY,
    });

    // The picture is on disk as well, because a golden is only reviewable when a
    // person can look at what its hash is the hash of.
    assert!(golden_png(GOLDEN_FILE, GOLDEN_KEY).is_file());
}

/// The input script reaches the pads on the frames it names: a run with no input
/// samples a resting pad and a run that holds `LEFT+B` on its last frame samples
/// the two lines pulled low.
///
/// Checked at the machine rather than at a golden because the point is the
/// harness's driving, not the picture; the fake ROM of [`backdrop_rom`] ignores
/// its pad, so this one samples port 1 into work RAM the way `boot.rs` does.
#[test]
fn harness_inputs_reach_the_machine() {
    let dir = TempDir::new("backdrop-inputs");
    let rom = dir.join("sampler.md");
    std::fs::write(&rom, rom_with_program(&sample_forever(0xA1_0003, PAD_COPY)))
        .expect("write the sampler ROM");

    let idle = run_frames(&rom, 2, &[]);
    assert_eq!(
        idle.bus().ram[PAD_COPY as usize & 0xFFFF],
        PADS_AT_REST,
        "a run with no input holds nothing"
    );

    // LEFT is pin bit 2 and B is pin bit 4 (segaretro, same table), and a held
    // line is low. Frame numbers are 1-based, so this is the last frame of the
    // run: the sample the program leaves in RAM is the one taken with the pad
    // held.
    let held = run_frames(&rom, 2, &[(2, Buttons::LEFT | Buttons::B)]);
    assert_eq!(
        held.bus().ram[PAD_COPY as usize & 0xFFFF],
        PADS_AT_REST & !0x04 & !0x10,
        "LEFT+B on frame 2 reaches the program"
    );
}

/// An unrecorded golden fails the test - and only after the PNG is on disk, so
/// the person chasing the failure has the picture to read (the ticket's "dump the
/// PNG BEFORE asserting").
///
/// This is the harness discriminating, which is the property a golden harness
/// most easily gets wrong: a run that asserts nothing still looks green.
#[test]
fn harness_dumps_png_before_asserting_the_golden() {
    let dir = TempDir::new("backdrop-unrecorded");
    let rom = write_backdrop_rom(&dir.join("backdrop.md"));
    let png = golden_png("rom-harness-no-such-suite", "no-such-key");
    let _ = std::fs::remove_file(&png);

    let outcome = std::panic::catch_unwind(|| {
        run_and_check(RomRun {
            rom: rom.clone(),
            frames: FRAMES,
            inputs: &[],
            golden_file: "rom-harness-no-such-suite.toml",
            key: "no-such-key",
        });
    });

    assert!(
        outcome.is_err(),
        "a golden key that is not recorded must fail the test, not pass it"
    );
    assert!(
        png.is_file(),
        "{} was not written: the PNG has to be dumped before the golden is \
         asserted, so a failing run always leaves a picture",
        png.display()
    );
    let _ = std::fs::remove_file(&png);
}

/// A missing ROM is a skip, never a failure: CI has no commercial ROMs, and a
/// harness that panicked there would turn an absent cartridge into a red build
/// (`docs/TESTING.md`, "External test data").
#[test]
fn harness_skips_when_rom_missing() {
    let dir = TempDir::new("missing-rom");
    let absent = dir.join("definitely-not-a-rom.md");
    assert!(!absent.exists());

    // Returns without panicking, after printing `SKIPPED:` (visible on a
    // `--nocapture` run of this test).
    run_and_check(RomRun {
        rom: absent.clone(),
        frames: FRAMES,
        inputs: &[(1, Buttons::START)],
        golden_file: GOLDEN_FILE,
        key: GOLDEN_KEY,
    });

    // The same lookup the run goes through, so the skip is that decision rather
    // than a second opinion: absent data, not unreadable data.
    assert_eq!(read_rom(&absent), None);
}
