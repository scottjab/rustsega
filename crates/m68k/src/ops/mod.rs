//! Instruction handlers: one per [`crate::decode::Op`] variant (M1-07, #15).
//!
//! [`M68k::step`] decodes the opcode and calls [`M68k::run_op`], which routes
//! the variant to its handler here. Every instruction ticket from M1-08 (#16)
//! onward adds a handler and a `match` arm; `step()` itself does not change.
//!
//! Handlers take the opcode and re-read their own operand fields from its bits
//! (which is why [`crate::decode::Op`] carries no payload), and take
//! `bus: &mut impl Bus` as soon as they touch memory — TRAP and the illegal
//! routes do, so both take it here.
//!
//! A handler returns the 68k clocks the instruction cost. [`M68k::step`] adds
//! that to [`M68k::cycles`] once, so a handler must not also charge the clocks
//! it returns; the self-charging bus helpers (`read_w` and friends) and the EA
//! layer's internal clocks are the exception, and how those three numbers add
//! up is M1-31 (#39/#40)'s business, not this module's. That is why the
//! exception handlers below return 0: the entry sequence they run charges its
//! own 34 clocks (see [`crate::exceptions`]).
//!
//! # The exception path
//!
//! `Op::Illegal` covers the reserved opcodes, and `Op::LineA` / `Op::LineF`
//! cover the `1010` and `1111` groups — see [`crate::decode`]. All three run
//! [`M68k::op_illegal`], which enters the illegal-instruction exception. That is
//! the 68000's behaviour: the MC68000 has no line emulator, so Formats I and J
//! in M68000UM are reserved encodings there and a reserved opcode raises the
//! illegal-instruction exception (M68000PM section 8, "ILLEGAL INSTRUCTION").
//! The separate line-A and line-F emulator vectors are a 68010 feature, and #20
//! (M1-12) adds them then — which is why `Op::LineA` and `Op::LineF` are
//! distinct variants rather than aliases of `Op::Illegal`.
//!
//! `Op::Trap` is the other opcode that is nothing but an exception: `4E40 + n`,
//! one word, vector `32 + n` (M68000PM section 8, "TRAP").
//!
//! The frame, the vector map and the measurements behind each number live in
//! [`crate::exceptions`] (M1-08, #16); this module only says which opcode takes
//! which route.

mod arith;
mod move_;

use crate::M68k;
use crate::bus::Bus;
use crate::decode::Op;
use crate::exceptions::{EXC_ILLEGAL, trap_vector};

/// Width of a mode or register field, the mask [`field`] applies.
const FIELD_MASK: u16 = 0b111;

/// One 3-bit field of `opcode` at `shift`.
///
/// Every handler here re-reads its own operand fields from the opcode, so this
/// is shared rather than repeated per module.
#[inline]
pub(crate) const fn field(opcode: u16, shift: u32) -> u8 {
    // `as` rather than `u8::from` because this is a `const fn` and `From` is not
    // a const trait on this toolchain (see `ea::sign_extend_word`).
    ((opcode >> shift) & FIELD_MASK) as u8
}

/// Width in bytes of a `TRAP #n` instruction: the single word `4E40 + n`, which
/// is what makes the address the frame carries `pc + 2` (M68000PM section 8,
/// "TRAP"; measured in `TRAP.json.gz`, where every frame holds `pc + 2`).
const TRAP_WIDTH: u32 = 2;

/// Low nibble of a TRAP opcode: the trap number, `4E40 + n`.
const TRAP_NUMBER_MASK: u16 = 0x000F;

impl M68k {
    /// Run the operation `op`, which [`crate::decode`] found for `opcode`, and
    /// return the 68k clocks it cost.
    pub(crate) fn run_op(&mut self, bus: &mut impl Bus, op: Op, opcode: u16) -> u32 {
        match op {
            Op::Illegal | Op::LineA | Op::LineF => self.op_illegal(bus),
            Op::Trap => self.op_trap(bus, opcode),
            // The move class (`0001`/`0010`/`0011` and `0111`). Each handler
            // re-reads the size, destination and source fields from the opcode,
            // so the tag carries no payload (see [`move_]).
            Op::Move => self.op_move(bus, opcode),
            Op::Movea => self.op_movea(bus, opcode),
            Op::Moveq => self.op_moveq(bus, opcode),
            // The add and subtract classes (`1101`/`1001`, `0000 0110`/`0000
            // 0100` and the two halves of `0101`). As above, each handler
            // re-reads its fields from the opcode (see [`arith]).
            Op::Add => self.op_add(bus, opcode),
            Op::Adda => self.op_adda(bus, opcode),
            Op::Addx => self.op_addx(bus, opcode),
            Op::Addi => self.op_addi(bus, opcode),
            Op::Addq => self.op_addq(bus, opcode),
            Op::Sub => self.op_sub(bus, opcode),
            Op::Suba => self.op_suba(bus, opcode),
            Op::Subx => self.op_subx(bus, opcode),
            Op::Subi => self.op_subi(bus, opcode),
            Op::Subq => self.op_subq(bus, opcode),
            // `Op::Unimplemented`, and every variant a later ticket adds before
            // it has a handler here: loud, so a missing instruction can never
            // be mistaken for a working one. A wildcard rather than naming
            // `Op::Unimplemented` so a new variant arrives here by default
            // instead of failing to compile in one place and being "fixed" by
            // a silent arm.
            _ => self.op_unimplemented(opcode),
        }
    }

    /// The illegal-instruction exception, [`EXC_ILLEGAL`] — and the route for
    /// the `1010` and `1111` groups as well.
    ///
    /// The 68000 has no line-emulator mechanism at all. M68000UM's instruction
    /// formats list Format I ("Line 1010 emulator instruction") and Format J
    /// ("Line 1111 emulator instruction"), but the MC68000 does not implement
    /// either, so those encodings are simply reserved and a reserved opcode
    /// raises the illegal-instruction exception — one vector, three routes. The
    /// separate line-A and line-F emulator vectors are a 68010 addition, and
    /// giving them their own vectors is M1-12 (#20)'s job; `Op::LineA` and
    /// `Op::LineF` stay distinct in the decode table so #20 has the distinction
    /// to route on. There is deliberately no line-A/line-F vector constant in
    /// this crate: see [`crate::exceptions`].
    ///
    /// The frame carries `self.pc`, the address of the opcode that could not be
    /// executed, so a handler can decode it (M68000PM "ILLEGAL INSTRUCTION").
    /// The queue is not popped: the exception discards it and refills from the
    /// handler.
    ///
    /// Charges the exception entry's 34 clocks through
    /// [`M68k::exception`](crate::M68k::exception) and returns 0, like every
    /// other handler here (see the module docs and the M1-31 note in
    /// [`M68k::step`]).
    pub(crate) fn op_illegal(&mut self, bus: &mut impl Bus) -> u32 {
        let faulting_pc = self.pc;
        self.exception(bus, EXC_ILLEGAL, faulting_pc);
        0
    }

    /// `TRAP #n`: enter the trap exception for the opcode's low nibble, with
    /// the address of the instruction *after* the TRAP in the frame, so the
    /// handler returns to the code following it (M68000PM section 8, "TRAP").
    ///
    /// TRAP is one word wide, so that address is `pc + 2` — measured, not
    /// assumed: all 8065 `TRAP.json.gz` cases have `pc + 2` in the frame.
    ///
    /// Clocks: as for [`M68k::op_illegal`], the 34 clocks are charged by the
    /// entry sequence itself and this returns 0.
    pub(crate) fn op_trap(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let number = (opcode & TRAP_NUMBER_MASK) as u8;
        let return_pc = self.pc.wrapping_add(TRAP_WIDTH);
        self.exception(bus, trap_vector(number), return_pc);
        0
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
}
