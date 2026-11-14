//! `rustsega` started as a process, in an environment this file chose (#156).
//!
//! `crates/server/tests/cli.rs` drives the parser and `crates/server/tests/api.rs`
//! drives the router; neither reaches what `main` does with them. This is the only
//! place that checks the four things an in-process test cannot: that `clap`'s
//! `env` feature is switched on, so the `RUSTSEGA_*` variables are actually read;
//! that an unusable `--rom-dir` leaves status 2 rather than a server with an empty
//! library; that `/api/info` reports the library the command line chose; and that
//! the `--rom-dir` it accepted is the one `GET /api/roms` lists (#157).
//!
//! Every child is started with `env_clear`. `nix develop` exports
//! `RUSTSEGA_ROM_DIR` for `testkit`'s commercial-ROM lookup, so an inherited
//! environment would decide `library` here rather than the code under test, and
//! `std::env::set_var` is `unsafe` in edition 2024. A child of a Nix-built binary
//! needs nothing else: no `PATH`, no `HOME`, no locale.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// How long a test waits for anything: the binary is built before nextest runs,
/// so anything past a few seconds is a hang rather than a slow start.
const LIMIT: Duration = Duration::from_secs(10);

/// The port the server would take with nothing configured, which is what says a
/// `RUSTSEGA_PORT` was read: see [`Server::start`].
const DEFAULT_PORT: u16 = 8080;

/// The directory of this crate: a real directory, to be the `--rom-dir` of a test.
fn a_directory() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A path under this crate that cannot exist, for the tests that ask for one.
fn missing_directory() -> std::path::PathBuf {
    a_directory().join("no-such-rom-dir")
}

/// A `rustsega` process, and the address it said it was listening on.
struct Server {
    child: Child,
    addr: SocketAddr,
}

impl Server {
    /// Start `rustsega` with `args`, in an environment holding only `env` plus
    /// `RUSTSEGA_BIND=127.0.0.1` and `RUSTSEGA_PORT=0`.
    ///
    /// Bind and port are passed as variables rather than as flags because reading
    /// them is the half of #156 an in-process parse cannot reach, and port 0 lets
    /// two of these run at once on a busy machine. The last assertion is what
    /// proves it happened: a `clap` that ignored the environment would be
    /// listening on 8080, and the request below would then be answered by
    /// whatever else is on 8080 rather than by this process.
    fn start(env: &[(&str, &str)], args: &[&str]) -> Server {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rustsega"));
        command
            .env_clear()
            .env("RUSTSEGA_BIND", "127.0.0.1")
            .env("RUSTSEGA_PORT", "0")
            .args(args)
            .stdin(Stdio::null())
            // `tracing_subscriber::fmt` writes the log to stdout.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in env {
            command.env(name, *value);
        }

        let mut child = command
            .spawn()
            .expect("`just check` builds the rustsega binary before running tests");
        let stdout = child.stdout.take().expect("stdout was piped");
        // `std::process::Child` has no `kill_on_drop`, and until this function
        // returns there is no `Server` whose `Drop` could do it instead.
        let addr = wait_for_listen(stdout).unwrap_or_else(|reason| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{reason}");
        });
        let server = Server { child, addr };
        assert_ne!(
            server.addr.port(),
            DEFAULT_PORT,
            "listening on the default port means RUSTSEGA_PORT was not read"
        );
        server
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read the log until it names the listening address.
///
/// On the pipe rather than by polling a socket, so there is no window in which the
/// process is up but the test has stopped looking, and none in which a connect
/// succeeds against something else that owns the port.
fn wait_for_listen(stdout: ChildStdout) -> Result<SocketAddr, String> {
    let (sender, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + LIMIT;
    let mut logged = Vec::new();
    loop {
        let Ok(line) = lines.recv_timeout(remaining(deadline)) else {
            return Err(format!("rustsega never listened; it logged {logged:?}"));
        };
        logged.push(line.clone());
        if let Some((_, addr)) = line.split_once("listening on http://") {
            return addr
                .trim()
                .parse()
                .map_err(|error| format!("bad address in the startup line {line:?}: {error}"));
        }
    }
}

/// Run `rustsega` with `args` in an otherwise empty environment to completion.
///
/// For the runs that must not reach the socket at all: the exit status and
/// whatever it complained with is the whole result.
fn run_to_exit(args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustsega"));
    command.env_clear().args(args);
    for (name, value) in env {
        command.env(name, *value);
    }
    command
        .stdin(Stdio::null())
        .output()
        .expect("the rustsega binary runs")
}

/// One `GET`, and the whole response, without an HTTP client dependency:
/// `Connection: close` is what ends the answer.
fn get(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("the server is listening");
    stream
        .set_read_timeout(Some(remaining(Instant::now() + LIMIT)))
        .expect("a read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .expect("the request was written");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("the response was read to the end");
    response
}

/// The status code, and the body, of a raw HTTP/1.1 response.
fn split(response: &str) -> (u16, &str) {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .expect("a header block, then a body");
    let code = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status code in {response:?}"));
    (code, body)
}

/// `now()` to `deadline`, for the API that wants a duration rather than a point.
fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

#[test]
fn a_server_on_a_clean_environment_serves_both_endpoints_and_has_no_library() {
    let server = Server::start(&[], &[]);

    let response = get(server.addr, "/healthz");
    let (code, body) = split(&response);
    assert_eq!(code, 200, "{response}");
    assert_eq!(body, "ok");

    let response = get(server.addr, "/api/info");
    let (code, body) = split(&response);
    assert_eq!(code, 200, "{response}");
    assert_eq!(
        body,
        format!(
            "{{\"version\":\"{}\",\"library\":false}}",
            env!("CARGO_PKG_VERSION")
        ),
        "with no ROM dir in the environment there is no library to report"
    );
}

#[test]
fn a_server_whose_environment_names_a_rom_dir_reports_a_library() {
    let server = Server::start(
        &[(
            "RUSTSEGA_ROM_DIR",
            a_directory().to_str().expect("a UTF-8 checkout path"),
        )],
        &[],
    );

    let response = get(server.addr, "/api/info");
    let (code, body) = split(&response);

    assert_eq!(code, 200, "{response}");
    assert!(
        body.contains("\"library\":true"),
        "the directory exists, so the library is there: {body}"
    );
}

#[test]
fn a_rom_dir_flag_replaces_the_one_in_the_environment() {
    // The environment asks for a directory that is not there: were the flag not
    // taking precedence, the process would have exited with 2 by now.
    let server = Server::start(
        &[(
            "RUSTSEGA_ROM_DIR",
            missing_directory().to_str().expect("a UTF-8 path"),
        )],
        &["--rom-dir", a_directory().to_str().expect("a UTF-8 path")],
    );

    let response = get(server.addr, "/api/info");
    let (_code, body) = split(&response);
    assert!(body.contains("\"library\":true"), "{body}");
}

#[test]
fn a_rom_dir_that_is_not_there_leaves_status_2_before_the_port_is_bound() {
    let output = run_to_exit(&["--rom-dir", missing_directory().to_str().unwrap()], &[]);

    assert!(
        !output.status.success(),
        "a bad --rom-dir must not look like a running server"
    );
    assert_eq!(output.status.code(), Some(2), "a wrong flag, not a crash");
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("--rom-dir") && message.contains("does not exist"),
        "{message}"
    );
}

#[test]
fn a_bind_the_environment_names_that_is_not_an_address_is_refused_too() {
    // The other half of "the variables are read": clap rejected a value that no
    // flag of this test ever mentioned.
    let output = run_to_exit(&[], &[("RUSTSEGA_BIND", "localhost")]);

    assert_eq!(output.status.code(), Some(2));
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("--bind"), "{message}");
}

#[test]
fn a_started_server_lists_and_serves_the_rom_dir_it_was_given() {
    // `crates/server/tests/library.rs` shows the scan and the router agreeing; this
    // shows that `main` ran the scan at all, which is the step in between that no
    // in-process test can reach (#157).
    let roms = tempdir().expect("a temp dir to point --rom-dir at");
    let name = "Sonic The Hedgehog (USA, Europe).md";
    fs::write(roms.path().join(name), b"rom bytes").expect("a fake cartridge");
    let server = Server::start(
        &[],
        &[
            "--rom-dir",
            roms.path().to_str().expect("a UTF-8 temp path"),
        ],
    );

    let response = get(server.addr, "/api/roms");
    let (code, body) = split(&response);
    assert_eq!(code, 200, "{response}");
    let listing: Vec<serde_json::Value> =
        serde_json::from_str(body).unwrap_or_else(|error| panic!("not JSON: {body}: {error}"));
    assert_eq!(listing.len(), 1, "{body}");
    assert_eq!(listing[0]["file"], name);
    let id = listing[0]["id"]
        .as_str()
        .expect("an entry has an id")
        .to_owned();
    assert_eq!(
        id,
        sha1_smol::Sha1::from(name).digest().to_string(),
        "what the browser is given is the hash of the path, never the path"
    );

    let response = get(server.addr, &format!("/api/roms/{id}"));
    let (code, body) = split(&response);
    assert_eq!(code, 200, "{response}");
    assert_eq!(body, "rom bytes");
}

#[test]
fn the_binary_answers_help_and_version() {
    let help = run_to_exit(&["--help"], &[]);
    assert!(help.status.success(), "help: {help:?}");

    let version = run_to_exit(&["--version"], &[]);
    assert!(version.status.success(), "version: {version:?}");
    assert!(
        String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")),
        "--version should say which version this is"
    );
}
