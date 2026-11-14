//! The `Genesis` API of docs/ARCHITECTURE.md, from the outside: `new` and its
//! errors, `run_frame` and the frame it leaves behind, and `set_pad` reaching the
//! game through the emulated controller port (M3-15, #76).
//!
//! Everything here but the last test runs on a fake ROM - a few words of 68000
//! code built by hand from M68000UM section 8 - so that the machine, not a
//! cartridge, is what is under test. The last one is a real game, is `#[ignore]`d
//! because CI has no commercial ROMs, and is the only place any of this is
//! proved against software nobody wrote for it.

use genesis::{Buttons, Config, Genesis, LoadError};
use testkit::require_data;

/// Where the fake ROMs put their program: past the 0x200 bytes a load needs, so
/// [`genesis::cart::parse_header`] accepts the image.
const CODE: u32 = 0x1000;
/// The reset stack pointer the fake ROMs ask for: the top of work RAM.
const STACK: u32 = 0x00FF_0000;
/// The 68000's VDP control port (plutiedev "Memory map").
const VDP_CONTROL: u32 = 0xC0_0004;
/// Where the pad test's program copies port 1's data register to. The cartridge
/// area is read-only, so it has to be work RAM; 0xFF0000 is the last mirror of
/// the 64 KiB at 0xE00000 (plutiedev "Memory map").
const PAD_COPY: u32 = 0xFF_0000;
/// The V counter of the first line of VBlank: line 224, the `E0h` of genvdp.txt
/// section 4 "Vertical interrupts" ("a level 6 interrupt will occur when the V
/// counter is at line E0h"), counting as itself because the NTSC counter counts
/// one per line up to 255 (`crates/vdp/src/hv.rs`, `v_count`).
const VBLANK_V_COUNTER: u16 = 0xE0;

/// The V counter of the beam right now, as a program reads it: `(V << 8) | H`
/// from `$C00008` (genvdp.txt section 5), and the V half is the line.
fn v_counter(machine: &Genesis) -> u16 {
    machine.bus().vdp.hv_counter() >> 8
}

/// A cartridge image with the reset table for [`STACK`] / [`CODE`] and `program`
/// at [`CODE`].
fn fake_rom(program: &[u16]) -> Vec<u8> {
    let need = CODE as usize + 2 * program.len();
    let mut rom = vec![0_u8; need.max(0x200)];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (word, at) in program.iter().zip(0..) {
        let at = CODE as usize + 2 * at;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom
}

/// `move.w #$value, (addr).L`. `MOVE.W #imm, (xxx).L` is `33FC imm32 addr32` -
/// MOVE's destination field is the swapped one, per
/// `crates/m68k/src/ops/move_.rs`'s module docs - and `m68k::disassemble` is what
/// checked the encoding.
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

/// The same at the VDP's control port: one register command, which is how every
/// real program sets the chip up (plutiedev "VDP registers").
fn vdp_command(value: u16) -> Vec<u16> {
    store_word(VDP_CONTROL, value)
}

/// `move.b (addr).L, (dest).L`, then `bra.b` back over it: a program that samples
/// an I/O register once per pass forever. `MOVE.B (xxx).L, (yyy).L` is `13F9
/// src32 dst32`; the `BRA.B` sits ten bytes into the loop and so steps back
/// twelve, negated (M68000UM section 8 "MOVE" and "BRA").
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

/// [`Buttons`] as the pins of a 3-button pad at rest read them: every line high,
/// and each held button pulling its own low (segaretro "Control Pad (Mega Drive)",
/// "Technical information").
fn pins(buttons: Buttons) -> u8 {
    let mut pins = 0x7F;
    for (bit, button) in [
        (0, Buttons::UP),
        (1, Buttons::DOWN),
        (2, Buttons::LEFT),
        (3, Buttons::RIGHT),
        (4, Buttons::B),
        (5, Buttons::C),
    ] {
        if buttons.contains(button) {
            pins &= !(1 << bit);
        }
    }
    pins
}

/// A ROM too small to hold the header at 0x100 is not a Genesis program, and the
/// error says so rather than the loader panicking.
#[test]
fn new_rejects_tiny_rom() {
    let rom = vec![0xAA_u8; 0x1FF];
    assert_eq!(
        Genesis::new(&rom, Config::default()).err(),
        Some(LoadError::TooSmall)
    );

    // And the empty file, which is the same case arrived at from the other end.
    assert_eq!(
        Genesis::new(&[], Config::default()).err(),
        Some(LoadError::TooSmall)
    );
}

/// A loaded machine has run the 68000's own reset: `SSP` from `$000000` and `PC`
/// from `$000004` (M68000UM section 5 "Reset"), so the first instruction the
/// first frame executes is the cartridge's first instruction and not zero.
#[test]
fn new_sets_pc_from_vector() {
    let rom = fake_rom(&[0x4E71]); // `NOP`, so the PC is still there to look at
    let machine = Genesis::new(&rom, Config::default()).expect("a ROM with a header loads");
    assert_eq!(machine.cpu().pc, CODE, "PC came from the long at $000004");
    assert_eq!(machine.cpu().ssp, STACK, "and SSP from the one at $000000");
}

/// The beam of a machine that has just been built is on the first line of VBlank,
/// so the first thing a ROM does - write a full set of VDP registers, at the top
/// of its start-up code - happens during blanking and is in time for line 0 of the
/// first frame (#310).
///
/// `Vdp::new` leaves a chip at line 0, and a line is drawn from the state latched
/// at the start of its active display - master clock 100 in H32 and 81 in H40
/// (#102) - so a beam that starts there puts the whole of a ROM's init code 40
/// clocks inside line 0 and one frame behind. [`run_frame_returns_frame_of_expected_size`]
/// is the mode change this is noticed in.
#[test]
fn new_starts_the_beam_in_vblank() {
    let machine =
        Genesis::new(&fake_rom(&[0x4E71]), Config::default()).expect("a ROM with a header loads");

    assert_eq!(
        v_counter(&machine),
        VBLANK_V_COUNTER,
        "the V counter of a machine before its first frame is the first line of VBlank"
    );
}

/// The reset button restarts the beam in VBlank with the 68000 and the VDP, so the
/// frame after a reset is drawn in the mode the program asks for at the top of it
/// rather than in the mode the freshly-reset VDP sits in (#310).
#[test]
fn reset_restarts_the_beam_in_vblank() {
    // R1 = $40 turns the display on and R12 = $81 is H40, so there is a mode for
    // the reset to lose and the program's re-run to win back (genvdp.txt section
    // 17 "$01" and "$0C").
    let mut program = vdp_command(0x8000 | (0x01 << 8) | 0x40);
    program.extend(vdp_command(0x8000 | (0x0C << 8) | 0x81));
    program.push(0x60FE); // `bra.b $`

    let mut machine =
        Genesis::new(&fake_rom(&program), Config::default()).expect("a ROM with a header loads");
    // Two frames, so this starts from the H40 the program asks for either way and
    // only the reset that follows is under test.
    machine.run_frame();
    machine.run_frame();
    assert_eq!(machine.frame().width, 320, "H40 before the reset");

    machine.reset();
    machine.run_frame();

    assert_eq!(
        machine.frame().width,
        320,
        "the re-run program writes R12 during blanking, so the first frame after a reset is the H40 it asked for and not the H32 the VDP reset into"
    );
}

/// One `run_frame` is one frame of the picture, and `frame` reports the geometry
/// the VDP's mode asks for: 320x224 in H40 and 256x224 in H32, so a front end
/// never draws a 320 wide row into a 256 wide frame. The geometry is the mode of
/// the frame's first line, and the console starts that first line in blanking
/// (#310), so a mode asked for at boot is in the very first frame; the test walks
/// a second frame anyway, because a mode a game holds has to survive a whole
/// raster of it.
#[test]
fn run_frame_returns_frame_of_expected_size() {
    // R1 = $40 turns the display on, R12 = $81 is H40 (RS0 and RS1) and R12 =
    // $00 the 32-cell display (genvdp.txt section 17 "$01" and "$0C").
    for (reg12, width) in [(0x81_u8, 320_usize), (0x00, 256)] {
        let mut program = vdp_command(0x8000 | (0x01 << 8) | 0x40);
        program.extend(vdp_command(0x8000 | (0x0C << 8) | u16::from(reg12)));
        program.push(0x60FE); // `bra.b $`, and the machine settles

        let mut machine = Genesis::new(&fake_rom(&program), Config::default())
            .expect("a ROM with a header loads");
        machine.run_frame();

        // The frame a mode is asked for in is the frame that shows it, because the
        // beam is in blanking while it is asked: `Genesis::new` parks it on the
        // first line of VBlank (#310), so this program's write to R12 lands well
        // before the latch of line 0 - the state a line is drawn from is taken at
        // the start of its active display, master clock 100 in H32 and 81 in H40
        // (M4-23, #102) - and line 0, which fixes the geometry of the frame, is
        // drawn in the mode asked for.
        //
        // This assertion read `(256, 224)` until #310: with a beam that started at
        // line 0 the same write landed at master clock 140 of line 0, 40 clocks
        // past that latch, and the first frame of every run came up in the mode the
        // VDP reset into. The second frame below no longer tells the two apart; it
        // is kept because a mode has to survive a whole raster of it to be the
        // display a front end draws.
        assert_eq!(
            (machine.frame().width, machine.frame().height),
            (width, 224),
            "R12 = {reg12:#04x} is written during blanking, so frame 0 is already {width} wide"
        );

        machine.run_frame();

        let frame = machine.frame();
        assert_eq!(
            (frame.width, frame.height),
            (width, 224),
            "R12 = {reg12:#04x} is still a {width} x 224 display a whole frame later"
        );

        // The frame buffer keeps room for the biggest picture the VDP can make,
        // so what proves the geometry is that the active area - and only it - was
        // drawn: every active row holds `width` backdrop pixels, and the first
        // line past the active area is still untouched.
        let backdrop = 0xFF00_0000;
        for y in [0, frame.height / 2, frame.height - 1] {
            let row = &frame.pixels[y * width..y * width + width];
            assert!(
                row.iter().all(|&p| p == backdrop),
                "line {y} of a {} x {} frame is drawn end to end",
                frame.width,
                frame.height
            );
        }
        assert_eq!(
            frame.pixels[frame.height * width],
            0,
            "and nothing past the last active line was painted"
        );
    }
}

/// `set_pad` is visible to the program: the byte it reads from port 1's data
/// register is the pad this call attached, and changing it changes the read on
/// the very next frame.
#[test]
fn set_pad_visible_to_game() {
    // `move.b ($A10003).L, ($FF0000).L` forever: work RAM holds port 1's pin
    // levels, sampled again and again (plutiedev "I/O ports", `IoData1`).
    let mut machine = Genesis::new(
        &fake_rom(&sample_forever(0xA1_0003, PAD_COPY)),
        Config::default(),
    )
    .expect("a ROM with a header loads");

    // Nothing attached yet, so every line is high: a game may use that to find
    // out that the port is empty (plutiedev "Controllers", "Checking if there's
    // a controller").
    machine.run_frame();
    assert_eq!(machine.bus().ram[PAD_COPY as usize & 0xFFFF], 0x7F);

    for buttons in [
        Buttons::UP | Buttons::C,
        Buttons::empty(),
        Buttons::DOWN | Buttons::LEFT | Buttons::RIGHT | Buttons::B,
    ] {
        machine.set_pad(0, buttons);
        machine.run_frame();
        assert_eq!(
            machine.bus().ram[PAD_COPY as usize & 0xFFFF],
            pins(buttons),
            "{buttons:?} reaches the program through the port"
        );
    }
}

/// The reset button, by the three things it is allowed to change: the 68000 goes
/// back to its own reset vector, the VDP goes back to its power-on registers, and
/// work RAM does not go anywhere - a reset is not a power cycle, and games leave
/// variables there on purpose. See [`Genesis::reset`] for what each of the three
/// cites.
#[test]
fn reset_restarts_the_68k_and_the_vdp_and_keeps_work_ram() {
    // One store into work RAM, one VDP register command, then a wait in place:
    // after a frame there is something to see in each of the three the reset
    // button is allowed to change.
    let mut program = store_word(PAD_COPY, 0x1234);
    program.extend(vdp_command(0x8000 | (0x0C << 8) | 0x81));
    program.push(0x60FE);

    let mut machine =
        Genesis::new(&fake_rom(&program), Config::default()).expect("a ROM with a header loads");
    machine.run_frame();
    assert_eq!(
        machine.bus().ram[0..2],
        [0x12, 0x34],
        "the program wrote work RAM"
    );
    assert_eq!(machine.bus().vdp.reg(12), 0x81, "and the VDP's register");

    machine.reset();

    assert_eq!(
        machine.cpu().pc,
        CODE,
        "the 68k is back at the vector it was reset from"
    );
    assert_eq!(
        machine.bus().vdp.reg(12),
        0,
        "the VDP went back to its power-on registers with the rest of the bus"
    );
    assert_eq!(
        machine.bus().ram[0..2],
        [0x12, 0x34],
        "and work RAM stayed where it was: a reset is not a power cycle"
    );
}

/// The audio path answers with a frame of host audio.
///
/// `drain_audio` used to be one of the API's stubs and asserted here as
/// appending nothing; M5-14 (#119) filled it in, and what it has to append is a
/// frame of host audio - 801 stereo samples for one 16.7 ms NTSC raster at
/// 48 kHz. The save-state stubs this test used to assert alongside it are a
/// format now (M6-07, #127) and have a test file of their own,
/// `tests/save_state.rs`.
#[test]
fn drain_audio_mixes_a_frame_of_host_audio() {
    let mut machine = Genesis::new(&fake_rom(&[0x60FE]), Config::default()).expect("loads");

    let mut samples = vec![1_i16; 8];
    machine.drain_audio(&mut samples);
    assert_eq!(
        samples, [1_i16; 8],
        "no frame has run, so there is nothing to mix yet"
    );

    machine.run_frame();
    machine.drain_audio(&mut samples);
    let mixed = samples.len() - 8;
    assert!(
        mixed > 1_400 && mixed.is_multiple_of(2) && mixed < 2_000,
        "one NTSC raster mixed {mixed} halves, which is not about 801 stereo samples"
    );
}

/// The real thing: Sonic 1 runs, the VDP draws it, and nothing panics on the way.
///
/// Local only - CI has no commercial ROMs, so this skips there. Run it with
/// `cargo nextest run -p genesis --run-ignored only --test boot`.
#[test]
#[ignore = "needs a commercial ROM; skips without one"]
fn sonic_boots_to_non_blank_screen() {
    let path = require_data!(testkit::commercial_rom(
        "Sonic The Hedgehog (USA, Europe).md"
    ));
    let rom = std::fs::read(&path).expect("Sonic is readable");
    let mut machine = Genesis::new(&rom, Config::default()).expect("Sonic loads");

    for _ in 0..300 {
        machine.run_frame();
    }

    // What this can prove today is that the game reached the chip: 300 frames of
    // Sonic write its title art into VRAM and its palette into CRAM. A picture is
    // not on the table yet, because the VDP's renderer is the backdrop-only stub
    // M4-06 (#84) left behind and real line rendering is M4-17 (#95): `frame()`
    // of any ROM at all is one colour however much the game has drawn.
    let vdp = &machine.bus().vdp;
    assert!(
        vdp.vram.iter().any(|&byte| byte != 0),
        "300 frames in, Sonic has still written nothing to VRAM"
    );
    assert!(
        vdp.cram.iter().any(|&word| word != 0),
        "and nothing to CRAM: the palette it is drawing with never arrived"
    );

    let frame = machine.frame();
    // TODO(M4-17 #95): the screen, once `Vdp::render_line` renders a line. Until
    // then this is the check the ticket was written with, in place and off:
    //
    // let colours: std::collections::HashSet<u32> =
    //     frame.pixels[..frame.width * frame.height].iter().copied().collect();
    // assert!(colours.len() > 4, "the screen is only {} colours", colours.len());

    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-output/sonic_300.png");
    testkit::dump_png(
        &out,
        &frame.pixels[..frame.width * frame.height],
        frame.width,
        frame.height,
    );
}

/// Boots one Realtec cart for `frames` and returns how many distinct colours its
/// last frame holds.
///
/// A Realtec cart is paged over its own image, so this is the test that the mapper
/// is doing something: a cart on the wrong paging reads its own data as a
/// different set of bytes and draws nothing, or draws noise.
///
/// `#[ignore]`d with the other real-ROM tests: CI has no commercial ROMs. Run with
/// `cargo nextest run -p genesis --run-ignored only --test boot`.
///
/// `require_data!` is not used here because it leaves the test early, and this is a
/// helper rather than a test: each test asks for its own ROM and passes it in.
fn realtec_colours(path: &std::path::Path, frames: u32) -> usize {
    colour_count(path, frames, "realtec_prefix.png")
}

/// Boots `path` for `frames` and returns how many distinct colours its last frame
/// holds, writing that frame to `target/test-output/<name>`.
///
/// The body of [`realtec_colours`], split out so that a cart outside the Realtec
/// family can be measured the same way - the number is the point, and a
/// before/after pair of numbers is what settles whether a cart-side change helped.
fn colour_count(path: &std::path::Path, frames: u32, name: &str) -> usize {
    use std::collections::HashSet;

    let rom = std::fs::read(path).expect("the cart is readable");
    let mut machine = Genesis::new(&rom, Config::default()).expect("the cart loads");

    for _ in 0..frames {
        machine.run_frame();
    }

    let frame = machine.frame();
    let pixels = &frame.pixels[..frame.width * frame.height];
    let colours: HashSet<u32> = pixels.iter().copied().collect();
    testkit::dump_png(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-output/")
            .join(name),
        pixels,
        frame.width,
        frame.height,
    );
    colours.len()
}

/// A blank screen is 4 distinct colours or fewer, the same floor
/// `crates/genesis/tests/commercial.rs` uses and the one `headless sweep` calls
/// `Blank` with (M6-05, #124).
const BLANK: usize = 4;

/// Earth Defense reaches its title screen after the 300-frame mark, so it is run to
/// 600 - the checkpoint `headless sweep` itself uses (docs/compat.md). At 300 it is
/// a 3-colour frame, and the cart is not the reason: the same is true of official
/// carts in the same sweep.
#[test]
#[ignore = "needs a commercial ROM; skips without one"]
fn earth_defense_boots_to_non_blank_screen() {
    let path = require_data!(testkit::find_commercial_rom("Earth Defense"));
    let colours = realtec_colours(&path, 600);
    assert!(
        colours > BLANK,
        "600 frames in, Earth Defense is {} distinct colours",
        colours
    );
}

/// Funny World & Balloon Boy, the cart whose colour count the paging raises rather
/// than rescues: 26 colours without the mapper, 31 with it.
#[test]
#[ignore = "needs a commercial ROM; skips without one"]
fn funny_world_and_balloon_boy_boots_to_non_blank_screen() {
    let path = require_data!(testkit::find_commercial_rom("Funny World & Balloon Boy"));
    let colours = realtec_colours(&path, 300);
    assert!(
        colours > BLANK,
        "300 frames in, Funny World & Balloon Boy is {} distinct colours",
        colours
    );
}

/// Whac-a-Critter: the clearest single-cart effect of the paging, 7 colours before
/// and 22 after.
#[test]
#[ignore = "needs a commercial ROM; skips without one"]
fn whac_a_critter_boots_to_non_blank_screen() {
    let path = require_data!(testkit::find_commercial_rom("Whac-a-Critter"));
    let colours = realtec_colours(&path, 300);
    assert!(
        colours > BLANK,
        "300 frames in, Whac-a-Critter is {} distinct colours",
        colours
    );
}

/// Tom Clown, the cart the mapper unbreaks: on `main` it panics inside 600 frames
/// with `not implemented: m68k opcode 0AC6`, and paged correctly it reaches a
/// 30-colour screen instead.
///
/// The opcode is not implemented, and #355 stays open: what the paging changes is
/// that the cart stops executing the bytes that held it.
#[test]
#[ignore = "needs a commercial ROM; skips without one"]
fn tom_clown_boots_to_non_blank_screen() {
    let path = require_data!(testkit::find_commercial_rom("Tom Clown"));
    let colours = realtec_colours(&path, 300);
    assert!(
        colours > BLANK,
        "300 frames in, Tom Clown is {} distinct colours",
        colours
    );
}

/// Virtua Racing, the one cart of the collection that carries the SVP: a Fujitsu
/// SSP1601 DSP with its own program ROM and 2 KB of DRAM, which the 68k talks to
/// through `0xA15000`-`0xA15009` and `0x300000`-`0x31FFFF` (notaz *SVP doc*
/// section 3 "Memory map").
///
/// Nothing in this tree models the SVP, so the cart waits on a DSP that never
/// answers and the frame stays `#000000` however long it runs - `1 colours` in
/// docs/compat.md, and the only two carts of the 2775 that never draw a pixel
/// (#374). This test therefore **fails on `main` and is `#[ignore]`d** for that
/// reason, not because the ROM is missing: it is the measurement the SVP work is
/// judged on, and un-ignoring it is the acceptance criterion of #374.
///
/// Run it with
/// `cargo nextest run -p genesis --run-ignored only --test boot virtua_racing`.
#[test]
#[ignore = "needs a commercial ROM, and #374: the SVP is unemulated, so this fails on main"]
fn virtua_racing_boots_to_non_blank_screen() {
    let path = require_data!(testkit::find_commercial_rom("Virtua Racing (USA)"));
    let colours = colour_count(&path, 300, "virtua_racing_300.png");
    assert!(
        colours > BLANK,
        "300 frames in, Virtua Racing is {} distinct colours: the cart is waiting on \
         its SVP DSP, which nothing here models (#374)",
        colours
    );
}

/// The four carts #375 names, one `#[ignore]`d boot test each, so that "one cause or
/// four?" is answered by a test run rather than by an afternoon at a disassembler.
///
/// **All four fail on `main`, and are `#[ignore]`d for that reason** and not because
/// the ROM is missing - the same shape as
/// [`virtua_racing_boots_to_non_blank_screen`] above. They are landed before the
/// fixes, as the measurement the fixes get judged on; each assertion names the cause
/// the cart was diagnosed into, and every number quoted is this tree's
/// `headless run <rom> --frames 1800 --colours --max-colours` output.
///
/// They ask for their ROM by exact file name through [`testkit::commercial_rom`]
/// rather than by prefix through `find_commercial_rom`, because prefix matching takes
/// the first name in sorted order and the collection holds a `QuackShot ... (Rev A)
/// (Alt)` image that sorts before the `(Rev A)` image under test - and the Alt is a
/// complete dump that boots, so a prefix would test the wrong cart and pass.
///
/// Run one with
/// `cargo nextest run -p genesis --run-ignored only --test boot <name>`.
///
/// `QuackShot (Rev A)`: the 68000 never gets as far as the VDP because the cart's own
/// boot self-test fails, and it fails because the image is not complete. The header
/// declares a ROM size of `0x13FFFF` - 1.25 MB - and the file is `0x80000`, so the
/// self-test's pass over the mirrored upper half reads the `0xFF` filler above the end
/// of the image (`Bus::read_rom_byte`) and sums wrong; the cart then parks on
/// `NOP; NOP; BRA.S $200` from frame 30 on, with R1 bit 6 clear and 0 of 64 CRAM words
/// written. The `(Alt)` dump of the same revision is `0x140000`, verifies, and draws 33
/// colours; so does the unmodified `-00` image, which is also `0x80000` but declares a
/// matching `0x07FFFF`. Filed as
/// [#435](https://github.com/scottjab/rustsega/issues/435).
#[test]
#[ignore = "needs a commercial ROM, and #435: this dump is short of the size its header declares, so the cart's own ROM check fails and it halts before the VDP"]
fn quackshot_rev_a_boots_to_non_blank_screen() {
    let path = require_data!(testkit::commercial_rom(
        "QuackShot Starring Donald Duck (World) (En,Ja) (Rev A).md"
    ));
    let colours = colour_count(&path, 300, "quackshot_rev_a_300.png");
    assert!(
        colours > BLANK,
        "300 frames in, QuackShot (Rev A) is {colours} distinct colours: the image declares \
         a ROM size past its own end, the boot self-test fails on the filler read there, and \
         the 68000 halts at $200 without touching the VDP (#435)"
    );
}

/// `Krusty's Super Fun House (USA, Europe)`: the same colour count, a different cause.
/// The display is enabled and 2348 VRAM bytes are loaded, but 0 of 64 CRAM words are
/// ever written, and the cart reports region `usa`. Forcing `--region europe` draws 30
/// colours where `usa` and `japan` both stay at 1 - so this cart takes one of two
/// branches off a read of the version register at `0xA10001` and only the PAL branch
/// finishes. Filed as [#438](https://github.com/scottjab/rustsega/issues/438).
#[test]
#[ignore = "needs a commercial ROM, and #438: its start-up does not finish in the region its header reports, so it never loads a palette; forced to PAL it draws"]
fn krustys_super_fun_house_boots_to_non_blank_screen() {
    let path = require_data!(testkit::commercial_rom(
        "Krusty's Super Fun House (USA, Europe).md"
    ));
    let colours = colour_count(&path, 300, "krustys_super_fun_house_300.png");
    assert!(
        colours > BLANK,
        "300 frames in, Krusty's Super Fun House is {colours} distinct colours: the display \
         is on and CRAM is empty, and the same image forced to --region europe draws 30 (#438)"
    );
}

/// `Time Killers (USA)`: the 68000 is stopped, but by a hardware read rather than by
/// itself. All 1800 frames are one three-instruction loop - `MOVE.W $A11100,D0` /
/// `BTST #$0,D0` / `BNE.S $40E` - which is the plutiedev "Using the Z80" poll between
/// asking the sound CPU for its bus and touching the sound side. The flag is bit 0 of
/// that read, and `Bus::read_z80_ctrl_word` answers a *word* read with the flag in bit
/// 8 and a fixed `1` in bit 0 whether or not the Z80 has been stopped, so the
/// condition the cart is waiting on is never reported. Filed as
/// [#437](https://github.com/scottjab/rustsega/issues/437).
#[test]
#[ignore = "needs a commercial ROM, and #437: it spins for ever on bit 0 of the word read of 0xA11100, which our bus-request read-back holds at 1"]
fn time_killers_boots_to_non_blank_screen() {
    let path = require_data!(testkit::commercial_rom("Time Killers (USA).md"));
    let colours = colour_count(&path, 300, "time_killers_300.png");
    assert!(
        colours > BLANK,
        "300 frames in, Time Killers is {colours} distinct colours: it polls bit 0 of the \
         word at $A11100 for the Z80 to release its bus, and that bit never reads 0 here (#437)"
    );
}

/// `Lemmings 2 - The Tribes (USA)`: the one of the four with no cause established yet.
/// The 68000 is running - `pc` moves and the cycle counter advances - the display is
/// enabled and 2348 VRAM bytes are loaded, but 0 CRAM words are written across 1800
/// frames and `pc` is at `$00125E` at 1789 of them. Region forcing does not move it and
/// it is not waiting on a register, so it is carried on the inventory of measured carts
/// with no cause, [#440](https://github.com/scottjab/rustsega/issues/440). The test is
/// landed anyway, so that whatever the cause turns out to be arrives with a guard
/// already attached to this cart.
#[test]
#[ignore = "needs a commercial ROM, and #440: measured to the frame boundary with no cause established, so this fails on main"]
fn lemmings_2_tribes_boots_to_non_blank_screen() {
    let path = require_data!(testkit::commercial_rom("Lemmings 2 - The Tribes (USA).md"));
    let colours = colour_count(&path, 300, "lemmings2_tribes_300.png");
    assert!(
        colours > BLANK,
        "300 frames in, Lemmings 2 - The Tribes is {colours} distinct colours: display on, \
         VRAM loaded, CRAM never written, cause not established (#440)"
    );
}

/// Boots `path`, runs it for `warm` frames, presses the console's Reset button,
/// then runs `after` more frames.
///
/// Returns the distinct-colour count of the **first** frame after the reset and of
/// the **last** one, in that order, and writes the last frame to
/// `target/test-output/<name>`.
///
/// The pair is what #382 is about: a reset clears the VDP's memories, so the frame
/// it restarts the cart into is blank, and the cart has to repaint from there. A
/// reset that leaves the VDP unable to raise `frame_done` never gets past that
/// first frame at all - `Scheduler::run_frame`'s own guard panics inside the loop
/// below and the second number is never produced.
///
/// Why the guard used to fire, measured on `ddd9eb0` (the commit before #371) with
/// the beam printed at every line of these two carts: [`vdp::Vdp::start_in_vblank`]
/// parks a reset chip on the line `frame_done` is raised on, at clock 0 of it, and
/// the scheduler's line boundaries are not re-phased to that position - so the
/// scheduler line that ends a raster ends with the beam *inside* line 223 rather
/// than on 224 (measured `line 223, mclk 3390` on Sonic and `line 223, mclk 3418`
/// on Global Gladiators, both a few clocks short of the line end). The VBlank line
/// is therefore one scheduler line further away than the raster, so a frame that
/// starts at a reset can run `lines_per_frame + 1` lines: 263 on NTSC and 314 on
/// PAL, the two numbers #382 quotes, both one past the bound this guard used to
/// carry. #371 is what fixed it - a frame is one VBlank-to-VBlank interval and the
/// bound is one interval plus a raster of slack - so those frames run and stop at
/// the flag. These two tests are the guard that it stays that way, and they are the
/// end-to-end half of it: a cart, a reset, and the frames after it. The mechanism
/// itself is held by the `Scheduler::run_frame` tests, by
/// `crates/genesis/tests/pal_timing.rs`'s `run_frame_spans_a_moved_vblank_line` and
/// by the `vdp::timing` interval tests.
fn post_reset_colours(path: &std::path::Path, warm: u32, after: u32, name: &str) -> (usize, usize) {
    use std::collections::HashSet;

    let rom = std::fs::read(path).expect("the cart is readable");
    let mut machine = Genesis::new(&rom, Config::default()).expect("the cart loads");

    for _ in 0..warm {
        machine.run_frame();
    }
    machine.reset();

    let mut first = 0_usize;
    let mut last = 0_usize;
    for frame in 0..after {
        machine.run_frame();
        let picture = machine.frame();
        let pixels = &picture.pixels[..picture.width * picture.height];
        let colours = pixels.iter().copied().collect::<HashSet<u32>>().len();
        if frame == 0 {
            first = colours;
        }
        last = colours;
    }

    let picture = machine.frame();
    testkit::dump_png(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-output/")
            .join(name),
        &picture.pixels[..picture.width * picture.height],
        picture.width,
        picture.height,
    );
    (first, last)
}

/// `Sonic The Hedgehog (USA, Europe)`: the Reset button mid-run, on an NTSC cart
/// (#382).
///
/// Three hundred frames is past the point the cart has fully programmed the VDP -
/// the same mark [`colour_count`] uses for the boot tests above - so the reset
/// restarts a cart that is holding the chip in a mode, not one that is still
/// setting it up.
#[test]
#[ignore = "needs a commercial ROM; skips without one"]
fn sonic_survives_a_reset() {
    let path = require_data!(testkit::commercial_rom(
        "Sonic The Hedgehog (USA, Europe).md"
    ));
    let (first, last) = post_reset_colours(&path, 300, 300, "sonic_post_reset.png");
    assert!(
        first <= BLANK,
        "the frame a reset restarts Sonic into is {first} distinct colours: the VDP's memories \
         go with the reset, so this frame should still be blank"
    );
    assert!(
        last > BLANK,
        "300 frames after a reset, Sonic is {last} distinct colours: the cart never repainted \
         the chip it reset, so the machine is stopped rather than restarted (#382)"
    );
}

/// `Global Gladiators (Europe)`: the same reset on a PAL cart, which is the other
/// half of #382 because the raster and the VBlank line it ends on are both
/// different (313 lines, and 240 in V30).
#[test]
#[ignore = "needs a commercial ROM; skips without one"]
fn global_gladiators_survives_a_reset() {
    let path = require_data!(testkit::commercial_rom("Global Gladiators (Europe).md"));
    let (first, last) = post_reset_colours(&path, 300, 300, "global_gladiators_post_reset.png");
    assert!(
        first <= BLANK,
        "the frame a reset restarts Global Gladiators into is {first} distinct colours: the \
         VDP's memories go with the reset, so this frame should still be blank"
    );
    assert!(
        last > BLANK,
        "300 frames after a reset, Global Gladiators is {last} distinct colours: the cart never \
         repainted the chip it reset (#382)"
    );
}
