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
/// A cartridge image whose backdrop colour is the byte it reads from port 1, so a
/// frame hash says what the pad was doing and a `--movie` that was parsed but
/// never played cannot pass one of the tests below.
///
/// ```text
///        move.w #$8140, ($C00004).L   R1: display on
///        move.w #$8705, ($C00004).L   R7: backdrop colour 5
///        move.w #$C00A, ($C00004).L   a CRAM write code for word 5
///        move.w #$0000, ($C00004).L   ...and its second half
/// loop:  move.b ($A10003).L, d0       port 1's pin levels, TH high
///        move.w d0, ($C00000).L       straight into CRAM word 5
///        bra    loop
/// ```
///
/// The `MOVE` encodings are `33FC imm16 addr32`, `33C0 addr32` and `1039 addr32`
/// (M68000UM section 8 "MOVE", destination field first), the `BRA.B` displacement
/// is taken from the address after the branch word, and the VDP register command
/// shape is the one `crates/genesis/tests/save_state.rs` uses.
fn pad_reader_rom() -> Vec<u8> {
    const STACK: u32 = 0x00FF_0000;
    const CODE: u32 = 0x1000;
    let program: Vec<u16> = [
        0x33FC_u16, 0x8140, 0x00C0, 0x0004, 0x33FC, 0x8705, 0x00C0, 0x0004, 0x33FC, 0xC00A, 0x00C0,
        0x0004, 0x33FC, 0x0000, 0x00C0, 0x0004, 0x1039, 0x00A1,
        0x0003, // loop: move.b ($A10003).L, d0
        0x33C0, 0x00C0, 0x0000, //         move.w d0, ($C00000).L
        0x60F2, //         bra.b -14, back to the move
    ]
    .into_iter()
    .collect();

    let mut rom = vec![0_u8; (CODE as usize + 2 * program.len()).max(0x200)];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (word, at) in program.iter().zip(0..) {
        let at = CODE as usize + 2 * at;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom[0x150..0x158].copy_from_slice(b"PAD ROM ");
    rom
}

/// Write `bytes` as a ROM at `path` and give them back, so the caller can also
/// name the ROM's movie hash.
fn write_rom(path: &Path, bytes: &[u8]) -> Vec<u8> {
    fs::write(path, bytes).expect("write rom");
    bytes.to_vec()
}

/// The text of a one-pad movie for `rom_hash`, with `frames` as its change list.
fn movie_text(rom_hash: u64, region: &str, frames: &[(u32, &str)]) -> String {
    let mut text =
        format!("rustsega-movie 1\nrom-hash 0x{rom_hash:016x}\nregion {region}\npads 1\n");
    for (frame, buttons) in frames {
        text.push_str(&format!("{frame} {buttons}\n"));
    }
    text
}

/// The lines of a movie file that mean something: what [`movie_text`] and what a
/// `--record` file agree on, without the comment line `Movie::format` writes.
fn movie_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .collect()
}

/// The point of M6-09 from the command line: `--record` turns a `--pad1` script
/// into a movie file, `--movie` plays that file back, and the run it plays is the
/// run that was recorded - frame for frame, not just without crashing.
#[test]
fn record_then_movie_hashes_the_same_run() {
    let dir = TempDir::new("movie-record");
    let rom = dir.join("pad.md");
    let bytes = write_rom(&rom, &pad_reader_rom());
    let rom_hash = genesis::movie::rom_hash(&bytes);
    let movie = dir.join("run.rsm");
    let rom = rom.to_str().expect("utf-8 path");
    let movie = movie.to_str().expect("utf-8 path");

    // Record: UP on the third frame, DOWN on the last, nothing on the rest. The
    // last frame has to be one of them because `--hash` reports the last frame,
    // and that is the frame whose colour this ROM took from the pad.
    let script = "UP@3,DOWN@6";
    let recorded = run(&[
        "run", rom, "--frames", "6", "--pad1", script, "--record", movie,
    ]);
    assert!(
        recorded.status.success(),
        "record failed: {:?}\nstderr: {}",
        recorded.status.code(),
        stderr(&recorded)
    );

    // `--pad1` counts frames from 1 and a movie counts them from 0, and the press
    // is momentary, so the file is a press on frame 2 and a release on frame 3.
    let text = fs::read_to_string(movie).expect("the movie was written");
    assert_eq!(
        movie_lines(&text),
        [
            "rustsega-movie 1",
            &format!("rom-hash 0x{rom_hash:016x}"),
            "region usa",
            "pads 1",
            "2 U",
            "3 .",
            "5 D"
        ],
        "the recorded movie is the script, 0-based: {text}"
    );

    // Replay it and hash: the same run, from the file rather than the script.
    let replayed = run(&["run", rom, "--frames", "6", "--movie", movie, "--hash"]);
    assert!(
        replayed.status.success(),
        "replay failed: {:?}\nstderr: {}",
        replayed.status.code(),
        stderr(&replayed)
    );
    let scripted = run(&["run", rom, "--frames", "6", "--pad1", script, "--hash"]);
    assert!(scripted.status.success());
    assert_eq!(
        frame_hash_line(&replayed),
        frame_hash_line(&scripted),
        "replaying the recording must reproduce the run it came from"
    );

    // And the hash really is the pad's, not a constant: the same ROM run with
    // nobody holding anything is a different picture.
    let idle = run(&["run", rom, "--frames", "6", "--hash"]);
    assert!(idle.status.success());
    assert_ne!(
        frame_hash_line(&replayed),
        frame_hash_line(&idle),
        "a run with a frame of UP held cannot hash the same as one with nothing held"
    );
}

/// `headless run --movie in.rsm --hash` is the ticket's own CLI case: a movie
/// written by hand, held for several frames, plays and hashes.
#[test]
fn hand_written_movie_runs_and_hashes() {
    let dir = TempDir::new("movie-run");
    let rom = dir.join("pad.md");
    let bytes = write_rom(&rom, &pad_reader_rom());
    let rom_hash = genesis::movie::rom_hash(&bytes);
    let movie = dir.join("in.rsm");
    fs::write(&movie, movie_text(rom_hash, "usa", &[(0, "."), (1, "U")])).expect("write movie");

    let args = [
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "4",
        "--movie",
        movie.to_str().expect("utf-8 path"),
        "--hash",
    ];
    let out = run(&args);
    assert!(
        out.status.success(),
        "run failed: {:?}\nstderr: {}",
        out.status.code(),
        stderr(&out)
    );
    let line = frame_hash_line(&out);
    // The input holds until the next listed frame, so this is UP held from frame
    // 1 to the end of the run - and twice round the same file it is the same hash.
    let again = run(&args);
    assert!(again.status.success());
    assert_eq!(line, frame_hash_line(&again), "the same movie twice");
}

/// A movie for another cartridge is refused rather than replayed into a game it
/// was not written for, the way a save state is (`docs/save-state-format.md`).
#[test]
fn movie_for_another_rom_is_refused() {
    let dir = TempDir::new("movie-wrong-rom");
    let rom = dir.join("pad.md");
    write_rom(&rom, &pad_reader_rom());
    let movie = dir.join("other.rsm");
    fs::write(
        &movie,
        movie_text(0x0123_4567_89ab_cdef, "usa", &[(0, ".")]),
    )
    .expect("write movie");

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "2",
        "--movie",
        movie.to_str().expect("utf-8 path"),
    ]);
    assert!(!out.status.success(), "a foreign movie must not run");
    assert!(
        stderr(&out).contains("different cartridge"),
        "the error must say what is wrong: {}",
        stderr(&out)
    );
}

/// A movie written for a different region is a different run of the machine, and
/// `--movie` says so instead of replaying it on the wrong frame rate.
#[test]
fn movie_for_another_region_is_refused() {
    let dir = TempDir::new("movie-wrong-region");
    let rom = dir.join("pad.md");
    let bytes = write_rom(&rom, &pad_reader_rom());
    let movie = dir.join("pal.rsm");
    fs::write(
        &movie,
        movie_text(genesis::movie::rom_hash(&bytes), "europe", &[(0, ".")]),
    )
    .expect("write movie");

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--frames",
        "2",
        "--movie",
        movie.to_str().expect("utf-8 path"),
    ]);
    assert!(
        !out.status.success(),
        "a PAL movie on an NTSC run must fail"
    );
    let text = stderr(&out);
    assert!(
        text.contains("region") && text.contains("europe"),
        "the error must name the region problem: {text}"
    );
}

/// A movie file that is not there names the path, and a file that is not a movie
/// says what the parser rejected - the two mistakes a person makes with a
/// hand-edited file.
#[test]
fn movie_file_mistakes_are_reported() {
    let dir = TempDir::new("movie-mistakes");
    let rom = dir.join("pad.md");
    write_rom(&rom, &pad_reader_rom());
    let missing = dir.join("nope.rsm");
    let not_a_movie = dir.join("junk.rsm");
    fs::write(&not_a_movie, "rustsega-movie 1\nregion usa\npads 1\n").expect("write junk");

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--movie",
        missing.to_str().expect("utf-8 path"),
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains(&missing.to_string_lossy().into_owned()),
        "the error must name the path: {}",
        stderr(&out)
    );

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--movie",
        not_a_movie.to_str().expect("utf-8 path"),
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("rom-hash"),
        "the parse error must be the CLI's answer: {}",
        stderr(&out)
    );
}

/// `--movie` is the input and `--pad1` is the input: holding both would mean one
/// of them is being ignored, so the CLI says which one it refused to guess.
#[test]
fn movie_and_pad1_do_not_mix() {
    let dir = TempDir::new("movie-both");
    let rom = dir.join("pad.md");
    let bytes = write_rom(&rom, &pad_reader_rom());
    let movie = dir.join("in.rsm");
    fs::write(
        &movie,
        movie_text(genesis::movie::rom_hash(&bytes), "usa", &[(0, ".")]),
    )
    .expect("write movie");

    let out = run(&[
        "run",
        rom.to_str().expect("utf-8 path"),
        "--movie",
        movie.to_str().expect("utf-8 path"),
        "--pad1",
        "A@2",
    ]);
    assert!(!out.status.success(), "both inputs at once is a mistake");
    assert!(
        stderr(&out).contains("--pad1"),
        "the error must say what cannot be combined: {}",
        stderr(&out)
    );
}
