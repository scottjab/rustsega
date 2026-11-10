//! The harness's own tests for the SingleStepTests 68000 suite (M1-03, #11).
//!
//! The harness is the piece every later m68k instruction ticket builds on, so
//! these tests check the harness rather than the CPU: it parses both file
//! layouts, it skips cleanly when the data is absent, it loads every field of
//! the suite's machine state into a real [`m68k::M68k`] and reads it back, and
//! its comparison notices a single wrong byte.
//!
//! Most of what follows is the harness's own fixtures: they poke one field of a
//! hand-written case and check that the comparison notices exactly that field.
//! Running real instruction files to green is what the per-group tests in
//! `crates/m68k/tests/*.rs` do; the whole-suite [`all_instructions`] run here is
//! the gate over all of them at once, and `docs/m68k-conformance.md` records
//! what it measured.
//!
//! # Why the fixtures changed in #15
//!
//! Until M1-07 (#15), `step()` ignored the opcode and returned 0, so the `4E71`
//! (`NOP`) control case below ran clean and a fixture expecting no mismatches
//! meant exactly that. `step()` now decodes (see `src/decode.rs`), finds that
//! `NOP` has no handler, and panics — loudly, which is what #15 requires, so a
//! suite can never report a passing case for an instruction that did not run.
//! The harness catches that panic per case and records it as one more
//! [`harness::Mismatch`].
//!
//! So every fixture that used to expect an empty mismatch list now expects
//! [`UNIMPLEMENTED_4E74`] alongside whatever difference it is really testing.
//! `BASE` deliberately stays an unimplemented opcode rather than becoming an
//! implemented one: these tests are about the comparison, not the instruction,
//! and one identical panic line on every case cannot mask a register, RAM or
//! cycle difference. It was `4E71` (`NOP`) until #25 implemented `NOP`, then
//! `4E70` (`RESET`) until #35 implemented that, which moved the control to
//! `4E74` — the licence this test alone has to move its anchor, see
//! `decode::tests::unknown_is_unimplemented_for_now`.
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

/// A whole, real-shaped case for an instruction that changes nothing: `final`
/// equals `initial`, and `length` is 0. It is the control for every test below.
///
/// `4E74` is still unimplemented — the hole `$4E70..=$4E77` leaves once RESET,
/// NOP, RTE, RTS, TRAPV and RTR all have handlers (#35) — so the state
/// comparison here passes because the fixture's `final` *is* its `initial` and
/// `step()` touches no register on the way to the panic. See the module docs for
/// what that means for the expectations below.
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
    "name": "4e74 [does nothing] 1",
    "initial": {
        "d0": 0, "d1": 1, "d2": 2, "d3": 3, "d4": 4, "d5": 5, "d6": 6, "d7": 7,
        "a0": 16, "a1": 17, "a2": 18, "a3": 19, "a4": 20, "a5": 21, "a6": 22,
        "usp": 16777216, "ssp": 8192, "sr": 9985, "pc": 3072,
        "prefetch": [20084, 10835],
        "ram": [[3072, 78], [3073, 116], [3076, 6], [3077, 121]]
    },
    "final": {
        "d0": 0, "d1": 1, "d2": 2, "d3": 3, "d4": 4, "d5": 5, "d6": 6, "d7": 7,
        "a0": 16, "a1": 17, "a2": 18, "a3": 19, "a4": 20, "a5": 21, "a6": 22,
        "usp": 16777216, "ssp": 8192, "sr": 9985, "pc": 3072,
        "prefetch": [20084, 10835],
        "ram": [[3072, 78], [3073, 116], [3076, 6], [3077, 121]]
    },
    "length": 0,
    "transactions": [["r", 4, 6, 3076, ".w", 1657]]
}"#;

/// The one mismatch every fixture below carries in addition to whatever it is
/// testing: `step()` decodes `4E74`, finds no handler, and panics naming the
/// opcode (`src/ops/mod.rs`), which [`harness::Machine::run`] catches per case
/// and records as a `panic` mismatch.
///
/// Quoted from an actual run rather than assembled from the `unimplemented!`
/// call, because the text is the harness's formatting as well as the panic's.
const UNIMPLEMENTED_4E74: &str = "panic: expected none, got not implemented: m68k opcode 4E74";

/// The failure line [`harness::run_cases`] keeps for a lone [`BASE`] case: the
/// case name it quotes, then nothing but the unimplemented opcode.
fn unimplemented_base_failure() -> String {
    format!("case \"4e74 [does nothing] 1\": {UNIMPLEMENTED_4E74}")
}

/// The same control case with `ILLEGAL` (`$4AFC`, M68000PM/AD section 8) as the
/// instruction under execution.
///
/// Where [`BASE`] panics, this one *returns*: it takes the illegal-instruction
/// exception, which pushes the frame and jumps to the vector long
/// (`src/exceptions.rs`, M1-08 #16). Tests that need the clock count compared
/// need a case that gets there, because `Machine::run` compares `length` on the
/// `Ok(cycles)` branch and records the panic on the other — they are mutually
/// exclusive, so a panicking case never reaches the cycle check.
///
/// The `final` side is the state #16 really produces, not the "nothing changed"
/// copy it had while the route was a stub (#15): the SSP has moved down a frame,
/// `pc` is the vector long, and the queue came from there.
fn illegal_case() -> harness::TestCase {
    let mut c = base();
    c.name = "4afc [does nothing] 1".to_string();
    // `prefetch[0]` is the word `step()` decodes. The two `pc` bytes are the
    // same instruction, and the suite lists them because they are part of the
    // machine state, so the fixture says `4AFC` in both places.
    c.initial.prefetch[0] = 0x4AFC;
    for state in [&mut c.initial, &mut c.final_state] {
        state.ram[0].1 = 0x4A;
        state.ram[1].1 = 0xFC;
    }
    // Exception entry reads the handler long from `$00000C`, which this
    // fixture's RAM does not list, so it comes back as zero: the "handler" is
    // address 0 and the queue refills from there.
    c.final_state.prefetch = [0, 0];
    c.final_state.pc = 0;
    c.final_state.ssp = SSP - 6;
    // The frame itself, listed byte by byte the way the suite does it. Only the
    // addresses a case lists are compared, so these six bytes really do assert
    // what the push wrote: SR where it was (`0x2701`), then the address of the
    // faulting opcode (`$00000C00`), which is how an illegal-instruction frame
    // lets a handler decode the opcode (M68000UM "Stack Frame Formats").
    c.final_state.ram.extend_from_slice(&[
        (SSP - 6, 0x27),
        (SSP - 5, 0x01),
        (SSP - 4, 0x00),
        (SSP - 3, 0x00),
        (SSP - 2, 0x0C),
        (SSP - 1, 0x00),
    ]);
    c
}

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
    // nothing but the unimplemented instruction: every register, both stack
    // pointers, SR, PC, both prefetch words and the listed RAM bytes matched,
    // because the fixture's `final` is its `initial`. The panic line is the
    // only thing left over, so it cannot be hiding a field the harness stopped
    // comparing.
    assert_eq!(
        run(&base(), harness::Check::StateAndCycles),
        vec![UNIMPLEMENTED_4E74]
    );
}

#[test]
fn flipping_one_ram_byte_reports_exactly_that_difference() {
    let mut c = base();
    c.final_state.ram[2].1 = 0xEE; // [3076, 6] -> [3076, 0xEE]
    // Still exactly one RAM line, naming exactly that byte and nothing else;
    // the panic line is the same on every case, so it is noise the test names
    // rather than something that could bury the difference.
    assert_eq!(
        run(&c, harness::Check::State),
        vec![
            "ram[0x00000C04]: expected 0xEE, got 0x06",
            UNIMPLEMENTED_4E74,
        ]
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
    // Five differences, which is exactly the harness's per-case reporting cap,
    // so the line is complete and carries no "+N more".
    assert_eq!(
        failure_lines(vec![c], harness::Check::State),
        vec![format!(
            "case \"4e74 [does nothing] 1\": d3: expected 0x00001234, got 0x00000003; \
             sr: expected 0x2700, got 0x2701; pc: expected 0x00000C02, got 0x00000C00; \
             prefetch[1]: expected 0x6789, got 0x2A53; {UNIMPLEMENTED_4E74}"
        )]
    );
}

#[test]
fn only_state_and_cycles_checks_the_cycle_count() {
    // `Check::State` is what an instruction ticket turns on first: the cycle
    // tables land per group later, so a group should not be blocked on them.
    let mut c = base();
    c.length = 4;
    // `Check::State` says nothing about clocks even when the count is wrong:
    // the only line it reports for this case is the unimplemented opcode, not a
    // cycle difference.
    assert_eq!(
        run(&c, harness::Check::State),
        vec![UNIMPLEMENTED_4E74],
        "State must not report a cycle difference"
    );

    // The cycle comparison itself needs a case that *returns*: `run` compares
    // `length` on the `Ok(cycles)` branch and records the panic on the other,
    // so `4E74` can never reach it. `4AFC` takes the exception, and since
    // M1-31a (#39) `step` reports the whole clock cost of the step, so it comes
    // back with the 34 the entry sequence charged into `M68k::cycles`.
    const ENTRY: u32 = 34;
    let mut returns = illegal_case();
    returns.length = ENTRY;
    assert_eq!(
        run(&returns, harness::Check::StateAndCycles),
        Vec::<String>::new(),
        "the step's 34 clocks are what `length` says, so StateAndCycles is clean"
    );
    returns.length = 4;
    assert_eq!(run(&returns, harness::Check::State), Vec::<String>::new());
    assert_eq!(
        run(&returns, harness::Check::StateAndCycles),
        vec!["cycles: expected 4, got 34"],
        "only StateAndCycles reports it"
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

    // Skipped, not failed: unskipped, the same case would add a second failure,
    // because the stub records the vector but does not take the exception.
    let report = harness::run_cases(&[base(), exception], harness::Check::StateSkipAddressErrors);
    // The control no longer passes either — it panics on the unimplemented
    // opcode — so `passed` is 0 and the count that matters is `skipped: 1`.
    assert_eq!((report.total, report.passed, report.skipped), (2, 0, 1));
    // And the skip left exactly the control's failure behind, naming the opcode
    // the core has no handler for. That is what makes `skipped: 1` mean "the
    // exception case was left out" rather than "the exception case passed": a
    // skipped case contributes no failure line, so its absence is the evidence.
    assert_eq!(
        report.failures,
        vec![unimplemented_base_failure()],
        "a skipped case is not a failure, and the surviving one names the opcode"
    );

    let report = harness::run_cases(&[base()], harness::Check::State);
    assert_eq!((report.total, report.passed, report.skipped), (1, 0, 0));
}

#[test]
fn an_incomplete_vector_is_not_skipped() {
    // Three of the four vector bytes: the long cannot be read out, so the case
    // has to be run like any other. Getting this wrong would silently drop real
    // cases from the suite and inflate the pass count.
    //
    // The case starts at the handler address too, so it is otherwise a clean
    // control: `skipped == 0` can then only mean "it was run", which is what
    // distinguishes it from the skipped case next door.
    let mut partial = base();
    partial.initial.ram.extend_from_slice(&HANDLER_BYTES[..3]);
    partial.initial.pc = HANDLER;
    partial.final_state.pc = HANDLER;
    assert!(!harness::is_address_error_case(&partial));

    let report = harness::run_cases(&[partial], harness::Check::StateSkipAddressErrors);
    assert_eq!((report.total, report.passed, report.skipped), (1, 0, 0));
    // It ran, and it reported: the only difference is the unimplemented opcode.
    // A case the predicate had dropped would have contributed no line at all.
    assert_eq!(report.failures, vec![unimplemented_base_failure()]);
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

/// The `Check` level the whole-suite gate runs at: state *and* clocks, with the
/// cases that end in the suite's exception handler left out.
///
/// State-only would leave the clock tables unmeasured, and the exception cases
/// cannot be checked at any level until #223 lands, so this is the strictest
/// level the suite can be held to today.
const GATE_CHECK: harness::Check = harness::Check::StateAndCyclesSkipAddressErrors;

/// Files the gate lets fail, each with the issue that carries it, as
/// `(file, issue)` with a comment saying what fails and on which axis.
///
/// This list is the whole of the suite's outstanding failure budget, so it is
/// checked both ways: a file that fails and is not here fails the gate, and a
/// file here that passes fails it too (see [`gate_diff`]). The list cannot
/// quietly grow stale, and `docs/m68k-conformance.md` documents the same set with
/// the measured numbers.
const KNOWN_FAILURES: &[(&str, &str)] = &[
    // Two cases, state, not clocks: their recorded `final.d2` cannot come from
    // their own `initial.d2` on any machine. Upstream data bug, #209; the
    // same two cases `shifts.rs` drops from `asl_b_file_passes`.
    ("ASL.b", "#209"),
    // Clocks only — every CHK case's state matches, which is what the
    // `Check::StateSkipAddressErrors` per-file test already proves. 4948 of the
    // 8065 cases run (the rest are the exception cases) and all 4948 are 4 or 6
    // clocks below the suite's `length`: a trapped CHK wants 38 + ea and a
    // passing one wants 10 + ea, against this core's 34 + ea and 4 + ea.
    // #241 carries the CHK clock law.
    ("CHK", "#241"),
];

/// The files that failed without an issue, and the issues whose files now pass.
///
/// A known file that has gone missing from the suite counts as "now passes":
/// the pin moved under us and the gate should say so rather than stop covering it.
fn gate_diff(reports: &[(String, harness::Report)]) -> (Vec<String>, Vec<String>) {
    let unlisted = reports
        .iter()
        .filter(|(_, report)| report.failed() > 0)
        .map(|(name, _)| name)
        .filter(|name| {
            KNOWN_FAILURES
                .iter()
                .all(|(file, _)| *file != name.as_str())
        })
        .map(String::from)
        .collect();
    let stale = KNOWN_FAILURES
        .iter()
        .filter(|(file, _)| {
            reports
                .iter()
                .all(|(name, report)| name != *file || report.failed() == 0)
        })
        .map(|(file, _)| (*file).to_string())
        .collect();
    (unlisted, stale)
}

/// One row per file, sorted: file, total, passed, failed, skipped.
///
/// `run_all_files` sorts by name, so two sweeps produce two tables that diff.
fn print_table(reports: &[(String, harness::Report)]) {
    println!(
        "{:<16}{:>8}{:>8}{:>8}{:>8}",
        "file", "total", "passed", "failed", "skipped"
    );
    for (name, report) in reports {
        println!(
            "{name:<16}{:>8}{:>8}{:>8}{:>8}",
            report.total,
            report.passed,
            report.failed(),
            report.skipped
        );
    }
}

/// The failure lines a [`harness::Report`] kept, under a heading naming the file
/// and its issue, so a reader sees the mismatch rather than inferring it from a
/// table cell.
fn print_failures(reports: &[(String, harness::Report)]) {
    for (name, report) in reports {
        if report.failed() == 0 {
            continue;
        }
        let issue = KNOWN_FAILURES
            .iter()
            .find(|(file, _)| *file == name)
            .map_or(String::new(), |(_, issue)| format!(" ({issue})"));
        println!("\n{name}{issue}: {} failing cases", report.failed());
        for failure in &report.failures {
            println!("  {failure}");
        }
    }
}

/// The grand total under the table: files, cases, skipped cases, failures.
fn print_totals(reports: &[(String, harness::Report)]) {
    let sum = |pick: fn(&harness::Report) -> usize| -> usize {
        reports.iter().map(|(_, report)| pick(report)).sum()
    };
    let failing = reports
        .iter()
        .filter(|(_, report)| report.failed() > 0)
        .count();
    println!(
        "\n{} files: {}/{} cases passed, {} cases skipped as exceptions, \
         {} cases failing in {failing} files",
        reports.len(),
        sum(|report| report.passed),
        sum(|report| report.total),
        sum(|report| report.skipped),
        sum(|report| report.failed()),
    );
}

/// The whole 68000 SingleStepTests suite in one run: every file, [`GATE_CHECK`],
/// printed as a per-file table and then gated.
///
/// It gates exactly as far as [`KNOWN_FAILURES`] says: one more failing file, or
/// one fewer, and the test fails with the difference. The gate is not the only
/// place a failure is recorded — `docs/m68k-conformance.md` holds the same list
/// with the numbers and the reason, and one issue per entry.
///
/// `#[ignore]`d so `just check` does not read the 195 MB suite; `just test-all`
/// and the dedicated CI step (`just test-all` is main-only) run it. Nextest
/// hides the output of a passing test, so the table only reaches the CI log from
/// the step that passes `--no-capture`; #186 is about the rest of `just
/// test-all`. Run it by hand with
/// `cargo nextest run -p m68k --run-ignored all --no-capture all_instructions`.
#[test]
#[ignore = "whole-suite 68000 conformance gate; run with --run-ignored all"]
fn all_instructions() {
    let started = std::time::Instant::now();
    let reports = harness::run_all_files(GATE_CHECK);
    if reports.is_empty() {
        // No data root: `run_all_files` already printed `SKIPPED:` for the
        // suite directory, and `just check` has to stay green without it.
        return;
    }
    print_table(&reports);
    print_failures(&reports);
    print_totals(&reports);
    println!("gate run took {:.1}s", started.elapsed().as_secs_f64());

    let (unlisted, stale) = gate_diff(&reports);
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "conformance gate at {GATE_CHECK:?} disagrees with KNOWN_FAILURES and docs/m68k-conformance.md:\n \
         {unlisted:?} fail without an issue\n \
         {stale:?} are listed but no longer fail\n\
         update KNOWN_FAILURES and docs/m68k-conformance.md together."
    );
}
