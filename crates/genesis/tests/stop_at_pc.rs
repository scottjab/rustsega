//! Stopping the console at an instruction address instead of at the end of a frame.
//!
//! This is the instruction-granular half of the external reference oracle of #526
//! (`scripts/ref/`): the work-RAM comparison of #525 compared both machines at
//! their own idea of "end of frame", and #526 found that the two ideas are not the
//! same place in the instruction stream, which makes a per-frame handshake byte
//! look like a persistent divergence. Both halves of the comparison therefore need
//! to stop at one address the game itself reaches once a frame - the entry of its
//! own vertical-blank routine - and [`genesis::Genesis::run_until_pc`] is our half.
//!
//! What the tests below hold:
//!
//! * the stop is at an instruction boundary, mid-line, with the writes either side
//!   of that boundary on either side of the stop;
//! * one stop per frame at a program that takes one vertical interrupt a frame,
//!   which is what makes an address usable as the comparison point;
//! * a stop that never happens is reported, not waited for;
//! * an anchored run does the rest of the machine no harm: the vertical interrupt
//!   still fires once a raster, and one raster of sound is still one frame's.

use genesis::{Config, Genesis};

/// Where the fake ROMs put their program: past the 0x200 bytes a load needs, so
/// [`genesis::cart::parse_header`] accepts the image.
const CODE: u32 = 0x1000;
/// The reset stack pointer the fake ROMs ask for: the top of work RAM.
const STACK: u32 = 0x00FF_0000;
/// The 68000's VDP control port (plutiedev "Memory map").
const VDP_CONTROL: u32 = 0xC0_0004;
/// The 68000's autovector slot for the vertical interrupt: `AUTO_VECTOR_BASE + 6`
/// is vector 30 (`crates/m68k/src/interrupts.rs`), so its long lives at `$78`.
/// This is the slot a real cartridge's VBlank entry is read from, and the reason
/// `--stop-at-pc` of `scripts/ref/run-ours.sh` names an address rather than an
/// interrupt.
const VINT_VECTOR: usize = 4 * 30;
/// Where the fake ROM's vertical interrupt handler goes: past the waiting
/// program, which lets one byte image hold both.
const VINT_HANDLER: u32 = CODE + 0x40;
/// Work RAM, the last mirror of the 64 KiB at `$E00000` (plutiedev "Memory map"),
/// and the thing the oracle compares.
const RAM: u32 = 0xFF_0000;
/// Scanlines in an NTSC raster: the same 262 `Scheduler::run_frame` waits on, and
/// what one leg of a once-a-frame stop is expected to cover.
const RASTER: u32 = 262;

/// A cartridge image: the reset table, [`STACK`] and [`CODE`], `program` at
/// [`CODE`], and `vectors` written into the vector table.
fn fake_rom(program: &[u16], vectors: &[(usize, u32)]) -> Vec<u8> {
    let need = CODE as usize + 2 * program.len();
    let mut rom = vec![0_u8; need.max(0x200)];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (at, value) in vectors {
        rom[*at..*at + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (word, at) in program.iter().zip(0..) {
        let at = CODE as usize + 2 * at;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom
}

/// `move.w #$value, (addr).L`: `MOVE.W #imm, (xxx).L` is `33FC imm32 addr32`
/// (`crates/genesis/tests/boot.rs` spells the same encoding), four words.
fn store_word(addr: u32, value: u16) -> Vec<u16> {
    let addr = addr.to_be_bytes();
    vec![
        0x33FC,
        value,
        u16::from_be_bytes([addr[0], addr[1]]),
        u16::from_be_bytes([addr[2], addr[3]]),
    ]
}

/// A program of `count` stores, each to its own word of work RAM with its own
/// value, then `BRA.B $`: straight-line code that reaches any given one of them
/// exactly once, so a stop between two of them is observable in RAM.
fn stores(count: usize) -> Vec<u16> {
    let mut program = Vec::new();
    for i in 0..count {
        program.extend(store_word(RAM + 2 * i as u32, (i + 1) as u16));
    }
    program.push(0x60FE);
    program
}

/// [`Genesis::run_until_pc`] on the `count` stores of [`stores`].
fn machine_with_stores(count: usize) -> Genesis {
    let rom = fake_rom(&stores(count), &[]);
    Genesis::new(&rom, Config::default()).expect("a ROM with a header loads")
}

/// The word of work RAM at `addr`.
fn word(machine: &Genesis, addr: u32) -> u16 {
    let ram: &[u8] = &machine.bus().ram[..];
    let at = (addr - RAM) as usize;
    u16::from_be_bytes([ram[at], ram[at + 1]])
}

/// A machine that takes a vertical interrupt once a raster: `move.w #$2000,sr` to
/// drop the interrupt mask [`genesis::Genesis::new`] leaves at 7, one VDP register
/// command for register 1 with IE0 set (genvdp.txt section 17 "$01" bit 5, in the
/// command form of `crates/vdp/src/ports.rs`), `BRA.B $` to wait in, and a handler
/// of `ADDQ.L #1,D0` / `RTE` (`$5280`/`$4E73`, the pair
/// `crates/genesis/src/scheduler.rs` uses as a per-entry counter) aimed at by the
/// level 6 vector.
fn machine_with_vint() -> Genesis {
    let mut program = vec![0x46FC, 0x2000]; // `move.w #$2000, sr`
    program.extend(store_word(VDP_CONTROL, 0x8000 | (1 << 8) | 0x20));
    program.push(0x60FE);
    // The handler rides along in the same image, padded up to its address: the
    // zeros between the wait and it are never run through.
    let handler = usize::try_from((VINT_HANDLER - CODE) / 2).expect("handler above code");
    program.resize(handler, 0);
    program.extend([0x5280_u16, 0x4E73]);
    let rom = fake_rom(&program, &[(VINT_VECTOR, VINT_HANDLER)]);
    Genesis::new(&rom, Config::default()).expect("a ROM with a header loads")
}

/// The stop is at an instruction boundary, not at a line or a frame boundary: the
/// stores before the target have reached work RAM and the target itself has not.
#[test]
fn run_until_pc_stops_before_the_instruction_at_the_address() {
    let mut machine = machine_with_stores(40);
    // The twentieth store, eight bytes (four words) apiece into the program. The
    // program is shorter than a scanline is long, so this is a stop in the middle
    // of the first line of the machine.
    let target = CODE + 8 * 20;

    assert!(machine.run_until_pc(target, 10));
    assert_eq!(
        machine.cpu().pc,
        target,
        "stopped on the address, not past it"
    );

    for i in 0..20usize {
        assert_eq!(
            word(&machine, RAM + 2 * i as u32),
            u16::try_from(i + 1).expect("a test index fits a word"),
            "store {i} ran"
        );
    }
    assert_eq!(
        word(&machine, RAM + 2 * 20),
        0,
        "and the store the stop sits in front of has not: work RAM starts zeroed, \
         so a non-zero value here is the stop having let the instruction through"
    );
}

/// A once-a-frame address is a usable comparison point: every leg of the run after
/// the first covers exactly one raster and takes exactly one vertical interrupt,
/// which is what lets a dump at it be called the same moment on another machine.
#[test]
fn a_once_a_frame_address_is_arrived_at_once_a_frame() {
    let mut machine = machine_with_vint();
    assert!(
        machine.run_until_pc(VINT_HANDLER, 4 * RASTER),
        "the handler is reached once the program has enabled IE0"
    );

    for leg in 0..5 {
        // The stop is an arrival, so the instruction the previous stop parked on
        // runs and the call moves - exactly one raster's way.
        assert!(
            machine.run_until_pc(VINT_HANDLER, 2 * RASTER),
            "leg {leg} arrived at the handler again"
        );
        assert_eq!(machine.cpu().pc, VINT_HANDLER);
    }
    // D0 counts the entries the handler has finished: the last stop is in front of
    // its own entry, so six stops have taken six interrupts and the count is 5.
    assert_eq!(
        machine.cpu().d[0],
        5,
        "one interrupt entry per stop, with none taken twice and none missed"
    );
}

/// An address this program never runs at is an answer, not a hang: the guard stops
/// and says so, and the machine is still a machine afterwards.
#[test]
fn an_unreachable_address_gives_up_and_leaves_the_machine_running() {
    let mut machine = machine_with_vint();
    // Past the end of this ROM's program and its handler: nothing branches here.
    let never = VINT_HANDLER + 0x200;

    assert!(
        !machine.run_until_pc(never, RASTER),
        "no arrival inside one raster of a program that never reaches {never:#08X}"
    );
    assert_ne!(machine.cpu().pc, never);

    // Still steerable: a frame runs, and the real comparison point is still there.
    machine.run_frame();
    assert!(machine.run_until_pc(VINT_HANDLER, 4 * RASTER));
}

/// Anchoring a run on an instruction does not quietly cost the machine a frame of
/// sound or an interrupt: the same raster driven as one `run_frame` and as a leg of
/// [`genesis::Genesis::run_until_pc`] produce the same number of samples and one
/// handler entry each.
#[test]
fn one_leg_is_one_frame_of_sound() {
    let mut machine = machine_with_vint();
    assert!(
        machine.run_until_pc(VINT_HANDLER, 4 * RASTER),
        "the handler is reached"
    );

    // The run to the first stop is power on's, from line 159 to the first VBlank
    // (#675), and not a raster: it is drained and the first whole leg after it is
    // the measure.
    let mut samples = Vec::new();
    machine.drain_audio(&mut samples);
    samples.clear();
    assert!(machine.run_until_pc(VINT_HANDLER, 2 * RASTER));
    machine.drain_audio(&mut samples);
    let frame_samples = samples.len();
    assert!(
        frame_samples > 0,
        "a raster of a running machine makes sound"
    );

    for _ in 0..3 {
        samples.clear();
        assert!(machine.run_until_pc(VINT_HANDLER, 2 * RASTER));
        machine.drain_audio(&mut samples);
        assert_eq!(
            samples.len(),
            frame_samples,
            "one raster between stops, one frame of samples"
        );
    }
    // Five stops in all, the first in front of the first entry and the four legs
    // in front of the next four: four entries have finished their `ADDQ`.
    assert_eq!(machine.cpu().d[0], 4, "one handler entry per leg");
}
