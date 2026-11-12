//! The versioned save state of `docs/save-state-format.md` (M6-07, #127).
//!
//! The container first - magic, version, ROM checksum, length - because that is
//! what a front end has to be able to trust before it touches a payload, and
//! then the machine: a state taken after thirty frames, put back, and run for
//! ten more, has to draw the same ten pictures as never saving at all.
//!
//! Like `boot.rs`, everything here runs on a fake ROM: the format and the
//! machine, not a cartridge, are under test.

use genesis::{Config, Genesis, StateError};

/// Where the fake ROM puts its program: past the 0x200 bytes a load needs, so
/// [`genesis::parse_header`] accepts the image.
const CODE: u32 = 0x1000;
/// The reset stack pointer the fake ROMs ask for: the top of work RAM.
const STACK: u32 = 0x00FF_0000;
/// The 68000's VDP control port (plutiedev "Memory map").
const VDP_CONTROL: u32 = 0xC0_0004;
/// The 68000's VDP data port.
const VDP_DATA: u32 = 0xC0_0000;
/// Where the loop leaves `d0` as well as on the VDP port: the last mirror of the
/// 64 KiB of work RAM at `0xE00000` (plutiedev "Memory map"), so the save state
/// has work RAM in it and not only VDP writes.
const RAM_COPY: u32 = 0xFF_0000;

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
/// the encoding `boot.rs` uses, checked against `m68k::disassemble`.
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

/// `move.w d0, (addr).L`. `MOVE.W D0,(xxx).L` is `33C0 addr32`: size `11` in
/// bits 13-12, the absolute-long address-mode sub-mode in bits 11-9, mode 111 in
/// bits 8-6 and the D0 source in bits 5-0 - `MOVE`'s destination fields are the
/// swapped ones, per `crates/m68k/src/ops/move_.rs`'s module docs, and
/// `m68k::disassemble` is what checked the word.
fn store_d0(addr: u32) -> Vec<u16> {
    let addr = addr.to_be_bytes();
    vec![
        0x33C0,
        u16::from_be_bytes([addr[0], addr[1]]),
        u16::from_be_bytes([addr[2], addr[3]]),
    ]
}

/// A program that paints the screen a colour that changes forever: R1 display on,
/// R7 backdrop colour 5, a CRAM write code for CRAM word 5, then a loop that
/// writes `d0` to the data port - which the address register points at that word,
/// and `d0` goes up by one each time round.
///
/// The point is a machine whose state is worth saving: every frame the palette is
/// a different colour, so a frame hash says something about the VDP's registers,
/// its address register, the write FIFO and the CPU's `d0` all at once, and the
/// chip is caught mid-FIFO rather than only ever between frames.
fn colour_cycler() -> Vec<u8> {
    let mut program = vec![0x7000]; // moveq #0,d0
    program.extend(store_word(VDP_CONTROL, 0x8140)); // R1: display on
    program.extend(store_word(VDP_CONTROL, 0x8705)); // R7: backdrop colour 5
    program.extend(store_word(VDP_CONTROL, 0xC00A)); // CRAM write, word 5
    program.extend(store_word(VDP_CONTROL, 0x0000)); // and its second half
    program.extend(store_d0(VDP_DATA)); // loop: move.w d0,(VDP_DATA)
    program.extend(store_d0(RAM_COPY)); //        move.w d0,(RAM_COPY)
    program.extend([0x5240]); //                 addq.w #1,d0
    program.extend([0x60F0]); //        bra -16, back to the first move
    fake_rom(&program)
}

/// A second ROM that differs from [`colour_cycler`] but is the same shape: the
/// machine on the other side of it has to notice.
fn other_rom() -> Vec<u8> {
    let mut rom = colour_cycler();
    // One byte of code, well past the header: `moveq #1,d0` instead of `#0`.
    rom[CODE as usize] = 0x78;
    rom
}

/// The picture the machine is showing, hashed the way every golden in the repo
/// is hashed.
fn frame_hash(machine: &Genesis) -> u64 {
    let frame = machine.frame();
    testkit::frame_hash(
        &frame.pixels[..frame.width * frame.height],
        frame.width,
        frame.height,
    )
}

/// Run `frames` frames and hash each one, so a comparison is per frame rather
/// than of a single end state.
fn run_and_hash(machine: &mut Genesis, frames: usize) -> Vec<u64> {
    (0..frames)
        .map(|_| {
            machine.run_frame();
            frame_hash(machine)
        })
        .collect()
}

/// The container of `docs/save-state-format.md`, as `save_state` writes it.
#[test]
fn header_written_and_checked() {
    let rom = colour_cycler();
    let mut machine = Genesis::new(&rom, Config::default()).expect("loads");
    for _ in 0..5 {
        machine.run_frame();
    }

    let state = machine.save_state();
    assert_eq!(
        &state[0..4],
        b"RSGS",
        "magic, the first four bytes of every save state"
    );
    assert_eq!(
        u32::from_le_bytes(state[4..8].try_into().expect("4 bytes of version")),
        1,
        "format version 1"
    );
    let checksum = u64::from_le_bytes(state[8..16].try_into().expect("8 bytes of checksum"));
    assert_eq!(
        checksum,
        testkit::fnv1a_64(&rom),
        "the ROM checksum is FNV-1a 64 over the loaded image, which is what lets \
         a state refuse a cartridge it was not made for"
    );
    let length = u32::from_le_bytes(state[16..20].try_into().expect("4 bytes of length")) as usize;
    assert_eq!(
        length,
        state.len() - 20,
        "the payload length is the rest of the file, and nothing else"
    );
    assert!(length > 1000, "a whole machine is in there, not a stub");

    // And reading it back passes the same check: the same ROM, the same version.
    machine
        .load_state(&state)
        .expect("a state this machine just wrote loads");
}

#[test]
fn wrong_magic_errors() {
    let rom = colour_cycler();
    let mut machine = Genesis::new(&rom, Config::default()).expect("loads");
    let mut state = machine.save_state();
    state[0] = b'X';

    assert_eq!(machine.load_state(&state), Err(StateError::BadMagic));
    // A state that is too short to hold the header is not a magic number match
    // either, and must not panic.
    assert_eq!(machine.load_state(b"RSG"), Err(StateError::BadMagic));
}

#[test]
fn newer_version_errors() {
    let rom = colour_cycler();
    let mut machine = Genesis::new(&rom, Config::default()).expect("loads");
    let mut state = machine.save_state();
    state[4..8].copy_from_slice(&2u32.to_le_bytes());

    assert_eq!(
        machine.load_state(&state),
        Err(StateError::UnsupportedVersion(2))
    );
    // And a version this build has never heard of in the other direction, with
    // no migration to get from it (out of scope of #127).
    state[4..8].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        machine.load_state(&state),
        Err(StateError::UnsupportedVersion(0))
    );
}

#[test]
fn wrong_rom_errors() {
    let mut machine = Genesis::new(&colour_cycler(), Config::default()).expect("loads");
    let state = machine.save_state();

    let mut other = Genesis::new(&other_rom(), Config::default()).expect("loads");
    assert_eq!(other.load_state(&state), Err(StateError::WrongRom));
    // The machine a refused load left behind still runs: a bad state is refused
    // before any of it is applied.
    assert!(!run_and_hash(&mut other, 1).is_empty());
    machine
        .load_state(&state)
        .expect("and its own state still loads");
}

/// A payload the length field promised but did not deliver is a corrupt state,
/// not a panic and not a truncated load.
#[test]
fn truncated_payload_errors() {
    let rom = colour_cycler();
    let mut machine = Genesis::new(&rom, Config::default()).expect("loads");
    let state = machine.save_state();

    let truncated = &state[..state.len() - 7];
    assert_eq!(machine.load_state(truncated), Err(StateError::Corrupt));

    let mut short = state.clone();
    let length = (u32::from_le_bytes(short[16..20].try_into().unwrap()) + 1) as usize;
    short[16..20].copy_from_slice(&(length as u32).to_le_bytes());
    assert_eq!(machine.load_state(&short), Err(StateError::Corrupt));
}

/// The one that matters: a machine restored from a state has to be the machine
/// that wrote it, mid-colour and all.
///
/// Ten frames from a save at frame thirty against ten frames from never saving
/// at all - the same hashes, in order, or the state lost something the running
/// machine needed.
#[test]
fn genesis_roundtrip_same_frame() {
    let rom = colour_cycler();
    let mut machine = Genesis::new(&rom, Config::default()).expect("loads");
    for _ in 0..30 {
        machine.run_frame();
    }

    let state = machine.save_state();
    let a = run_and_hash(&mut machine, 10);
    assert!(
        a.windows(2).any(|pair| pair[0] != pair[1]),
        "the frames are not changing, so comparing them would prove nothing: \
         the fake ROM has stopped running or the display is blank"
    );
    let later = machine.save_state();
    assert_ne!(
        later, state,
        "ten frames of running have to change the machine, or comparing what a \
         restore puts back proves nothing either"
    );

    machine
        .load_state(&state)
        .expect("the state of this machine on this ROM loads");
    assert_eq!(
        machine.save_state(),
        state,
        "a restore has to put the machine back as its own state described it"
    );
    let b = run_and_hash(&mut machine, 10);

    assert_eq!(
        a, b,
        "ten frames after a restore are not the ten frames of never saving"
    );
}

/// A save state is the machine, not a memo about the machine that wrote it:
/// loading one into a second console that has never run a frame has to make the
/// two the same console, byte for byte, and let them run on identically.
///
/// This is the check a round trip on one machine cannot do. A field that a save
/// state quietly stopped carrying would be found here rather than at home, where
/// the machine already holds its own value for it: a console that has not run
/// holds the state a console that has never run holds, which is not the state of
/// frame thirty.
#[test]
fn state_transplants_to_a_fresh_machine() {
    let rom = colour_cycler();
    let mut first = Genesis::new(&rom, Config::default()).expect("loads");
    for _ in 0..30 {
        first.run_frame();
    }
    let state = first.save_state();
    assert!(
        first.bus().ram[..].iter().any(|&byte| byte != 0),
        "the fake ROM has to leave something in work RAM, or a state that lost \
         the RAM would go unnoticed here"
    );

    let mut fresh = Genesis::new(&rom, Config::default()).expect("loads");
    assert_ne!(
        fresh.save_state(),
        state,
        "a console that has run thirty frames and one that has not must differ, \
         or loading one from the other proves nothing"
    );

    fresh.load_state(&state).expect("loads");
    assert_eq!(
        fresh.save_state(),
        state,
        "the transplanted console is not the console that was saved"
    );
    let first = run_and_hash(&mut first, 5);
    let fresh = run_and_hash(&mut fresh, 5);
    assert_eq!(
        first, fresh,
        "the transplanted console does not go on drawing the same pictures"
    );
}
