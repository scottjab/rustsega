//! The CLI from the outside (M6-01, #120): `--help`, a missing ROM, a hash on
//! stdout, a PNG on disk, and the hash being the same twice.
//!
//! Everything runs the real binary through `env!("CARGO_BIN_EXE_headless")`, so
//! what is under test is the argument parsing, the exit status and the exact
//! text a person or a script reading it depends on. The emulator underneath is
//! covered by `crates/genesis/tests`; these tests only ever boot a fake ROM.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The headless binary under test, as cargo built it for this test target.
fn headless() -> Command {
    Command::new(env!("CARGO_BIN_EXE_headless"))
}

/// A scratch directory that cleans up after itself.
///
/// `testkit::tmp::TempDir` is test-only inside testkit and so unreachable from
/// here; the CLI tests need the same three lines of their own.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rustsega-headless-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn as_path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A cartridge image that loads and runs: the reset vectors for a program of
/// `bra.b` back onto itself at 0x1000, i.e. an infinite loop that costs nothing
/// to run, plus enough bytes for the header at 0x100 that
/// [`genesis::cart::parse_header`] requires (`docs/ARCHITECTURE.md`, "genesis").
///
/// The two words of the vector table and the `BRA.B $=0` encoding are from
/// M68000UM section 5 "Reset" and section 8 "BRA"; the shape is the same one
/// `crates/genesis/tests/boot.rs` uses.
fn write_fake_rom(path: &Path) {
    const STACK: u32 = 0x00FF_0000;
    const CODE: u32 = 0x1000;
    let mut rom = vec![0_u8; 0x1002];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    // 0x60FE = BRA.B with an 8-bit displacement of -2: this instruction again.
    rom[CODE as usize..CODE as usize + 2].copy_from_slice(&0x60FE_u16.to_be_bytes());
    // A readable title, so the summary line has something to show.
    rom[0x150..0x15A].copy_from_slice(b"FAKE ROM  ");
    fs::write(path, rom).expect("write fake rom");
}

/// Run `headless` with `args` and fail the caller if it could not even start.
fn run(args: &[&str]) -> Output {
    headless()
        .args(args)
        .output()
        .expect("run the headless binary")
}

/// stdout as text, for asserting on what the CLI reports.
fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// stderr as text: the summary line and any error message.
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `frame_hash=0x...` line from stdout, panicking with the whole output if
/// it is not there or is not a 16-digit hex value.
fn frame_hash_line(out: &Output) -> String {
    let text = stdout(out);
    let line = text
        .lines()
        .find(|line| line.starts_with("frame_hash=0x"))
        .unwrap_or_else(|| panic!("no frame_hash line in stdout: {text}"));
    let value = line.trim().trim_start_matches("frame_hash=0x");
    assert_eq!(
        value.len(),
        16,
        "hash must be 16 hex digits, got {line:?}\nall stdout: {text}"
    );
    assert!(
        value.chars().all(|c| c.is_ascii_hexdigit()),
        "hash must be hex, got {line:?}"
    );
    line.trim().to_string()
}

/// `--help` is how anyone finds out how to use this, so it must be a success
/// rather than "one ROM argument is required".
#[test]
fn help_works() {
    let out = run(&["--help"]);
    assert!(
        out.status.success(),
        "--help exited {:?}\nstderr: {}",
        out.status.code(),
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(
        text.contains("run"),
        "--help should describe the run subcommand: {text}"
    );
}

/// A ROM that is not there is the commonest mistake and has to say which path
/// it could not read, not just "No such file or directory".
#[test]
fn missing_rom_errors_nicely() {
    let dir = TempDir::new("missing-rom");
    let missing = dir.join("no-such-rom.md");
    let out = run(&["run", missing.to_str().expect("utf-8 path")]);
    assert!(
        !out.status.success(),
        "a missing ROM must not succeed\nstdout: {}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains(&missing.to_string_lossy().into_owned()),
        "the error must name the path: {}",
        stderr(&out)
    );
}

/// The headline output of the tool: run a ROM for N frames and get a frame hash
/// out, on stdout, one line, machine readable.
#[test]
fn run_fake_rom_prints_hash() {
    let dir = TempDir::new("hash");
    let rom = dir.join("fake.md");
    write_fake_rom(&rom);

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "2",
        "--hash",
    ]);
    assert!(
        out.status.success(),
        "run failed: {:?}\nstderr: {}",
        out.status.code(),
        stderr(&out)
    );
    let line = frame_hash_line(&out);
    // A hash of all-zero pixels would still be a number, so say which line we
    // got as well as that it is shaped right.
    assert_ne!(line, "frame_hash=0x0000000000000000");
}

/// `--dump-png` is how an agent looks at a frame, so the file has to exist and
/// really be a PNG.
#[test]
fn dump_png_creates_file() {
    let dir = TempDir::new("png");
    let rom = dir.join("fake.md");
    write_fake_rom(&rom);
    let png = dir.join("out.png");

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "1",
        "--dump-png",
        png.to_str().expect("utf-8 path"),
    ]);
    assert!(
        out.status.success(),
        "run failed: {:?}\nstderr: {}",
        out.status.code(),
        stderr(&out)
    );
    let bytes = fs::read(&png).unwrap_or_else(|e| panic!("{} should exist: {e}", png.display()));
    assert!(
        bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "{} is not a PNG ({} bytes)",
        png.display(),
        bytes.len()
    );
}

/// `--dump-png` at a directory is a mistake a person makes, and the answer is
/// one line of `error:`, not a panic from inside the PNG writer.
#[test]
fn dump_png_to_a_directory_errors_nicely() {
    let dir = TempDir::new("png-dir");
    let rom = dir.join("fake.md");
    write_fake_rom(&rom);

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "1",
        "--dump-png",
        dir.as_path().to_str().expect("utf-8 path"),
    ]);
    assert!(
        !out.status.success(),
        "dumping onto a directory must not succeed\nstdout: {}",
        stdout(&out)
    );
    let text = stderr(&out);
    assert!(
        text.contains("is a directory"),
        "the error must say what is wrong: {text}"
    );
    assert!(
        !text.contains("panicked"),
        "a clean error, not a panic: {text}"
    );
}

/// A WAV file header as the `wav` module writes it, parsed back for a test to
/// check. Only the fields `--dump-wav` promises are kept; a malformed or short
/// file is a panic with the bytes that caused it.
struct WavHeader {
    format: u16,
    channels: u16,
    rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits: u16,
    data_len: u32,
    file_len: usize,
}

/// Parse a RIFF/WAVE file down to [`WavHeader`], failing the test on any tag or
/// size that is not where the format says it is (Microsoft "New Wave Form").
fn wav_header(path: &Path) -> WavHeader {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("{} should exist: {e}", path.display()));
    assert!(
        bytes.len() >= 44,
        "{} is only {} bytes",
        path.display(),
        bytes.len()
    );
    let u16_at = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().expect("2 bytes"));
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    assert_eq!(
        &bytes[0..4],
        b"RIFF",
        "{} is not a RIFF file",
        path.display()
    );
    assert_eq!(
        &bytes[8..12],
        b"WAVE",
        "{} is not a WAVE file",
        path.display()
    );
    assert_eq!(&bytes[12..16], b"fmt ", "no fmt chunk");
    assert_eq!(u32_at(16), 16, "fmt chunk must be the 16-byte PCM form");
    assert_eq!(&bytes[36..40], b"data", "no data chunk after fmt");
    WavHeader {
        format: u16_at(20),
        channels: u16_at(22),
        rate: u32_at(24),
        byte_rate: u32_at(28),
        block_align: u16_at(32),
        bits: u16_at(34),
        data_len: u32_at(40),
        file_len: bytes.len(),
    }
}

/// `--dump-wav` is how anyone listens to what the emulator made, so the file has
/// to exist and really be a valid 16-bit stereo PCM WAV at the console's rate
/// (M6-02, #121).
#[test]
fn cli_dump_wav_creates_file() {
    let dir = TempDir::new("wav");
    let rom = dir.join("fake.md");
    write_fake_rom(&rom);
    let wav = dir.join("out.wav");

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "2",
        "--dump-wav",
        wav.to_str().expect("utf-8 path"),
    ]);
    assert!(
        out.status.success(),
        "run failed: {:?}\nstderr: {}",
        out.status.code(),
        stderr(&out)
    );
    let header = wav_header(&wav);
    assert_eq!(header.format, 1, "PCM");
    assert_eq!(header.channels, 2, "stereo");
    assert_eq!(header.rate, 48_000, "the console's default sample rate");
    assert_eq!(header.block_align, 4);
    assert_eq!(header.bits, 16);
    assert_eq!(header.byte_rate, 48_000 * 4);
    // Two frames of an NTSC raster is about 801 stereo samples' worth of audio
    // apiece (docs/ARCHITECTURE.md via `Genesis::drain_audio`), so the data
    // chunk is well past the header and its length matches the file on disk.
    assert!(
        header.data_len as usize + 44 == header.file_len,
        "data chunk ({}) and file ({}) must agree",
        header.data_len,
        header.file_len
    );
    assert!(header.data_len > 0, "two frames must produce audio samples");
}

/// `--audio-hash` is the machine-readable half of `--dump-wav`: one line on
/// stdout, shaped like `frame_hash=0x...` so a script can read either.
#[test]
fn audio_hash_flag() {
    let dir = TempDir::new("audio-hash");
    let rom = dir.join("fake.md");
    write_fake_rom(&rom);
    let args = [
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "2",
        "--audio-hash",
    ];

    let out = run(&args);
    assert!(
        out.status.success(),
        "run failed: {:?}\nstderr: {}",
        out.status.code(),
        stderr(&out)
    );
    let text = stdout(&out);
    let line = text
        .lines()
        .find(|line| line.starts_with("audio_hash=0x"))
        .unwrap_or_else(|| panic!("no audio_hash line in stdout: {text}"));
    let value = line.trim().trim_start_matches("audio_hash=0x");
    assert_eq!(value.len(), 16, "hash must be 16 hex digits, got {line:?}");
    assert!(
        value.chars().all(|c| c.is_ascii_hexdigit()),
        "hash must be hex, got {line:?}"
    );

    // The same machine run twice is the same audio: the property every golden
    // value in the repo rests on (`docs/TESTING.md`, "Golden files").
    let again = run(&args);
    assert!(again.status.success());
    let other = stdout(&again);
    let other_line = other
        .lines()
        .find(|line| line.starts_with("audio_hash=0x"))
        .unwrap_or_else(|| panic!("no audio_hash line in stdout: {other}"));
    assert_eq!(line, other_line.trim(), "the same run must hash the same");
}

/// Two runs of the same ROM for the same number of frames are the same machine
/// state, so they must print the same hash: the property every golden frame
/// hash in the repo rests on (`docs/TESTING.md`, "Golden files").
#[test]
fn hash_is_deterministic() {
    let dir = TempDir::new("deterministic");
    let rom = dir.join("fake.md");
    write_fake_rom(&rom);
    let args = [
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "3",
        "--hash",
    ];

    let first = run(&args);
    let second = run(&args);
    assert!(first.status.success() && second.status.success());
    assert_eq!(
        frame_hash_line(&first),
        frame_hash_line(&second),
        "the same ROM run twice must hash the same"
    );
}
