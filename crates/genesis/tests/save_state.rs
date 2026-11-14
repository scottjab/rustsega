//! The versioned save state of `docs/save-state-format.md` (M6-07, #127).
//!
//! The container first - magic, version, ROM checksum, length - because that is
//! what a front end has to be able to trust before it touches a payload, and
//! then the machine: a state taken after thirty frames, put back, and run for
//! ten more, has to draw the same ten pictures as never saving at all.
//!
//! Up to the commercial round trip at the bottom, everything here runs on a fake
//! ROM: the format and the machine, not a cartridge, are under test. The last
//! three tests are the same rule on the games of `commercial_list.toml`, because
//! a fake ROM has no sound CPU, no save RAM and no mapper to get wrong.

mod common;

use genesis::{Buttons, Config, Genesis, StateError};

use common::{Game, load_games, panic_text, read_rom, run_bytes};

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

// ---------------------------------------------------------------------------
// The same round trip on the games people actually play (M6-08, #128).
//
// The tests above run on a fake ROM, which never releases the Z80 from reset and
// never declares a save. Anything only real cartridges use - the sound CPU
// running, cart SRAM, EEPROM, the Super Street Fighter II mapper, a game that
// writes the I/O chip mid-frame - is invisible to them, and the mutation testing
// of #127 showed exactly that: `Bus::z80_busreq`, `Bus::tmss_unlocked`,
// `Io::serial` and the two sound channels a fake never reached survived a
// mutation. A field missed by the state cannot be found with a machine that does
// not have it, so here is the machine that does.
//
// Both suites are the same rule, and both say which frame a divergence happened
// on rather than only that there was one: the first is a clue about what was
// lost, and "some frame differs" is not.
// ---------------------------------------------------------------------------

/// Frames a game runs before the save point.
///
/// `commercial_list.toml` puts its START presses at 300 and 301, so 300 is the
/// frame the last of them is held for: the game has booted, the sound CPU has
/// been let out of reset, and the state under test is taken on its way into the
/// first level rather than on the Sega trade mark.
const SAVE_AT: u32 = 300;

/// Frames compared on each side of the restore, about two seconds of game.
///
/// Per frame rather than once at the end: a field the state lost usually says
/// nothing until something reads it, and a single end-of-run hash would call a
/// run that went wrong halfway and a run that never went wrong the same thing.
/// The first diverging frame is what names the field.
const AFTER: u32 = 120;

/// The number of frames after a restore during which the mixed host output is
/// compared for its length but not its samples, because the mixer's output filters
/// are settling onto live signal and the run being compared against never stopped.
///
/// A load drops the mixer's filter memory - `crate::audio::Mixer::seek` resets the
/// DC blocker and the board's low-pass to their defaults, because a mixer that has
/// mixed nothing has default poles - so after a restore the filters start again
/// from zero. Against a reference that was never interrupted, which is what these
/// tests compare against, that costs a decaying difference on the first frames:
/// measured over the nineteen carts of `commercial_list.toml`, on both a
/// same-console and a fresh-console restore, the largest difference at the load is
/// about 7400 of 32768, and it falls by about eight times every frame - the 20 Hz
/// pole of the DC blocker - to exactly nothing. Every one of the thirty-eight runs
/// is bit-exact from frame +8; the worst is +7. Sixteen frames are allowed, so
/// this is a margin and not a threshold a cart is being fitted to.
///
/// The window is about this asymmetry only, and not about a limit in the restore.
/// Two consoles loaded with the same state have no uninterrupted side to be
/// measured against, so they should agree exactly from the first frame after the
/// load - the stronger property of
/// [#451](https://github.com/scottjab/rustsega/issues/451).
///
/// This is not a licence to ignore the mixer. Every frame after the window is
/// compared sample for sample, the sample *count* of every frame is compared
/// including these, and the chips before the mixer are compared on every frame by
/// every test here. It is the filters' convergence only that is allowed to differ,
/// only where the measurement says it does, and only for a quarter of a second.
const MIXER_SETTLE_FRAMES: u32 = 16;

/// What a round trip judges a restore on, beyond the picture, the machine's own
/// sound and the cart's save.
#[derive(Debug, Clone, Copy)]
struct Compare {
    /// Put the state into a console built from the same ROM and never run, rather
    /// than into the console that wrote it.
    into_a_fresh_console: bool,
    /// Compare [`Genesis::drain_audio`] as well as [`chip_audio_hash`].
    ///
    /// The mixed output is compared exactly from [`MIXER_SETTLE_FRAMES`] frames
    /// after the restore, and for its length alone inside that window; see there
    /// for why the filters make anything else a lie. Every frame of every host-
    /// audio test is length-checked, which is what catches
    /// [#362](https://github.com/scottjab/rustsega/issues/362): the defect was
    /// four frames of no samples at all, and it is caught on the frame it happens.
    host_audio: bool,
}

/// One frame of a run, as the round trip judges it.
#[derive(Debug, Clone, Copy)]
struct FrameStamp {
    /// This frame's number in the game's own run - 1-based, like
    /// [`common::run_frames`]' inputs - so a failure says which frame the game
    /// was on rather than an index into a list.
    frame: u32,
    /// The picture.
    video: u64,
    /// The two sound chips' frame, before the host's mixer: see
    /// [`chip_audio_hash`] for why a save state is judged on that as well.
    chips: u64,
    /// How many samples [`Genesis::drain_audio`] handed the host, compared on
    /// every frame a mixed-output comparison runs on, including the frames
    /// whose samples are inside [`MIXER_SETTLE_FRAMES`] and so not compared.
    ///
    /// The length is the part of the mixer's output that a restore has to get
    /// right however the filters are remembering: it is the mixer's position on
    /// its own output grid, and the defect of
    /// [#362](https://github.com/scottjab/rustsega/issues/362) was a mixer left
    /// behind the machine, which shows here as frames of no samples at all.
    mixed_len: usize,
    /// What [`Genesis::drain_audio`] handed the host. Compared from
    /// [`MIXER_SETTLE_FRAMES`] frames after the restore; see there.
    mixed: u64,
}

/// The frame's sound as the machine made it: both chips' samples, and the master
/// clock each one starts at.
///
/// This is the audio a save state is responsible for - `ym2612` and `sn76489`'s
/// own state, the sample buffers and the chips' positions in the master clock
/// (`crate::audio::Audio::frame`) - and none of it can be seen in a picture,
/// which is why a state that lost it would draw a perfect game and be silent or
/// out of step for ever after.
fn chip_audio_hash(machine: &Genesis) -> u64 {
    let frame = machine.bus().audio.frame();
    let mut bytes = Vec::new();
    for (left, right) in frame.ym {
        bytes.extend_from_slice(&left.to_le_bytes());
        bytes.extend_from_slice(&right.to_le_bytes());
    }
    for sample in frame.psg {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    for index in [frame.ym_first, frame.psg_first] {
        bytes.extend_from_slice(&index.to_le_bytes());
    }
    testkit::fnv1a_64(&bytes)
}

/// The frame's sound as the host heard it: the hash of what
/// [`Genesis::drain_audio`] appended, and how many samples that was.
///
/// Both, because the two are compared on different frames. The count is the
/// mixer's position on its own output grid and is compared on every frame; the
/// samples are compared once the mixer's output filters have settled, which is
/// what [`MIXER_SETTLE_FRAMES`] is for.
fn drain_audio_hash(machine: &mut Genesis) -> (u64, usize) {
    let mut samples = Vec::new();
    machine.drain_audio(&mut samples);
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    (testkit::fnv1a_64(&bytes), samples.len())
}

/// Run `frames` frames from the absolute frame `first`, holding the pad as
/// `inputs` says (the 1-based rule of [`common::run_frames`], so the START
/// presses of a game's list entry mean the same thing on both sides of a
/// restore), and stamp every one of them.
fn run_stamped(
    machine: &mut Genesis,
    first: u32,
    frames: u32,
    inputs: &[(u32, Buttons)],
) -> Vec<FrameStamp> {
    (0..frames)
        .map(|offset| {
            let frame = first + offset;
            let buttons = inputs
                .iter()
                .find(|(number, _)| *number == frame)
                .map_or_else(Buttons::empty, |(_, buttons)| *buttons);
            machine.set_pad(0, buttons);
            machine.run_frame();
            let (mixed, mixed_len) = drain_audio_hash(machine);
            FrameStamp {
                frame,
                video: frame_hash(machine),
                chips: chip_audio_hash(machine),
                mixed_len,
                mixed,
            }
        })
        .collect()
}

impl FrameStamp {
    /// Why this frame is not the frame `want` was, as `word -> want got` for
    /// each hash that moved, or `None` when nothing did.
    fn diff(&self, want: &FrameStamp, judge: Compare) -> Option<String> {
        let mut moved = Vec::new();
        if self.video != want.video {
            moved.push(format!("picture {:x} {:x}", want.video, self.video));
        }
        if self.chips != want.chips {
            moved.push(format!("chips {:x} {:x}", want.chips, self.chips));
        }
        if judge.host_audio {
            // The count is compared on every frame, the settling window included:
            // a mixer left behind the machine is a frame with no samples in it,
            // which is the defect and it is not something the filters excuse.
            if self.mixed_len != want.mixed_len {
                moved.push(format!(
                    "output samples {} {}",
                    want.mixed_len, self.mixed_len
                ));
            }
            // And the samples, from the frame the mixer's filters have settled by.
            if self.settled() && self.mixed != want.mixed {
                moved.push(format!("output {:x} {:x}", want.mixed, self.mixed));
            }
        }
        (!moved.is_empty()).then(|| format!("frame {}: {}", self.frame, moved.join(", ")))
    }

    /// Is this frame far enough past the state taken at [`SAVE_AT`] that the mixed
    /// output is compared sample for sample? [`MIXER_SETTLE_FRAMES`] says why the
    /// first frames after a restore are not.
    fn settled(&self) -> bool {
        self.frame > SAVE_AT.saturating_add(MIXER_SETTLE_FRAMES)
    }
}

/// Every frame of `after` that is not the frame `before` was, every one of them.
fn divergences(before: &[FrameStamp], after: &[FrameStamp], judge: Compare) -> Vec<String> {
    before
        .iter()
        .zip(after)
        .filter_map(|(want, got)| got.diff(want, judge))
        .collect()
}

/// One game: run it to [`SAVE_AT`], take the state, run [`AFTER`] frames as the
/// reference, then put the state back - into that console, or into a second
/// console built from the same ROM and never run - and run the same frames again.
///
/// `None` when the cartridge is not on disk: a skip, never a failure, because CI
/// has no games (docs/TESTING.md, "External test data").
fn round_trip(game: &Game, judge: Compare) -> Option<Vec<String>> {
    let rom = testkit::commercial_rom(&format!("{}.md", game.prefix))?;
    let bytes = read_rom(&rom)?;
    let inputs: Vec<(u32, Buttons)> = game
        .start_press
        .iter()
        .map(|&frame| (frame, Buttons::START))
        .collect();

    let mut machine = run_bytes(&bytes, SAVE_AT, &inputs);
    let state = machine.save_state();
    // The cart's save as the state is meant to carry it, hashed here at the save
    // point: [`Genesis::sram`] is the battery-backed RAM itself, and a game that
    // never reads it back inside [`AFTER`] frames would go on drawing a perfect
    // picture with a state that had quietly dropped it - which is why #127's
    // mutation testing of it survived, the cart it mutated had no save at all.
    let saved = machine.sram().map(testkit::fnv1a_64);
    let before = run_stamped(&mut machine, SAVE_AT + 1, AFTER, &inputs);
    assert_ne!(
        machine.save_state(),
        state,
        "{}: {} frames of running left the machine exactly as it was, so a state \
         that lost everything would pass this test too",
        game.key,
        AFTER,
    );

    let mut restored = if judge.into_a_fresh_console {
        Genesis::new(&bytes, Config::default())
            .unwrap_or_else(|e| panic!("{}: cannot build a second console: {e}", game.key))
    } else {
        machine
    };
    restored.load_state(&state).unwrap_or_else(|e| {
        panic!(
            "{}: a state this console wrote for itself was refused: {e}",
            game.key
        )
    });
    // The cart's own save, machine state like any other and the one thing here
    // that no frame would miss losing. A cart with no save says nothing either
    // way, so `None` on both sides is not a difference.
    if saved.is_some() {
        assert_eq!(
            restored.sram().map(testkit::fnv1a_64),
            saved,
            "{}: the cart's save RAM is not the save the state was written with",
            game.key,
        );
    }
    // And the state of the restored console, byte for byte the state that went
    // in: a field that is encoded but restored wrong - a latch that a `Bus` built
    // for a load puts back at its default - says nothing about the next 120
    // frames and everything about a state that does not mean what it says.
    assert_eq!(
        restored.save_state(),
        state,
        "{}: a console loaded with its own state does not describe itself as that \
         state any more, so the state did not put back something it carried",
        game.key,
    );
    let after = run_stamped(&mut restored, SAVE_AT + 1, AFTER, &inputs);

    Some(divergences(&before, &after, judge))
}

/// A cart held out of the round trip for a cause that is not the save state.
///
/// This is not a filter, and the difference matters. A held-out cart is still
/// run, and its frames are still compared; what changes is only which answer is
/// expected. Divergence is reported and passes the sweep. Going clean is a
/// failure, with the issue named, so the list cannot quietly outlive the bug it
/// was written for - which is the only reason a list like this is safe to have.
struct HeldOut {
    /// The `commercial_list.toml` key, checked against the list at run time: a
    /// cart renamed out from under an entry here is reported, not ignored.
    key: &'static str,
    /// The issue that tracks the cause.
    issue: &'static str,
    /// Why this is not a save-state bug.
    why: &'static str,
}

const HELD_OUT: &[HeldOut] = &[];

/// How many of one game's diverging frames a failure names before the count of
/// the rest stands in for them: a game that diverges at all usually diverges
/// every frame, and nineteen of those at 120 lines apiece cannot be read.
const REPORT_FRAMES: usize = 12;

/// One game's line(s) in a sweep's failure list.
fn game_failure(key: &str, frames: &[String]) -> String {
    let named = frames
        .iter()
        .take(REPORT_FRAMES)
        .map(|frame| format!("  {key}: {frame}"))
        .collect::<Vec<_>>()
        .join("\n");
    match frames.len().checked_sub(REPORT_FRAMES) {
        None | Some(0) => named,
        Some(more) => {
            format!("{named}\n  {key}: ...and {more} more of the {AFTER} frames compared")
        }
    }
}

/// The whole list, one game at a time, every result reported: see
/// `commercial.rs`'s module comment for why a commercial suite collects rather
/// than asserts as it goes, and [`common::panic_text`] for why a panic in one
/// game is that game's result rather than the end of the run.
fn round_trip_sweep(judge: Compare) {
    let mut passed = 0;
    let mut skipped = 0;
    let mut failures: Vec<String> = Vec::new();

    let mut held_out = 0;
    let mut seen: Vec<&'static str> = Vec::new();

    for game in load_games() {
        let held = HELD_OUT.iter().find(|entry| entry.key == game.key);
        if let Some(entry) = held {
            seen.push(entry.key);
        }
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| round_trip(&game, judge)));
        match outcome {
            Ok(Some(frames)) if frames.is_empty() => match held {
                // A cart on the list went clean, so the list is wrong now.
                Some(entry) => failures.push(format!(
                    "{}: held out as {} but it no longer diverges - the round trip is \n\n         clean, so remove it from HELD_OUT",
                    game.key, entry.issue
                )),
                None => passed += 1,
            },
            Ok(Some(frames)) => match held {
                Some(entry) => {
                    held_out += 1;
                    eprintln!(
                        "  {} held out ({}): {}",
                        game.key, entry.issue, entry.why
                    );
                    let _ = frames;
                }
                None => failures.push(game_failure(&game.key, &frames)),
            },
            Ok(None) => skipped += 1,
            Err(payload) => failures.push(format!(
                "{}: panicked: {}",
                game.key,
                panic_text(payload.as_ref()).trim()
            )),
        }
    }

    // An entry naming no cart in the list is as stale as one whose cart went
    // clean, and it is just as easy to miss, so it is checked rather than assumed.
    for entry in HELD_OUT {
        if !seen.contains(&entry.key) {
            failures.push(format!(
                "HELD_OUT names {key}, which is not in commercial_list.toml",
                key = entry.key
            ));
        }
    }

    eprintln!(
        "save state round trip: {passed} passed, {skipped} skipped, {held_out} held out, {} failed",
        failures.len()
    );
    if !failures.is_empty() {
        panic!(
            "{} game(s) diverged after a save state restore:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// A game restored from its own state goes on playing the game it was playing.
///
/// The state is taken at frame [`SAVE_AT`], mid-`START`-press, and [`AFTER`]
/// frames of picture and chip sound are compared frame by frame against the same
/// [`AFTER`] frames from a console that was loaded with that state. Anything the
/// state does not carry shows up here: #127 proved the format on a fake ROM that
/// has no sound CPU, no save RAM and no mapper, and those are exactly the parts
/// of a machine a real game uses within the first five seconds.
///
/// The host-facing audio is not compared on this one, and that is a division of
/// labour rather than a hedge: this test asks what the *state* carried, so it
/// judges sound by the samples the machine made, which is the part a state can get
/// wrong and the part a host's resampler cannot. What the mixer does with those
/// samples is the other two tests' question, and they answer it -
/// [`commercial_save_state_is_audible_after_a_load`] on the console that wrote the
/// state and [`commercial_save_state_transplants_to_a_fresh_console`] on one that
/// never ran.
///
/// Until [#362](https://github.com/scottjab/rustsega/issues/362) the split was not
/// clean, because a console reloaded with a state three seconds older than its
/// mixer was a console the mixer had run past, and that silenced the mixed output
/// without touching this test. It does not any more: a load re-anchors the mixer
/// (`crate::audio::Mixer::seek`), and the mixed output is compared with the state
/// in both of the tests that look at it.
///
/// Needs cartridges, so CI skips it and `just test-commercial` runs it.
#[test]
#[ignore = "needs commercial ROMs; run with `just test-commercial`"]
fn commercial_save_state_round_trip_is_deterministic() {
    round_trip_sweep(Compare {
        into_a_fresh_console: false,
        host_audio: false,
    });
}

/// The same rule on a second console: a state is the machine, not a memo about
/// the console that wrote it.
///
/// A state written by a console that has run three hundred frames goes into one
/// that has run none, and the two have to go on being one machine. This is the
/// stronger of the two because it is the one a player meets - load a slot in a
/// fresh emulator - and the one a same-console round trip cannot do: a field the
/// state stopped carrying is still in the console that wrote it, so only a
/// console that never had it can notice it is gone.
///
/// The host-facing audio is compared here as well, because both sides of this
/// comparison see the same samples at the same master clocks: a state that lost a
/// chip, a sample buffer or a chip clock cannot produce the same output. The two
/// mixers are put in the same place by the restore, which is what
/// [`crate::audio::Mixer::seek`] does and what
/// [#362](https://github.com/scottjab/rustsega/issues/362) was about: a state
/// loaded into a console whose mixer had never mixed a sample used to leave it
/// behind the machine, and the first thing out of it was silence.
///
/// The output filters of the two sides are not the same and cannot be: the console
/// that wrote the state has a mixer holding what it mixed on the way to the state,
/// and this one has never mixed anything. That is [`MIXER_SETTLE_FRAMES`] frames
/// of decaying difference and then exactly nothing, and it is the mixer's own
/// memory rather than the machine's.
///
/// Needs cartridges, so CI skips it and `just test-commercial` runs it.
#[test]
#[ignore = "needs commercial ROMs; run with `just test-commercial`"]
fn commercial_save_state_transplants_to_a_fresh_console() {
    round_trip_sweep(Compare {
        into_a_fresh_console: true,
        host_audio: true,
    });
}

/// The host-facing audio across a restore into the console that wrote the state.
///
/// This is [`commercial_save_state_round_trip_is_deterministic`] with the mixer's
/// own output compared as well. Before
/// [#362](https://github.com/scottjab/rustsega/issues/362) it failed, and not
/// quietly: a console loaded back into a state older than its own mixer heard
/// nothing for as many frames as it had run after that state, because
/// `Mixer::mix` mixes only forward from the master clock the mixer has reached and
/// a load did not move it back with the machine. Four frames of the twelve
/// compared were literally empty - zero samples, not silence - on all nineteen
/// carts, against a reference of about 1602 samples a frame. It is green now that
/// [`crate::Genesis::load_state`] re-anchors the mixer on the machine it has put
/// back, and this is the test that says so.
///
/// What it holds to: every frame's sample count, and every frame's samples from
/// [`MIXER_SETTLE_FRAMES`] after the restore. The frames before that are the
/// mixer's output filters re-settling from a past the machine no longer has, which
/// no save state carries and none can - see [`MIXER_SETTLE_FRAMES`] and
/// `docs/save-state-format.md`.
#[test]
#[ignore = "needs commercial ROMs; run with `just test-commercial`"]
fn commercial_save_state_is_audible_after_a_load() {
    round_trip_sweep(Compare {
        into_a_fresh_console: false,
        host_audio: true,
    });
}
