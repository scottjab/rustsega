//! Motorola 68000 CPU core.
//!
//! No dependencies on other crates (see `docs/ARCHITECTURE.md`): the CPU talks
//! to memory and I/O through the [`Bus`] trait.
//!
//! Start a CPU with [`M68k::new`] then [`M68k::reset`], which loads the stack
//! pointer and program counter from the reset vector table like real hardware
//! does.
//!
//! Not implemented yet: the prefetch queue (M1-04, #12), addressing modes,
//! instructions, exceptions and interrupts. [`M68k::step`] is a stub.

mod bus;
mod cpu;
mod sr;

/// Always compiled, not `#[cfg(test)]`: integration tests under
/// `crates/m68k/tests/` and other crates need `m68k::testbus::TestBus` too.
pub mod testbus;

pub use bus::{ADDR_MASK, Bus};
pub use cpu::{M68k, RESET_CYCLES};
pub use sr::{CCR_MASK, SR_C, SR_INT_MASK, SR_MASK, SR_N, SR_RESET, SR_S, SR_T, SR_V, SR_X, SR_Z};
