//! The harness's own tests for the SingleStepTests 68000 suite (M1-03, #11).
//!
//! The harness is the piece every later m68k instruction ticket builds on, so
//! these tests check the harness rather than the CPU: it parses both file
//! layouts, it skips cleanly when the data is absent, it loads every field of
//! the suite's machine state into a real [`m68k::M68k`] and reads it back, and
//! its comparison notices a single wrong byte.
//!
//! Nothing here runs a real instruction file to green, and that is deliberate.
//! `M68k::step` is still a stub (M1-04, #12 onward), so no instruction file can
//! pass yet; [`all_instructions`] is the `#[ignore]`d whole-suite run that
//! reports how far the core actually is, per file and in total.
//!
//! Cases are written as JSON rather than as struct literals so they go through
//! the same parse path as the real suite — a field the harness stopped
//! comparing shows up here instead of as a silent pass — and then poked one
//! field at a time, which is how `crates/z80/tests/singlestep.rs` does it.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::write::GzEncoder;

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `usp` in [`BASE`]: 0x0100_0000, deliberately different from `ssp` so a test
/// can tell which one the CPU picked up as the active stack pointer.
const USP: u32 = 16_777_216;
/// `ssp` in [`BASE`]: 0x0000_2000, the value the real suite almost always uses.
const SSP: u32 = 8192;

/// A whole, real-shaped case for an instruction that does nothing: `final`
/// equals `initial`, and `length` is 0, which is exactly what today's stub
/// `M68k::step` leaves behind. It is the control for every test below.
///
/// Field names and layout are copied from the pinned suite
/// (`test-roms/680x0/68000/v1/README.md`): `d0`..`d7`, `a0`..`a6`, `usp`,
/// `ssp`, `sr`, `pc`, `prefetch` and `ram` in both states, plus `length` and
/// `transactions` per case. There is no `a7` in the suite — `a[7]` is whichever
/// stack pointer SR's S bit selects. The key set is identical in every file
/// (`MOVE.w`, `TAS`, `CHK` and `MOVE.q` diff the same) and both `ram` and
/// `transactions` are positional arrays, so one set of types covers the suite
/// and a file that fails to parse is a harness bug, not a schema quirk.
///
/// `transactions` is spelled out here even though the harness never parses it,
/// to pin that: comparing bus transactions is out of scope for this suite, and
/// skipping them is what keeps a ~1.4 GB suite cheap to read.
const BASE: &str = r#"{
    "name": "4e71 [does nothing] 1",
    "initial": {
        "d0": 0, "d1": 1, "d2": 2, "d3": 3, "d4": 4, "d5": 5, "d6": 6, "d7": 7,
        "a0": 16, "a1": 17, "a2": 18, "a3": 19, "a4": 20, "a5": 21, "a6": 22,
        "usp": 16777216, "ssp": 8192, "sr": 9985, "pc": 3072,
        "prefetch": [20081, 10835],
        "ram": [[3072, 78], [3073, 113], [3076, 6], [3077, 121]]
    },
    "final": {
        "d0": 0, "d1": 1, "d2": 2, "d3": 3, "d4": 4, "d5": 5, "d6": 6, "d7": 7,
        "a0": 16, "a1": 17, "a2": 18, "a3": 19, "a4": 20, "a5": 21, "a6": 22,
        "usp": 16777216, "ssp": 8192, "sr": 9985, "pc": 3072,
        "prefetch": [20081, 10835],
        "ram": [[3072, 78], [3073, 113], [3076, 6], [3077, 121]]
    },
    "length": 0,
    "transactions": [["r", 4, 6, 3076, ".w", 1657]]
}"#;

/// Parse a case from a JSON literal. The panic prints the parse error, which is
/// the useful failure for a test whose own JSON went stale.
fn case(json: &str) -> harness::TestCase {
    serde_json::from_str(json).expect("test case JSON should parse")
}

/// The control case.
fn base() -> harness::TestCase {
    case(BASE)
}

/// [`BASE`] with both states' `sr` replaced, which is how the S bit and the
/// interrupt mask get varied without duplicating 20 lines of JSON per test.
/// The suite spells `sr` in decimal, so the target is the decimal form of
/// 0x2701.
fn base_with_sr(sr: u16) -> harness::TestCase {
    let json = BASE.replace("\"sr\": 9985", &format!("\"sr\": {sr}"));
    case(&json)
}

/// Run one case through a reused machine and return just the mismatch lines,
/// which is all a test here should have to look at.
fn run(case: &harness::TestCase, check: harness::Check) -> Vec<String> {
    harness::Machine::new()
        .run(case, check)
        .iter()
        .map(harness::Mismatch::line)
        .collect()
}

/// The lines a [`harness::Report`] would keep for `cases`: the same
/// case-prefixed, `"; "`-joined one-line-per-failing-case form that
/// [`harness::assert_file_passes`] prints and that an instruction ticket reads
/// off CI. [`run`] deliberately stops short of that, so a test that wants the
/// real failure line has to go through the report.
fn failure_lines(cases: Vec<harness::TestCase>, check: harness::Check) -> Vec<String> {
    harness::run_cases(&cases, check).failures
}

/// A temp directory that removes itself, so these tests can point the harness at
/// a data root that is not the real one. `std::env::set_var` is `unsafe` in
/// edition 2024 and the workspace forbids `unsafe`, so the harness takes a base
/// directory as an argument (the `testkit::test_rom_in` convention) and the
/// tests drive it through this instead.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rustsega-m68k-sst-{tag}-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// The suite's `NOP` file: one file per instruction, and this one is the
/// harness's parse and state-load self-check.
const NOP: &str = "NOP";

#[test]
fn harness_parses_nop_file() {
    let Some(cases) = harness::load_file(NOP) else {
        eprintln!("{}", harness::skip_line(NOP));
        return;
    };
    assert!(
        !cases.is_empty(),
        "{NOP} held no tests: the suite layout changed?"
    );
    // The suite's own label format: opcode, mnemonic in brackets, case index.
    // Failure reports quote it back, so it is part of the contract.
    assert!(
        cases[0].name.starts_with("4e71 [NOP]"),
        "unexpected case name {:?}",
        cases[0].name
    );

    // State round trip, no execution: every field of the suite's initial state
    // has to land in a real CPU and read back out unchanged. A field the
    // harness forgot to load, or loaded into the wrong register, fails here —
    // while there is no instruction yet that could hide the mistake.
    let mut cpu = m68k::M68k::new();
    harness::apply(&cases[0].initial, &mut cpu);
    let mut want = cases[0].initial.clone();
    want.ram.clear(); // RAM lives in the bus, not in the CPU, so `snapshot` cannot see it.
    assert_eq!(
        harness::snapshot(&cpu),
        want,
        "loading {:?} and reading it back must be the identity",
        cases[0].name
    );
    // SR.S selects which stack pointer `a[7]` is, and `a[7]` is not a suite
    // field, so the round trip above cannot cover it.
    assert_eq!(
        cpu.a[7],
        cases[0].initial.active_sp(),
        "a[7] must be the stack pointer SR.S selects"
    );
}

#[test]
fn identical_states_compare_equal() {
    // A case whose expected state is what the CPU actually ends up in reports
    // nothing at all. Under `StateAndCycles` this pins the cycle comparison too,
    // since `length: 0` is what the stub `step` returns.
    assert_eq!(
        run(&base(), harness::Check::StateAndCycles),
        Vec::<String>::new()
    );
}

#[test]
fn flipping_one_ram_byte_reports_exactly_that_difference() {
    let mut c = base();
    c.final_state.ram[2].1 = 0xEE; // [3076, 6] -> [3076, 0xEE]
    assert_eq!(
        run(&c, harness::Check::State),
        vec!["ram[0x00000C04]: expected 0xEE, got 0x06"]
    );
}

#[test]
fn one_failure_line_carries_every_difference() {
    // Registers, SR, PC and the prefetch queue are all compared, and a failing
    // case lists its differences on one line, so a future instruction ticket
    // can read the whole diff at once. `sr` here keeps S set so `a[7]` stays
    // consistent and only the four edited fields are reported.
    let mut c = base();
    c.final_state.d3 = 0x1234;
    c.final_state.sr = 0x2700;
    c.final_state.pc = 3074;
    c.final_state.prefetch[1] = 0x6789;
    assert_eq!(
        failure_lines(vec![c], harness::Check::State),
        vec![
            "case \"4e71 [does nothing] 1\": d3: expected 0x00001234, got 0x00000003; \
             sr: expected 0x2700, got 0x2701; pc: expected 0x00000C02, got 0x00000C00; \
             prefetch[1]: expected 0x6789, got 0x2A53"
        ]
    );
}

#[test]
fn only_state_and_cycles_checks_the_cycle_count() {
    // `Check::State` is what an instruction ticket turns on first: the cycle
    // tables land per group later, so a group should not be blocked on them.
    let mut c = base();
    c.length = 4;
    assert_eq!(run(&c, harness::Check::State), Vec::<String>::new());
    assert_eq!(
        run(&c, harness::Check::StateAndCycles),
        vec!["cycles: expected 4, got 0"]
    );
}

#[test]
fn a7_follows_the_s_bit() {
    // `a[7]` is the active stack pointer: the SSP in supervisor mode, the USP in
    // user mode (M68000UM, "Status Register"). The suite is ~99% supervisor, so
    // the user-mode half of that would go untested by real data for a long time.
    //
    // S is bit 13, so 0x2000 on its own is still *supervisor*; the user-mode
    // value here is 0x0700, the same interrupt mask with S clear (`m68k::SR_S`).
    assert_eq!(base_with_sr(0x2700).initial.active_sp(), SSP);
    assert_eq!(base_with_sr(0x0700).initial.active_sp(), USP);

    // And check it is the CPU that ends up that way, not just the predicate:
    // loading must not let `set_sr`'s stack-pointer swap move the value just
    // loaded into `a[7]`.
    for sr in [0x2700u16, 0x0700] {
        let want = if sr & m68k::SR_S != 0 { SSP } else { USP };
        let mut cpu = m68k::M68k::new();
        harness::apply(&base_with_sr(sr).initial, &mut cpu);
        assert_eq!(cpu.a[7], want, "a[7] after loading sr = {sr:#06X}");
        assert_eq!(cpu.ssp, SSP, "the stored SSP is the SSP in either mode");
        assert_eq!(cpu.usp, USP, "the stored USP is the USP in either mode");
    }
}

/// The four bytes of a handler long at $00000C..=$00000F: 0x0000_1400, the
/// address the pinned suite puts in its exception vectors.
const HANDLER_BYTES: [(u32, u8); 4] = [(12, 0), (13, 0), (14, 20), (15, 0)];
/// 0x0000_1400: where `final.pc` lands when the CPU takes that vector.
const HANDLER: u32 = 5120;

#[test]
fn exception_cases_are_detected_and_skipped() {
    // Address errors are M1-29 (#37). Until then `Check::StateSkipAddressErrors`
    // leaves those cases alone, and the suite spells them out: the handler
    // long is in `initial.ram` at $00000C..=$00000F and `final.pc` is that long.
    assert!(
        !harness::is_address_error_case(&base()),
        "a case with nothing at $C..=$F is not an exception case"
    );

    let mut exception = base();
    exception.initial.ram.extend_from_slice(&HANDLER_BYTES);
    exception.final_state.pc = HANDLER;
    assert!(
        harness::is_address_error_case(&exception),
        "a case that ends at the $C vector is an exception case"
    );

    // Skipped, not failed: unskipped the same case would be a plain failure,
    // because the stub does not take the exception.
    let report = harness::run_cases(&[base(), exception], harness::Check::StateSkipAddressErrors);
    assert_eq!((report.total, report.passed, report.skipped), (2, 1, 1));
    assert!(
        report.failures.is_empty(),
        "a skipped case is not a failure"
    );

    let report = harness::run_cases(&[base()], harness::Check::State);
    assert_eq!((report.total, report.passed, report.skipped), (1, 1, 0));
}

#[test]
fn an_incomplete_vector_is_not_skipped() {
    // Three of the four vector bytes: the long cannot be read out, so the case
    // has to be run like any other. Getting this wrong would silently drop real
    // cases from the suite and inflate the pass count.
    //
    // The case starts at the handler address too, so it is otherwise a clean
    // control: `(1, 1, 0)` can then only mean "run and passed", which is what
    // distinguishes it from the skipped case next door.
    let mut partial = base();
    partial.initial.ram.extend_from_slice(&HANDLER_BYTES[..3]);
    partial.initial.pc = HANDLER;
    partial.final_state.pc = HANDLER;
    assert!(!harness::is_address_error_case(&partial));

    let report = harness::run_cases(&[partial], harness::Check::StateSkipAddressErrors);
    assert_eq!((report.total, report.passed, report.skipped), (1, 1, 0));
}

#[test]
fn a_missing_data_root_skips_rather_than_failing() {
    // `just check` has to be green with or without `just fetch`, so "no data"
    // is a skip all the way through the API rather than an error. An empty temp
    // directory stands in for a checkout that never fetched the suite.
    let empty = TempDir::new("missing");
    assert!(harness::suite_dir_in(empty.as_ref()).is_none());
    assert!(harness::load_file_in(empty.as_ref(), NOP).is_none());
    assert!(harness::all_files_in(empty.as_ref()).is_none());
    assert!(
        harness::run_file_in(empty.as_ref(), NOP, harness::Check::State).is_none(),
        "a run with no data reports nothing rather than failing"
    );
    assert!(
        harness::run_all_files_in(empty.as_ref(), harness::Check::State).is_empty(),
        "the sweep prints SKIPPED and reports nothing rather than panicking"
    );
}

#[test]
fn a_missing_file_skips_instead_of_failing() {
    // Ticket #11: a missing file prints `SKIPPED: <file> not found` and returns
    // without failing — including when the suite itself is present and the name
    // is simply wrong. Green either way, with or without the data fetched.
    assert_eq!(harness::skip_line(NOP), "SKIPPED: NOP not found");
    harness::assert_file_passes("NO-SUCH-INSTRUCTION-9f3b", harness::Check::State);
}

#[test]
fn plain_json_and_gzipped_files_both_load() {
    // The pinned suite is all `.json.gz`, but the ticket requires both layouts
    // and a future pin could land plain `.json`. A scratch data root with one
    // file of each shape, plus a non-JSON file that must not be mistaken for an
    // instruction file.
    let root = TempDir::new("ext");
    let suite = root.as_ref().join(harness::SUITE_DIR);
    fs::create_dir_all(&suite).expect("create suite dir");

    fs::write(suite.join("PLAIN.json"), format!("[{BASE}]")).expect("write plain json");
    let mut encoder = GzEncoder::new(
        File::create(suite.join("GZIPPED.json.gz")).expect("create gzip file"),
        Compression::default(),
    );
    encoder
        .write_all(format!("[{BASE}]").as_bytes())
        .expect("write gzip body");
    encoder.finish().expect("finish gzip file");
    fs::write(suite.join("README.md"), "not a test file").expect("write readme");

    for name in ["PLAIN", "GZIPPED"] {
        let loaded = harness::load_file_in(root.as_ref(), name);
        assert_eq!(
            loaded.map_or(0, |cases| cases.len()),
            1,
            "{name} should load to one case"
        );
    }
    assert_eq!(
        harness::all_files_in(root.as_ref()).expect("the scratch suite is present"),
        ["GZIPPED", "PLAIN"],
        "both layouts are enumerated, README.md is not, and the list is sorted"
    );
}

#[test]
fn every_instruction_file_in_the_suite_is_enumerated() {
    // The M1-32 (#42) gate walks this list, so it has to be the whole suite:
    // every instruction file, sorted, and nothing that is not a test file.
    let Some(names) = harness::all_files() else {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    };
    assert_eq!(
        names.len(),
        124,
        "the pinned 680x0 suite holds 124 instruction files (docs/TESTING.md)"
    );
    assert!(names.iter().any(|name| name == "NOP"), "NOP is missing");
    assert!(
        names.iter().any(|name| name == "MOVE.w"),
        "MOVE.w is missing"
    );
    assert!(
        !names.iter().any(|name| name.contains(".json")),
        "names go back into `run_file`, which adds the extension itself"
    );
    assert!(
        !names.iter().any(|name| name.eq_ignore_ascii_case("README")),
        "the suite's README.md is not an instruction file"
    );
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        names, sorted,
        "`read_dir` order is arbitrary; reports must sort"
    );
}

/// Every file in the suite, one line each, then the grand total.
///
/// `#[ignore]`d because no instruction is implemented yet (`M68k::step` is a
/// stub until M1-04, #12, onward), so this reports instead of asserting; M1-32
/// (#42) is where it becomes a gate. Run it with
/// `cargo nextest run -p m68k --run-ignored all --no-capture all_instructions`;
/// `just test-all` on `main` runs it too, but nextest hides the output of a test
/// that passes, so its counts only reach the CI log once #186 lands.
#[test]
#[ignore = "reports the whole suite; no instruction is implemented yet"]
fn all_instructions() {
    let reports = harness::run_all_files(harness::Check::StateSkipAddressErrors);
    let files = reports.len();
    let files_passed = reports
        .iter()
        .filter(|(_, report)| report.failures.is_empty())
        .count();
    let cases: usize = reports.iter().map(|(_, report)| report.total).sum();
    let passed: usize = reports.iter().map(|(_, report)| report.passed).sum();
    let skipped: usize = reports.iter().map(|(_, report)| report.skipped).sum();
    println!(
        "68000 SingleStepTests: {files_passed}/{files} files pass; \
         {passed}/{cases} cases passed, {skipped} cases skipped as exceptions \
         (Check::StateSkipAddressErrors, #185 / M1-29 #37), {} files failed",
        files - files_passed,
    );
}
