//! Save-state round trip of the Z80 state (M6-07a, #126).
//!
//! Run the CPU into a non-default state, serialise with postcard, read it back,
//! then run both copies for more instructions and require the same result.
//!
//! `Z80` has no `PartialEq`, so the copies are compared by their `Debug` view,
//! which prints every field including the hidden `WZ`, `q`, `p` and the
//! interrupt latches — the fields a save state most easily loses.

use z80::{IntMode, Z80, Z80Bus};

/// Program at `0x0000`: `LD B,0x20` / `LD A,1` / loop `ADD A,A` / `DJNZ loop` /
/// `LD (0x9000),A` / `HALT`. A counted loop means a snapshot taken in the
/// middle of it carries `B`, `A`, the flags and `PC` all at once.
const PROGRAM: [u8; 11] = [
    0x06, 0x20, // LD B,0x20
    0x3E, 0x01, // LD A,1
    0x87, // ADD A,A       (0x0004, the DJNZ target)
    0x10, 0xFD, // DJNZ 0x0004
    0x32, 0x00, 0x90, // LD (0x9000),A
    0x76, // HALT
];

/// Instructions run before the snapshot: inside the loop, so the state is
/// mid-run rather than at one of its ends.
const STEPS_BEFORE: usize = 5;

/// Instructions run on both copies after the round trip: enough to leave the
/// loop, store to memory and settle into the `HALT`.
const STEPS_AFTER: usize = 50;

/// 64 KiB of RAM with the program at `0x0000`, and no ports: enough to run the
/// program above and read the stored byte back.
struct Ram {
    mem: Vec<u8>,
}

impl Ram {
    fn new() -> Self {
        let mut mem = vec![0; 0x1_0000];
        mem[0..PROGRAM.len()].copy_from_slice(&PROGRAM);
        Self { mem }
    }
}

impl Z80Bus for Ram {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[usize::from(addr)]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.mem[usize::from(addr)] = value;
    }

    fn io_read(&mut self, _port: u16) -> u8 {
        0xFF
    }

    fn io_write(&mut self, _port: u16, _value: u8) {}
}

#[test]
fn state_roundtrip() {
    let mut cpu = Z80::new();
    cpu.set_im(IntMode::Mode2);
    cpu.set_i(0xC0);
    let mut bus = Ram::new();
    for _ in 0..STEPS_BEFORE {
        cpu.step(&mut bus);
    }
    // The `STEPS_BEFORE` steps are `LD B,0x20`, `LD A,1`, `ADD A,A`, `DJNZ`,
    // `ADD A,A`, so the snapshot lands inside the loop one trip in: `B` is down
    // by one, `A` has doubled twice, and `PC` is back on the `DJNZ` that holds
    // it there. The point is that the state is mid-program, not at either end.
    assert_eq!(cpu.b(), 0x20 - 1, "one trip round the loop so far");
    assert_eq!(cpu.a(), 0x04, "two doubles of the 1 `LD A,1` loaded");
    assert_eq!(cpu.pc(), 0x0005, "on the `DJNZ` of the loop");
    assert_ne!(cpu.af(), 0xFFFF, "the run must leave a non-default AF");

    let bytes = postcard::to_allocvec(&cpu).expect("serialise Z80");
    let mut restored: Z80 = postcard::from_bytes(&bytes).expect("deserialise Z80");
    assert_eq!(
        format!("{restored:?}"),
        format!("{cpu:?}"),
        "the state survives the trip unchanged"
    );

    // Both copies run on their own copy of the bus, so a difference in the
    // bytes the program stores is a difference in state too.
    let mut bus_ran = Ram::new();
    let mut bus_restored = Ram::new();
    for _ in 0..STEPS_AFTER {
        cpu.step(&mut bus_ran);
        restored.step(&mut bus_restored);
    }

    assert_eq!(
        format!("{restored:?}"),
        format!("{cpu:?}"),
        "the two copies ended in different states"
    );
    assert_eq!(
        bus_ran.mem[0x9000], bus_restored.mem[0x9000],
        "the two copies stored different bytes"
    );
}

/// The half of the Z80 the program above leaves at its default: the shadow bank,
/// `IX`/`IY`, the two interrupt flip-flops, the EI latch, `WZ`, `q`, `p`, the
/// vector, and the NMI and INT lines.
///
/// Those are set through the accessors `tests/harness` uses for the
/// conformance suite, because no instruction sequence in this file reaches them,
/// and the round trip has to carry them anyway: a save state that lost `iff1`
/// or `nmi_pending` resumes a chip that never takes the interrupt it was in the
/// middle of taking, and one that lost `WZ`, `q` or `p` fails the very next
/// undocumented `X`/`Y` flag test. The `Debug` comparison is the assertion - it
/// prints all of them - and the run after it says the poked state is still
/// runnable rather than merely restorable.
#[test]
fn state_roundtrip_hidden_state() {
    let mut cpu = Z80::new();
    let mut bus = Ram::new();
    for _ in 0..STEPS_BEFORE {
        cpu.step(&mut bus);
    }

    cpu.set_im(IntMode::Mode2);
    cpu.set_i(0xC0);
    cpu.set_int_vector(0xC7);
    cpu.set_af_shadow(0x1234);
    cpu.set_bc_shadow(0x5678);
    cpu.set_de_shadow(0x9ABC);
    cpu.set_hl_shadow(0xDEF0);
    cpu.set_ix(0x1A2B);
    cpu.set_iy(0x3C4D);
    cpu.set_iff1(true);
    cpu.set_ei_pending(true);
    cpu.set_wz(0xE0A0);
    cpu.set_q(0x80);
    cpu.set_p(true);
    cpu.set_sp(0xFFFE);
    cpu.set_r(0x5A);
    // The two input lines, last: they are what the next `step` reads, and the
    // state on both sides of the trip has to hold an interrupt that has arrived
    // but not been taken.
    cpu.set_int_line(true);
    cpu.nmi();

    let bytes = postcard::to_allocvec(&cpu).expect("serialise Z80");
    let mut restored: Z80 = postcard::from_bytes(&bytes).expect("deserialise Z80");
    assert_eq!(
        format!("{restored:?}"),
        format!("{cpu:?}"),
        "the hidden state survives the trip unchanged"
    );

    let mut bus_ran = Ram::new();
    let mut bus_restored = Ram::new();
    for _ in 0..STEPS_AFTER {
        cpu.step(&mut bus_ran);
        restored.step(&mut bus_restored);
    }

    assert_eq!(
        format!("{restored:?}"),
        format!("{cpu:?}"),
        "the two copies ended in different states"
    );
    assert_eq!(
        bus_ran.mem[0x9000], bus_restored.mem[0x9000],
        "the two copies stored different bytes"
    );
}
