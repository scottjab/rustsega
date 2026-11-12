//! Save-state round trip of the FM chip state (M6-07a, #126).
//!
//! Write the chip into a non-default state, clock some samples, serialise with
//! postcard, read it back, then clock both copies and require the same samples
//! and the same state afterwards.
//!
//! The samples are the part that says the restored chip is *runnable*: a lost
//! phase counter, envelope state or LFO step still leaves a chip that answers
//! `clock` with a number, it just is not the next number of the same waveform.
//! The state comparison is `postcard` bytes rather than `Debug`, because
//! [`Ym2612`] deliberately has no `Debug` (see `crates/genesis/src/audio.rs`)
//! and the serde encoding is the whole state anyway.

use ym2612::Ym2612;

/// The two part I ports: A1/A0 of the chip decoded to address-latch and
/// data-write. `ym2612` keeps these private, and they are the two the Z80
/// reaches through `$4000`/`$4002` (YM2608 application manual
/// "3. Register descriptions").
const PORT_ADDRESS_1: u8 = 0;
const PORT_DATA_1: u8 = 1;

/// Latch `register` on part I's address port and write `value` to its data port.
fn reg(chip: &mut Ym2612, register: u8, value: u8) {
    chip.write(PORT_ADDRESS_1, register);
    chip.write(PORT_DATA_1, value);
}

/// A chip with the LFO running, both timers loaded and enabled, and a voice on
/// channel 1 that is mid-attack: the phase counters, the envelopes, the LFO
/// counter, both timer counters and the BUSY counter are all live at the moment
/// of the snapshot.
///
/// The `&A4` before `&A0` is deliberate - a part I frequency write latches the
/// block and the top three F-Num bits and only applies them with the `&A0` write
/// that follows (YM2608 application manual `&A0`/`&A4`), so the latch itself is
/// part of what has to survive.
fn patched() -> Ym2612 {
    let mut chip = Ym2612::new();
    // `&22`: LFO on, frequency 7, the fastest triangle there is.
    reg(&mut chip, 0x22, 0x0F);
    // `&24`-`&27`: both timers loaded and enabled, so both counters run and the
    // two status flags get set while the chip is being clocked.
    reg(&mut chip, 0x24, 0xFF);
    reg(&mut chip, 0x25, 0x03);
    reg(&mut chip, 0x26, 0x40);
    reg(&mut chip, 0x27, 0x0F);
    // A voice on the four slots of channel 1: multiply by 1, a per-slot total
    // level, and three rates up so the envelope machine is really moving.
    for slot in 0..4u8 {
        reg(&mut chip, 0x30 + 4 * slot, 0x01);
        reg(&mut chip, 0x40 + 4 * slot, slot);
        reg(&mut chip, 0x50 + 4 * slot, 0x1F);
        reg(&mut chip, 0x60 + 4 * slot, 0x0F);
        reg(&mut chip, 0x70 + 4 * slot, 0x1F);
    }
    reg(&mut chip, 0xA4, 0x24);
    reg(&mut chip, 0xA0, 0x11);
    // `&B0`: feedback 3, algorithm 3. Then key on all four slots.
    reg(&mut chip, 0xB0, 0x1B);
    reg(&mut chip, 0x28, 0xF0);
    chip
}

/// Samples run before the snapshot: long enough to move the timers, the LFO and
/// the envelopes, short enough to stay mid-flight.
const SAMPLES_BEFORE: usize = 500;

/// Samples run on both copies after the round trip. Several times the length of
/// the fastest envelope and of the LFO cycle, so a sub-generator that was
/// restored to its reset state cannot hide inside the run.
const SAMPLES_AFTER: usize = 4000;

/// The whole chip state as canonical bytes: postcard writes a given state one
/// way, so equal bytes is the same claim as "every field is equal".
fn state(chip: &Ym2612) -> Vec<u8> {
    postcard::to_allocvec(chip).expect("serialise Ym2612")
}

/// Assert two chips hold the same state, naming the first byte that moved rather
/// than dumping two register banks into the failure message.
fn assert_same_state(one: &Ym2612, other: &Ym2612, message: &str) {
    let (left, right) = (state(one), state(other));
    if left != right {
        let first = left
            .iter()
            .zip(&right)
            .position(|(a, b)| a != b)
            .map_or_else(
                || format!("{} (length only)", left.len()),
                |index| index.to_string(),
            );
        panic!("{message}: state differs from byte {first}");
    }
}

/// Clock `chip` `count` times and collect what it puts out.
fn clock_for(chip: &mut Ym2612, count: usize) -> Vec<(i16, i16)> {
    (0..count).map(|_| chip.clock()).collect()
}

#[test]
fn state_roundtrip() {
    let mut chip = patched();
    let before = clock_for(&mut chip, SAMPLES_BEFORE);
    assert_ne!(
        state(&chip),
        state(&Ym2612::new()),
        "the run must leave a non-default state or the round trip proves nothing"
    );
    assert!(
        before.iter().any(|(left, right)| *left != 0 || *right != 0),
        "a chip that never made a sample cannot show a restored one going wrong"
    );

    let bytes = postcard::to_allocvec(&chip).expect("serialise Ym2612");
    let mut restored: Ym2612 = postcard::from_bytes(&bytes).expect("deserialise Ym2612");
    assert_same_state(&chip, &restored, "the state survived the trip");
    assert_eq!(
        restored.read_status(),
        chip.read_status(),
        "the status byte reads differently on the restored chip"
    );

    // And the restored chip runs on its own: no further writes, only clocks.
    let ran = clock_for(&mut chip, SAMPLES_AFTER);
    let ran_restored = clock_for(&mut restored, SAMPLES_AFTER);
    assert_eq!(
        ran, ran_restored,
        "the two copies produced different samples"
    );
    assert!(
        ran.iter().any(|(left, right)| *left != 0 || *right != 0),
        "the post-trip run is silent, so comparing it proved nothing"
    );
    assert_same_state(&chip, &restored, "the two copies ended differently");
}

/// The register banks are the biggest thing in the state and the easiest thing
/// for a save state to lose, so this one checks a pair of registers that only
/// the banks themselves could carry: `&2A` and `&2B`, the PCM DAC.
#[test]
fn dac_registers_survive_the_trip() {
    let mut chip = Ym2612::new();
    reg(&mut chip, 0x2B, 0x80); // DAC enable, channel 6
    reg(&mut chip, 0x2A, 0xC0); // and a sample well off the centre
    let before = clock_for(&mut chip, 8);

    let mut untouched = Ym2612::new();
    let untouched = clock_for(&mut untouched, 8);
    assert_ne!(
        before, untouched,
        "the `&2A`/`&2B` pair has to be audible, or this test proves nothing"
    );

    let bytes = postcard::to_allocvec(&chip).expect("serialise Ym2612");
    let mut restored: Ym2612 = postcard::from_bytes(&bytes).expect("deserialise Ym2612");
    assert_same_state(&chip, &restored, "the DAC registers survived the trip");

    let ran = clock_for(&mut chip, 256);
    let ran_restored = clock_for(&mut restored, 256);
    assert_eq!(
        ran, ran_restored,
        "a restored chip does not mix the DAC the way the chip that wrote it does"
    );
}
