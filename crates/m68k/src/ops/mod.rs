//! Instruction handlers: one per [`crate::decode::Op`] variant (M1-07, #15).
//!
//! [`M68k::step`] decodes the opcode and calls [`M68k::run_op`], which routes
//! the variant to its handler here. Every instruction ticket from M1-08 (#16)
//! onward adds a handler and a `match` arm; `step()` itself does not change.
//!
//! Handlers take the opcode and re-read their own operand fields from its bits
//! (which is why [`crate::decode::Op`] carries no payload), and take
//! `bus: &mut impl Bus` as soon as they touch memory — the exception stubs
//! below do not, so they do not take it yet.
//!
//! A handler returns the 68k clocks the instruction cost. [`M68k::step`] adds
//! that to [`M68k::cycles`] once, so a handler must not also charge the clocks
//! it returns; the self-charging bus helpers (`read_w` and friends) and the EA
//! layer's internal clocks are the exception, and how those three numbers add
//! up is M1-31 (#39/#40)'s business, not this module's.

use crate::cpu::M68k;
use crate::decode::Op;

/// Exception vector numbers, as recorded by [`M68k::pending_exception`].
///
/// These are the M68000UM "Exception Vector Memory Map" numbers, i.e. the slot
/// address divided by four: vector `n` has its handler long at `$000000 + 4n`.
/// The `ILLEGAL` entry is vector 3, the long at `$00000C` — which is what this
/// crate already documents (`tests/harness/mod.rs`: `$000008` is *address
/// error*, `$00000C` is *illegal instruction*), and what the pinned 680x0
/// SingleStepTests suite does: the cases that end in an exception fetch their
/// handler long from `$00000C` (measured over `CHK.json.gz` and `DIVS.json.gz`,
/// whose reserved-size-field cases all land there).
///
/// Ticket #15 gave 4, 10 and 11 for these three. 4 is the divide-by-zero slot
/// at `$000010` and cannot be illegal instruction, and 10/11 are the
/// `Unassigned` slots at `$000028`/`$00002C`; the ticket numbers are recorded
/// on #15 and #16 owns reconciling the whole map.
pub(crate) const EXC_ILLEGAL: u8 = 3;
/// Line A (`1010`) emulator trap: vector 34, handler long at `$000088`.
///
/// UNCONFIRMED: the pinned 680x0 suite has no A-line or F-line file, so no
/// measurement of ours pins this slot — M1-08 (#16) verifies it against
/// M68000UM section 8 ("Exception Processing") when the real frame exists.
pub(crate) const EXC_LINE_A: u8 = 34;
/// Line F (`1111`) emulator trap: vector 35, handler long at `$00008C`.
///
/// UNCONFIRMED, exactly as [`EXC_LINE_A`]: no suite coverage, #16 verifies.
pub(crate) const EXC_LINE_F: u8 = 35;

impl M68k {
    /// Run the operation `op`, which [`crate::decode`] found for `opcode`, and
    /// return the 68k clocks it cost.
    pub(crate) fn run_op(&mut self, op: Op, opcode: u16) -> u32 {
        match op {
            Op::Illegal => self.op_illegal(),
            Op::LineA => self.op_line_a(),
            Op::LineF => self.op_line_f(),
            // `Op::Unimplemented`, and every variant a later ticket adds before
            // it has a handler here: loud, so a missing instruction can never
            // be mistaken for a working one. A wildcard rather than naming
            // `Op::Unimplemented` so a new variant arrives here by default
            // instead of failing to compile in one place and being "fixed" by
            // a silent arm.
            _ => self.op_unimplemented(opcode),
        }
    }

    /// `ILLEGAL` and any encoding decode called illegal: the illegal-instruction
    /// exception, vector [`EXC_ILLEGAL`].
    pub(crate) fn op_illegal(&mut self) -> u32 {
        self.exception_stub(EXC_ILLEGAL)
    }

    /// The `1010` group: the line A emulator trap, vector [`EXC_LINE_A`].
    pub(crate) fn op_line_a(&mut self) -> u32 {
        self.exception_stub(EXC_LINE_A)
    }

    /// The `1111` group: the line F emulator trap, vector [`EXC_LINE_F`].
    pub(crate) fn op_line_f(&mut self) -> u32 {
        self.exception_stub(EXC_LINE_F)
    }

    /// An instruction the core does not have yet.
    ///
    /// Panicking is the requirement (ticket #15): a silent no-op would let the
    /// conformance sweep report a passing case that never executed anything.
    /// The SingleStepTests harness catches this per case and reports the text
    /// (see `tests/harness/mod.rs`), so a whole-suite run still finishes.
    fn op_unimplemented(&self, opcode: u16) -> ! {
        unimplemented!("m68k opcode {opcode:04X}")
    }

    /// Record that the CPU took the exception `vector`, and nothing else.
    ///
    /// A stub on purpose, and deliberately an incomplete one: real exception
    /// entry stops the current instruction, pushes the 6-byte frame (SR then
    /// PC) under the new SSP, and fetches the handler long from
    /// `$000000 + 4 * vector` — that is M1-08 (#16). Half-building a frame here
    /// would leave a PC, queue and stack that #16 has to unpick, so this
    /// changes no register, no memory and no clock; it only leaves the vector
    /// number somewhere a test can see which route a decoded opcode took.
    fn exception_stub(&mut self, vector: u8) -> u32 {
        self.pending_exception = Some(vector);
        0
    }
}
