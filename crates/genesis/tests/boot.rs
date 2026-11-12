//! The `Genesis` API of docs/ARCHITECTURE.md, from the outside: `new` and its
//! errors, `run_frame` and the frame it leaves behind, and `set_pad` reaching the
//! game through the emulated controller port (M3-15, #76).
//!
//! Everything here but the last test runs on a fake ROM - a few words of 68000
//! code built by hand from M68000UM section 8 - so that the machine, not a
//! cartridge, is what is under test. The last one is a real game, is `#[ignore]`d
//! because CI has no commercial ROMs, and is the only place any of this is
//! proved against software nobody wrote for it.

use genesis::{Buttons, Config, Genesis, LoadError, StateError};
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

/// The audio path answers, and the save-state stubs that are still stubs answer
/// with nothing rather than with a `todo!()` panic, so a front end can call them
/// all on a build that has no save states yet.
///
/// `drain_audio` used to be one of the stubs and asserted here as appending
/// nothing; M5-14 (#119) filled it in, and what it has to append is a frame of
/// host audio - 801 stereo samples for one 16.7 ms NTSC raster at 48 kHz.
#[test]
fn drain_audio_mixes_and_the_state_stubs_answer() {
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

    assert_eq!(machine.save_state().err(), Some(StateError::Unsupported));
    assert_eq!(machine.load_state(&[0, 1, 2]), Err(StateError::Unsupported));
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
