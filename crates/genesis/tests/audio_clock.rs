//! The two sound chips running inside the console at their own rates, with writes
//! from both CPUs landing at the right sample (M5-13, #118).
//!
//! The machine has one master clock and the chips do not run on it: the YM2612
//! answers once every 144 68000 clocks and the PSG once every 16 Z80 T-states
//! (docs/ARCHITECTURE.md, "Clocks and timing"), so `genesis` has to say when each
//! one is asked. The rule is "catch up on write" - a chip is run up to the
//! current master clock immediately before anything writes it, and again at the
//! end of every line - which is what keeps a write in the middle of a frame
//! landing on the sample boundary it belongs to rather than at the end of the
//! line that made it.
//!
//! Everything here drives the real [`Bus`] and the real [`Scheduler`]. Nothing in
//! this file resamples, mixes or filters: that is M5-14 (#119), and
//! [`genesis::Genesis::drain_audio`] is still a stub.

use genesis::audio::{PSG_TICK_MCLK, YM_SAMPLE_MCLK};
use genesis::z80_bus::Z80View;
use genesis::{Bus, Region, Scheduler};
use m68k::Bus as _;
use z80::Z80 as Z80Cpu;
use z80::Z80Bus as _;

/// The version register of an export NTSC console with no TMSS, so the VDP and the
/// sound side are reachable from the first instruction
/// ([`genesis::Region::Usa::version_register`]).
const VERSION: u8 = 0xA0;
/// Master clocks in one scanline: the scheduler's own
/// `MASTER_CLOCKS_PER_LINE` (docs/ARCHITECTURE.md, "Clocks and timing").
const LINE: u64 = 3420;
/// One NTSC raster: 262 of those lines, 896_040 master clocks.
const FRAME: u64 = 262 * LINE;
/// Where the 68k's test program starts, clear of the reset vector table.
const CODE: u32 = 0x00_0010;
/// `Z80Reset: equ $A11200` (plutiedev, "Using the Z80"): the 68k's own hand on the
/// sound CPU's reset line, which the YM2612 shares.
const Z80_RESET: u32 = 0xA11_200;
/// The YM2612's four ports as the Z80 sees them: `$4000`-`$4003`, part I address /
/// data then part II address / data (plutiedev, "YM2612 register reference").
const YM_PORT: u16 = 0x4000;
/// The PSG port, from the Z80 (plutiedev, "Memory map").
/// The same PSG port from the 68k: `$C00011`, the odd lane of the VDP's PSG pair.
const PSG_PORT: u32 = 0xC0_0011;
/// Status bit 7: the chip is still busy with its last write.
const STATUS_BUSY: u8 = 0x80;
/// Status bit 0: timer A overflowed with `&27` bit 2 set.
const STATUS_TIMER_A: u8 = 0x01;
/// `&24`/`&25` are timer A's loader and `&27` its control (YM2608 application
/// manual, "3. Register descriptions").
const REG_TIMER_A_LOW: u8 = 0x24;
const REG_TIMER_A_HIGH: u8 = 0x25;
const REG_CONTROL: u8 = 0x27;
/// `&27` bit 2 starts timer A raising the status flag and bit 0 runs it.
const TIMER_A_ENABLE_LOAD: u8 = 0x05;

/// The 68k's program: `ADDQ.L #$1,D0` (`$5280`) then `BRA.B` back to it
/// (`$60FC`). The same two words `crates/genesis/src/scheduler.rs` runs its
/// timing tests on.
fn rom_image() -> Vec<u8> {
    let mut rom = vec![0_u8; 0x20];
    rom[0..4].copy_from_slice(&0x00FF_0000_u32.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    rom[0x10..0x12].copy_from_slice(&0x5280_u16.to_be_bytes());
    rom[0x12..0x14].copy_from_slice(&0x60FC_u16.to_be_bytes());
    rom
}

/// A running machine: the 68k counting in a cartridge loop, the Z80 looping
/// `INC A; JR -3` in sound RAM, and the Z80's reset released so both chips and
/// therefore both sound paths have clocks to spend.
fn machine() -> (m68k::M68k, Z80Cpu, Bus) {
    let mut bus = Bus::new(rom_image(), VERSION);
    bus.vdp.start_in_vblank(); // #371: parked on the VBlank line, as `Bus::reset_devices` leaves it
    bus.z80.ram[0..3].copy_from_slice(&[0x3C, 0x18, 0xFD]);
    bus.write_word(Z80_RESET, 0x0100);
    let mut cpu = m68k::M68k::new();
    cpu.reset(&mut bus);
    let mut z80 = Z80Cpu::new();
    z80.reset();
    (cpu, z80, bus)
}

/// One YM2612 write on the Z80's own bus view, which is the half of the machine
/// most YM writes arrive through.
fn z80_ym_write(bus: &mut Bus, port: u8, value: u8) {
    Z80View::new(bus).write(YM_PORT + u16::from(port), value);
}

/// The YM2612 status byte, read the way the sound CPU reads it: any of the four
/// ports answers with the one register the chip can read.
fn z80_ym_status(bus: &mut Bus) -> u8 {
    Z80View::new(bus).read(YM_PORT)
}

/// One NTSC frame is 262 lines of 3420 master clocks and the YM2612 is asked once
/// every 1008 of them (144 68000 clocks at 7 master clocks each, so about 53267 Hz;
/// docs/ARCHITECTURE.md, "Clocks and timing"). 896040 / 1008 is 888.9, so a frame
/// holds 888 or 889 samples and never either a fixed 888 or a fixed 889.
///
/// What matters over more than one frame is that the fraction is carried rather
/// than rounded away: nine frames are 8000 samples, which is the frame total
/// divided once at the end, and not 9 * 888 = 7992 or 9 * 889 = 8001. A scheduler
/// that dropped the remainder at each line boundary, or clocked the chip a fixed
/// number of times per frame, fails the total while passing the per-frame range.
#[test]
fn ym_sample_count_per_frame_ntsc() {
    let (mut cpu, mut z80, mut bus) = machine();
    let mut scheduler = Scheduler::new(Region::Usa);

    let counts = (0..9)
        .map(|_| {
            scheduler.run_frame(&mut cpu, &mut z80, &mut bus);
            bus.audio.ym_samples.len()
        })
        .collect::<Vec<_>>();
    for &count in &counts {
        assert!(
            (888..=889).contains(&count),
            "{count} samples is not the 888 or 889 a frame of {FRAME} master clocks holds \
             at one every {YM_SAMPLE_MCLK}: {counts:?}"
        );
    }

    let total: usize = counts.iter().sum();
    assert_eq!(
        total,
        (9 * FRAME / YM_SAMPLE_MCLK) as usize,
        "nine frames of the master clock are this many samples exactly, with the fraction \
         carried from line to line rather than rounded per frame: {counts:?}"
    );
}

/// The PSG ticks on the Z80 clock divided by 16, which is 240 master clocks and
/// about 223722 Hz (SMS Power, "SN76489", tone generator; docs/ARCHITECTURE.md,
/// "Clocks and timing"). A frame is therefore 896040 / 240 = 3733.5 ticks: 3733
/// for the first frame and 3734 for the next, and - the point of the second
/// assertion - exactly 7467 for the two of them together.
#[test]
fn psg_tick_count_per_frame() {
    let (mut cpu, mut z80, mut bus) = machine();
    let mut scheduler = Scheduler::new(Region::Usa);

    let counts = (0..2)
        .map(|_| {
            scheduler.run_frame(&mut cpu, &mut z80, &mut bus);
            bus.audio.psg_samples.len()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        vec![3733, 3734],
        "3733.5 ticks a frame, the half carried into the next one"
    );
    let total: usize = counts.iter().sum();
    assert_eq!(
        total,
        (2 * FRAME / PSG_TICK_MCLK) as usize,
        "two frames are the whole ticks of two frames, not twice the whole ticks of one"
    );
}

/// A PSG volume write in the middle of a frame takes effect at the tick that
/// follows it, not at the end of the line it happened on.
///
/// The chip is silent at power on - every channel at attenuation 15 - so
/// `0x90`, channel 0's volume at 0, is the loudest thing that can be asked for
/// and the sample stream goes from all zeroes to full scale at one exact index
/// (SMS Power, "SN76489", volume control). Writing it at master clock `X` has to
/// make sample number `X / 240` the first non-zero one.
#[test]
fn write_mid_frame_lands_at_right_sample() {
    let mut bus = Bus::new(Vec::new(), VERSION);
    bus.vdp.start_in_vblank(); // #371: parked on the VBlank line, as `Bus::reset_devices` leaves it
    // 100 whole PSG ticks, plus a bit: a write that is not on a tick boundary, so
    // that "the tick it lands on" is `X / 240` rather than something the test
    // could pass by rounding.
    let write_at: u64 = 100 * PSG_TICK_MCLK + 7;
    bus.audio.set_now(0);
    bus.audio.advance_to(write_at);
    assert_eq!(
        bus.audio.psg_samples.len(),
        100,
        "the clocks before the write are 100 ticks of silence"
    );
    assert!(
        bus.audio.psg_samples.iter().all(|&s| s == 0),
        "and all 100 of them are silent"
    );

    bus.write_byte(PSG_PORT, 0x90);

    bus.audio.advance_to(write_at + 20 * PSG_TICK_MCLK);
    let samples = &bus.audio.psg_samples;
    assert_eq!(samples.len(), 120);
    assert!(
        samples[..100].iter().all(|&s| s == 0),
        "nothing before the write got louder"
    );
    assert!(
        samples[100] != 0,
        "the tick the write lands in, index 100 = {write_at} / {PSG_TICK_MCLK}, is the \
         first one that hears it: {:?}",
        &samples[100..104]
    );
    assert!(
        samples[100..].iter().all(|&s| s != 0),
        "and every tick after it keeps hearing it"
    );
}

/// A YM2612 register written by the Z80 reaches the chip.
///
/// The chip has no register readback - `crates/ym2612` deliberately keeps its
/// state accessors `#[cfg(test)]` for that reason - so the register is read
/// through the one effect it has on the status byte: load timer A with 1023
/// (`&24` = 0xFF, `&25` = 0x03), start it with its flag enabled (`&27` = 0x05),
/// and the overflow flag is status bit 0 one output sample later (YM2608
/// application manual, "3. Register descriptions", `&24` to `&27`). A write that
/// never arrived would leave the loader at 0 and the flag 1024 samples away.
#[test]
fn z80_ym_write_works() {
    let mut bus = Bus::new(Vec::new(), VERSION);
    bus.vdp.start_in_vblank(); // #371: parked on the VBlank line, as `Bus::reset_devices` leaves it
    bus.audio.set_now(0);

    assert_eq!(
        z80_ym_status(&mut bus) & STATUS_TIMER_A,
        0,
        "a chip that has been written nothing has no timer flag"
    );

    z80_ym_write(&mut bus, 0, REG_TIMER_A_LOW);
    z80_ym_write(&mut bus, 1, 0xFF);
    z80_ym_write(&mut bus, 0, REG_TIMER_A_HIGH);
    z80_ym_write(&mut bus, 1, 0x03);
    z80_ym_write(&mut bus, 0, REG_CONTROL);
    z80_ym_write(&mut bus, 1, TIMER_A_ENABLE_LOAD);

    bus.audio.advance_to(YM_SAMPLE_MCLK);
    assert_ne!(
        z80_ym_status(&mut bus) & STATUS_TIMER_A,
        0,
        "timer A loaded with 1023 and started overflows on the first sample clock"
    );
}

/// The YM busy flag is time, so a read has to catch the chip up too.
///
/// A data write holds status bit 7 for 32 internal cycles, which is 1.33 output
/// samples - set one sample after the write and clear two samples after it
/// (`crates/ym2612`, `BUSY_INTERNAL_CYCLES`). A driver waits on exactly that bit,
/// so a read that answered from a chip that had not been clocked since the write
/// would report the flag forever.
#[test]
fn ym_status_read_catches_the_chip_up() {
    let mut bus = Bus::new(Vec::new(), VERSION);
    bus.vdp.start_in_vblank(); // #371: parked on the VBlank line, as `Bus::reset_devices` leaves it
    bus.audio.set_now(0);
    assert_eq!(
        z80_ym_status(&mut bus) & STATUS_BUSY,
        0,
        "idle to start with"
    );

    z80_ym_write(&mut bus, 0, REG_TIMER_A_LOW);
    z80_ym_write(&mut bus, 1, 0x30);
    assert_ne!(
        z80_ym_status(&mut bus) & STATUS_BUSY,
        0,
        "a data write sets BUSY at the instant it happens"
    );

    bus.audio.advance_to(YM_SAMPLE_MCLK);
    assert_ne!(
        z80_ym_status(&mut bus) & STATUS_BUSY,
        0,
        "one sample on, 32 - 24 = 8 cycles are still owed"
    );

    bus.audio.advance_to(2 * YM_SAMPLE_MCLK);
    assert_eq!(
        z80_ym_status(&mut bus) & STATUS_BUSY,
        0,
        "two samples on it has run down, and the read that says so is the read the \
         driver is polling with"
    );
}

/// Both CPUs reach the same two chips rather than one each: the 68k's
/// `$A04000`-`$A04003` and `$A07F11` and the Z80's `$4000`-`$4003` and `$7F11`
/// are the same wires (plutiedev, "Memory map" and "YM2612 register reference").
#[test]
fn both_cpus_write_the_same_chips() {
    let mut bus = Bus::new(Vec::new(), VERSION);
    bus.vdp.start_in_vblank(); // #371: parked on the VBlank line, as `Bus::reset_devices` leaves it
    bus.audio.set_now(0);
    // The 68k owns the sound side only while it holds the Z80's bus.
    bus.write_word(0xA11_100, 0x0100);

    bus.write_byte(0xA0_4000, REG_TIMER_A_LOW);
    bus.write_byte(0xA0_4001, 0xFF);
    bus.write_byte(0xA0_4000, REG_TIMER_A_HIGH);
    bus.write_byte(0xA0_4001, 0x03);
    bus.write_byte(0xA0_4000, REG_CONTROL);
    bus.write_byte(0xA0_4001, TIMER_A_ENABLE_LOAD);
    bus.audio.advance_to(YM_SAMPLE_MCLK);
    assert_ne!(
        bus.read_byte(0xA0_4000) & STATUS_TIMER_A,
        0,
        "the 68k's own YM window reaches the chip"
    );

    bus.write_byte(PSG_PORT, 0x9F);
    assert_eq!(
        bus.audio.psg.attenuation(0),
        15,
        "and the 68k's PSG port reaches the PSG"
    );
}

/// The frame buffers are reserved once and reused, which is the acceptance
/// criterion "no allocation per frame" turned into something a run can fail.
///
/// `Vec::clear` keeps capacity, so a buffer whose capacity moves after the first
/// frame is a buffer that was `Vec::new`ed or grown inside the frame loop
/// (docs/ARCHITECTURE.md: "Core crates must not allocate per frame in hot paths.
/// Allocate buffers once."). The capacities asserted for are the preallocation
/// [`genesis::audio::Audio::new`] takes for a PAL frame, the longest one.
#[test]
fn frame_buffers_are_reused_not_reallocated() {
    let (mut cpu, mut z80, mut bus) = machine();
    let mut scheduler = Scheduler::new(Region::Usa);
    scheduler.run_frame(&mut cpu, &mut z80, &mut bus);

    let ym_capacity = bus.audio.ym_samples.capacity();
    let psg_capacity = bus.audio.psg_samples.capacity();
    assert!(
        ym_capacity >= 889 && psg_capacity >= 3734,
        "the first frame found room for a whole raster of samples: {ym_capacity}, \
         {psg_capacity}"
    );

    for _ in 0..8 {
        scheduler.run_frame(&mut cpu, &mut z80, &mut bus);
        assert_eq!(
            bus.audio.ym_samples.capacity(),
            ym_capacity,
            "the YM2612 buffer was cleared, not rebuilt"
        );
        assert_eq!(
            bus.audio.psg_samples.capacity(),
            psg_capacity,
            "and so was the PSG's"
        );
    }
}
