//! The command line of the `rustsega` binary (M8-02, #156).
//!
//! Parsing is exercised through [`Args::try_parse_from`], which is the same code
//! `Args::parse` runs with `std::env::args`. The environment is not set from
//! here: `std::env::set_var` is `unsafe` since edition 2024 and the workspace
//! forbids `unsafe`, so the `#[arg(env = ...)]` wiring is pinned by reading it
//! back off the built [`clap::Command`] instead.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser as _};
use server::cli::Args;
use tracing::level_filters::LevelFilter;

/// The crate directory, which is a directory and so is legal for `--rom-dir`.
fn a_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The four defaults `--help` promises, read off the command.
///
/// Not read off a bare `try_parse_from`, which is the same claim once the
/// environment is allowed to answer: `nix develop` exports `RUSTSEGA_ROM_DIR`
/// (flake.nix) for `testkit`'s commercial-ROM lookup, so in this checkout a bare
/// parse reports that directory rather than no directory. The declared default is
/// what `--help` prints and what a parse falls back to when nothing overrode it -
/// and `crates/server/tests/running_binary.rs` covers the parse with an
/// environment it chose itself.
#[test]
fn the_documented_defaults_are_loopback_port_8080_info_and_no_library() {
    assert_eq!(default_of("bind"), Some("127.0.0.1".to_owned()));
    assert_eq!(default_of("port"), Some("8080".to_owned()));
    assert_eq!(default_of("log"), Some("info".to_owned()));
    assert_eq!(
        default_of("rom-dir"),
        None,
        "the ROM library is opt-in: a server that found one by itself would be a server nobody asked to"
    );
}

/// The command line of the ticket, in one parse.
///
/// The `--rom-dir` of this test is not the `--rom-dir` of the ambient
/// `RUSTSEGA_ROM_DIR`, so the assertion doubles as proof that a flag wins over
/// the environment - which is `clap`'s precedence, and the reason the dev shell's
/// exported variable cannot silently outrank what the operator typed.
#[test]
fn the_flags_of_the_ticket_are_parsed() {
    let args = Args::try_parse_from([
        "rustsega",
        "--bind",
        "0.0.0.0",
        "--port",
        "9000",
        "--rom-dir",
        "/tmp/roms",
    ])
    .expect("the ticket's own command line must parse");

    assert_eq!(args.bind, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(args.port, 9000);
    assert_eq!(args.rom_dir, Some(PathBuf::from("/tmp/roms")));
    assert_eq!(args.addr().to_string(), "0.0.0.0:9000");
}

#[test]
fn the_log_level_is_a_tracing_level() {
    let args = Args::try_parse_from(["rustsega", "--log", "debug"]).unwrap();
    assert_eq!(args.log, LevelFilter::DEBUG);

    // The names are the tracing ones, and they are what a typo is judged against.
    for level in ["trace", "debug", "info", "warn", "error", "off"] {
        Args::try_parse_from(["rustsega", "--log", level])
            .unwrap_or_else(|error| panic!("--log {level} must parse: {error}"));
    }
    let error = Args::try_parse_from(["rustsega", "--log", "verbose"])
        .expect_err("--log only takes a tracing level");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn a_port_that_is_not_a_port_is_an_error() {
    for port in ["notaport", "0.0", "65536", "-1"] {
        let error =
            Args::try_parse_from(["rustsega", "--port", port]).expect_err("--port must be a u16");
        assert_eq!(error.exit_code(), 2, "--port {port} was accepted");
    }
}

#[test]
fn an_address_that_is_not_an_address_is_an_error() {
    let error = Args::try_parse_from(["rustsega", "--bind", "localhost"])
        .expect_err("--bind takes an address, not a name");

    assert_eq!(error.exit_code(), 2);
}

#[test]
fn every_flag_has_its_documented_env_var() {
    assert_eq!(env_of("bind"), "RUSTSEGA_BIND");
    assert_eq!(env_of("port"), "RUSTSEGA_PORT");
    assert_eq!(env_of("rom-dir"), "RUSTSEGA_ROM_DIR");
    assert_eq!(env_of("log"), "RUSTSEGA_LOG");
}

/// The default `--flag` declares, as `--help` would print it.
fn default_of(flag: &str) -> Option<String> {
    let command = Args::command();
    let arg = command
        .get_arguments()
        .find(|arg| arg.get_long() == Some(flag))
        .unwrap_or_else(|| panic!("no --{flag} flag"));
    arg.get_default_values()
        .first()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
}

/// The environment variable `--flag` falls back to, as declared on the command.
///
/// `set_var` is `unsafe` in edition 2024, so this reads the declaration rather
/// than the environment: it proves the `#[arg(env = ...)]` attribute is there,
/// and `clap`'s own `env` feature is what turns it into a read at parse time.
fn env_of(flag: &str) -> String {
    let command = Args::command();
    let arg = command
        .get_arguments()
        .find(|arg| arg.get_long() == Some(flag))
        .unwrap_or_else(|| panic!("no --{flag} flag"));
    arg.get_env()
        .and_then(|value| value.to_str())
        .unwrap_or_else(|| panic!("--{flag} reads no environment variable"))
        .to_owned()
}

#[test]
fn help_documents_every_flag_and_its_env_var() {
    // The acceptance criterion is that `rustsega --help` explains all of it, so
    // this is that help text, searched for the flag names and the env names.
    let help = format!(
        "{}\n{}",
        Args::command().render_help(),
        Args::command().render_long_help(),
    );

    for flag in ["--bind", "--port", "--rom-dir", "--log"] {
        assert!(help.contains(flag), "--help never mentions {flag}");
    }
    for var in [
        "RUSTSEGA_BIND",
        "RUSTSEGA_PORT",
        "RUSTSEGA_ROM_DIR",
        "RUSTSEGA_LOG",
    ] {
        assert!(help.contains(var), "--help never mentions {var}");
    }
}

#[test]
fn a_rom_dir_that_is_not_there_is_refused_with_usage_status() {
    let missing = a_directory().join("no-such-rom-dir");
    let error = server::cli::check_rom_dir(Some(missing.clone()))
        .expect_err("a missing ROM dir must not start the server");

    assert_eq!(error.exit_code(), 2);
    let rendered = error.to_string();
    assert!(
        rendered.contains(&missing.display().to_string()) && rendered.contains("rom-dir"),
        "the error has to say which flag and which path: {rendered}"
    );
}

#[test]
fn a_rom_dir_that_is_a_file_is_refused_with_usage_status() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("Cargo.toml")
        .to_path_buf();
    let error =
        server::cli::check_rom_dir(Some(file)).expect_err("a file is not a directory to list");

    assert_eq!(error.exit_code(), 2);
}

#[test]
fn a_real_directory_is_accepted_and_a_missing_flag_stays_a_missing_library() {
    assert_eq!(server::cli::check_rom_dir(None).unwrap(), None);
    assert_eq!(
        server::cli::check_rom_dir(Some(a_directory())).unwrap(),
        Some(a_directory())
    );
}
