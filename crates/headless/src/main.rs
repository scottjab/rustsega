//! Headless CLI runner: runs a ROM without a browser, used by integration tests
//! and by anyone who wants a frame out of the emulator without a window.
//!
//! This file is only the command-line shape - the subcommands `run`, `vgm` and
//! `sweep`, whose whole behaviour lives in the modules of the same name - and the
//! one thing it does itself is turn a `Result` into an exit status.
//!
//! Failures are reported as `error: <what and which path>` on stderr and exit
//! status 1; `--help` is a success. See the crate's tests in `tests/cli.rs`.

use clap::Parser;

use headless::run::RunArgs;
use headless::sweep::SweepArgs;
use headless::vgm::VgmArgs;
use headless::{run, sweep, vgm};

/// The whole command line: `headless run <ROM> [--frames N] [--dump-png PATH]
/// [--dump-wav PATH] [--hash] [--audio-hash] [--region auto|usa|japan|europe]
/// [--pad1 BUTTONS@FRAME,...] [--movie PATH] [--record PATH]`, or
/// `headless vgm <VGM> [--out PATH]`.
#[derive(Parser)]
#[command(
    name = "headless",
    version,
    about = "Run a Sega Genesis ROM without a browser."
)]
enum Cli {
    /// Run a ROM for N frames, then print its frame hash and/or dump a PNG.
    Run(RunArgs),
    /// Play a VGM or VGZ file through the YM2612 and the SN76489.
    Vgm(VgmArgs),
    /// Run every ROM in a directory and write a compatibility report.
    Sweep(SweepArgs),
}

fn main() -> std::process::ExitCode {
    let played = match Cli::parse() {
        Cli::Run(args) => run::run(args),
        Cli::Vgm(args) => vgm::run(args),
        Cli::Sweep(args) => sweep::run(args),
    };
    match played {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // `{:#}` is the whole anyhow context chain: "cannot read ROM \"x\":
        // No such file or directory (os error 2)".
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
