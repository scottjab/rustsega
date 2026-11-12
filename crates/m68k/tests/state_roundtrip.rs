//! Save-state round trip of the 68000 state (M6-07a, #126).
//!
//! The ticket's test: run the CPU into a non-default state, serialise it with
//! postcard, read it back, then run both copies for more instructions and
//! require the same result. `M68k` is `PartialEq` over every field including the
//! private ones, so the comparison covers the prefetch queue, the pending
//! address error, the IPL edge detector and the cycle total, not just the
//! visible register file.

use m68k::M68k;
use m68k::testbus::cpu_with_program;

/// A program that leaves the CPU with registers, a memory write and a queue
/// behind, and ends in a self-loop so the steps after the round trip stay
/// inside instructions rather than running into the illegal opcode `0x0000`.
///
/// `MOVEQ #0x5A,D0` / `MOVE.B D0,($2000).W` / `MOVEQ #2,D1` / `BRA #-2`, the
/// encodings of `ops::move_` and `ops::flow`'s tests.
const PROGRAM: [u16; 5] = [0x705A, 0x11C0, 0x2000, 0x7202, 0x60FE];

/// Instructions run before the snapshot: enough to have written memory and
/// filled two data registers.
const STEPS_BEFORE: usize = 3;

/// Instructions run on both copies after the round trip.
const STEPS_AFTER: usize = 20;

#[test]
fn state_roundtrip() {
    let (mut cpu, mut bus) = cpu_with_program(&PROGRAM);
    for _ in 0..STEPS_BEFORE {
        cpu.step(&mut bus);
    }
    assert_ne!(
        cpu,
        M68k::new(),
        "the run must leave a non-default state or the round trip proves nothing"
    );
    assert_eq!(cpu.d[0], 0x5A, "and that state is the one the program made");

    let bytes = postcard::to_allocvec(&cpu).expect("serialise M68k");
    let mut restored: M68k = postcard::from_bytes(&bytes).expect("deserialise M68k");
    assert_eq!(cpu, restored, "the state survives the trip unchanged");

    // Both copies run on their own copy of the bus the snapshot was taken on,
    // so a difference in any later bus write shows up too.
    let mut bus_ran = bus.clone();
    let mut bus_restored = bus.clone();
    for _ in 0..STEPS_AFTER {
        cpu.step(&mut bus_ran);
        restored.step(&mut bus_restored);
    }

    assert_eq!(cpu, restored, "the two copies ended in different states");
    assert!(
        bus_ran.mem == bus_restored.mem,
        "the two copies wrote different memory"
    );
}
