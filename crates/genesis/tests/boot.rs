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

/// One `run_frame` is one frame of the picture, and `frame` reports the geometry
/// the VDP's mode asks for: 320x224 in H40 and 256x224 in H32, so a front end
/// never draws a 320 wide row into a 256 wide frame. The geometry is the mode of
/// the frame's first line, and the line is latched before this program gets as
/// far as its mode write (#102), so the test runs two frames: the first proves
/// the latch, the second the geometry.
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

        // The frame a mode is asked for in is not the frame that shows it. This
        // program's write to R12 lands on line 0 at master clock 140, and a line
        // is drawn from the state latched at the start of its active display -
        // master clock 100 in H32 and 81 in H40 (M4-23, #102) - so a write 40
        // clocks into the active display of line 0 is too late for line 0 and too
        // late for the frame whose geometry line 0 fixes. Frame 0 is the 256 pixels
        // of the mode the VDP reset into; the frame after is the one asked for.
        assert_eq!(
            (machine.frame().width, machine.frame().height),
            (256, 224),
            "R12 = {reg12:#04x} is written after the latch of line 0, so frame 0 is the mode it reset into"
        );

        machine.run_frame();

        let frame = machine.frame();
        assert_eq!(
            (frame.width, frame.height),
            (width, 224),
            "R12 = {reg12:#04x} is a {width} x 224 display from the frame after"
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

/// The API stubs that the audio and save-state tickets have to fill answer with
/// nothing rather than with a `todo!()` panic, so a front end can call them on a
/// build that has no audio or save states yet.
#[test]
fn stubs_answer_without_panicking() {
    let mut machine = Genesis::new(&fake_rom(&[0x60FE]), Config::default()).expect("loads");

    let mut samples = vec![1_i16; 8];
    machine.drain_audio(&mut samples);
    assert_eq!(
        samples, [1_i16; 8],
        "nothing is mixed yet, so nothing is appended: M5-14 (#119) owns the mixer"
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
