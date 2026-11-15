//! Instruction trail: a fixed ring of the program counters the CPU last ran
//! (#436).
//!
//! # Why a trail and not a log
//!
//! The question this exists to answer is "which branch into that halt was
//! taken", and a halt looks like any other address from the outside: a running
//! CPU and a halted one both have a `pc`. What separates them is the *edge*
//! that got there, which is gone by the time anyone looks unless the last few
//! hundred addresses were kept. A ring of the last [`TRAIL_LEN`] `pc`s costs one
//! store per instruction when armed and keeps a fixed amount of memory, so it
//! can be armed on a machine that is about to misbehave and read back afterwards
//! without changing what the machine does.
//!
//! # What is in it
//!
//! One entry per executed instruction, recorded by [`crate::M68k::step`] from
//! [`crate::M68k::pc`] — the address of the opcode word, before the instruction
//! runs (see [`crate::M68k::pc`] for that convention) — so:
//!
//! - the entries are the instruction stream in order, oldest first, and
//!   consecutive entries of a straight-line program differ by the instruction
//!   length of the earlier one;
//! - an instruction never started does not appear. [`crate::M68k::step`]
//!   records below its interrupt check, so a step taken up by an interrupt
//!   contributes nothing and the interrupted instruction is recorded when it
//!   actually runs (the same rule [`crate::trace`] states for the trace
//!   exception: an instruction that never ran does not trace, and it does not
//!   trail either);
//! - `STOP` and `HALT` are recorded once, at the instruction that stopped the
//!   CPU. A stopped CPU runs no instructions, so it adds nothing more, which is
//!   what makes a `STOP` halt show up as the end of the trail rather than as
//!   the wall of one address it would be if the record were taken per step.
//!
//! # Stopping the record
//!
//! A ring that keeps recording has a short memory of the event that mattered: a
//! cart parked in a three-instruction loop overwrites the entry edge within a
//! few microseconds of taking it. So a trail may name an address, and
//! [`Trail::record`] freezes the ring the first time the CPU is about to run at
//! it — the entry edge is already in the ring at that moment, and nothing after
//! it can push it out. [`Trail::caught`] reports the freeze; the reader stops
//! its run on it. A trail armed with `None` never freezes and simply holds the
//! last [`TRAIL_LEN`] instructions of the run, which is the dump for a cart that
//! parks somewhere the caller cannot name in advance.
//!
//! # Cost and state
//!
//! Unarmed is `None`, and [`crate::M68k::step`] tests that `Option` and does
//! nothing else: no allocation and no store per instruction for a machine that
//! is not being looked at. A trail is a diagnostic and not chip state, so it is
//! `serde(skip)`ped on [`crate::M68k`] and is never in a save state.

use crate::cpu::M68k;

/// How many program counters a trail holds: the last 512 executed instructions,
/// which is a few lines of straight-line code and comfortably more than the
/// start-up device check every cart with a boot-time halt runs (#436).
pub const TRAIL_LEN: usize = 512;

/// The ring itself; see [the module docs](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trail {
    /// The ring, always exactly [`TRAIL_LEN`] long. Only
    /// `entries`'s first [`Trail::len`] slots — read from [`Trail::next`]
    /// backwards — are meaningful, and the rest is whatever an earlier lap left.
    ring: Vec<u32>,
    /// Where the next entry goes: the write cursor, and the oldest slot once the
    /// ring is full.
    next: usize,
    /// Entries recorded so far, saturating at [`TRAIL_LEN`].
    len: usize,
    /// The address that freezes the record, [`None`] for a trail that never
    /// freezes; see [`Trail::new`].
    stop_at: Option<u32>,
    /// Whether `stop_at` has been reached, which is what stops `record` dead.
    caught: bool,
}

impl Trail {
    /// An empty trail that freezes when the CPU is about to run at `stop_at`, or
    /// never freezes when that is `None`.
    pub(crate) fn new(stop_at: Option<u32>) -> Self {
        Self {
            ring: vec![0; TRAIL_LEN],
            next: 0,
            len: 0,
            stop_at,
            caught: false,
        }
    }

    /// The recorded addresses, oldest first.
    pub fn entries(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.len);
        let start = (self.next + TRAIL_LEN - self.len) % TRAIL_LEN;
        for i in 0..self.len {
            out.push(self.ring[(start + i) % TRAIL_LEN]);
        }
        out
    }

    /// How many addresses [`Trail::entries`] returns: up to [`TRAIL_LEN`].
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing has been recorded yet, which is only true of a trail
    /// armed on the step before the first one ran.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether `stop_at` was reached and the ring has been frozen against it.
    pub fn caught(&self) -> bool {
        self.caught
    }

    /// The address that freezes this trail, if it names one.
    pub fn stop_at(&self) -> Option<u32> {
        self.stop_at
    }

    /// Add one executed instruction, unless the trail has already caught.
    ///
    /// The `stop_at` test is part of the record rather than a separate call so
    /// the freeze and the entry that caused it cannot be separated: `pc` is in
    /// the ring when `caught` flips, so the address the caller is hunting
    /// appears at the end of [`Trail::entries`] with its history in front of it.
    pub(crate) fn record(&mut self, pc: u32) {
        if self.caught {
            return;
        }
        self.ring[self.next] = pc;
        self.next = (self.next + 1) % TRAIL_LEN;
        if self.len < TRAIL_LEN {
            self.len += 1;
        }
        if self.stop_at == Some(pc) {
            self.caught = true;
        }
    }
}

impl M68k {
    /// Start recording an instruction trail on this CPU; see [`crate::trail`].
    ///
    /// `stop_at` freezes the ring the first time the CPU is about to run at that
    /// address — see [`Trail::record`] — and `None` leaves the trail running to
    /// the end of the run. Arming again starts a fresh trail, which is what
    /// makes it safe to arm a machine that has already been running.
    ///
    /// Diagnostics are armed from outside the emulator: the caller is a tool
    /// like `headless run --trace-to`, never the emulated hardware.
    pub fn arm_trail(&mut self, stop_at: Option<u32>) {
        self.trail = Some(Trail::new(stop_at));
    }

    /// The instruction trail, or `None` while [`M68k::arm_trail`] has not run.
    pub fn trail(&self) -> Option<&Trail> {
        self.trail.as_ref()
    }

    /// Whether the armed trail has reached its `stop_at` address: false when
    /// there is no trail, and false when the trail has none.
    pub fn trail_caught(&self) -> bool {
        self.trail.as_ref().is_some_and(Trail::caught)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testbus::cpu_with_program;

    /// `NOP`, so a program word steps without doing anything else.
    const NOP: u16 = 0x4E71;
    /// Where [`cpu_with_program`] loads its program: the base every expected
    /// entry below is written against.
    const PROGRAM: u32 = 0x0000_1000;

    /// [`M68k::pc`] for `cpu_with_program`'s word `n`.
    fn at(n: u32) -> u32 {
        PROGRAM + 2 * n
    }

    /// Stepping with no trail armed leaves none, which is the default state the
    /// emulator runs in and the one the hot path is written for.
    #[test]
    fn unarmed_cpu_records_nothing() {
        let (mut cpu, mut bus) = cpu_with_program(&[NOP, NOP]);

        cpu.step(&mut bus);

        assert!(cpu.trail().is_none());
        assert!(!cpu.trail_caught(), "no trail, so nothing to catch");
    }

    /// The whole of what a trail is for, in one line: the addresses of the
    /// instructions that ran, in order, oldest first — including the branch
    /// target of a taken branch, which is the edge #436 asks for.
    #[test]
    fn trail_records_the_instruction_stream_in_order() {
        // NOP; BRA.S to $1006; NOP; NOP — three executed instructions, with the
        // branch skipping the word after it (the displacement is relative to the
        // branch's own address + 2, so `+2` at $1004 lands at $1006).
        let (mut cpu, mut bus) = cpu_with_program(&[NOP, 0x6002, NOP, NOP]);
        cpu.arm_trail(None);

        for _ in 0..3 {
            cpu.step(&mut bus);
        }

        assert_eq!(
            cpu.trail().expect("armed").entries(),
            [at(0), at(1), at(3)],
            "the branch target is in the trail, the skipped NOP is not"
        );
    }

    /// An instruction that never ran is not recorded: `BRA.S $self` is one entry
    /// for the branch, not one per step of waiting on it, and the loop shows up
    /// as the addresses it really executes.
    #[test]
    fn trail_of_a_self_branch_loop_repeats_the_two_addresses_of_the_loop() {
        let (mut cpu, mut bus) = cpu_with_program(&[NOP, 0x60FE]); // BRA.S $self
        cpu.arm_trail(None);

        for _ in 0..5 {
            cpu.step(&mut bus);
        }
        let trail = cpu.trail().expect("armed");

        assert_eq!(trail.entries(), [at(0), at(1), at(1), at(1), at(1)]);
    }

    /// The ring is fixed: past [`TRAIL_LEN`] entries the oldest fall out and the
    /// order stays oldest first across the wrap.
    #[test]
    fn trail_keeps_the_last_entries_across_the_wrap() {
        // A six-instruction loop run for longer than the ring holds, so the ring
        // is exactly full and the assertion can check the order across the wrap
        // and not just the count: five `NOP`s and a `BRA.S` back to $1000 (the
        // displacement is -12 from the branch's own address + 2).
        let (mut cpu, mut bus) = cpu_with_program(&[NOP, NOP, NOP, NOP, NOP, 0x60F4]);
        cpu.arm_trail(None);

        let loop_addresses: Vec<u32> = (0..6).map(at).collect();
        let steps = TRAIL_LEN + 40;
        let executed: Vec<u32> = (0..steps)
            .map(|i| loop_addresses[i % loop_addresses.len()])
            .collect();
        for _ in 0..steps {
            cpu.step(&mut bus);
        }
        let trail = cpu.trail().expect("armed");

        assert_eq!(trail.len(), TRAIL_LEN, "the ring is exactly full");
        assert_eq!(trail.entries(), executed[steps - TRAIL_LEN..]);
    }

    /// A trail named an address freezes there: the target is the last entry, and
    /// nothing the CPU runs afterwards is recorded. That is what keeps the entry
    /// edge of a halt loop readable on a cart that spins for a thousand frames.
    #[test]
    fn trail_freezes_on_first_arrival_at_stop_at() {
        let (mut cpu, mut bus) = cpu_with_program(&[NOP, NOP, NOP, NOP]);
        cpu.arm_trail(Some(at(2)));

        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert!(!cpu.trail_caught(), "0x1000 and 0x1002 are not the target");

        cpu.step(&mut bus);
        assert!(cpu.trail_caught(), "this step runs at 0x1004");
        let frozen = cpu.trail().expect("armed").entries();
        assert_eq!(frozen, [at(0), at(1), at(2)]);

        for _ in 0..10 {
            cpu.step(&mut bus);
        }
        assert_eq!(
            cpu.trail().expect("armed").entries(),
            frozen,
            "frozen means frozen: nothing after the arrival is recorded"
        );
    }

    /// `stop_at` with no trail makes no sense to ask for twice, so `arm_trail`
    /// is the only place the two are joined; and a caught trail reports the
    /// address it caught on.
    #[test]
    fn trail_reports_what_it_was_armed_to_stop_at() {
        let (mut cpu, mut bus) = cpu_with_program(&[NOP]);
        cpu.arm_trail(Some(at(0)));

        cpu.step(&mut bus);

        let trail = cpu.trail().expect("armed");
        assert!(trail.caught());
        assert_eq!(trail.stop_at(), Some(at(0)));
        assert_eq!(trail.entries(), [at(0)]);
    }

    /// A stopped CPU adds nothing to the trail: `STOP` is one entry — the
    /// instruction that stopped the machine — and the steps after it run no
    /// instructions, so a `STOP` halt reads as the end of a trail.
    #[test]
    fn a_stopped_cpu_adds_nothing_to_the_trail() {
        // STOP #$2700 — the halt stub of #436's `Mamono Hunter Youko` — then NOPs.
        let (mut cpu, mut bus) = cpu_with_program(&[NOP, 0x4E72, 0x2700, NOP]);
        cpu.arm_trail(None);

        cpu.step(&mut bus);
        cpu.step(&mut bus);
        let after_stop = cpu.trail().expect("armed").entries();
        assert_eq!(after_stop, [at(0), at(1)]);

        for _ in 0..5 {
            cpu.step(&mut bus);
        }
        assert_eq!(
            cpu.trail().expect("armed").entries(),
            after_stop,
            "the CPU is stopped, so it ran nothing to record"
        );
    }

    /// Re-arming starts over: a caller can watch one address, read the trail,
    /// then arm another without rebuilding the machine.
    #[test]
    fn arming_again_starts_a_fresh_trail() {
        let (mut cpu, mut bus) = cpu_with_program(&[NOP, NOP, NOP]);

        cpu.step(&mut bus);
        cpu.arm_trail(None);
        cpu.step(&mut bus);

        assert_eq!(cpu.trail().expect("armed").entries(), [at(1)]);
    }

    /// [`Trail::entries`] on a trail that has run nothing at all — the state
    /// between [`M68k::arm_trail`] and the first step.
    #[test]
    fn a_fresh_trail_is_empty() {
        let (mut cpu, _bus) = cpu_with_program(&[NOP]);
        cpu.arm_trail(None);

        let trail = cpu.trail().expect("armed");
        assert!(trail.is_empty());
        assert_eq!(trail.entries(), [] as [u32; 0]);
        assert!(!trail.caught());
    }
}
