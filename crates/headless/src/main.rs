//! Headless CLI runner: runs a ROM without a browser, used by integration tests
//! and by anyone who wants a frame out of the emulator without a window.
//!
//! This file is only the command-line shape - one subcommand, `run` - whose whole
//! behaviour lives in the `run` module. `sweep` (M6-05, #125) will be the second
//! variant.
//!
//! Failures are reported as `error: <what and which path>` on stderr and exit
//! status 1; `--help` is a success. See the crate's tests in `tests/cli.rs`.

mod run;
mod wav;

use clap::Parser;

use crate::run::RunArgs;

/// The whole command line: `headless run <ROM> [--frames N] [--dump-png PATH]
/// [--dump-wav PATH] [--hash] [--audio-hash] [--region auto|usa|japan|europe]
/// [--pad1 BUTTONS@FRAME,...]`.
#[derive(Parser)]
#[command(
    name = "headless",
    version,
    about = "Run a Sega Genesis ROM without a browser."
)]
enum Cli {
    /// Run a ROM for N frames, then print its frame hash and/or dump a PNG.
    Run(RunArgs),
}

fn main() -> std::process::ExitCode {
    let Cli::Run(args) = Cli::parse();
    match run::run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // `{:#}` is the whole anyhow context chain: "cannot read ROM \"x\":
        // No such file or directory (os error 2)".
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
