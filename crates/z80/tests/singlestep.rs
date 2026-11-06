//! Runs the SingleStepTests Z80 suite against the core, one opcode file at a
//! time.
//!
//! These are the harness's own tests: it parses, it skips cleanly when the data
//! is absent, it proves the comparison works by passing on `NOP`, and it
//! notices when a case is deliberately wrong. Later instruction tickets add
//! their own files calling [`harness::run_files`] with the opcodes they cover.

#[path = "harness/mod.rs"]
mod harness;

/// The `NOP` file, used as the harness's self-check.
const NOP: &str = "00";

/// A one-instruction case with every field spelled out, which the tests below
/// then poke one register at a time.
///
/// Written as JSON rather than as a struct literal so the tests go through the
/// same parse path as the real suite: a field the harness stopped comparing
/// shows up as a difference here, not as a silent pass.
const BASE: &str = r#"{
    "name": "fake 0000",
    "initial": {
        "pc": 256, "sp": 8192,
        "a": 17, "f": 0, "b": 1, "c": 2, "d": 3, "e": 4, "h": 5, "l": 6,
        "af_": 16, "bc_": 32, "de_": 48, "hl_": 64,
        "ix": 4660, "iy": 4864, "i": 7, "r": 8,
        "wz": 4096, "ei": 0, "p": 0, "q": 0, "iff1": 0, "iff2": 0, "im": 0,
        "ram": [[256, 0]]
    },
    "final": {
        "pc": 257, "sp": 8192,
        "a": 17, "f": 0, "b": 1, "c": 2, "d": 3, "e": 4, "h": 5, "l": 6,
        "af_": 16, "bc_": 32, "de_": 48, "hl_": 64,
        "ix": 4660, "iy": 4864, "i": 7, "r": 9,
        "wz": 4096, "ei": 0, "p": 0, "q": 0, "iff1": 0, "iff2": 0, "im": 0,
        "ram": [[256, 0]]
    },
    "cycles": [[256, null, "----"], [256, 0, "r-m-"], [null, null, "----"], [null, null, "----"]]
}"#;

/// Parse [`BASE`]. `NOP`-shaped `step` leaves exactly this state behind, so an
/// unmodified case is the control for every test below.
fn base_case() -> harness::TestCase {
    serde_json::from_str(BASE).expect("the built-in fake case should parse")
}

/// Run a case and return the mismatch lines, which is all a test should have to
/// look at.
fn run(case: &harness::TestCase) -> Vec<String> {
    harness::run_case(case)
        .iter()
        .map(harness::Mismatch::line)
        .collect()
}

#[test]
fn harness_parses_nop_file() {
    let Some(cases) = harness::load_file(NOP) else {
        eprintln!(
            "SKIPPED: missing test data: Z80 SingleStepTests ({})",
            harness::SUITE_DIR
        );
        return;
    };
    assert!(!cases.is_empty(), "00.json held no tests");
    // The suite's own label format, which failure reports quote back.
    assert!(
        cases[0].name.starts_with("00"),
        "unexpected case name {:?}",
        cases[0].name
    );
}

#[test]
fn nop_passes() {
    // `step` is NOP today, so a green run here means the comparison itself is
    // right: a thousand random starting states agree on every field, including
    // the hidden ones.
    harness::run_files(&[NOP]);
}

#[test]
fn rotted_prefixes_pass() {
    // A `DD`/`FD` in front of an instruction that never mentions `HL` is a
    // 4 T-state NOP, so `DD 00` is 8 T-states of nothing. These two are the
    // only prefixed files the decode skeleton gets through (M2-03, #46), and
    // they are what pin the prefix R and PC accounting against hardware.
    harness::run_files(&["dd 00", "fd 00"]);
}

#[test]
fn harness_detects_mismatch() {
    let mut case = base_case();
    case.final_state.a = 0x22;
    assert_eq!(run(&case), vec!["a: expected 0x22, got 0x11"]);
}

#[test]
fn harness_detects_nothing_in_an_untouched_case() {
    // The control for the test above: if this were not clean, the single
    // mismatch it reports would prove nothing.
    assert_eq!(run(&base_case()), Vec::<String>::new());
}

#[test]
fn harness_detects_hidden_state_mismatch() {
    // The undocumented registers are the reason this harness exists, so a
    // difference in one of them has to be visible too.
    let mut case = base_case();
    case.final_state.wz = 0x1234;
    case.final_state.q = 1;
    assert_eq!(
        run(&case),
        vec![
            "wz: expected 0x1234, got 0x1000",
            "q: expected 0x01, got 0x00"
        ]
    );
}

#[test]
fn harness_detects_ram_mismatch() {
    let mut case = base_case();
    case.final_state.ram[0].1 = 0xFF;
    assert_eq!(run(&case), vec!["ram[0x0100]: expected 0xFF, got 0x00"]);
}

#[test]
fn harness_detects_missing_io_write() {
    let mut case = base_case();
    // An `OUT` the core never performed: the stub does no I/O at all. Port
    // 0x66FF is A=0x66 in the high byte, 0xFF in the low.
    case.ports
        .push(harness::PortOp(0x66FF, 0x66, harness::PortKind::Write));
    assert_eq!(
        run(&case),
        vec!["io_write[0]: expected port 0x66FF value 0x66, got no access"]
    );
}

#[test]
fn harness_detects_wrong_tstate_count() {
    let mut case = base_case();
    case.cycles.pop();
    assert_eq!(run(&case), vec!["tstates: expected 3, got 4"]);
}

/// The whole suite, one line per file.
///
/// `#[ignore]`d because most files fail until their opcodes exist, so it reports
/// instead of asserting; M2-17 (#60) is where it becomes a gate. Run it with
/// `cargo nextest run -p z80 --run-ignored all all_opcodes`, or via `just
/// test-all` on `main`.
#[test]
#[ignore = "reports the whole suite; most opcodes are not implemented yet"]
fn all_opcodes() {
    harness::run_all_files();
}
