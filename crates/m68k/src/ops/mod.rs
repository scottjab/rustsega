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

/// The illegal-instruction exception: vector 3, handler long at `$00000C`.
///
/// This is the only exception vector this module needs, because it is the only
/// one the 68000 takes from a decoded opcode: [`Op::LineA`] and [`Op::LineF`]
/// take it too (see [`M68k::op_illegal`]).
///
/// # Which number a vector is: the convention this crate uses
///
/// Vector `n` here means "the handler long is at `$000000 + 4 * n`" — the
/// number is the slot address divided by four. That is not a style choice, it
/// is what the pinned suite does, and it is measurable: every `TRAP #n` case in
/// `TRAP.json.gz` installs its handler long at `$0080 + 4 * n` (measured over
/// all sixteen — `4e40` reads `$0080..=$0083`, `4e42` reads `$0088..=$008B`,
/// `4e4f` reads `$00BC..=$00BF`), which makes TRAP #n vector `32 + n`. The
/// harness reads the same map from the other end (`tests/harness/mod.rs`:
/// `$000008` is *address error*, `$00000C` is *illegal instruction*), and the
/// 178 087 exception cases the sweep skips all fetch from `$00000C`.
///
/// Motorola's "Exception Vector Memory Map" figure labels the same slots one
/// higher, because it counts the reset pair as vectors 1 and 2: illegal
/// instruction is "vector 4" there and vector 3 here. Same slot, same long,
/// two labelling conventions — which is why tickets #15 and #16 both quote "4"
/// for it. Anyone adding a vector: take the slot *address* from the hardware
/// map first and divide by four, rather than quoting a number out of a manual.
/// M1-08 (#16) reconciles the whole map in `exceptions.rs`; the measurement is
/// recorded on #15 and on the reconciliation issue so the two cannot diverge.
pub(crate) const EXC_ILLEGAL: u8 = 3;

impl M68k {
    /// Run the operation `op`, which [`crate::decode`] found for `opcode`, and
    /// return the 68k clocks it cost.
    pub(crate) fn run_op(&mut self, op: Op, opcode: u16) -> u32 {
        match op {
            Op::Illegal | Op::LineA | Op::LineF => self.op_illegal(),
            // `Op::Unimplemented`, and every variant a later ticket adds before
            // it has a handler here: loud, so a missing instruction can never
            // be mistaken for a working one. A wildcard rather than naming
            // `Op::Unimplemented` so a new variant arrives here by default
            // instead of failing to compile in one place and being "fixed" by
            // a silent arm.
            _ => self.op_unimplemented(opcode),
        }
    }

    /// The illegal-instruction exception, vector [`EXC_ILLEGAL`] — and the
    /// route for the `1010` and `1111` groups as well.
    ///
    /// The 68000 has no line-emulator mechanism at all. M68000UM's instruction
    /// formats list Format I ("Line 1010 emulator instruction") and Format J
    /// ("Line 1111 emulator instruction"), but the MC68000 does not implement
    /// either, so those encodings are simply reserved and a reserved opcode
    /// raises the illegal-instruction exception — one vector, three routes. The
    /// separate line-A and line-F emulator vectors are a 68010 addition, and
    /// giving them their own vectors is M1-12 (#20)'s job; `Op::LineA` and
    /// `Op::LineF` stay distinct in the decode table so #20 has the distinction
    /// to route on. There is deliberately no line-A/line-F vector constant here:
    /// the pinned 680x0 suite has no A-line or F-line file, so nothing of ours
    /// could pin one, and a number nobody can check is worse than none.
    pub(crate) fn op_illegal(&mut self) -> u32 {
        self.exception_stub(EXC_ILLEGAL)
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
