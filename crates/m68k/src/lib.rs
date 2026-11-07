//! Motorola 68000 CPU core.
//!
//! No dependencies on other crates (see `docs/ARCHITECTURE.md`): the CPU talks
//! to memory and I/O through the [`Bus`] trait.
//!
//! Start a CPU with [`M68k::new`] then [`M68k::reset`], which loads the stack
//! pointer and program counter from the reset vector table like real hardware
//! does.
//!
//! Instructions are fetched through a two-word prefetch queue (M1-04, #12):
//! `pc` is the address of the queued opcode and `prefetch[1]` the following
//! word, which is the convention the SingleStepTests suite uses — see
//! [`M68k::pc`].
//!
//! [`M68k::step`] decodes the opcode at the front of the queue through the
//! table in [`decode`] and dispatches to the handlers in [`ops`] (M1-07, #15).
//! No instruction is implemented yet, so every real opcode takes the loud
//! `unimplemented!` path; what exists is the routing, the illegal / line A /
//! line F exception stubs, and the effective-address layer the handlers will
//! use.
//!
//! Not implemented yet: instructions, real exception frames, interrupts.

mod bus;
mod cpu;
mod decode;
mod ea;
mod exceptions;
mod ops;
mod sr;

/// Always compiled, not `#[cfg(test)]`: integration tests under
/// `crates/m68k/tests/` and other crates need `m68k::testbus::TestBus` too.
pub mod testbus;

pub use bus::{ADDR_MASK, Bus};
pub use cpu::{M68k, RESET_CYCLES};
pub use ea::{Ea, Size};
pub use exceptions::{
    EXC_CHK, EXC_DIV_ZERO, EXC_ILLEGAL, EXC_PRIVILEGE, EXC_TRAPV, TRAP_VECTOR_BASE, trap_vector,
};
pub use sr::{CCR_MASK, SR_C, SR_INT_MASK, SR_MASK, SR_N, SR_RESET, SR_S, SR_T, SR_V, SR_X, SR_Z};
