//! Save-state round trip of the PSG state (M6-07a, #126).
//!
//! Write the registers into a non-default state, run some /16 clock ticks,
//! serialise with postcard, read it back, then clock both copies and require the
//! same samples and the same state afterwards.
//!
//! The samples are the real assertion: a restored chip whose tone counters were
//! back at zero, or whose noise shift register had been reset to
//! [`sn76489::Sn76489::new`]'s seed, would still accept writes and still return
//! *a* sample, but the waveform of the second half would not be the waveform the
//! first half was mid-way through. `Sn76489` has no `PartialEq`, so the state
//! itself is compared through its `Debug` view, which prints every field of the
//! four channels, the latch, the noise generator and the channel 3 connection.

use sn76489::Sn76489;

/// Register writes that make the chip non-default: two tone periods, one volume,
/// and the noise control — which latches on channel 3 and therefore also moves
/// channel 3's output onto the noise generator.
///
/// The bytes are the `1 cc t dddd` latch form of SMS Power "SN76489", register
/// writes: `8F`/`3C` is channel 0 tone period 0x3CF, `90` is channel 0 volume 0
/// (loudest), `AF` is channel 1 tone period 0x00F on its own, and `E4` is the
/// noise control — white, shift rate 0, so period 0x10 and not channel 3's tone
/// register — which also moves channel 3's output onto the noise generator.
fn patched() -> Sn76489 {
    let mut psg = Sn76489::new();
    psg.write(0x8F); // channel 0 tone, low nibble 0xF
    psg.write(0x3C); // and the high six bits: period 0x3CF
    psg.write(0x90); // channel 0 volume 0
    psg.write(0xAF); // channel 1 tone, period 0x00F
    psg.write(0xE4); // noise: white, shift rate 0; channel 3 now carries it
    psg
}

/// Ticks run before the snapshot. Not a multiple of any of the three periods in
/// [`patched`], so the counters are caught mid-cycle rather than at a reload.
const TICKS_BEFORE: usize = 137;

/// Ticks run on both copies after the round trip: long enough that a lost
/// counter or a lost output bit shows up in the samples rather than by luck.
const TICKS_AFTER: usize = 2000;

/// Clock `psg` `count` times and collect what it puts out.
fn clock_for(psg: &mut Sn76489, count: usize) -> Vec<i16> {
    (0..count).map(|_| psg.clock()).collect()
}

#[test]
fn state_roundtrip() {
    let mut psg = patched();
    let before = clock_for(&mut psg, TICKS_BEFORE);
    assert_ne!(
        format!("{psg:?}"),
        format!("{:?}", Sn76489::new()),
        "the run must leave a non-default state or the round trip proves nothing"
    );
    assert_eq!(
        psg.tone_period(0),
        0x3CF,
        "and it is the one the writes made"
    );
    assert_eq!(psg.noise_control(), 0b100, "white noise, shift rate 0");
    assert!(
        before.iter().any(|sample| *sample != 0),
        "a chip that never made a sample cannot show a restored one going wrong"
    );

    let bytes = postcard::to_allocvec(&psg).expect("serialise Sn76489");
    let mut restored: Sn76489 = postcard::from_bytes(&bytes).expect("deserialise Sn76489");
    assert_eq!(
        format!("{restored:?}"),
        format!("{psg:?}"),
        "the state survives the trip unchanged"
    );

    // And the restored chip is runnable on its own: no write, no re-seed, just
    // clocks — and it follows the copy that was never serialised.
    let ran = clock_for(&mut psg, TICKS_AFTER);
    let ran_restored = clock_for(&mut restored, TICKS_AFTER);
    assert_eq!(
        ran, ran_restored,
        "the two copies produced different samples"
    );
    assert!(
        ran.iter().any(|sample| *sample != 0),
        "the post-trip run is silent, so comparing it proved nothing"
    );
    assert_eq!(
        format!("{restored:?}"),
        format!("{psg:?}"),
        "the two copies ended in different states"
    );
}

/// The latch survives too: a chip restored just after a latch byte, before its
/// data byte, still takes that data byte for the register the latch names.
#[test]
fn state_roundtrip_mid_latch_pair() {
    let mut psg = patched();
    psg.write(0x80); // latch channel 0 tone; the low nibble write has not landed
    psg.clock();

    let bytes = postcard::to_allocvec(&psg).expect("serialise Sn76489");
    let mut restored: Sn76489 = postcard::from_bytes(&bytes).expect("deserialise Sn76489");

    psg.write(0x2A);
    restored.write(0x2A);
    let ran = clock_for(&mut psg, TICKS_AFTER);
    let ran_restored = clock_for(&mut restored, TICKS_AFTER);
    assert_eq!(
        ran, ran_restored,
        "the data byte after the round trip went somewhere else"
    );
    assert_eq!(restored.tone_period(0), psg.tone_period(0));
}
