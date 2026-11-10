//! The 68000 CPU state: register file, status register helpers, bus accessors.
//!
//! No instruction execution yet: `M68k::step` is a stub until the decode and
//! instruction tickets (M1-05 onward). Everything else here is the contract
//! those tickets build on.

use crate::address_error::{AccessKind, PendingFault};
use crate::bus::{ADDR_MASK, Bus};
use crate::sr::{CCR_MASK, SR_C, SR_INT_MASK, SR_MASK, SR_N, SR_RESET, SR_S, SR_V, SR_X, SR_Z};

/// Address of the initial stack pointer in the reset vector table (M68000UM
/// section 5 "Reset": the processor fetches the initial SSP from $000000).
const RESET_VECTOR_SP: u32 = 0x0000_0000;
/// Address of the reset program counter in the reset vector table.
const RESET_VECTOR_PC: u32 = 0x0000_0004;

/// Cost of the reset sequence itself in 68k clocks, excluding the two long
/// vector fetches (the bus helpers charge those). M68000UM section 5 "Reset"
/// gives the reset sequence a fixed length of 132 clock cycles.
pub const RESET_CYCLES: u32 = 132;

/// One 68000 bus cycle is 4 clock cycles (M68000UM, "Bus Cycles": S0–S3, four
/// clock phases per access). Every bus access charged by the helpers below is
/// one bus cycle, so 4 clocks; a long is two word accesses, so 8.
pub(crate) const BUS_CYCLE_CLOCKS: u64 = 4;

/// One opcode fetch: the word [`M68k::advance_opcode`] pops off the queue is one
/// bus cycle, so 4 clocks.
///
/// Every execution time in M68000UM appendix "Instruction Timing" counts that
/// fetch (`NOP` is 4), and [`M68k::step`] charges it through the queue, so a
/// handler returning a documented total hands back the total **less** these 4
/// rather than the whole of it (M1-31, #39).
pub(crate) const OPCODE_CLOCKS: u32 = BUS_CYCLE_CLOCKS as u32;

/// Clocks charged to [`M68k::cycles`] between `start` and `now`, the form
/// [`M68k::step`] returns.
///
/// `cycles` is a running total that outlives one instruction, so what a step
/// cost is the difference rather than the value: the bus accessors and the EA
/// layer's internal cycles self-charge into `cycles`, so the difference is
/// exactly this step's traffic plus whatever the handler added back (M1-31, #39).
#[inline]
fn step_clocks(start: u64, now: u64) -> u32 {
    (now - start) as u32
}

/// Clocks [`M68k::step`] reports for a step taken while the HALT pin is
/// asserted, until M1-31 (#39) times the interrupt-acknowledge sequence.
/// Clocks for a `step` taken while HALT is asserted.
///
/// The real cost is the interrupt-acknowledge sequence, which is M1-31 (#39);
/// the number here only has to be non-zero so a halted CPU still moves the
/// scheduler along.
const HALTED_CLOCKS: u32 = 4;

/// Clocks for a `step` taken while `STOP` has the CPU halted, the same stand-in
/// as [`HALTED_CLOCKS`] for the same reason: the processor is not executing. The
/// step that *wakes* up is not charged this — it charges the interrupt
/// acknowledge and entry (M1-28, #36, [`crate::interrupts`]).
pub(crate) const STOPPED_CLOCKS: u32 = 4;

/// How far ahead of [`M68k::pc`] the prefetch queue has already read, in bytes.
///
/// The queue holds two words ([`M68k::prefetch`]), `pc` is the address of the
/// front one, so the next word the CPU has *not* yet fetched is at
/// `pc + FETCH_AHEAD` and every queue refill reads from there.
const FETCH_AHEAD: u32 = 4;

/// Clocks a jump pays for one displacement word it reads out of the prefetch
/// queue: see [`M68k::jump_ext_word`].
const JUMP_WORD_CLOCKS: u64 = 2;

/// Clocks for a result the CPU made itself — a condition-code test, the status
/// register — landing in a **data register**.
///
/// A memory destination pays nothing on top of its own write cycle; a data
/// register is not on the bus, so the write is a short internal cycle instead.
/// Measured on `Scc.json.gz`, where `Scc` to `Dn` is 6 clocks when the condition
/// holds and 4 when it does not (so it really is the write, not the test), and
/// on `MOVEfromSR.json.gz`, where `MOVE SR,D0` is 6 and every memory destination
/// is the unchanged read-then-write (M1-31c, #41).
pub(crate) const INTERNAL_RESULT_WRITE_CLOCKS: u32 = 2;

/// The 68000 register file and CPU state.
///
/// Fields are plain data (no `Box<dyn _>`, no references) so the save-state
/// milestone can serialise them, and are public so tests and the `genesis`
/// crate can poke them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M68k {
    /// Data registers D0–D7.
    pub d: [u32; 8],
    /// Address registers A0–A7. `a[7]` is always the *active* stack pointer:
    /// the SSP in supervisor mode (S = 1), the USP in user mode (S = 0).
    pub a: [u32; 8],
    /// User stack pointer. Holds the USP while it is not the active pointer
    /// (i.e. while S = 1); see [`M68k::set_sr`].
    pub usp: u32,
    /// Supervisor stack pointer. Holds the SSP while it is not the active
    /// pointer (i.e. while S = 0); see [`M68k::set_sr`].
    pub ssp: u32,
    /// Program counter: the address of the word the CPU is working on — the
    /// address of `prefetch[0]`, the opcode now being executed. Always even and
    /// always 24 bit.
    ///
    /// # The prefetch convention
    ///
    /// [`M68k::prefetch`] is a two-word window onto the instruction stream that
    /// always satisfies `prefetch == [word(pc), word(pc + 2)]`, so the next word
    /// the CPU has not yet fetched is at `pc + 4`. The core uses exactly the
    /// suite's convention so the SingleStepTests harness can copy `pc` and
    /// `prefetch` straight across (see `tests/harness/mod.rs`: `apply` and
    /// `snapshot` are 1:1, with no adapter to drift out of step).
    ///
    /// Derived from the suite, not assumed. In
    /// `test-roms/680x0/68000/v1/NOP.json.gz`, case `4e71 [NOP] 1`: `initial`
    /// is `pc = 0x000C00`, `prefetch = [0x4E71, 0x2A53]`, and `initial.ram`
    /// lists exactly one word — `0x000C04 = 06 79`. `0x4E71` is the `NOP`
    /// opcode the case name quotes, so the opcode word sits *at* `pc`, not
    /// below it. The case's own bus trace has exactly one read, `$000C04` (the
    /// word $0679), and `final` is `pc = 0x000C02`,
    /// `prefetch = [0x2A53, 0x0679]`: the freshly fetched word became
    /// `prefetch[1]` at `final.pc + 2`, while `prefetch[0]` is the
    /// already-queued word at the new `pc` and was never re-read. Same shape in
    /// `UNLINK.json.gz` case `4e58 [UNLINK A0] 1`: `final.pc` is
    /// `initial.pc + 2`, `final.prefetch[0]` is `initial.prefetch[1]`, and of
    /// that case's three reads the one from the instruction stream is again
    /// `initial.pc + 4` (`$000C04`), supplying `final.prefetch[1]`.
    ///
    /// Measured over the whole pinned suite (124 files, 1 000 060 cases): every
    /// case whose `ram` lists the bytes at `pc` and `pc + 2` — 986 556 of them,
    /// in both `initial` and `final` — satisfies the invariant, with no
    /// counterexample, and instruction-stream reads cluster at
    /// `pc + 4` (910 000), `pc + 6` (236 825), `pc + 8` (19 527) and `pc + 10`
    /// (1 257) — the queue refill ladder. Nothing fetches from `pc` or `pc + 2`,
    /// because the queue already has those two words.
    ///
    /// One trap worth naming: M68000UM's prose ("Programming Model", Program
    /// Counter) says the program counter "always points to the memory location
    /// containing the next byte or word to be accessed", which is `pc + 4` here,
    /// because the next access is the queue refill. This field deliberately uses
    /// the suite's numbering — the address of the opcode being executed — not
    /// that one. PC-relative addressing and exception frames therefore use
    /// `pc + <words already consumed>`, never `pc` raw.
    pub pc: u32,
    /// Status register. Only the bits in [`SR_MASK`] exist; write it through
    /// [`M68k::set_sr`] so the mask and the stack pointer swap happen.
    pub sr: u16,
    /// Prefetch queue: `[word(pc), word(pc + 2)]`, the pair the SingleStepTests
    /// model and the core has to reproduce.
    ///
    /// Index 0 is IR/IRD, the opcode being executed; index 1 is IRC, the next
    /// word, already fetched (M68000UM, "Programming Model" and "Instruction
    /// Processing": operands and the following instruction are fetched into the
    /// instruction registers while the instruction in IR is executing, so the
    /// CPU is always a word or two ahead of itself). Both words are always the
    /// ones at `pc` and `pc + 2` — see [`M68k::pc`] for the evidence — and the
    /// only way to move the window is `pop_word`, so nothing can break the
    /// invariant.
    pub prefetch: [u16; 2],
    /// Interrupt level currently asserted on the IPL pins, 0..=7. 0 = none.
    pub ipl: u8,
    /// The IPL level the previous sample saw, 0..=7.
    ///
    /// [`crate::interrupts`] is the only reader: level 7 is the NMI, and it is
    /// taken on the edge *into* 7, so the CPU has to remember what the pins read
    /// last time to avoid re-acknowledging a line someone is simply holding
    /// high (M68000UM section 6 "Interrupt Processing", M1-28 #36).
    pub(crate) ipl_sampled: u8,
    /// True while the CPU is stopped by the `STOP` instruction.
    pub stopped: bool,
    /// True while the HALT pin is asserted (the CPU stops accessing the bus).
    pub halted: bool,
    /// IRD: the opcode word of the instruction [`M68k::step`] is executing,
    /// snapshotted from `prefetch[0]` before the operation runs. The group 0
    /// address error frame pushes it (M68000UM "Address Error", and measured:
    /// the IRD word in `JMP.json.gz` case `4ed1 [JMP (A1)] 2` is the `JMP`
    /// opcode, although the queue has moved on by the time its fetch faults).
    pub(crate) ird: u16,
    /// The address error raised by an access in the instruction now executing,
    /// not yet entered ([`crate::address_error`]). [`M68k::step`] takes it once
    /// the opcode has finished; while it is set every checked bus access is
    /// suppressed, which is what keeps the rest of the faulting instruction off
    /// the bus.
    pub(crate) pending_address_error: Option<PendingFault>,
    /// Vector number of the exception the last [`M68k::step`] took, or `None`
    /// if that step did not take one ([`crate::ops`] holds the numbers).
    ///
    /// Stub state, from M1-07 (#15): the illegal / line A / line F routes
    /// record the vector and stop there. M1-08 (#16) replaces this with real
    /// exception entry — the 6-byte frame, the handler long fetch — and then
    /// the field is what the frame is built from, which is why it is plain CPU
    /// data rather than something only tests can see.
    pub pending_exception: Option<u8>,
    /// True once a route that does not trace has run in the instruction
    /// [`M68k::step`] is executing: illegal instruction / line A / line F, a
    /// privilege violation, or an address error ([`crate::trace`], M68000UM
    /// section 6.3.8).
    ///
    /// CPU state rather than a return value because the routes set it from deep
    /// inside a handler while the trace decision is taken by `step` once that
    /// handler returns; and it cannot be read back out of
    /// [`M68k::pending_exception`], because
    /// [`crate::EXC_ADDRESS_ERROR`] and [`crate::EXC_ILLEGAL`] are numerically
    /// the same vector (see [`crate::address_error`]).
    pub(crate) trace_suppressed: bool,
    /// Running total of 68k clocks consumed since the last [`M68k::reset`].
    /// Bus accesses charge themselves (see [`M68k::read_w`]), and
    /// [`M68k::step`] will add instruction cycles as they land; the scheduler
    /// in `genesis` drains this the same way it drains the return value of
    /// `step`.
    pub cycles: u64,
}

impl Default for M68k {
    fn default() -> Self {
        Self::new()
    }
}

impl M68k {
    /// A CPU in a defined but un-reset state. Call [`M68k::reset`] before
    /// running it: the 68000 only initialises itself on a reset.
    pub fn new() -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            usp: 0,
            ssp: 0,
            pc: 0,
            sr: 0,
            prefetch: [0; 2],
            ipl: 0,
            ipl_sampled: 0,
            stopped: false,
            halted: false,
            ird: 0,
            pending_address_error: None,
            pending_exception: None,
            trace_suppressed: false,
            cycles: 0,
        }
    }

    /// Hardware reset (M68000UM section 5 "Reset"). Loads the initial stack
    /// pointer from address 0 and the program counter from address 4, then
    /// leaves the CPU in supervisor mode with interrupts masked.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        // Clear everything before touching SR so the stack-pointer swap that
        // `set_sr` performs below moves only zeroes: on real hardware both
        // stack pointers are uninitialised until the vector fetch.
        self.d = [0; 8];
        self.a = [0; 8];
        self.usp = 0;
        self.ssp = 0;
        self.pc = 0;
        self.prefetch = [0; 2];
        self.ipl = 0;
        // The pins are sampled again after the reset, so a level 7 that is
        // still sitting there counts as a fresh edge (#36).
        self.ipl_sampled = 0;
        self.stopped = false;
        self.halted = false;
        self.ird = 0;
        self.pending_address_error = None;
        self.pending_exception = None;
        self.trace_suppressed = false;
        self.cycles = 0;
        self.set_sr(SR_RESET);

        // M68000UM section 5 "Reset": SSP from $000000, PC from $000004, in
        // that order, each one long read (high word first).
        let ssp = self.read_l(bus, RESET_VECTOR_SP);
        let pc = self.read_l(bus, RESET_VECTOR_PC);
        self.ssp = ssp;
        // SR.S is already 1, so `a[7]` is the active SSP.
        self.a[7] = ssp;
        // The vector fetch is over, so the CPU starts fetching instructions at
        // the new PC: the queue is full before the first instruction executes
        // (M68000UM section 5 "Reset", which ends with the processor fetching
        // from the loaded PC). `jump_to` is exactly that — two word fetches.
        self.jump_to(bus, pc);

        self.cycles += u64::from(RESET_CYCLES);
    }

    /// Run one instruction (or one exception). Returns the 68k clocks used.
    ///
    /// The instruction is `prefetch[0]`, because the front of the queue is the
    /// word under execution (see [`M68k::pc`]); the operation comes from the
    /// decode table ([`crate::decode`]) and the handler from [`crate::ops`],
    /// which re-reads its operand fields from the opcode bits.
    ///
    /// Nothing here pops the queue: the pop that finishes an instruction is
    /// [`M68k::advance_opcode`], which the handlers call once they have read
    /// their operands. The exception routes do not pop either — an exception
    /// discards the queue and refills it from the handler, which is
    /// [`crate::exceptions`] and what [`M68k::step`]'s `bus` argument is for
    /// (M1-08, #16).
    ///
    /// Before an opcode is decoded the interrupt pins are sampled
    /// ([`M68k::check_interrupts`], M1-28 #36), and a taken interrupt is the
    /// whole of that step: no decode, no operand, no instruction clocks.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        let start = self.cycles;
        if self.halted {
            // HALT asserted: the CPU stops accessing the bus and re-samples the
            // interrupt pins. The real cost is the interrupt-acknowledge
            // sequence, which is M1-31 (#39/#40); the number here only has to
            // be non-zero so a halted CPU still moves the scheduler along.
            self.cycles += u64::from(HALTED_CLOCKS);
            return HALTED_CLOCKS;
        }

        if self.stopped {
            // STOP left the CPU halted: no fetch, no decode, no opcode. `STOP`
            // waits for an interrupt, so the pins are still sampled, and a
            // request above the mask both clears `stopped` and is entered like
            // any other interrupt (M1-28, #36). With no request there is
            // nothing to do but burn these clocks.
            if self.check_interrupts(bus) {
                self.stopped = false;
                return step_clocks(start, self.cycles);
            }
            self.cycles += u64::from(STOPPED_CLOCKS);
            return STOPPED_CLOCKS;
        }

        // One step, at most one exception: what the previous step recorded is
        // history by now.
        self.pending_exception = None;

        // Interrupts are taken here, between instructions, and nothing below
        // this point runs when one is taken: the queue is thrown away by the
        // exception entry and the clocks are the entry's (M1-28, #36).
        if self.check_interrupts(bus) {
            return step_clocks(start, self.cycles);
        }

        let opcode = self.prefetch[0];
        // The group 0 frame's IRD is this word, whoever faults later (#37);
        // see [`M68k::ird`].
        self.ird = opcode;
        // Trace is decided from the T bit as it stands *before* the instruction
        // runs, so an instruction that sets T is not traced and one that clears
        // T still is (M68000UM section 6.3.8, [`crate::trace`]). The suppression
        // record is cleared here too: it belongs to the instruction that set it.
        let traced = self.trace_armed();
        self.trace_suppressed = false;
        let used = self.run_op(bus, crate::decode::decode(opcode), opcode);
        // An address error raised by any access the opcode performed is entered
        // here, after the access layer stopped letting the instruction reach the
        // bus (M1-29, #37). It is not an early return: the entry has already
        // thrown the queue away and moved `pc` to the handler by then.
        self.take_address_error(bus);
        // Then the trace exception for the instruction that has just completed —
        // after any group 2 trap it took, and before the next step's interrupt
        // check (M68000UM section 6.3.8, [`crate::trace`]).
        self.take_trace(bus, traced);
        // A handler returns only the clocks nothing else charged for: its own
        // execution overhead. Every bus access and every EA-layer internal cycle
        // has already self-charged into `cycles`, so adding the handler's number
        // and returning the step's delta is what makes `step` report the whole
        // cost of the step without counting an access twice (M1-31, #39).
        self.cycles += u64::from(used);
        step_clocks(start, self.cycles)
    }

    /// Mask `value` with [`SR_MASK`] and store it in SR, swapping the stack
    /// pointers if the S bit changed.
    ///
    /// Every SR change in the core goes through here — that is what keeps the
    /// nonexistent SR bits at zero and keeps `a[7]` pointing at the stack
    /// pointer that matches the new mode (M68000UM, "Status Register": the S
    /// bit selects which stack pointer is active).
    pub fn set_sr(&mut self, value: u16) {
        let value = value & SR_MASK;
        if (self.sr ^ value) & SR_S != 0 {
            self.swap_stack_pointers();
        }
        self.sr = value;
    }

    /// Move the active stack pointer between `a[7]` and the inactive slot.
    /// Called by [`M68k::set_sr`] while `self.sr` still holds the *old* value,
    /// so `S` says which pointer `a[7]` currently holds.
    fn swap_stack_pointers(&mut self) {
        if self.sr & SR_S != 0 {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        } else {
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }
    }

    /// Set the active stack pointer, in both places it lives.
    ///
    /// `a[7]` mirrors whichever pointer SR.S selects, and `ssp`/`usp` are the
    /// stored halves a save state (and the conformance harness, which compares
    /// both) reads back — [`M68k::swap_stack_pointers`] keeps them consistent
    /// across a mode change, and this keeps them consistent across a push or a
    /// pop. Writing only `a[7]` leaves the stored pointer for the current mode
    /// one frame behind.
    pub(crate) fn set_active_sp(&mut self, value: u32) {
        if self.is_supervisor() {
            self.ssp = value;
        } else {
            self.usp = value;
        }
        self.a[7] = value;
    }

    /// The CCR: the low byte of SR, masked with [`CCR_MASK`].
    #[inline]
    pub fn ccr(&self) -> u16 {
        self.sr & CCR_MASK
    }

    /// Write the CCR (low byte of SR) without touching T, S or the interrupt
    /// mask, so no stack pointer swap can happen. Bits above [`CCR_MASK`] are
    /// dropped. This is what `ANDI/ORI/EORI` to CCR will use.
    pub fn set_ccr(&mut self, value: u16) {
        // Go through `set_sr` so the masking invariant holds; S cannot change
        // (it is outside `CCR_MASK`) so the stack pointers cannot move.
        let upper = self.sr & !CCR_MASK;
        self.set_sr(upper | (value & CCR_MASK));
    }

    /// Carry flag C.
    #[inline]
    pub fn flag_c(&self) -> bool {
        self.sr & SR_C != 0
    }

    /// Overflow flag V.
    #[inline]
    pub fn flag_v(&self) -> bool {
        self.sr & SR_V != 0
    }

    /// Zero flag Z.
    #[inline]
    pub fn flag_z(&self) -> bool {
        self.sr & SR_Z != 0
    }

    /// Negative flag N.
    #[inline]
    pub fn flag_n(&self) -> bool {
        self.sr & SR_N != 0
    }

    /// Extend flag X.
    #[inline]
    pub fn flag_x(&self) -> bool {
        self.sr & SR_X != 0
    }

    /// Supervisor mode (SR.S = 1).
    #[inline]
    pub fn is_supervisor(&self) -> bool {
        self.sr & SR_S != 0
    }

    /// The current interrupt mask (SR.I2–I0) as a level, 0..=7.
    #[inline]
    pub fn int_mask(&self) -> u8 {
        ((self.sr & SR_INT_MASK) >> 8) as u8
    }

    /// Set or clear a single SR bit. `mask` is one of the `SR_*` constants.
    #[inline]
    pub fn set_flag(&mut self, mask: u16, on: bool) {
        self.set_sr(if on { self.sr | mask } else { self.sr & !mask });
    }

    /// Read the byte at `addr`, masked to 24 bits, and charge one bus cycle.
    ///
    /// Byte accesses are never alignment-checked: A0 is the byte-select line
    /// (M68000UM "Address Definition"), so an odd byte address is ordinary.
    #[inline]
    pub fn read_b(&mut self, bus: &mut impl Bus, addr: u32) -> u8 {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.read_byte(addr & ADDR_MASK)
    }

    /// Read the big endian word at `addr`, masked to 24 bits, and charge one
    /// bus cycle.
    ///
    /// An odd `addr` is the address error: the read does not happen, `0` is
    /// returned, and [`crate::address_error`] enters on the way out of
    /// [`M68k::step`] (M1-29, #37). While a fault is already pending the access
    /// is suppressed the same way.
    #[inline]
    pub fn read_w(&mut self, bus: &mut impl Bus, addr: u32) -> u16 {
        if self.alignment_fault(addr, AccessKind::DataRead, false) {
            return 0;
        }
        self.read_w_raw(bus, addr)
    }

    /// Read the big endian long at `addr`: high word at `addr`, low word at
    /// `addr + 2`, two bus cycles charged (8 clocks).
    ///
    /// Faults on an odd `addr`, and on a `addr` whose second word would lie past
    /// `$FFFFFF` — this used to wrap, and the 68000 faults instead
    /// (M68000UM "Address Error"). See [`M68k::read_w`] for what a fault does to
    /// the return value.
    #[inline]
    pub fn read_l(&mut self, bus: &mut impl Bus, addr: u32) -> u32 {
        if self.alignment_fault(addr, AccessKind::DataRead, true) {
            return 0;
        }
        let addr = addr & ADDR_MASK;
        let high = u32::from(self.read_w_raw(bus, addr));
        let low = u32::from(self.read_w_raw(bus, (addr + 2) & ADDR_MASK));
        (high << 16) | low
    }

    /// Write a byte, masked to 24 bits, and charge one bus cycle. Never
    /// alignment-checked, like [`M68k::read_b`].
    #[inline]
    pub fn write_b(&mut self, bus: &mut impl Bus, addr: u32, value: u8) {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.write_byte(addr & ADDR_MASK, value);
    }

    /// Write a big endian word at `addr`, masked to 24 bits, and charge one bus
    /// cycle. An odd `addr` is the address error and the write does not happen:
    /// see [`M68k::read_w`].
    #[inline]
    pub fn write_w(&mut self, bus: &mut impl Bus, addr: u32, value: u16) {
        if self.alignment_fault(addr, AccessKind::DataWrite, false) {
            return;
        }
        self.write_w_raw(bus, addr, value)
    }

    /// Write a big endian long: high word at `addr`, low word at `addr + 2`,
    /// two bus cycles charged (8 clocks). Faults like [`M68k::read_l`] and then
    /// writes nothing at all.
    #[inline]
    pub fn write_l(&mut self, bus: &mut impl Bus, addr: u32, value: u32) {
        if self.alignment_fault(addr, AccessKind::DataWrite, true) {
            return;
        }
        let addr = addr & ADDR_MASK;
        self.write_w_raw(bus, addr, (value >> 16) as u16);
        self.write_w_raw(bus, (addr + 2) & ADDR_MASK, value as u16);
    }

    // ---- Unchecked accesses -------------------------------------------------
    //
    // The exception paths' own bus traffic must never alignment-fault: the
    // vector table read and the frame writes in `exceptions.rs` and
    // `address_error.rs` have to succeed whatever the address is (M1-29, #37).
    // They use these instead of the checked accessors above; nothing else
    // should, and the difference is deliberately two names rather than a
    // special-cased address range.

    /// [`M68k::read_w`] without the alignment check. For exception entry only.
    pub(crate) fn read_w_raw(&mut self, bus: &mut impl Bus, addr: u32) -> u16 {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.read_word(addr & ADDR_MASK)
    }

    /// [`M68k::read_l`] without the alignment check. For exception entry only.
    pub(crate) fn read_l_raw(&mut self, bus: &mut impl Bus, addr: u32) -> u32 {
        let addr = addr & ADDR_MASK;
        let high = u32::from(self.read_w_raw(bus, addr));
        let low = u32::from(self.read_w_raw(bus, (addr + 2) & ADDR_MASK));
        (high << 16) | low
    }

    /// [`M68k::write_w`] without the alignment check. For exception entry only.
    pub(crate) fn write_w_raw(&mut self, bus: &mut impl Bus, addr: u32, value: u16) {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.write_word(addr & ADDR_MASK, value);
    }

    /// [`M68k::write_l`] without the alignment check. For exception entry only.
    pub(crate) fn write_l_raw(&mut self, bus: &mut impl Bus, addr: u32, value: u32) {
        let addr = addr & ADDR_MASK;
        self.write_w_raw(bus, addr, (value >> 16) as u16);
        self.write_w_raw(bus, (addr + 2) & ADDR_MASK, value as u16);
    }

    /// Fetch one instruction-stream word: [`M68k::read_w`] with the *code fetch*
    /// fault kind, so an odd fetch reports `0x1E` in the frame's status word
    /// rather than the data-read `0x15` (M68000UM "Address Error", and measured
    /// on `JMP.json.gz` case `4ed1 [JMP (A1)] 2`).
    #[inline]
    fn fetch_w(&mut self, bus: &mut impl Bus, addr: u32) -> u16 {
        if self.alignment_fault(addr, AccessKind::CodeFetch, false) {
            return 0;
        }
        self.read_w_raw(bus, addr)
    }

    // ---- Prefetch queue: the only way words enter and leave it (M1-04, #12) ----
    //
    // Because `pc` is defined as the address of the queue front, taking a word
    // out of the front is the same rotation whether that word is an extension
    // word of the instruction being executed or the opcode of an instruction
    // that has just finished. `pop_word` is the one implementation of it, so the
    // `prefetch == [word(pc), word(pc + 2)]` invariant cannot be broken by one
    // of the callers below getting the bookkeeping wrong.

    /// Take the front word out of the queue and refill the vacated slot: the
    /// back word moves to the front, the word at `pc + 4` is fetched into the
    /// back, and `pc` moves up one word to keep pointing at the front. Returns
    /// the word that came out of the front.
    ///
    /// The returned word was fetched earlier, so this costs one bus cycle, not
    /// two: 4 clocks for the refill read. That is why a `NOP` is 4 clocks and
    /// why a write to the word already sitting in the queue does not reach the
    /// CPU until the queue has moved on (self-modifying code).
    ///
    /// The refill is a *code fetch*, so an odd fetch address is an address error
    /// with the code-fetch status bits (M1-29, #37, [`crate::address_error`]).
    /// Every instruction fetch in the core goes through here, so the check
    /// belongs here rather than in its callers.
    fn pop_word(&mut self, bus: &mut impl Bus) -> u16 {
        let word = self.prefetch[1];
        // The refill address is computed before `pc` moves: the queue's next
        // unread word is 4 bytes ahead of the *old* front.
        let next = (self.pc + FETCH_AHEAD) & ADDR_MASK;
        self.prefetch[0] = word;
        self.pc = (self.pc + 2) & ADDR_MASK;
        self.prefetch[1] = self.fetch_w(bus, next);
        word
    }

    /// The instruction's extension word: immediate data, a displacement, an
    /// absolute address — whatever follows the opcode in the instruction stream.
    ///
    /// Hands over the word already sitting at the back of the queue (it was
    /// fetched while the CPU was still decoding) and charges only the refill,
    /// 4 clocks. See `pop_word`, and [`M68k::ext_addr`] for the address that
    /// word has while it is still queued.
    pub(crate) fn fetch_ext_word(&mut self, bus: &mut impl Bus) -> u16 {
        self.pop_word(bus)
    }

    /// The word already sitting at the back of the queue — the instruction's
    /// extension word — taken **without** the refill read [`M68k::pop_word`]
    /// charges.
    ///
    /// Only for an instruction that is about to throw the queue away. A branch
    /// that takes reloads both slots at its target, so the read that would have
    /// refilled the extension word's slot never reaches the bus: the word was
    /// fetched while the opcode was still being decoded. Measured on the
    /// bus traces of `Bcc.json.gz` — case `6300 [Bcc #] 2193` (`Bcc.W`, taken,
    /// `length` 10) lists `n 2` and the two target fetches and nothing else,
    /// while `6200 [Bcc #] 193` (the same opcode, not taken, `length` 12) lists
    /// the fetch at `pc + 4` that the pop does owe (M1-31c, #41).
    pub(crate) const fn queued_ext_word(&self) -> u16 {
        self.prefetch[1]
    }

    /// [`M68k::fetch_ext_word`] for the **last** extension word of an
    /// instruction that is about to [`M68k::jump_to`]: takes the word out of the
    /// queue — `pc` and `prefetch[0]` move exactly as they would — but skips the
    /// refill read, which is the whole cost of a pop and which the discarded
    /// queue makes pointless.
    ///
    /// `prefetch[1]` is left holding the word that moved to the front, so the
    /// `prefetch == [word(pc), word(pc + 2)]` invariant is broken between here
    /// and the `jump_to` that restores it. Nothing may read the queue in
    /// between, which is why this is only for a jump: `push_long`, `write_ea`
    /// and friends never touch it.
    pub(crate) fn pop_queued_ext_word(&mut self) -> u16 {
        let word = self.prefetch[1];
        self.prefetch[0] = word;
        self.pc = (self.pc + 2) & ADDR_MASK;
        word
    }

    /// [`M68k::pop_queued_ext_word`] at the 2-clock price a *jump* pays for the
    /// displacement word it reads: the refill read is cancelled with the queue,
    /// and what is left is turning the word into an address.
    ///
    /// Measured per control mode on `JMP.json.gz` (M1-31c, #41): `JMP (An)` is 8
    /// clocks — `jump_to` alone — and `JMP (d16,A0)`, `JMP (xxx).W` and
    /// `JMP (d16,PC)` are 10, one word's 2 on top. `JMP (d8,A0,Xn)` is 14 and is
    /// **not** priced this way: see [`M68k::indexed_addr`].
    pub(crate) fn jump_ext_word(&mut self) -> u16 {
        let word = self.pop_queued_ext_word();
        self.cycles += JUMP_WORD_CLOCKS;
        word
    }

    /// [`M68k::fetch_ext_long`] for an instruction that is about to discard the
    /// queue: the first word still costs its refill read, because that read is
    /// what brings the second word in, and the second word costs nothing.
    /// `JMP (xxx).l`'s trace is exactly that — one read at `pc + 4` and the two
    /// fetches at the target, 12 clocks (`4ef9 [JMP (xxx).l] 43`).
    pub(crate) fn pop_queued_ext_long(&mut self, bus: &mut impl Bus) -> u32 {
        let high = u32::from(self.fetch_ext_word(bus));
        let low = u32::from(self.pop_queued_ext_word());
        (high << 16) | low
    }

    /// A long extension word: two queue words, high word first, so 8 clocks and
    /// `pc` advances by 4.
    pub(crate) fn fetch_ext_long(&mut self, bus: &mut impl Bus) -> u32 {
        let high = u32::from(self.fetch_ext_word(bus));
        let low = u32::from(self.fetch_ext_word(bus));
        (high << 16) | low
    }

    /// End of an instruction: `IRC` (the next opcode, already fetched) becomes
    /// the queue front and a new word is fetched into `IRC`.
    ///
    /// Called once at the end of every instruction, which is what leaves `pc`
    /// and `prefetch` in exactly the state a SingleStepTests case starts a case
    /// with: `pc` on the next opcode, the queue two words wide from there.
    pub(crate) fn advance_opcode(&mut self, bus: &mut impl Bus) {
        self.pop_word(bus);
    }

    /// Throw the queue away and refill it with two fresh word fetches from
    /// `addr`: afterwards `pc == addr` and
    /// `prefetch == [word(addr), word(addr + 2)]`. Costs 8 clocks.
    ///
    /// Branches, jumps, `RTE`/`RTS`/`RTR` and exception entry all leave the CPU
    /// with a queue full of words from a stream it is no longer executing, so
    /// they refill instead of sliding the window with `pop_word` — that
    /// is also why a taken branch costs more than a not-taken one here
    /// (M68000UM, "Instruction Processing": the branch processor loads the new
    /// PC and instruction fetching restarts there, so the words the queue had
    /// already fetched are thrown away).
    ///
    /// `pc` keeps the whole 32-bit `addr`; only the two fetches are masked, which
    /// is the difference between a branch target and a bus address. Measured: 907
    /// of `DBcc.json.gz`'s, 7 of `BSR.json.gz`'s and 4 of `Bcc.json.gz`'s cases
    /// end with a `pc` above `$FFFFFF` (#193 is the same finding for `LEA`/`PEA`).
    pub(crate) fn jump_to(&mut self, bus: &mut impl Bus, addr: u32) {
        self.pc = addr;
        // The fetches are handed the unmasked `addr` on purpose: `read_w_raw`
        // masks on the way to the bus, and an address error from one of them
        // has to report the address that was asked for, not the 24 bit one
        // (M1-29, #37).
        self.prefetch[0] = self.fetch_w(bus, addr);
        self.prefetch[1] = self.fetch_w(bus, addr.wrapping_add(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sr::{SR_C, SR_INT_MASK, SR_N, SR_T, SR_V, SR_X};
    use crate::testbus::cpu_with_program;

    /// Where [`cpu_with_program`] loads its program words (its own
    /// `PROGRAM_ADDR`, which is private to `testbus`), so the queue tests can
    /// name the addresses the queue is reading.
    const PROGRAM_BASE: u32 = 0x0000_1000;

    /// Minimal 64 KB RAM bus for these unit tests. M1-02 (#10) adds the real
    /// `TestBus` with bus-transaction recording for the conformance suites;
    /// this one only needs to remember the addresses it was asked about.
    struct Ram64k {
        mem: Vec<u8>,
        /// Every address the CPU asked the bus about, in order.
        accesses: Vec<u32>,
    }

    impl Ram64k {
        fn new() -> Self {
            Self {
                mem: vec![0; 1 << 16],
                accesses: Vec::new(),
            }
        }

        /// Mirror of `M68k::read_*`: only ever called with 24 bit addresses in
        /// these tests, so no masking here — that asymmetry is the point of
        /// `address_is_masked_to_24_bits`.
        fn peek_byte(&self, addr: u32) -> u8 {
            self.mem[addr as usize]
        }

        fn peek_word(&self, addr: u32) -> u16 {
            let i = addr as usize;
            u16::from_be_bytes([self.mem[i], self.mem[i + 1]])
        }

        fn peek_long(&self, addr: u32) -> u32 {
            let i = addr as usize;
            u32::from_be_bytes([
                self.mem[i],
                self.mem[i + 1],
                self.mem[i + 2],
                self.mem[i + 3],
            ])
        }

        fn poke_long(&mut self, addr: u32, value: u32) {
            let i = addr as usize;
            self.mem[i..i + 4].copy_from_slice(&value.to_be_bytes());
        }
    }

    impl Bus for Ram64k {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.accesses.push(addr);
            self.peek_byte(addr)
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.accesses.push(addr);
            self.peek_word(addr)
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.accesses.push(addr);
            let i = addr as usize;
            self.mem[i] = value;
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.accesses.push(addr);
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }
    }

    /// Bus with the reset vector table: SSP = 0x00FF_0000, PC = 0x0000_0200.
    fn bus_with_reset_vector() -> Ram64k {
        let mut bus = Ram64k::new();
        bus.poke_long(0x0000_0000, 0x00FF_0000);
        bus.poke_long(0x0000_0004, 0x0000_0200);
        bus
    }

    #[test]
    fn reset_loads_ssp_and_pc() {
        let mut bus = bus_with_reset_vector();
        let mut cpu = M68k::new();

        cpu.reset(&mut bus);

        assert_eq!(cpu.a[7], 0x00FF_0000, "SSP comes from address 0");
        assert_eq!(cpu.pc, 0x0000_0200, "PC comes from address 4");
        assert_eq!(cpu.sr, 0x2700, "S=1, T=0, interrupt mask 7");
        assert!(cpu.is_supervisor());
        assert_eq!(cpu.int_mask(), 7);
        // The reset vector fetches are two long accesses, high word first, and
        // then the queue is filled with two word fetches from the PC that was
        // just loaded (M1-04, #12).
        assert_eq!(
            bus.accesses,
            [
                0x0000_0000, // SSP high
                0x0000_0002, // SSP low
                0x0000_0004, // PC high
                0x0000_0006, // PC low
                0x0000_0200, // prefetch[0], at the new PC
                0x0000_0202, // prefetch[1]
            ]
        );
    }

    #[test]
    fn set_sr_masks_unused_bits() {
        let mut cpu = M68k::new();

        cpu.set_sr(0xFFFF);

        assert_eq!(cpu.sr, 0xA71F);
        assert_eq!(cpu.sr & !SR_MASK, 0);
    }

    #[test]
    fn switching_to_user_mode_swaps_stack_pointers() {
        let mut cpu = M68k::new();
        cpu.set_sr(SR_S);
        cpu.a[7] = 0x1000; // active SSP
        cpu.usp = 0x2000; // stored USP

        cpu.set_sr(0x0000); // -> user mode

        assert_eq!(cpu.a[7], 0x2000, "a[7] becomes the USP");
        assert_eq!(cpu.ssp, 0x1000, "the old a[7] is now the stored SSP");
        assert_eq!(cpu.usp, 0x2000);

        cpu.set_sr(SR_S); // -> supervisor again

        assert_eq!(cpu.a[7], 0x1000);
        assert_eq!(cpu.ssp, 0x1000);
        assert_eq!(cpu.usp, 0x2000, "a[7] moved back into the USP slot");
    }

    #[test]
    fn ccr_helpers() {
        let mut cpu = M68k::new();
        cpu.set_sr(0);

        cpu.set_flag(SR_X, true);
        assert!(cpu.flag_x(), "X set");
        cpu.set_flag(SR_N, true);
        assert!(cpu.flag_n(), "N set");
        cpu.set_flag(SR_Z, true);
        assert!(cpu.flag_z(), "Z set");
        cpu.set_flag(SR_V, true);
        assert!(cpu.flag_v(), "V set");
        cpu.set_flag(SR_C, true);
        assert!(cpu.flag_c(), "C set");
        assert_eq!(cpu.sr, SR_X | SR_N | SR_Z | SR_V | SR_C);
        assert_eq!(cpu.ccr(), SR_X | SR_N | SR_Z | SR_V | SR_C);

        cpu.set_flag(SR_X, false);
        cpu.set_flag(SR_N, false);
        cpu.set_flag(SR_Z, false);
        cpu.set_flag(SR_V, false);
        cpu.set_flag(SR_C, false);
        assert_eq!(cpu.sr, 0);
        assert!(!cpu.flag_x());
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v());
        assert!(!cpu.flag_c());
        assert_eq!(cpu.ccr(), 0);

        // set_ccr writes only the low byte: T, S and the interrupt mask stay.
        cpu.set_sr(SR_T | SR_S | SR_INT_MASK | SR_C);
        cpu.set_ccr(SR_Z);
        assert_eq!(cpu.sr, SR_T | SR_S | SR_INT_MASK | SR_Z);
        assert!(cpu.flag_z());
        assert!(!cpu.flag_c());
        // Bits outside the CCR cannot leak in.
        cpu.set_ccr(0xFFFF);
        assert_eq!(cpu.ccr(), CCR_MASK);
        // S is untouched, so the stack pointers do not move.
        cpu.set_sr(SR_S);
        cpu.a[7] = 0x1000;
        cpu.usp = 0x2000;
        cpu.set_ccr(SR_C);
        assert!(cpu.is_supervisor());
        assert_eq!(cpu.a[7], 0x1000);
        assert_eq!(cpu.usp, 0x2000);
    }

    #[test]
    fn address_is_masked_to_24_bits() {
        let mut bus = Ram64k::new();
        bus.poke_long(0x0000_0004, 0xDEAD_BEEF);
        let mut cpu = M68k::new();

        assert_eq!(cpu.read_w(&mut bus, 0xFF00_0004), 0xDEAD);
        assert_eq!(bus.accesses, [0x0000_0004]);

        assert_eq!(cpu.read_l(&mut bus, 0xFF00_0004), 0xDEAD_BEEF);
        assert_eq!(
            bus.accesses,
            [0x0000_0004, 0x0000_0004, 0x0000_0006],
            "a long is two word accesses, high word first"
        );

        cpu.write_b(&mut bus, 0xFF00_0008, 0x12);
        cpu.write_w(&mut bus, 0xFF00_000A, 0x3456);
        cpu.write_l(&mut bus, 0xFF00_000C, 0x789A_BCDE);
        assert_eq!(bus.accesses, [4, 4, 6, 8, 0x0A, 0x0C, 0x0E]);
        assert_eq!(bus.peek_byte(0x0008), 0x12);
        assert_eq!(bus.peek_word(0x000A), 0x3456);
        assert_eq!(bus.peek_long(0x000C), 0x789A_BCDE);
    }

    #[test]
    fn bus_helpers_charge_four_clocks_per_access() {
        let mut bus = Ram64k::new();
        let mut cpu = M68k::new();
        assert_eq!(cpu.cycles, 0);

        cpu.read_b(&mut bus, 0x0000_0010);
        cpu.read_w(&mut bus, 0x0000_0010);
        cpu.write_b(&mut bus, 0x0000_0010, 1);
        cpu.write_w(&mut bus, 0x0000_0010, 2);
        assert_eq!(cpu.cycles, 16, "four single bus cycles");

        cpu.read_l(&mut bus, 0x0000_0010);
        assert_eq!(cpu.cycles, 24, "a long is two bus cycles");

        cpu.write_l(&mut bus, 0x0000_0010, 3);
        assert_eq!(cpu.cycles, 32, "a long is two bus cycles");
    }

    /// Reset charges the fixed 132-clock reset sequence *plus* every bus cycle
    /// the sequence performs: four word reads for the two long vector fetches
    /// and two word reads to fill the prefetch queue from the new PC, so
    /// 132 + 16 + 8 = 156 clocks.
    ///
    /// The `+ 16` this test asserted before M1-04 (#12) was deliberately changed
    /// to `+ 24`: the queue did not exist then, and "reset leaves the queue full"
    /// is two bus cycles the model did not previously pay for. `RESET_CYCLES`
    /// still means the reset sequence alone, which is what the 132 in the name
    /// refers to.
    #[test]
    fn reset_charges_132_clocks_plus_the_vector_and_queue_fetches() {
        let mut bus = bus_with_reset_vector();
        let mut cpu = M68k::new();

        cpu.reset(&mut bus);

        assert_eq!(bus.accesses.len(), 6, "four vector words, two queue words");
        assert_eq!(
            cpu.cycles,
            u64::from(RESET_CYCLES) + 16 + 8,
            "the 132-clock sequence, 16 clocks of vector fetch, 8 clocks of prefetch"
        );
    }

    // ---- M1-04 (#12): the prefetch queue and the instruction fetch ----

    #[test]
    fn reset_fills_prefetch() {
        let (cpu, _bus) = cpu_with_program(&[0x4E71, 0x1234]); // NOP, then a data word

        assert_eq!(
            cpu.pc, PROGRAM_BASE,
            "pc is the address of the queued opcode"
        );
        assert_eq!(
            cpu.prefetch,
            [0x4E71, 0x1234],
            "reset fills the queue with two word fetches from the new PC"
        );
    }

    #[test]
    fn fetch_extension_word_refills() {
        let (mut cpu, mut bus) = cpu_with_program(&[0x4E71, 0x2A53, 0x0679, 0xB50C]);
        let cycles = cpu.cycles;

        assert_eq!(
            cpu.fetch_ext_word(&mut bus),
            0x2A53,
            "the extension word comes out of the queue, already fetched"
        );
        assert_eq!(
            cpu.prefetch,
            [0x2A53, 0x0679],
            "the queue slid one word and refilled with the word at pc + 4"
        );
        assert_eq!(
            cpu.pc,
            PROGRAM_BASE + 2,
            "pc is the address of the queue front"
        );
        assert_eq!(
            cpu.cycles - cycles,
            4,
            "one word read only: the extension itself cost no bus access"
        );

        assert_eq!(cpu.fetch_ext_word(&mut bus), 0x0679, "and again");
        assert_eq!(cpu.prefetch, [0x0679, 0xB50C]);
        assert_eq!(cpu.pc, PROGRAM_BASE + 4);
        assert_eq!(cpu.cycles - cycles, 8, "one more word read");
    }

    #[test]
    fn fetch_extension_long_is_high_word_first() {
        let (mut cpu, mut bus) = cpu_with_program(&[0x4E71, 0x1234, 0x5678, 0x9ABC]);
        let cycles = cpu.cycles;

        assert_eq!(
            cpu.fetch_ext_long(&mut bus),
            0x1234_5678,
            "a long extension is two queue words, high first"
        );
        assert_eq!(cpu.pc, PROGRAM_BASE + 4);
        assert_eq!(cpu.prefetch, [0x5678, 0x9ABC]);
        assert_eq!(cpu.cycles - cycles, 8, "two word reads");
    }

    /// The end-of-instruction rotation, checked against the exact numbers
    /// `NOP.json.gz` case `4e71 [NOP] 1` records: `pc` $000C00 → $000C02 and the
    /// queue `[4E71, 2A53]` → `[2A53, 0679]`, from one word read at the old
    /// `pc + 4` — which is the whole of what a `NOP` is billed for (4 clocks).
    #[test]
    fn advance_opcode_finishes_the_instruction() {
        let (mut cpu, mut bus) = cpu_with_program(&[0x4E71, 0x2A53, 0x0679]);
        let cycles = cpu.cycles;

        cpu.advance_opcode(&mut bus);

        assert_eq!(cpu.pc, PROGRAM_BASE + 2, "pc is on the next opcode");
        assert_eq!(
            cpu.prefetch,
            [0x2A53, 0x0679],
            "the next opcode moved to the front of the queue and one new word was fetched"
        );
        assert_eq!(
            cpu.cycles - cycles,
            4,
            "one word read, the fetch a NOP pays for"
        );
    }

    #[test]
    fn refill_after_jump() {
        let (mut cpu, mut bus) = cpu_with_program(&[0x4E71, 0x2A53]);
        bus.load_words(0x0000_2000, &[0x4EB9, 0x1234, 0x5678]); // JSR abs.l ...
        let cycles = cpu.cycles;

        cpu.jump_to(&mut bus, 0x0000_2000);

        assert_eq!(cpu.pc, 0x0000_2000);
        assert_eq!(
            cpu.prefetch,
            [0x4EB9, 0x1234],
            "the jump throws the queue away and refetches two words from the target"
        );
        assert_eq!(cpu.cycles - cycles, 8, "two word fetches");
    }

    #[test]
    fn self_modifying_code_runs_old_word() {
        let (mut cpu, mut bus) = cpu_with_program(&[0x4E71, 0x2A53]);

        // The word at pc + 2 is already sitting in the queue, so rewriting it
        // in memory cannot reach the CPU before the queue does.
        bus.write_word(cpu.pc + 2, 0x1234);
        assert_eq!(
            bus.read_word(cpu.pc + 2),
            0x1234,
            "memory itself did change"
        );

        assert_eq!(
            cpu.fetch_ext_word(&mut bus),
            0x2A53,
            "the queue returns the word it fetched before the write"
        );
        assert_eq!(
            cpu.prefetch[0], 0x2A53,
            "and that word is now the queue front"
        );
    }

    // ---- step(): decode the queue front, dispatch, record the route ----

    /// Where a `step` that takes an exception pushes its frame, and the handler
    /// the vector table points at — both inside [`Ram64k`], and both different
    /// from `PROGRAM_BASE`, so a test can tell a frame from a fetch.
    const STACK_TOP: u32 = 0x0000_2000;
    const ILLEGAL_HANDLER: u32 = 0x0000_3000;

    /// A CPU with `opcode` in the queue front, which is what `step` runs. The
    /// registers are non-reset values on purpose, so any state a test finds
    /// changed is the step's doing and not a coincidence of `M68k::new`. The
    /// vector table points every vector at [`ILLEGAL_HANDLER`] and the stack is
    /// inside the bus, because a route that takes an exception now writes both.
    fn cpu_at_opcode(opcode: u16) -> (M68k, Ram64k) {
        let mut cpu = M68k::new();
        cpu.pc = PROGRAM_BASE;
        cpu.prefetch = [opcode, 0x4E71];
        cpu.d[0] = 0x1111_2222;
        cpu.a[0] = 0x0010_4000;
        cpu.set_sr(0x2700);
        let mut bus = Ram64k::new();
        cpu.ssp = STACK_TOP;
        cpu.a[7] = STACK_TOP;
        for slot in (0x00..=0x7Fu32).step_by(4) {
            bus.poke_long(slot, ILLEGAL_HANDLER);
        }
        bus.poke_long(ILLEGAL_HANDLER, 0x1234_5678);
        (cpu, bus)
    }

    /// Illegal, line A and line F all record the illegal-instruction vector and
    /// take its frame — the 68000 has no line emulator, so all three reserved
    /// routes take the one exception (M1-07, #15 routed them; M1-08, #16 gave
    /// them the frame; real line-A/F vectors are the 68010's, M1-12, #20).
    #[test]
    fn step_records_the_exception_vector_of_each_illegal_route() {
        use crate::exceptions::EXC_ILLEGAL;
        for (opcode, name) in [
            (0x4AFC, "ILLEGAL"),
            (0xA123, "line A"),
            (0xF123, "line F"),
            (0xA000, "line A, group start"),
            (0xAFFF, "line A, group end"),
            (0xF000, "line F, group start"),
            (0xFFFF, "line F, group end"),
        ] {
            let (mut cpu, mut bus) = cpu_at_opcode(opcode);
            let before = cpu.clone();
            assert_eq!(
                u64::from(cpu.step(&mut bus)),
                crate::exceptions::EXCEPTION_ENTRY_CLOCKS,
                "{name}: the whole step is the entry sequence, which charges its own 34 clocks"
            );
            assert_eq!(
                cpu.pending_exception,
                Some(EXC_ILLEGAL),
                "{name} (${opcode:04X}) must record the illegal-instruction vector"
            );
            // The frame, which is what the route is for: SR where it was, the
            // address of the opcode that could not execute, handler in `pc`.
            assert_eq!(cpu.a[7], STACK_TOP - 6, "{name} pushed a frame");
            assert_eq!(
                bus.peek_word(STACK_TOP - 6),
                0x2700,
                "{name}: SR word at SP, as it was before the exception"
            );
            assert_eq!(
                bus.peek_long(STACK_TOP - 4),
                PROGRAM_BASE,
                "{name}: PC long at SP + 2, pointing at the faulting opcode"
            );
            assert_eq!(
                cpu.pc, ILLEGAL_HANDLER,
                "{name}: PC from the long at $00000C"
            );
            assert_eq!(
                cpu.cycles - before.cycles,
                crate::exceptions::EXCEPTION_ENTRY_CLOCKS,
                "{name} costs 34 clocks (M68000UM execution times)"
            );
            // Only the exception's own fields move: no register is the
            // exception's business.
            assert_eq!(cpu.d, before.d, "{name} must not touch Dn");
            assert_eq!(cpu.a[..7], before.a[..7], "{name} must not touch An");
        }
    }

    /// The record describes the step that just ran, not the last exception the
    /// CPU ever took.
    #[test]
    fn step_replaces_the_previous_step_s_exception_record() {
        use crate::exceptions::EXC_ILLEGAL;
        let (mut cpu, mut bus) = cpu_at_opcode(0x4AFC);
        cpu.step(&mut bus);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
        // Another exception next, so the record is written twice and the point
        // of the test — replaced, not kept or merged — still applies. `step`
        // runs the queue front, so the queue is what decides (`step` refilled
        // it with the handler's words, which are `NOP`-free on purpose
        // elsewhere).
        cpu.prefetch = [0xA000, 0x4AFC];
        cpu.step(&mut bus);
        assert_eq!(
            cpu.pending_exception,
            Some(EXC_ILLEGAL),
            "the same route twice still records it once"
        );

        cpu.pending_exception = Some(99);
        cpu.prefetch[0] = 0xA000;
        cpu.step(&mut bus);
        assert_eq!(
            cpu.pending_exception,
            Some(EXC_ILLEGAL),
            "replaced, not kept"
        );
    }

    /// An unimplemented instruction is a panic, not a silent no-op: a suite
    /// that reported a passing case for an instruction that never ran would be
    /// worse than one that stops.
    #[test]
    fn step_panics_loudly_on_an_unimplemented_opcode() {
        // `$4E74`, which has no handler and is the hole left in `$4E70..=$4E77`
        // once RESET, NOP, RTE, RTS, TRAPV and RTR are taken. This used to be
        // `4E71` before M1-17 (#25), then `4E70` before #35 gave RESET one; what
        // the test needs is any opcode that has none, and `decode::tests` keeps
        // `4E74` as the same anchor on the other side of the decoder.
        let (mut cpu, mut bus) = cpu_at_opcode(0x4E74);
        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cpu.step(&mut bus)))
            .expect_err("an opcode with no handler must not let step return");
        let text = error
            .downcast_ref::<String>()
            .expect("the panic payload is a formatted message");
        assert!(
            text.contains("m68k opcode 4E74"),
            "the panic must name the opcode it could not run, got {text:?}"
        );
    }

    /// The instruction `step` runs is `prefetch[0]`, not whatever a read of
    /// `pc` would return: the queue front here is a line A opcode while the
    /// word behind it is the unimplemented `$4E74`, so a `step` that fetched
    /// from `pc` or decoded `prefetch[1]` would panic instead of routing.
    #[test]
    fn step_runs_the_instruction_at_the_front_of_the_queue() {
        use crate::exceptions::{EXC_ILLEGAL, EXCEPTION_ENTRY_CLOCKS};
        let (mut cpu, mut bus) = cpu_at_opcode(0x4E74);
        cpu.prefetch = [0xA000, 0x4E74];
        assert_eq!(
            u64::from(cpu.step(&mut bus)),
            EXCEPTION_ENTRY_CLOCKS,
            "an illegal opcode costs the exception entry, which is the whole step"
        );
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));

        // And the same opcode written into memory at `pc` is still not what it
        // runs, because the queue front is.
        let (mut cpu, mut bus) = cpu_at_opcode(0xA000);
        bus.mem[PROGRAM_BASE as usize] = 0x4E;
        bus.mem[PROGRAM_BASE as usize + 1] = 0x74;
        assert_eq!(u64::from(cpu.step(&mut bus)), EXCEPTION_ENTRY_CLOCKS);
        assert_eq!(cpu.pending_exception, Some(EXC_ILLEGAL));
    }

    /// With HALT asserted the CPU stops accessing the bus; the clocks are a
    /// placeholder until M1-31 (#39) times the interrupt-acknowledge sequence.
    #[test]
    fn step_charges_halted_clocks_and_touches_no_bus() {
        let (mut cpu, mut bus) = cpu_at_opcode(0x4E71); // would panic if decoded
        cpu.halted = true;
        assert_eq!(cpu.step(&mut bus), HALTED_CLOCKS);
        assert_ne!(HALTED_CLOCKS, 0, "a halted step must still move time on");
        assert_eq!(cpu.cycles, u64::from(HALTED_CLOCKS));
        assert_eq!(cpu.step(&mut bus), HALTED_CLOCKS);
        assert_eq!(
            cpu.cycles,
            u64::from(HALTED_CLOCKS) * 2,
            "and keeps moving it on"
        );
        assert!(
            bus.accesses.is_empty(),
            "no bus traffic while halted: {:?}",
            bus.accesses
        );
        assert_eq!(cpu.pc, PROGRAM_BASE, "HALT is not a fetch");
        assert_eq!(cpu.pending_exception, None, "HALT is not an exception");
    }
}
