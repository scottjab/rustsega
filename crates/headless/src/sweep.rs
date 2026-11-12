//! The `sweep` subcommand: run every ROM in a directory for a while and write a
//! compatibility report (M6-05, #124).
//!
//! # Why a child process per ROM
//!
//! A real collection contains a ROM that panics and a ROM that never comes back.
//! Neither may take the sweep with it, so nothing here emulates anything: each
//! cartridge is run by a fresh `headless run` child (the same binary, via
//! [`std::env::current_exe`], so a sweep always measures the build under test),
//! and the parent only watches an exit status, a wall clock and two output files.
//! A panic dies in the child, a hang is killed at `--timeout-secs`, and both
//! become one row of the report rather than the end of the run.
//!
//! # What a row means
//!
//! [`Status`] is deliberately coarse - the sweep names problems for a human to
//! triage (M6-13), it does not diagnose them. `Boots` means the machine ran the
//! whole run and the last frame had more than [`BLANK_COLOUR_LIMIT`] distinct
//! colours; it says nothing about whether the picture is *right*, which is the
//! same claim [`docs/compat.md`](../../../../docs/compat.md) makes about the
//! commercial smoke suite.
//!
//! Nothing here reads like a ROM: no cartridge byte is ever printed, and the only
//! ROM content a row carries is the title, serial and region of its header.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use genesis::Config;

use crate::run;

/// Frames for `--frames` when the caller does not say: ten seconds of NTSC, the
/// same checkpoint the commercial smoke suite runs to (`crates/genesis/tests/commercial.rs`).
const DEFAULT_FRAMES: u32 = 600;

/// Seconds for `--timeout-secs` when the caller does not say.
///
/// Generous against the ~0.7 s a real game takes to reach 600 frames: this is a
/// hangs-only limit, so it has to be far enough above a healthy run that a slow
/// host does not turn working games into `Timeout` rows.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// How often a waiting parent asks its child whether it is done.
///
/// A poll, not a spawn-per-millisecond: at 20 ms the wait costs nothing per ROM
/// and a hung child is still killed within a frame or two of its limit.
const POLL: Duration = Duration::from_millis(20);

/// A frame with this many distinct colours or fewer is a blank screen.
///
/// The same limit and the same reasoning as `BLANK_COLOUR_LIMIT` in
/// `crates/genesis/tests/commercial.rs`: a game that has reached a title card
/// shows tens of colours, while the chip's reset state and a border sit at one.
pub const BLANK_COLOUR_LIMIT: usize = 4;

/// How one ROM of the sweep went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The image is not a Mega Drive cartridge - see [`skip_reason`].
    Skipped,
    /// The image has no header to load, so the machine was never started.
    LoadError,
    /// The child process panicked.
    Panic,
    /// The child was still running when `--timeout-secs` ran out and was killed.
    Timeout,
    /// The run finished but the last frame is blank.
    Blank,
    /// The run finished and the last frame is a picture.
    Boots,
}

impl Status {
    /// The word this row is printed as, in the CSV and in the markdown.
    pub fn name(self) -> &'static str {
        match self {
            Self::Skipped => "Skipped",
            Self::LoadError => "LoadError",
            Self::Panic => "Panic",
            Self::Timeout => "Timeout",
            Self::Blank => "Blank",
            Self::Boots => "Boots",
        }
    }

    /// Is this a problem worth a section of the markdown report?
    ///
    /// `Boots` is not news, and `Skipped` is not a fault: it is hardware this
    /// emulator does not have, listed by count in the summary and nowhere else.
    fn is_problem(self) -> bool {
        !matches!(self, Self::Boots | Self::Skipped)
    }
}

/// Every status, in the order the report prints them: best to worst, with the
/// out-of-scope images last.
const STATUS_ORDER: [Status; 6] = [
    Status::Boots,
    Status::Blank,
    Status::Panic,
    Status::Timeout,
    Status::LoadError,
    Status::Skipped,
];

/// One ROM's row of the report.
#[derive(Debug, Clone)]
pub struct RomResult {
    /// File name, relative to the swept directory.
    pub file: String,
    /// Cartridge title, [`run::title_of`]'s answer.
    pub title: String,
    /// Cartridge serial at 0x180, e.g. `GM 00001009-00`.
    pub serial: String,
    /// Region the machine would run it as: `usa`, `japan` or `europe`.
    pub region: String,
    /// How it went.
    pub status: Status,
    /// Distinct colours of the last frame; zero for a run that never got there.
    pub distinct_colours: usize,
    /// `frame_hash` of the last frame, when there was one.
    pub frame_hash: Option<u64>,
    /// Wall-clock seconds this ROM took, child start-up included.
    pub seconds: f64,
    /// What the report says about it: a panic message, a skip reason, a count.
    pub notes: String,
}

impl RomResult {
    /// A row with no run behind it: metadata plus a verdict.
    fn new(file: String, status: Status, seconds: f64, notes: String) -> Self {
        Self {
            file,
            title: String::new(),
            serial: String::new(),
            region: String::new(),
            status,
            distinct_colours: 0,
            frame_hash: None,
            seconds,
            notes,
        }
    }
}

/// The `sweep` arguments, as clap sees them.
#[derive(Debug, Clone, Args)]
pub struct SweepArgs {
    /// Directory of ROM images to run.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
    /// Frames to run each ROM for.
    #[arg(long, default_value_t = DEFAULT_FRAMES, value_name = "N")]
    pub frames: u32,
    /// ROMs to run at once.
    #[arg(long, value_name = "N")]
    pub jobs: Option<usize>,
    /// Wall-clock seconds a ROM may take before it is killed and called a hang.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS, value_name = "SECS")]
    pub timeout_secs: u64,
    /// Write the full per-ROM table here as CSV.
    #[arg(long, value_name = "PATH")]
    pub csv: Option<PathBuf>,
    /// Write the problem-only report here as markdown.
    #[arg(long, value_name = "PATH")]
    pub markdown: Option<PathBuf>,
    /// Sweep only the first N ROMs: a dry run over a real directory.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

/// Sweep `args.dir` and write the report.
pub fn run(args: SweepArgs) -> Result<()> {
    let exe = std::env::current_exe().context("cannot find this executable to re-run")?;
    let rows = sweep_with(&exe, &args)?;

    if let Some(path) = &args.csv {
        write_csv(path, &rows).with_context(|| format!("cannot write CSV {:?}", path))?;
    }
    if let Some(path) = &args.markdown {
        let text = markdown(&rows, args.frames, args.timeout_secs);
        fs::write(path, text).with_context(|| format!("cannot write markdown {:?}", path))?;
    }

    let mut line = format!("sweep: {} ROMs", rows.len());
    for status in STATUS_ORDER {
        let count = rows.iter().filter(|row| row.status == status).count();
        if count > 0 {
            line.push_str(&format!(" {}={count}", status.name()));
        }
    }
    eprintln!("{line}");
    Ok(())
}

/// Every ROM under `dir`, sorted, `.md`/`.bin`/`.smd` only (case-insensitive).
///
/// The local collection is flat, so a directory is not a candidate; anything
/// without one of the three extensions is not a cartridge and is not mentioned.
fn collect_roms(dir: &Path) -> Result<Vec<PathBuf>> {
    const EXTENSIONS: [&str; 3] = ["md", "bin", "smd"];
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).with_context(|| format!("cannot read directory {:?}", dir))?;
    for entry in entries {
        let path = entry
            .with_context(|| format!("cannot read directory {:?}", dir))?
            .path();
        if !path.is_file() {
            continue;
        }
        let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        let extension = extension.to_ascii_lowercase();
        if EXTENSIONS.contains(&extension.as_str()) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Why this image must not be run, or `None` when it should be.
///
/// The collection is a No-Intro set of Mega Drive dumps plus the odd thing that
/// is not one: a Sega CD BIOS or a 32X cartridge cannot be run by this emulator
/// and would be reported as a fault of ours if the sweep tried. Detection is the
/// 0x100 system type - the same field TMSS reads - and both spellings of the
/// licensed prefix, since a handful of local dumps carry a leading space
/// (`cart::smd`) and [`genesis::cart::parse_header`] keeps what it is given.
///
/// It fails open: an unrecognised system type is run, not skipped, so a real
/// cartridge is never quietly left out of the report.
pub fn skip_reason(system_type: &str, size: u64) -> Option<&'static str> {
    if size == 0 {
        return Some("empty file");
    }
    // One word per system type for the comparisons below: `SEGA 32X`, `SEGA32X`
    // and `SEGA 32X ` are the same piece of hardware.
    let flat = system_type
        .trim()
        .to_ascii_uppercase()
        .replace([' ', '-'], "");
    if flat.starts_with("SEGA32X") {
        return Some("Sega 32X cartridge");
    }
    if flat.starts_with("SEGACD") || flat.contains("CDROM") {
        return Some("Sega CD image");
    }
    None
}

/// Run `args` with `exe` as the binary each ROM is run by.
///
/// The executable is a parameter rather than a call to `current_exe` so an
/// integration test can sweep a temporary directory of fake ROMs with the real
/// `headless` binary: inside a unit test `current_exe` is the test harness, which
/// does not speak the `run` subcommand at all.
pub fn sweep_with(exe: &Path, args: &SweepArgs) -> Result<Vec<RomResult>> {
    let mut candidates = collect_roms(&args.dir)?;
    if let Some(limit) = args.limit {
        candidates.truncate(limit);
    }
    let jobs = args.jobs.unwrap_or_else(default_jobs).max(1);
    let timeout = Duration::from_secs(args.timeout_secs.max(1));
    let total = candidates.len();
    let scratch = scratch_dir()?;

    let started = Instant::now();
    let candidates = Arc::new(candidates);
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = std::sync::mpsc::channel::<RomResult>();
    let mut workers = Vec::new();
    for _ in 0..jobs {
        let exe = exe.to_path_buf();
        let candidates = Arc::clone(&candidates);
        let next = Arc::clone(&next);
        let done = Arc::clone(&done);
        let sender = sender.clone();
        let scratch = scratch.clone();
        let frames = args.frames;
        workers.push(thread::spawn(move || {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= candidates.len() {
                    break;
                }
                let path = &candidates[index];
                let result = one(&exe, path, index, frames, timeout, &scratch);
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                if finished.is_multiple_of(100) || finished == total {
                    eprintln!(
                        "sweep: {finished}/{total} roms, {:.0}s",
                        started.elapsed().as_secs_f64()
                    );
                }
                let _ = sender.send(result);
            }
        }));
    }
    drop(sender);

    let mut rows: Vec<RomResult> = receiver.into_iter().collect();
    for worker in workers {
        let _ = worker.join();
    }
    let _ = fs::remove_dir_all(&scratch);
    rows.sort_by(|left, right| left.file.cmp(&right.file));
    Ok(rows)
}

/// `--jobs` when the caller does not say: one thread per CPU.
fn default_jobs() -> usize {
    thread::available_parallelism().map_or(4, |n| n.get())
}

/// A directory for the children's stdout and stderr, removed with the sweep.
///
/// Files rather than pipes because a pipe is a deadlock waiting for a chatty
/// child: the parent must keep reading while the child keeps writing, and the
/// parent is instead waiting for the child to finish.
fn scratch_dir() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("rustsega-sweep-{}", std::process::id()));
    fs::create_dir_all(&path)
        .with_context(|| format!("cannot create scratch directory {:?}", path))?;
    Ok(path)
}

/// One ROM: read its header, run it in a child, and say how it went.
///
/// Never fails. Every way this can go wrong is a row of the report, because one
/// unreadable cartridge out of 2775 is not a reason to stop the sweep.
fn one(
    exe: &Path,
    path: &Path,
    index: usize,
    frames: u32,
    timeout: Duration,
    scratch: &Path,
) -> RomResult {
    let started = Instant::now();
    let file = file_name(path);
    let seconds = || started.elapsed().as_secs_f64();

    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => return RomResult::new(file, Status::LoadError, seconds(), err.to_string()),
    };
    let size = bytes.len() as u64;
    // The header of an interleaved `.smd` is only readable once the image is
    // back in address order, which is `normalize_rom`'s job and the first thing
    // `Genesis::new` does; the child does it again for itself.
    let image = genesis::cart::normalize_rom(&bytes);
    drop(bytes);
    let header = match genesis::cart::parse_header(&image) {
        Ok(header) => header,
        Err(err) => return RomResult::new(file, Status::LoadError, seconds(), err.to_string()),
    };
    let title = run::title_of(&header).unwrap_or_else(|| "unknown".to_string());
    let serial = header.serial.clone();
    let region = run::region_name(Config::default().resolve_region(&header)).to_string();
    if let Some(reason) = skip_reason(&header.system_type, size) {
        let mut row = RomResult::new(file, Status::Skipped, seconds(), reason.to_string());
        row.title = title;
        row.serial = serial;
        row.region = region;
        return row;
    }

    let (timed_out, code, stdout, stderr) = run_child(exe, path, frames, timeout, scratch, index);
    let (frame_hash, colours) = parse_report(&stdout);
    let attempt = Attempt {
        timed_out,
        code,
        colours,
        stderr: &stderr,
    };
    let (status, notes) = classify(&attempt);
    RomResult {
        file,
        title,
        serial,
        region,
        status,
        distinct_colours: colours.unwrap_or(0),
        frame_hash,
        seconds: seconds(),
        notes,
    }
}

/// The file name to report: inside the swept directory, never a path that leaks
/// the owner's layout into a committed report.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Run `rom` in a child process, killing it at `timeout`.
///
/// Returns `(timed_out, exit code, stdout, stderr)`. The exit code is `None` when
/// the child could not be started or was killed by a signal, which is what
/// distinguishes "it panicked" from "we stopped it" in [`classify`].
fn run_child(
    exe: &Path,
    rom: &Path,
    frames: u32,
    timeout: Duration,
    scratch: &Path,
    index: usize,
) -> (bool, Option<i32>, String, String) {
    let out_path = scratch.join(format!("{index}.out"));
    let err_path = scratch.join(format!("{index}.err"));
    let child = File::create(&out_path)
        .and_then(|out| File::create(&err_path).map(|err| (out, err)))
        .and_then(|(out, err)| {
            Command::new(exe)
                .arg("run")
                .arg(rom)
                .arg("--frames")
                .arg(frames.to_string())
                .arg("--hash")
                .arg("--colours")
                .stdout(Stdio::from(out))
                .stderr(Stdio::from(err))
                .spawn()
        });
    let mut child = match child {
        Ok(child) => child,
        Err(err) => {
            return (
                false,
                None,
                String::new(),
                format!("cannot start child: {err}"),
            );
        }
    };

    let started = Instant::now();
    let mut timed_out = false;
    // A `wait` that cannot answer is not a status: it is a failure of ours to
    // watch the child, and its text goes into the notes beside the `None` code.
    let mut wait_error: Option<String> = None;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                wait_error = Some(format!("cannot wait for the child: {err}"));
                break None;
            }
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            timed_out = true;
            break child.wait().ok().and_then(|status| status.code());
        }
        thread::sleep(POLL);
    };

    let stdout = fs::read_to_string(&out_path).unwrap_or_default();
    let mut stderr = fs::read_to_string(&err_path).unwrap_or_default();
    if let Some(err) = wait_error {
        stderr.push_str(&err);
    }
    let _ = fs::remove_file(&out_path);
    let _ = fs::remove_file(&err_path);
    (timed_out, exit, stdout, stderr)
}

/// What one child run reported, as [`classify`] wants it.
#[derive(Debug, Clone)]
pub struct Attempt<'a> {
    /// The wall-clock limit ran out and the child was killed.
    pub timed_out: bool,
    /// The child's exit code; `None` when it never ran or was killed.
    pub code: Option<i32>,
    /// `colours=` from its stdout, if it got that far.
    pub colours: Option<usize>,
    /// Its stderr, which is where a panic and an `error:` line land.
    pub stderr: &'a str,
}

/// One child run's outcome, as a status and the note the report shows with it.
///
/// The order of the checks is the whole policy:
///
/// 1. a wall-clock kill is a [`Status::Timeout`], whatever else the child managed
///    to print before it died;
/// 2. a panic is a [`Status::Panic`], and it is found in the text rather than the
///    exit code because 101 is also what a `headless run` that could not load its
///    ROM answers with - the message is what tells them apart;
/// 3. a clean exit with a blank frame is [`Status::Blank`], and one with a
///    picture is [`Status::Boots`]. A clean exit that reported no colour count is
///    blank by our own rule: we asked for the number, so not having it means we
///    cannot claim it drew anything;
/// 4. everything else is [`Status::LoadError`], named by the child's `error:` line.
pub fn classify(attempt: &Attempt<'_>) -> (Status, String) {
    if attempt.timed_out {
        let note = note_from(attempt.stderr).unwrap_or_default();
        return (Status::Timeout, note);
    }
    if let Some(message) = panic_message(attempt.stderr) {
        return (Status::Panic, message);
    }
    match attempt.code {
        Some(0) => {
            let colours = attempt.colours.unwrap_or(0);
            let status = if colours <= BLANK_COLOUR_LIMIT {
                Status::Blank
            } else {
                Status::Boots
            };
            (status, format!("{colours} colours"))
        }
        _ => (
            Status::LoadError,
            note_from(attempt.stderr).unwrap_or_else(|| format!("child exit {:?}", attempt.code)),
        ),
    }
}

/// The panic message of a child's stderr, if it panicked.
///
/// Rust prints a panic as a location line, `thread 'main' panicked at src/lfo.rs:120:5:`,
/// then the payload on the line after it. The payload is what names the failure -
/// `index out of bounds`, the thing worth a row of a report - so that is what is
/// taken; a stderr with only the location line (an older rustc, or a payload of
/// its own) reports the location line.
fn panic_message(stderr: &str) -> Option<String> {
    let mut lines = stderr.lines();
    let header = lines.by_ref().find(|line| line.contains("panicked at"))?;
    let payload = match lines.find(|line| !line.trim().is_empty()) {
        Some(next) => next.trim(),
        None => header.trim(),
    };
    Some(shorten(payload, 200))
}

/// The child's own explanation of a failure: its `error: <what>` line, which is
/// how `main.rs` reports anything that went wrong, else the first line of its
/// output at all.
fn note_from(stderr: &str) -> Option<String> {
    let line = stderr
        .lines()
        .find(|line| line.starts_with("error:"))
        .or_else(|| stderr.lines().find(|line| !line.trim().is_empty()))?;
    Some(shorten(line.trim(), 200))
}

/// `text` cut to `max` characters, with the ellipsis that says it was cut.
///
/// A panic payload can be long and a CSV cell that runs to a paragraph makes the
/// report unreadable; 200 is more than enough to name the chip that failed.
fn shorten(text: &str, max: usize) -> String {
    let text = text.replace(['\n', '\r'], " ");
    if text.chars().count() <= max {
        return text;
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

/// `frame_hash=0x...` and `colours=N` off a child's stdout.
///
/// The child prints one `key=value` per flag, so this is the only place that has
/// to know what `headless run --hash --colours` looks like.
fn parse_report(stdout: &str) -> (Option<u64>, Option<usize>) {
    let mut frame_hash = None;
    let mut colours = None;
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "frame_hash" => {
                let hex = value
                    .trim()
                    .trim_start_matches("0x")
                    .trim_start_matches("0X");
                frame_hash = u64::from_str_radix(hex, 16).ok();
            }
            "colours" => colours = value.trim().parse::<usize>().ok(),
            _ => {}
        }
    }
    (frame_hash, colours)
}

/// The CSV header, exactly once, before every row.
const CSV_HEADER: &str = "file,title,serial,region,status,distinct_colors,frame_hash,seconds,notes";

/// Write the full per-ROM table.
pub fn write_csv(path: &Path, rows: &[RomResult]) -> Result<()> {
    let mut out = String::new();
    out.push_str(CSV_HEADER);
    out.push('\n');
    for row in rows {
        let hash = row
            .frame_hash
            .map_or_else(String::new, |hash| format!("0x{hash:016x}"));
        let fields = [
            row.file.clone(),
            row.title.clone(),
            row.serial.clone(),
            row.region.clone(),
            row.status.name().to_string(),
            row.distinct_colours.to_string(),
            hash,
            format!("{:.3}", row.seconds),
            row.notes.clone(),
        ];
        let cells: Vec<String> = fields.iter().map(|field| csv_field(field)).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(())
}

/// One CSV field, quoted if it has to be (RFC 4180).
fn csv_field(text: &str) -> String {
    if text.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// The markdown report: counts per status, then every problem, grouped by status.
///
/// The full per-ROM table is the CSV's job; this page has to stay readable at
/// 2775 ROMs, so a game that booted appears only as a number.
pub fn markdown(rows: &[RomResult], frames: u32, timeout_secs: u64) -> String {
    let mut out = String::new();
    out.push_str("# Compatibility sweep\n\n");
    out.push_str(&format!(
        "`headless sweep` over the local ROM collection: every cartridge run for {frames} frames \
         in its own child process with a {timeout_secs}s wall-clock limit. `Boots` means the run \
         finished and its last frame had more than {limit} distinct colours, which is a claim \
         about the machine, not about the game: no row here says the picture is right, only that \
         something was drawn. The full per-ROM table is \
         [`compat.csv`](compat.csv); this page lists the problems.\n\n",
        limit = BLANK_COLOUR_LIMIT,
    ));

    out.push_str("| Status | ROMs |\n|---|---|\n");
    for status in STATUS_ORDER {
        let count = rows.iter().filter(|row| row.status == status).count();
        if count > 0 {
            out.push_str(&format!("| {} | {count} |\n", status.name()));
        }
    }
    out.push('\n');

    for status in STATUS_ORDER {
        if !status.is_problem() {
            continue;
        }
        let group: Vec<&RomResult> = rows.iter().filter(|row| row.status == status).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "### {} ({} - see the CSV for the full table)\n\n",
            status.name(),
            group.len()
        ));
        out.push_str("| ROM | Title | Notes |\n|---|---|---|\n");
        for row in group {
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                row.file,
                markdown_cell(&row.title),
                markdown_cell(&row.notes)
            ));
        }
        out.push('\n');
    }
    out
}

/// One markdown table cell: no pipe, no newline, whatever the ROM said.
fn markdown_cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `classify` with the shape every case except a timeout shares.
    fn attempt(code: Option<i32>, colours: Option<usize>, stderr: &str) -> Attempt<'_> {
        Attempt {
            timed_out: false,
            code,
            colours,
            stderr,
        }
    }

    #[test]
    fn classify_names_a_timeout_whatever_the_child_printed() {
        let attempt = Attempt {
            timed_out: true,
            code: None,
            colours: Some(40),
            stderr: "error: killed\n",
        };
        let (status, notes) = classify(&attempt);
        assert_eq!(status, Status::Timeout);
        assert_eq!(notes, "error: killed");
    }

    #[test]
    fn classify_names_a_panic_from_its_message() {
        // The real shape of a Rust panic on stderr, and the payload is what the
        // report wants: this is the shape of #349's `Lfo::tick` failures.
        let stderr = concat!(
            "thread 'main' panicked at crates/ym2612/src/lfo.rs:120:5:\n",
            "index out of bounds: the len is 8 but the index is 8\n",
        );
        let (status, notes) = classify(&attempt(Some(101), None, stderr));
        assert_eq!(status, Status::Panic);
        assert_eq!(
            notes,
            "index out of bounds: the len is 8 but the index is 8"
        );
    }

    #[test]
    fn classify_names_a_load_error_rather_than_a_panic_without_a_panic() {
        let (status, notes) = classify(&attempt(
            Some(1),
            None,
            "error: cannot read ROM \"x\": No such file or directory (os error 2)\n",
        ));
        assert_eq!(status, Status::LoadError);
        assert!(
            notes.contains("No such file"),
            "the note should name the failure: {notes}"
        );
    }

    #[test]
    fn classify_calls_a_blank_frame_blank() {
        let (status, notes) = classify(&attempt(Some(0), Some(1), ""));
        assert_eq!(status, Status::Blank);
        assert_eq!(notes, "1 colours");
    }

    #[test]
    fn classify_calls_a_picture_boots() {
        let (status, notes) = classify(&attempt(Some(0), Some(40), ""));
        assert_eq!(status, Status::Boots);
        assert_eq!(notes, "40 colours");
    }

    #[test]
    fn classify_blanks_at_the_same_limit_as_the_commercial_suite() {
        let at = |colours: usize| classify(&attempt(Some(0), Some(colours), "")).0;
        assert_eq!(at(BLANK_COLOUR_LIMIT), Status::Blank);
        assert_eq!(at(BLANK_COLOUR_LIMIT + 1), Status::Boots);
        // A clean exit that never reported a colour count cannot claim a picture.
        assert_eq!(classify(&attempt(Some(0), None, "")).0, Status::Blank);
    }

    #[test]
    fn skip_reason_skips_32x_and_sega_cd_and_nothing_else() {
        assert_eq!(
            skip_reason("SEGA MEGA DRIVE", 0x80000),
            None,
            "a licensed Mega Drive cart runs"
        );
        assert_eq!(skip_reason(" SEGA GENESIS", 0x80000), None);
        assert_eq!(skip_reason("SEGA 32X", 0x80000), Some("Sega 32X cartridge"));
        assert_eq!(skip_reason("SEGA-32X", 0x80000), Some("Sega 32X cartridge"));
        assert_eq!(
            skip_reason("SEGA CD ROM", 0x80000),
            Some("Sega CD image"),
            "a Sega CD BIOS is not a cartridge fault"
        );
        assert_eq!(skip_reason("SEGA MEGA DRIVE", 0), Some("empty file"));
    }

    #[test]
    fn parse_report_reads_the_two_lines_a_child_prints() {
        let stdout = "frame_hash=0x0001020304050607\ncolours=42\n";
        assert_eq!(parse_report(stdout), (Some(0x0001020304050607), Some(42)));
        assert_eq!(parse_report(""), (None, None));
        assert_eq!(parse_report("nonsense\n"), (None, None));
    }

    #[test]
    fn csv_field_quotes_only_what_needs_it() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    /// A row, for the report tests, without the eleven fields nobody is testing.
    fn row(file: &str, status: Status, notes: &str) -> RomResult {
        RomResult {
            file: file.to_string(),
            title: file.to_uppercase(),
            serial: "GM 00000000-00".to_string(),
            region: "usa".to_string(),
            status,
            distinct_colours: 0,
            frame_hash: None,
            seconds: 1.0,
            notes: notes.to_string(),
        }
    }

    #[test]
    fn markdown_report_counts_every_status_and_lists_the_problems() {
        let rows = vec![
            row("good.md", Status::Boots, "40 colours"),
            row("also_good.md", Status::Boots, "12 colours"),
            row("noisy.md", Status::Panic, "index out of bounds"),
            row("dark.md", Status::Blank, "1 colours"),
        ];
        let text = markdown(&rows, 600, 30);

        // A summary table of counts per status.
        assert!(
            text.contains("| Boots | 2 |\n"),
            "no count row for Boots:\n{text}"
        );
        assert!(text.contains("| Panic | 1 |\n"), "no count row for Panic");
        assert!(text.contains("| Blank | 1 |\n"), "no count row for Blank");
        assert!(
            !text.contains("| Timeout |"),
            "a status with no ROMs gets no row"
        );

        // And one section per problem status, naming the ROM and what it said.
        assert!(
            text.contains("### Panic (1 - see the CSV for the full table)"),
            "no Panic section:\n{text}"
        );
        assert!(text.contains("| `noisy.md` |"));
        assert!(text.contains("index out of bounds"));
        assert!(text.contains("| `dark.md` |"));

        // A game that booted is a number, not a row: the CSV has the table.
        assert!(
            !text.contains("`good.md`"),
            "a booting game should not be listed:\n{text}"
        );
        assert!(
            text.contains("600 frames"),
            "the report should say its settings"
        );
    }

    #[test]
    fn markdown_report_keeps_a_title_on_one_line() {
        let rows = vec![row("bad.md", Status::Panic, "line one\n| line two")];
        let text = markdown(&rows, 600, 30);
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("bad.md"))
            .collect();
        assert_eq!(lines.len(), 1, "one row per ROM:\n{text}");
        assert!(
            lines[0].contains("\\|"),
            "the pipe must be escaped: {}",
            lines[0]
        );
    }

    #[test]
    fn csv_has_a_header_and_one_row_per_rom() {
        let dir = std::env::temp_dir().join(format!("rustsega-sweep-csv-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("out.csv");
        let rows = vec![row("a.md", Status::Boots, "40 colours")];
        write_csv(&path, &rows).expect("write csv");
        let text = fs::read_to_string(&path).expect("read csv");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], CSV_HEADER);
        assert_eq!(lines.len(), 2, "one row per ROM:\n{text}");
        assert!(lines[1].starts_with("a.md,"));
        fs::remove_dir_all(&dir).expect("clean temp dir");
    }
}
