//! PAL timing: the 313-line raster, the ~49.7 Hz it makes, and the 240-line
//! display that only a PAL chip will scan (M6-12, #132).
//!
//! Three numbers say what a PAL Mega Drive is, and each has its own owner:
//!
//! * the master clock, 53_203_424 Hz, is [`Region::master_clock_hz`];
//! * the raster, 313 lines of 3420 master clocks, is [`genesis::Scheduler`] for
//!   the count and [`vdp::Vdp`] for the beam;
//! * the 240-line display, V30, is R1 bit 3 *and* the chip's PAL pin, which is
//!   [`vdp::Vdp::active_height`].
//!
//! Nothing else in the machine knows the region is different: a line is 3420
//! master clocks in either region, so the 50 Hz of PAL is the 313 lines and the
//! slower crystal and never a second line length. The first two tests measure
//! that instead of reading it back, so a scheduler that counted a different
//! raster from the one the beam drew, or a mixer that paced PAL audio off the
//! NTSC crystal, fails here rather than in someone's living room.

use genesis::{Bus, Config, Genesis, Region, RegionSetting, Scheduler};
use m68k::Bus as _;
use m68k::M68k;
use testkit::require_data;
use z80::Z80;

/// Master clocks in one scanline, in either region (docs/ARCHITECTURE.md,
/// "Clocks and timing").
const MASTER_CLOCKS_PER_LINE: u64 = 3420;
/// Scanlines in one PAL raster.
///
/// Kabuto's hardware notes, "Timing table - General", gives the PAL V counter
/// progression as `0x000-0x102, 0x1CA-0x1FF`: 259 lines plus 54 is 313. The
/// NTSC row of the same table, `0x000-0x0EA, 0x1E5-0x1FF`, is 235 plus 27, and
/// that is the 262 of [`NTSC_LINES`]. `crates/vdp/src/hv.rs` reads the two
/// progressions as the same two jumps.
const PAL_LINES: u64 = 313;
/// Scanlines in one NTSC raster, the control that keeps the PAL number honest.
const NTSC_LINES: u64 = 262;
/// R1 bit 3 (V30): the 240-line display (genvdp.txt section 17 "$01";
/// [`vdp::Vdp::v30_mode`]).
const R1_V30: u8 = 1 << 3;
/// R1 bit 5 (IE0): enable the VINT, whose line number is what
/// [`v30_vint_at_line_240`] measures.
const R1_VINT: u8 = 1 << 5;
/// R1 bit 6 (DISPLAY): on, so the chip is in the state a game runs in.
const R1_DISPLAY: u8 = 1 << 6;
/// Where the fake cartridge puts its program, past the header at 0x100.
const CODE: u32 = 0x1000;
/// Where the counting loop sits in the small image the clock tests use.
const LOOP: u32 = 0x10;
/// The reset stack pointer of the fake cartridge: the top of work RAM.
const STACK: u32 = 0x00FF_0000;
/// The 68000's VDP control port (plutiedev, "Memory map").
const VDP_CONTROL: u32 = 0xC0_0004;
/// `Z80Reset: equ $A11200` (plutiedev, "Using the Z80"): held by [`Bus::new`],
/// released by the loop-counting tests so the line loop has both chips to run,
/// which is what a running console has.
const Z80_RESET: u32 = 0xA11_200;
/// The host rate the audio pacing is measured at.
const SAMPLE_RATE: u32 = 48_000;

/// The 68k's program: `ADDQ.L #$1,D0` (`$5280`) then `BRA.B` back to it
/// (`$60FC`), the loop `crates/genesis/src/scheduler.rs` runs its own timing
/// tests on.
fn counting_loop() -> Vec<u8> {
    let mut rom = vec![0_u8; 0x20];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&LOOP.to_be_bytes());
    rom[0x10..0x12].copy_from_slice(&0x5280_u16.to_be_bytes());
    rom[0x12..0x14].copy_from_slice(&0x60FC_u16.to_be_bytes());
    rom
}

/// The console of the timing tests: `region`'s version register, the beam parked
/// on the first line of VBlank the way [`Genesis::new`] parks it, the 68k in its
/// counting loop and the Z80 released from reset and looping `INC A; JR -3` in
/// sound RAM (`3C 18 FD`), which is the pair of programs
/// `crates/genesis/tests/audio_clock.rs` runs.
fn console(region: Region) -> (M68k, Z80, Bus, Scheduler) {
    let mut bus = Bus::new(counting_loop(), region.version_register(false));
    bus.vdp.start_in_vblank();
    bus.z80.ram[0..3].copy_from_slice(&[0x3C, 0x18, 0xFD]);
    bus.write_word(Z80_RESET, 0x0100);
    let mut cpu = M68k::new();
    cpu.reset(&mut bus);
    let z80 = Z80::new();
    let scheduler = Scheduler::new(region);
    (cpu, z80, bus, scheduler)
}

/// The scanlines and the master clocks of one raster of `region`, counted off
/// the running machine: from the VBlank the parked beam is waiting for to the
/// next one.
///
/// The beam's count is what [`vdp::Vdp::frame_done`] raises on, and the clock is
/// the scheduler's own, so the two numbers are the emulator's answer to
/// "how long is a frame" from the two chips that have to agree about it.
fn measure_raster(region: Region) -> (u64, u64) {
    let (mut cpu, mut z80, mut bus, mut scheduler) = console(region);
    let start = scheduler.master_clock;
    let mut lines = 0_u64;
    while !bus.vdp.frame_done() {
        scheduler.run_line(&mut cpu, &mut z80, &mut bus);
        lines += 1;
        assert!(
            lines <= PAL_LINES * 2,
            "{lines} lines and the beam never came back to VBlank"
        );
    }
    (lines, scheduler.master_clock - start)
}

/// PAL draws 313 scanlines a frame; NTSC draws 262.
///
/// The count is taken off the beam - `Vdp::frame_done` is the VBlank the VDP
/// raises when it wraps - rather than from [`Scheduler`], because the bug worth
/// catching is the two of them disagreeing: a PAL machine whose scheduler
/// counted 262 lines would still run at a plausible rate and would only show up
/// as a picture that skipped every second VBlank.
#[test]
fn pal_frame_is_313_lines() {
    let (europe, _) = measure_raster(Region::Europe);
    let (usa, _) = measure_raster(Region::Usa);
    let (japan, _) = measure_raster(Region::Japan);
    assert_eq!(europe, PAL_LINES, "a PAL raster is 313 scanlines");
    assert_eq!(usa, NTSC_LINES, "an NTSC raster is 262");
    assert_eq!(japan, NTSC_LINES, "and so is Japan's");
}

/// One PAL raster is `313 * 3420` master clocks, and the PAL crystal is
/// 53_203_424 Hz, so a frame is 1/49.70 s: the "50 Hz" of "PAL is 50 Hz"
/// (docs/ARCHITECTURE.md, "Clocks and timing", for both numbers).
///
/// The master clocks come from the machine again - the scheduler's own count
/// across one raster of `run_line`s - because 313 lines is only 49.7 Hz if the
/// line loop spends exactly 3420 of them per line, which is what that count
/// says. The NTSC pair is the control, and it is the pair rather than either
/// half that proves the two crystals have not been swapped: 59.92 against
/// 49.70 is a ratio of 1.205, and the ratio is what a host pacing its output
/// on.
///
/// The mixer is the third place the same number appears, and the one a player
/// hears. At 48 kHz a PAL raster is 965.8 host samples against NTSC's 801.1,
/// and both are `sample_rate / frame_rate`, so the audio is paced off the
/// region's crystal exactly as the picture is.
#[test]
fn pal_frame_rate_about_50hz() {
    assert_eq!(Region::Europe.master_clock_hz(), 53_203_424);
    assert_eq!(Region::Usa.master_clock_hz(), 53_693_175);

    let (pal_lines, pal_mclk) = measure_raster(Region::Europe);
    assert_eq!(pal_lines, PAL_LINES);
    assert_eq!(
        pal_mclk,
        PAL_LINES * MASTER_CLOCKS_PER_LINE,
        "one PAL raster is 1_070_460 master clocks"
    );
    let pal_hz = Region::Europe.master_clock_hz() as f64 / pal_mclk as f64;
    assert!(
        (pal_hz - 49.70).abs() < 0.01,
        "PAL runs at {pal_hz:.4} Hz, not about 49.70"
    );

    let (ntsc_lines, ntsc_mclk) = measure_raster(Region::Usa);
    assert_eq!(ntsc_lines, NTSC_LINES);
    assert_eq!(
        ntsc_mclk,
        NTSC_LINES * MASTER_CLOCKS_PER_LINE,
        "one NTSC raster is 896_040 master clocks"
    );
    let ntsc_hz = Region::Usa.master_clock_hz() as f64 / ntsc_mclk as f64;
    assert!(
        (ntsc_hz - 59.92).abs() < 0.01,
        "NTSC runs at {ntsc_hz:.4} Hz, not about 59.92"
    );
    assert!(
        (ntsc_hz / pal_hz - 1.205).abs() < 0.001,
        "{ntsc_hz:.2} Hz over {pal_hz:.2} Hz"
    );

    assert_hz(host_frames_per_second(Region::Europe), 49.70, "PAL audio");
    assert_hz(host_frames_per_second(Region::Usa), 59.92, "NTSC audio");
}

/// The frame rate the audio comes out at, in Hz: `FRAMES` rasters of `region`
/// mixed at [`SAMPLE_RATE`] produce this many stereo pairs per second of
/// machine time, which is `sample_rate / frame_rate` and nothing else.
///
/// The mixer is the only place the two crystals are different sizes
/// (`Region::master_clock_hz` says so outright), so this is the audio half of
/// "PAL is 50 Hz" and the place a mixer that ignored the region would show up.
fn host_frames_per_second(region: Region) -> f64 {
    const FRAMES: usize = 20;
    let mut machine = Genesis::new(
        &cart(region, &[0x60FE]),
        Config {
            region: RegionSetting::Force(region),
            sample_rate: SAMPLE_RATE,
            ..Config::default()
        },
    )
    .expect("a ROM with a header loads");

    let mut out = Vec::new();
    // The frame the parked beam was already waiting for goes first and is
    // thrown away, so what is counted is `FRAMES` whole rasters.
    machine.run_frame();
    machine.drain_audio(&mut out);
    out.clear();
    for _ in 0..FRAMES {
        machine.run_frame();
        machine.drain_audio(&mut out);
    }
    let pairs = out.len() / 2;
    assert!(out.len() % 2 == 0, "stereo pairs, {} samples", out.len());
    f64::from(SAMPLE_RATE) * FRAMES as f64 / pairs as f64
}

/// Assert a measured rate against `expected`, to two decimals.
fn assert_hz(measured: f64, expected: f64, what: &str) {
    assert!(
        (measured - expected).abs() < 0.01,
        "{what} is paced at {measured:.4} Hz, not {expected:.2}"
    );
}

/// The VINT lands on V counter `0xF0`, line 240, on a PAL chip in V30, and on
/// `0xE0`, line 224, in V28.
///
/// genvdp.txt section 4 names only `E0h` - "a level 6 interrupt will occur when
/// the V counter is at line E0h" - because it never discusses V30 at all.
/// Kabuto's hardware notes do, in the "Interrupts - VBlank set" row of the
/// timing table: `Vcounter changes from 0xDF to 0xE0` in V28 and `from 0xEF to
/// 0xF0` in V30, which is the same interrupt on the first line past the 240
/// lines V30 scans. `crates/vdp/src/timing.rs` starts VBlank on
/// [`vdp::Vdp::active_height`] for exactly that reason, so this checks the chip
/// against the table rather than against itself.
#[test]
fn v30_vint_at_line_240() {
    assert_eq!(
        vint_v_counter(R1_DISPLAY | R1_VINT | R1_V30),
        240,
        "PAL in V30 takes its VINT on line 240, the 0xF0 of Kabuto's table"
    );
    assert_eq!(
        vint_v_counter(R1_DISPLAY | R1_VINT),
        224,
        "PAL in V28 keeps the E0h of genvdp.txt section 4"
    );
    assert_eq!(
        vint_v_counter_on(Region::Usa, R1_DISPLAY | R1_VINT | R1_V30),
        224,
        "NTSC is V28 whatever bit 3 says: see `v30_frame_height_240`"
    );
}

/// The V counter of the first line on which the VDP offers a level 6 interrupt
/// to the 68000, on a PAL console with R1 written before the first line runs.
fn vint_v_counter(reg1: u8) -> u16 {
    vint_v_counter_on(Region::Europe, reg1)
}

/// [`vint_v_counter`] on a console of `region`.
fn vint_v_counter_on(region: Region, reg1: u8) -> u16 {
    let (mut cpu, mut z80, mut bus, mut scheduler) = console(region);
    bus.vdp.write_register(1, reg1);
    for _ in 0..PAL_LINES as u32 * 2 {
        scheduler.run_line(&mut cpu, &mut z80, &mut bus);
        if bus.vdp.pending_irq() == Some(6) {
            return bus.vdp.hv_counter() >> 8;
        }
    }
    panic!("no VINT in two rasters with R1 = {reg1:#04x}");
}

/// A PAL chip in V30 draws 240 lines; an NTSC chip handed the same bit draws the
/// 224 of V28 and says nothing.
///
/// The 240 is Kabuto's "Vertical Display" row: 28-row mode is "11 border, 224
/// picture, 8 border" and 30-row mode takes "8 more lines from each of the
/// borders, i.e. 16 total", so V30 is 240 lines of picture in either standard
/// and what it borrows is blanking. The blanking it borrows from does not
/// stretch: the vertical retrace is the same 262-line frame, which is why
/// plutiedev's "$81xx" says "V30 mode only works properly on PAL systems. ...
/// if the game is running on a NTSC system, you must set this bit to 0 or the
/// console will misbehave" - the picture runs into the vertical sync and a CRT
/// will not hold it. Kabuto's "V28/V30 tricks" section is the same fact from
/// the other side, describing programs that move the border by writing the bit
/// mid-frame: "having V28 active for a single VDP cycle during line 224 or V30
/// for line 240 will start the border".
///
/// So an NTSC machine with bit 3 set is showing 240 lines of picture through a
/// 224-line frame, and this emulator's answer is to render the 224 the frame
/// has room for and warn nobody: the bit is legal to write, a game may hold it
/// set across a region change, and there is no console-side diagnostic to
/// produce (M4-19, #97; `vdp::Vdp::active_height` carries the same note). What
/// the hardware does instead - a rolling picture, and no bottom border on a
/// capture - is the thing this test records as deliberately not modelled.
#[test]
fn v30_frame_height_240() {
    let pal = running_machine(Region::Europe);
    assert!(pal.bus().vdp.v30_mode(), "the cart asked for V30");
    assert_eq!(
        pal.bus().vdp.active_height(),
        240,
        "a PAL chip in V30 scans 240 lines"
    );
    assert_eq!(pal.frame().height, 240, "and hands over 240 of them");

    let ntsc = running_machine(Region::Usa);
    assert!(ntsc.bus().vdp.v30_mode(), "the same cart, the same bit");
    assert_eq!(
        ntsc.bus().vdp.active_height(),
        224,
        "NTSC ignores it: no room for 240 lines in a 262 line frame"
    );
    assert_eq!(ntsc.frame().height, 224);
}

/// A machine of `region` running [`cart`] for one frame, so the cart's own
/// register write is what set the mode up.
fn running_machine(region: Region) -> Genesis {
    let mut machine = Genesis::new(
        &cart(region, &[0x60FE]),
        Config {
            region: RegionSetting::Force(region),
            ..Config::default()
        },
    )
    .expect("a ROM with a header loads");
    machine.run_frame();
    machine
}

/// A cartridge that turns the display on and selects V30 as its first act, then
/// runs `tail`, for the tests that want the console told about a mode by a
/// program rather than by a test writing the register itself.
///
/// `MOVE.W #imm, (xxx).L` is `33FC imm32 addr32` - the destination field of
/// MOVE is the swapped one, `crates/m68k/src/ops/move_.rs` - and the immediate
/// is plutiedev's "VDP register setup" command for register 1: `100r rrrr` with
/// the set-register bit and the value.
fn cart(region: Region, tail: &[u16]) -> Vec<u8> {
    let mut rom = vec![0_u8; CODE as usize + 0x40];
    rom[0x100..0x200].fill(b' ');
    rom[0x1F0..0x1F3].copy_from_slice(region_field(region).as_bytes());
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());

    let command: u16 = 0x8000 | (1 << 8) | u16::from(R1_DISPLAY | R1_V30);
    let at = CODE as usize;
    rom[at..at + 2].copy_from_slice(&0x33FC_u16.to_be_bytes());
    rom[at + 2..at + 4].copy_from_slice(&command.to_be_bytes());
    rom[at + 4..at + 8].copy_from_slice(&VDP_CONTROL.to_be_bytes());
    let mut at = at + 8;
    for word in tail {
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
        at += 2;
    }
    rom
}

/// The three-byte region field at 0x1F0 that claims `region` and nothing else,
/// in the old one-letter-per-region format ([`genesis::cart::header`]).
fn region_field(region: Region) -> &'static str {
    match region {
        Region::Europe => "E  ",
        Region::Usa => "U  ",
        Region::Japan => "J  ",
    }
}

/// A cart that claims Europe and nothing else is a PAL machine.
///
/// One header field, four consequences, checked in the four places they land:
/// the decoder reads `E` as Europe and nothing else
/// ([`genesis::cart::Regions`]), [`Config::resolve_region`] makes it
/// [`Region::Europe`], [`Region::version_register`] sets bit 6 (VMOD) of
/// 0xA10001 - the only thing a program can read, and so the only thing that
/// tells a game it is on a PAL console - and the VDP is built on the same pin,
/// which is what lets that game's V30 request have 240 lines.
///
/// The control is the same cart claiming America: 0xA0, no VMOD, and the same
/// V30 request rendering 224 lines.
#[test]
fn europe_rom_autodetects_pal() {
    let mut europe = Genesis::new(&cart(Region::Europe, &[0x60FE]), Config::default())
        .expect("a ROM with a header loads");
    assert_eq!(
        europe.bus().version,
        0xE0,
        "an E-only cart gets the PAL version register: overseas, PAL, no disk"
    );
    assert_eq!(europe.bus().version & (1 << 6), 1 << 6, "VMOD, bit 6");

    europe.run_frame();
    assert!(europe.bus().vdp.v30_mode(), "the cart got its V30");
    assert_eq!(
        europe.frame().height,
        240,
        "and a PAL chip answers it with 240 lines, which is the autodetect \
         arriving at the beam"
    );

    let mut usa = Genesis::new(&cart(Region::Usa, &[0x60FE]), Config::default())
        .expect("a ROM with a header loads");
    assert_eq!(usa.bus().version, 0xA0, "a U-only cart is a Genesis");
    assert_eq!(usa.bus().version & (1 << 6), 0, "and NTSC");
    usa.run_frame();
    assert_eq!(usa.frame().height, 224, "and V30 asks for nothing there");
}

/// A European-only cartridge boots into the 240-line display it asks for.
///
/// `ToeJam & Earl in Panic on Funkotron (Europe).md` claims Europe and nothing
/// else in its region field, so it is autodetected as PAL rather than forced to
/// it, and it is a cart that uses V30: it comes up in the 224 lines of V28 and
/// moves itself to 320x240 a few frames in. That is the V30 path exercised by
/// software rather than by a test register write, which is the only way to find
/// out that the 240-line frame is real.
///
/// `#[ignore]`d because CI has no commercial ROMs; `just test-all` runs it, and
/// `just test-commercial` runs this file with `--run-ignored only`.
#[test]
#[ignore]
fn europe_only_commercial_rom_boots_a_240_line_frame() {
    let path = require_data!(testkit::commercial_rom(
        "ToeJam & Earl in Panic on Funkotron (Europe).md"
    ));
    let rom = std::fs::read(&path).expect("the cart is readable");
    let mut machine = Genesis::new(&rom, Config::default()).expect("the cart loads");
    assert_eq!(machine.bus().version, 0xE0, "autodetected as PAL");

    let mut v30_at = None;
    for frame in 1..=600_u32 {
        machine.run_frame();
        if machine.bus().vdp.v30_mode() {
            v30_at = Some(frame);
            break;
        }
    }
    let frame = v30_at.expect("the cart selects V30 within ten seconds of running");
    assert_eq!(machine.frame().height, 240, "V30 from frame {frame}");
    assert_eq!(machine.frame().width, 320, "in H40 as well");

    let pixels = &machine.frame().pixels[..machine.frame().width * machine.frame().height];
    assert!(
        pixels.iter().any(|pixel| *pixel != 0),
        "the 240 line frame is a picture rather than a blank"
    );
}
