//! Where in the raster a console that has just been switched on starts the 68000
//! (#675).
//!
//! The VDP is running before the 68000 is: /VRES stays asserted a fixed time after
//! the system reset is released, so the CPU's first instruction lands at the same
//! point of the frame on every power on. Genesis Plus GX measured that point on a
//! VA4 PAL MD1 with `move.w #$8104,$C00004` / `move.w $C00008,d0` as the first two
//! instructions: the counter reads `$9F21` at 60 Hz and `$8421` at 50 Hz, "the
//! same value ... on every power ON" (`core/genesis.c`, `gen_reset`). Those are
//! lines 159 and 132.
//!
//! What hangs on it is every frame count of the boot: a cart's start-up work runs
//! from that line to the first VBlank it waits for, and starting the CPU on the
//! VBlank line instead hands the first frame a whole raster of CPU time rather than
//! 65 lines of it. `Sonic & Knuckles + Sonic 3` then finishes its RAM clear a frame
//! earlier than hardware, takes every VBlank of the title one frame early, and the
//! TASVideos movie #6890 presses Start on the SEGA screen a frame after the one that
//! reads it.

use genesis::{Config, Genesis, Region, RegionSetting};

/// Where the program starts, past the vectors and the header at `$100`.
const CODE: u32 = 0x1000;
const STACK: u32 = 0x00FF_0000;
/// Work RAM word the program leaves the counter in.
const SAMPLE: u32 = 0xFF_0000;

/// `move.w $C00008,$FF0000` then `bra.b $`: the HV counter, read by the very
/// first instruction the 68000 runs, parked in work RAM (M68000UM section 8,
/// "MOVE" with absolute long source and destination, and "BRA").
fn first_read_rom() -> Vec<u8> {
    let src = 0xC0_0008_u32.to_be_bytes();
    let dst = SAMPLE.to_be_bytes();
    let program = [
        0x33F9,
        u16::from_be_bytes([src[0], src[1]]),
        u16::from_be_bytes([src[2], src[3]]),
        u16::from_be_bytes([dst[0], dst[1]]),
        u16::from_be_bytes([dst[2], dst[3]]),
        0x60FE,
    ];
    let mut rom = vec![0_u8; CODE as usize + 2 * program.len()];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = CODE as usize + 2 * index;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom
}

fn machine(region: Region) -> Genesis {
    let config = Config {
        region: RegionSetting::Force(region),
        ..Config::default()
    };
    Genesis::new(&first_read_rom(), config).expect("the program loads")
}

fn first_v_count(region: Region) -> u8 {
    let mut machine = machine(region);
    machine.run_frame();
    let at = (SAMPLE & 0xFFFF) as usize;
    machine.bus().ram[at]
}

#[test]
fn the_first_instruction_reads_line_9f_on_an_ntsc_console() {
    assert_eq!(
        first_v_count(Region::Usa),
        0x9F,
        "GPGX's power-on measurement: $9F21 at 60 Hz"
    );
}

#[test]
fn the_first_instruction_reads_line_84_on_a_pal_console() {
    assert_eq!(
        first_v_count(Region::Europe),
        0x84,
        "GPGX's power-on measurement: $8421 at 50 Hz"
    );
}

/// The first frame is the CPU's start to the first VBlank, not a raster: from
/// line 159 to line 224 is 65 lines, and the frames after it are whole rasters.
#[test]
fn the_first_frame_runs_from_power_on_to_the_first_vblank() {
    let mut machine = machine(Region::Usa);
    let before = machine.cpu().cycles;
    machine.run_frame();
    let first = machine.cpu().cycles - before;
    let before = machine.cpu().cycles;
    machine.run_frame();
    let second = machine.cpu().cycles - before;

    // `cycles` is the instructions' own clocks, without the DRAM refresh the
    // scheduler charges beside them (`genesis::refresh`), so a raster is not 262
    // lines of it. The second frame is a whole raster, and the first is measured
    // against it: the refresh share is the same in both.
    let lines = first as f64 * 262.0 / second as f64;
    assert!(
        (lines - 65.0).abs() < 1.0,
        "the first frame ran {lines:.1} lines of the 68000, not the 65 from line 159 to 224"
    );
}
