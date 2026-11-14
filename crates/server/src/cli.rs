//! The command line of the single binary (M8-02, #156).
//!
//! Every setting is a flag with an environment equivalent, because the same
//! binary is run by a person from a shell and by a container that can only pass
//! variables. Precedence is `clap`'s: a flag on the command line beats the
//! variable, which beats the default here.
//!
//! Parsing is kept apart from the one filesystem check ([`check_rom_dir`]) so
//! that `crates/server/tests/cli.rs` can drive it from a `&str` array with no
//! working directory involved.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{CommandFactory, Error, Parser};
use tracing::level_filters::LevelFilter;

/// `--bind` when neither the flag nor `RUSTSEGA_BIND` says otherwise: this
/// machine only. A server that answers its whole network by default is a hole
/// nobody decided to open, and the emulator has no authentication to stand in it.
const DEFAULT_BIND: &str = "127.0.0.1";

/// `--port` when neither the flag nor `RUSTSEGA_PORT` says otherwise.
const DEFAULT_PORT: u16 = 8080;

/// `--log` when neither the flag nor `RUSTSEGA_LOG` says otherwise. A name, not a
/// [`LevelFilter`], so that `--help` prints it the way the level names are
/// written in the tracing docs.
const DEFAULT_LOG: &str = "info";

/// Serve the rustsega frontend over HTTP.
///
/// The frontend is compiled into this binary by `nix develop -c just build`.
/// Without it the server runs and serves one page explaining how to build it.
///
/// Every flag has an environment variable, so a container needs no command line.
/// TLS is deliberately not built in: terminate it in a reverse proxy in front of
/// this, and forward the `Host` header.
#[derive(Parser, Debug)]
#[command(name = "rustsega", version)]
pub struct Args {
    /// Address to bind: 127.0.0.1 for this machine only, 0.0.0.0 for the network
    ///
    /// Loopback by default, and an address rather than a hostname so that no
    /// name lookup happens at startup: a server that will not start because
    /// `localhost` resolved to `::1` while the client came in over IPv4 is a
    /// confusing failure, and `getaddrinfo` is not what should decide it.
    #[arg(long, default_value = DEFAULT_BIND, env = "RUSTSEGA_BIND", value_name = "ADDR")]
    pub bind: IpAddr,

    /// Port to bind
    ///
    /// `0` asks the operating system for a free one, and the startup line then
    /// names the port that came back.
    #[arg(
        long,
        default_value_t = DEFAULT_PORT,
        env = "RUSTSEGA_PORT",
        value_name = "PORT"
    )]
    pub port: u16,

    /// Directory of ROMs to offer the frontend
    ///
    /// Has to exist, and be a directory, before the server starts: a typo would
    /// otherwise be an empty library, which is not the kind of thing that reports
    /// itself. It is scanned once at startup and served as `GET /api/roms`
    /// (M8-03, #157).
    ///
    /// `RUSTSEGA_ROM_DIR` is the name this repo already uses for the owner's
    /// collection (`docs/TESTING.md`), so inside `nix develop` there is a ROM
    /// directory before anyone asks for one. That is the point of the shared name.
    #[arg(long, env = "RUSTSEGA_ROM_DIR", value_name = "DIR")]
    pub rom_dir: Option<PathBuf>,

    /// Log level: trace, debug, info, warn, error, off
    #[arg(long, default_value = DEFAULT_LOG, env = "RUSTSEGA_LOG", value_name = "LEVEL")]
    pub log: LevelFilter,
}

impl Args {
    /// The socket `--bind` and `--port` chose, for binding and for the startup
    /// log line. A `SocketAddr` rather than a `String` so that an IPv6 address
    /// prints as `[::]:8080` and not as `:::8080`.
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
    }
}

/// Check `--rom-dir` against the filesystem, before anything is listening.
///
/// A typo in the one directory the library serves is otherwise invisible from
/// the browser: the list is simply empty. So it is an error here, and a
/// [`clap`] one, so that `main` can hand it to [`Error::exit`] and a script sees
/// status 2 - the status a bad flag leaves, which is how a supervisor tells a
/// wrong command line from a broken server.
///
/// `metadata` follows a symlink, so `--rom-dir` may well be a link to a mount.
pub fn check_rom_dir(rom_dir: Option<PathBuf>) -> Result<Option<PathBuf>, Error> {
    let Some(path) = rom_dir else {
        return Ok(None);
    };
    match path.metadata() {
        Ok(meta) if meta.is_dir() => Ok(Some(path)),
        Ok(_) => Err(usage(format!(
            "--rom-dir {} is a file; give a directory",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(usage(format!(
            "--rom-dir {} does not exist",
            path.display()
        ))),
        // Permission denied and the rest: the OS already says what is wrong with
        // the path better than this file can guess.
        Err(error) => Err(usage(format!("--rom-dir {}: {error}", path.display()))),
    }
}

/// A usage error: `clap`'s own format, on stderr, leaving status 2.
///
/// Through [`Command::error`](clap::Command::error) rather than [`Error::raw`],
/// so the line carries the usage and the `try '--help'` footer a person at a
/// terminal expects - and ends in a newline, which a raw message does not.
fn usage(message: String) -> Error {
    Args::command().error(ErrorKind::InvalidValue, message)
}
