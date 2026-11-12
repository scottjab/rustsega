//! The parts of the `headless` CLI, as a library.
//!
//! They are here rather than in `main.rs` so `tests/` can drive them directly:
//! `tests/cli.rs` runs the binary, because argument parsing and the exit status
//! are what those tests are about, and `tests/vgm.rs` needs the parser and the
//! player themselves to build a VGM in memory and hash the PCM it plays to.
//! `main.rs` is the argument parsing and the exit status and nothing else.

pub mod run;
pub mod sweep;
pub mod vgm;
pub mod wav;
