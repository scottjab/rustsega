//! Harness for the SingleStepTests Z80 suite: parse one opcode file, run each
//! of its ~1000 cases through [`Z80::step`], and compare the whole machine
//! state afterwards.
//!
//! This is the piece every later instruction ticket reuses: implementing an
//! opcode group means adding a test that calls [`run_files`] with those opcode
//! file names, and watching it go green.
//!
//! Comparison is deliberately total. The suite checks internal state nobody
//! else can see (`wz`, `q`, `p`, the EI latch), so the harness compares those
//! too: an instruction that gets the visible registers right and the hidden
//! ones wrong would pass a weaker harness and still fail the conformance gate
//! in M2-17 (#60).
//!
//! What is *not* compared is the per-T-state bus trace in `cycles`; only its
//! length (the expected T-state count) is. Per-cycle comparison is out of scope
//! for this ticket.
//!
//! # Layout
//!
//! The suite is fetched by `scripts/fetch-test-data.sh` into
//! `$RUSTSEGA_TEST_ROM_DIR/z80/v1/`, one file per opcode, named after its opcode
//! bytes: `00.json` (NOP), `cb 00.json`, `dd cb __ 06.json`. Pass the name
//! without `.json`, exactly as it appears on disk.
//!
//! # Dead code
//!
//! Each integration-test binary compiles its own copy of this module and only
//! sees the items it uses, so an item used solely by a later ticket's test file
//! looks unused here. That is the point of a shared harness, so the lint is off
//! for the whole module rather than sprinkled through it.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::Deserialize;
use z80::{IntMode, Z80, Z80Bus};

/// Where the suite lives under `$RUSTSEGA_TEST_ROM_DIR`, for skip messages.
pub const SUITE_DIR: &str = "z80/v1";

/// Mismatch lines printed for one failing case before we stop that file.
///
/// A wrong opcode fails almost every case in its file in the same way, and a
/// wall of 1000 identical reports buries the other files' output.
const MAX_REPORTED_MISMATCHES: usize = 3;

/// One `[address, value]` pair from a test's `ram` array.
type RamByte = (u16, u8);

/// One test case: a starting state, one instruction, the state expected after
/// it, and the T-states it should have taken.
#[derive(Debug, Deserialize)]
pub struct TestCase {
    /// Human-readable label like `C5 0005` (opcode plus a case index).
    pub name: String,
    pub initial: CpuState,
    /// The state expected after the instruction, spelled `final` in the JSON.
    #[serde(rename = "final")]
    pub final_state: CpuState,
    /// Bus state sampled between T-states. Only `len()` is used here, so the
    /// individual states are left as unparsed values.
    pub cycles: Vec<serde_json::Value>,
    /// Expected I/O traffic. Absent unless the instruction touches a port.
    #[serde(default)]
    pub ports: Vec<PortOp>,
}

/// One entry of a test's `ports` array: `[port, value, "r"|"w"]`.
///
/// A tuple struct because JSON arrays map onto tuple structs, which keeps the
/// positional format from needing a custom deserializer.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PortOp(
    /// The full 16-bit I/O address: `A` in the high byte, `C` or the opcode
    /// operand in the low byte.
    pub u16,
    /// Value read out of the port, or written to it.
    pub u8,
    pub PortKind,
);

/// Direction of an entry in a test's `ports` array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum PortKind {
    /// `IN`: the bus must return this value.
    #[serde(rename = "r")]
    Read,
    /// `OUT`: the core must write this value to this port.
    #[serde(rename = "w")]
    Write,
}

/// Register file plus the sparse RAM contents, as one JSON `initial`/`final`
/// object.
///
/// Every field is required on purpose: a case that does not spell out the
/// hidden state would silently compare against whatever the harness guessed,
/// which is how a conformance suite stops meaning anything. If a future version
/// of the suite starts omitting fields, this should fail loudly.
#[derive(Debug, Deserialize)]
pub struct CpuState {
    pub pc: u16,
    pub sp: u16,
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    #[serde(rename = "af_")]
    pub af_shadow: u16,
    #[serde(rename = "bc_")]
    pub bc_shadow: u16,
    #[serde(rename = "de_")]
    pub de_shadow: u16,
    #[serde(rename = "hl_")]
    pub hl_shadow: u16,
    pub ix: u16,
    pub iy: u16,
    pub i: u8,
    pub r: u8,
    /// The hidden `WZ` register (MEMPTR).
    pub wz: u16,
    /// The EI delay latch. `ei` in the JSON, [`Z80::ei_pending`] in the core.
    pub ei: u8,
    /// Set when the last instruction was `LD A,I` or `LD A,R`.
    pub p: u8,
    /// Set when the last instruction left the flags alone.
    pub q: u8,
    pub iff1: u8,
    pub iff2: u8,
    pub im: u8,
    /// Sparse RAM: only the addresses that matter are listed.
    pub ram: Vec<RamByte>,
}

/// A single difference between what the core did and what the suite expects.
#[derive(Debug, Clone)]
pub struct Mismatch {
    /// Name to report the field under, e.g. `a`, `wz`, `ram[0x8123]`.
    pub field: String,
    pub expected: String,
    pub got: String,
}

impl Mismatch {
    /// The one-line form: `field: expected X, got Y`.
    pub fn line(&self) -> String {
        format!(
            "{}: expected {}, got {}",
            self.field, self.expected, self.got
        )
    }
}

/// The suite directory, or `None` when the data has not been fetched.
///
/// Checked as a directory rather than through [`testkit::test_rom`], which only
/// accepts files.
fn suite_dir() -> Option<PathBuf> {
    let dir = testkit::test_rom_dir().join(SUITE_DIR);
    dir.is_dir().then_some(dir)
}

/// Parse one opcode file. `None` means the suite is not fetched, which callers
/// turn into a skip; a missing or unreadable file inside a fetched suite is a
/// hard error, because that means the caller named an opcode that does not
/// exist.
pub fn load_file(name: &str) -> Option<Vec<TestCase>> {
    let path = suite_dir()?.join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is not readable: {e} (typo in the opcode name?)",
            path.display()
        )
    });
    match serde_json::from_str::<Vec<TestCase>>(&text) {
        Ok(cases) => Some(cases),
        Err(e) => panic!("{} does not parse: {e}", path.display()),
    }
}

/// Names of every opcode file in the suite, sorted, without `.json`. `None`
/// when the suite is not fetched.
pub fn all_files() -> Option<Vec<String>> {
    let dir = suite_dir()?;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let entry = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        let path = entry.path();
        if path.is_file()
            && path.extension().is_some_and(|ext| ext == "json")
            && let Some(stem) = path.file_stem()
        {
            names.push(stem.to_string_lossy().into_owned());
        }
    }
    // read_dir order is arbitrary; the summary is only diffable if it is not.
    names.sort();
    Some(names)
}

/// The bus the suite expects: 64 KiB of RAM with the case's sparse contents
/// preloaded, and the case's I/O traffic queued up and recorded.
///
/// `IN` hands back the queued value whether or not the core asked for the port
/// the suite expected, so a wrong port shows up as one clean I/O mismatch
/// instead of derailing every register comparison after it. An `IN` with
/// nothing queued gets `0xFF`, which is what a floating data bus reads as.
#[derive(Debug)]
pub struct TestBus {
    ram: Vec<u8>,
    pending_reads: VecDeque<(u16, u8)>,
    reads: Vec<(u16, u8)>,
    writes: Vec<(u16, u8)>,
}

impl TestBus {
    /// A bus with `ram` preloaded (everything else zero) and the reads from
    /// `ports` queued for `IN`.
    pub fn new(ram: &[RamByte], ports: &[PortOp]) -> Self {
        let mut bus = Self {
            ram: vec![0; 0x1_0000],
            pending_reads: ports
                .iter()
                .filter(|op| op.2 == PortKind::Read)
                .map(|op| (op.0, op.1))
                .collect(),
            reads: Vec::new(),
            writes: Vec::new(),
        };
        for &(addr, value) in ram {
            bus.ram[usize::from(addr)] = value;
        }
        bus
    }

    /// Byte at `addr` after the run, for comparing against `final.ram`.
    pub fn peek(&self, addr: u16) -> u8 {
        self.ram[usize::from(addr)]
    }

    /// Ports and values the core actually read from, in order.
    pub fn reads(&self) -> &[(u16, u8)] {
        &self.reads
    }

    /// Ports and values the core actually wrote to, in order.
    pub fn writes(&self) -> &[(u16, u8)] {
        &self.writes
    }
}

impl Z80Bus for TestBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.ram[usize::from(addr)]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.ram[usize::from(addr)] = value;
    }

    fn io_read(&mut self, port: u16) -> u8 {
        let value = match self.pending_reads.pop_front() {
            Some((_, value)) => value,
            None => 0xFF,
        };
        self.reads.push((port, value));
        value
    }

    fn io_write(&mut self, port: u16, value: u8) {
        self.writes.push((port, value));
    }
}

/// Load the initial state into a fresh core and bus, run one instruction, and
/// return every difference from `final_state`.
///
/// Empty means the case passed.
pub fn run_case(case: &TestCase) -> Vec<Mismatch> {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new(&case.initial.ram, &case.ports);
    apply(&case.initial, &mut cpu);

    let tstates = cpu.step(&mut bus);

    let mut mismatches = Vec::new();
    compare_state(&cpu, &case.final_state, &mut mismatches);
    compare_ram(&bus, &case.final_state.ram, &mut mismatches);
    compare_io(&case.ports, &bus, &mut mismatches);
    check_number(
        &mut mismatches,
        "tstates",
        case.cycles.len() as u64,
        u64::from(tstates),
    );
    mismatches
}

/// Push the register file into `cpu`.
fn apply(state: &CpuState, cpu: &mut Z80) {
    cpu.set_pc(state.pc);
    cpu.set_sp(state.sp);
    cpu.set_a(state.a);
    cpu.set_flags(state.f);
    cpu.set_b(state.b);
    cpu.set_c(state.c);
    cpu.set_d(state.d);
    cpu.set_e(state.e);
    cpu.set_h(state.h);
    cpu.set_l(state.l);
    cpu.set_af_shadow(state.af_shadow);
    cpu.set_bc_shadow(state.bc_shadow);
    cpu.set_de_shadow(state.de_shadow);
    cpu.set_hl_shadow(state.hl_shadow);
    cpu.set_ix(state.ix);
    cpu.set_iy(state.iy);
    cpu.set_i(state.i);
    cpu.set_r(state.r);
    cpu.set_wz(state.wz);
    cpu.set_iff1(state.iff1 != 0);
    cpu.set_iff2(state.iff2 != 0);
    cpu.set_im(IntMode::from_u8(state.im));
    cpu.set_ei_pending(state.ei != 0);
    cpu.set_p(state.p != 0);
    cpu.set_q(state.q);
}

/// Compare every register, visible and hidden.
fn compare_state(cpu: &Z80, want: &CpuState, out: &mut Vec<Mismatch>) {
    check_byte(out, "a", want.a, cpu.a());
    check_byte(out, "f", want.f, cpu.f());
    check_byte(out, "b", want.b, cpu.b());
    check_byte(out, "c", want.c, cpu.c());
    check_byte(out, "d", want.d, cpu.d());
    check_byte(out, "e", want.e, cpu.e());
    check_byte(out, "h", want.h, cpu.h());
    check_byte(out, "l", want.l, cpu.l());
    check_word(out, "af_", want.af_shadow, cpu.af_shadow());
    check_word(out, "bc_", want.bc_shadow, cpu.bc_shadow());
    check_word(out, "de_", want.de_shadow, cpu.de_shadow());
    check_word(out, "hl_", want.hl_shadow, cpu.hl_shadow());
    check_word(out, "ix", want.ix, cpu.ix());
    check_word(out, "iy", want.iy, cpu.iy());
    check_word(out, "sp", want.sp, cpu.sp());
    check_word(out, "pc", want.pc, cpu.pc());
    check_word(out, "wz", want.wz, cpu.wz());
    check_byte(out, "i", want.i, cpu.i());
    check_byte(out, "r", want.r, cpu.r());
    check_bit(out, "iff1", want.iff1, cpu.iff1());
    check_bit(out, "iff2", want.iff2, cpu.iff2());
    check_bit(out, "ei", want.ei, cpu.ei_pending());
    check_bit(out, "p", want.p, cpu.p());
    check_byte(out, "q", want.q, cpu.q());
    check_number(out, "im", u64::from(want.im), u64::from(cpu.im().as_u8()));
}

/// Compare the bytes the case says it cares about.
///
/// Only the listed addresses are checked, which is what the suite specifies,
/// but it means a stray write to an address the case never mentions goes
/// unnoticed. Anything that walks off into memory normally trips a register or
/// T-state difference as well, so this is a known hole rather than a suspected
/// one.
fn compare_ram(bus: &TestBus, want: &[RamByte], out: &mut Vec<Mismatch>) {
    for &(addr, value) in want {
        check_byte(out, &format!("ram[{addr:#06X}]"), value, bus.peek(addr));
    }
}

/// Compare the I/O traffic the core produced against the case's `ports`.
fn compare_io(want: &[PortOp], bus: &TestBus, out: &mut Vec<Mismatch>) {
    let reads: Vec<PortOp> = want
        .iter()
        .copied()
        .filter(|op| op.2 == PortKind::Read)
        .collect();
    let writes: Vec<PortOp> = want
        .iter()
        .copied()
        .filter(|op| op.2 == PortKind::Write)
        .collect();
    compare_port_ops(out, "io_read", &reads, bus.reads());
    compare_port_ops(out, "io_write", &writes, bus.writes());
}

/// Compare one direction of the port traffic: expected ops against the port and
/// value the core used, in order.
fn compare_port_ops(out: &mut Vec<Mismatch>, kind: &str, want: &[PortOp], got: &[(u16, u8)]) {
    for (i, op) in want.iter().enumerate() {
        match got.get(i) {
            Some(&(port, value)) => {
                check_word(out, &format!("{kind}[{i}].port"), op.0, port);
                check_byte(out, &format!("{kind}[{i}].value"), op.1, value);
            }
            None => out.push(mismatch(
                &format!("{kind}[{i}]"),
                format!("port {:#06X} value {:#04X}", op.0, op.1),
                "no access".to_string(),
            )),
        }
    }
    for (i, &(port, value)) in got.iter().enumerate().skip(want.len()) {
        out.push(mismatch(
            &format!("{kind}[{i}]"),
            "no access".to_string(),
            format!("port {:#06X} value {:#04X}", port, value),
        ));
    }
}

/// Record a mismatch unless two byte-sized things agree.
fn check_byte(out: &mut Vec<Mismatch>, field: &str, want: u8, got: u8) {
    if want != got {
        out.push(mismatch(
            field,
            format!("{want:#04X}"),
            format!("{got:#04X}"),
        ));
    }
}

/// Record a mismatch unless two word-sized things agree.
fn check_word(out: &mut Vec<Mismatch>, field: &str, want: u16, got: u16) {
    if want != got {
        out.push(mismatch(
            field,
            format!("{want:#06X}"),
            format!("{got:#06X}"),
        ));
    }
}

/// Record a mismatch unless two plain numbers agree.
fn check_number(out: &mut Vec<Mismatch>, field: &str, want: u64, got: u64) {
    if want != got {
        out.push(mismatch(field, want.to_string(), got.to_string()));
    }
}

/// Record a mismatch unless a suite bit matches a core `bool`.
fn check_bit(out: &mut Vec<Mismatch>, field: &str, want: u8, got: bool) {
    if (want != 0) != got {
        out.push(mismatch(field, (want != 0).to_string(), got.to_string()));
    }
}

/// One `field: expected, got` pair.
fn mismatch(field: &str, expected: String, got: String) -> Mismatch {
    Mismatch {
        field: field.to_string(),
        expected,
        got,
    }
}

/// The message out of a caught panic, for cases where the core panicked instead
/// of producing a wrong value.
fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = payload.downcast_ref::<String>() {
        msg.clone()
    } else if let Some(msg) = payload.downcast_ref::<&str>() {
        (*msg).to_string()
    } else {
        "panic with no message".to_string()
    }
}

/// How a file went: how many cases ran, and the first failure if there was one.
///
/// A failing file only ran up to its first bad case, so `passed + failed` is how
/// many were checked, not how many exist. The totals say so too, because
/// otherwise "3 cases failed" reads like the whole file was run.
struct FileReport {
    passed: usize,
    failed: usize,
    /// Label of the first failing case, with its mismatches.
    first_failure: Option<(String, Vec<Mismatch>)>,
}

/// Run one file, stopping at its first failing case so the log stays readable.
///
/// `run_case` is caught rather than allowed to unwind, because an unimplemented
/// opcode panics on purpose (that is how the decode skeleton reports a missing
/// instruction) and the sweep has to keep going past it to the other 1500 files.
/// The panic message becomes the mismatch line, so it reads the same in either
/// case. The default panic hook still writes the message to stderr.
fn run_one_file(cases: &[TestCase]) -> FileReport {
    let mut report = FileReport {
        passed: 0,
        failed: 0,
        first_failure: None,
    };
    for case in cases {
        let mismatches =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_case(case))) {
                Ok(mismatches) => mismatches,
                Err(payload) => vec![mismatch(
                    "panic",
                    "no panic".to_string(),
                    panic_text(&payload),
                )],
            };
        if mismatches.is_empty() {
            report.passed += 1;
            continue;
        }
        // One bad case is enough to say what is wrong with a file: a wrong
        // opcode fails its whole file the same way.
        report.failed += 1;
        let shown = mismatches
            .iter()
            .take(MAX_REPORTED_MISMATCHES)
            .cloned()
            .collect();
        report.first_failure = Some((case.name.clone(), shown));
        break;
    }
    report
}

impl FileReport {
    /// Print this file's line, plus the mismatch lines of its first failure.
    fn print(&self, name: &str, total: usize) {
        let checked = self.passed + self.failed;
        if self.failed == 0 {
            println!("{name}.json: PASS {}/{}", self.passed, total);
            return;
        }
        let (case, mismatches) = self
            .first_failure
            .as_ref()
            .unwrap_or_else(|| panic!("{} reported a failure with no case", name));
        println!("{name}.json: FAIL, first bad case {case} (checked {checked}/{total})");
        for mismatch in mismatches {
            println!("  {}", mismatch.line());
        }
    }
}

/// Run every test in each named opcode file, then fail with a summary if any
/// of the files had a failing case.
///
/// Skips cleanly when the suite has not been fetched, so a checkout without
/// `just fetch` stays green instead of failing every CPU test.
///
/// A failing file stops at its first bad case (reporting up to
/// [`MAX_REPORTED_MISMATCHES`] mismatches) and the next file is attempted, so
/// one unimplemented opcode group cannot hide the state of the others.
pub fn run_files(names: &[&str]) {
    if suite_dir().is_none() {
        eprintln!("SKIPPED: missing test data: Z80 SingleStepTests ({SUITE_DIR})");
        return;
    }
    let mut failed: Vec<&str> = Vec::new();
    let mut passed_cases = 0;
    for name in names {
        let cases = load_file(name).unwrap_or_else(|| unreachable!("suite_dir checked above"));
        let report = run_one_file(&cases);
        report.print(name, cases.len());
        passed_cases += report.passed;
        if report.failed > 0 {
            failed.push(name);
        }
    }
    assert!(
        failed.is_empty(),
        "{passed_cases} cases passed; {} of {} opcode files failed: {} \
         (each failing file ran only to its first bad case, and shows at most \
         {MAX_REPORTED_MISMATCHES} mismatches)",
        failed.len(),
        names.len(),
        failed.join(", "),
    );
}

/// Silence the default panic hook while the sweep walks into ~1600 unimplemented
/// opcodes on purpose. The message is already in the report line, and 1600
/// copies of it on stderr bury the rest of the log.
///
/// The hook is process-global, which is why only the sweep uses this and not
/// [`run_files`]: a test panicking in another thread while it is installed would
/// lose its message.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

struct QuietPanicHook(Option<PanicHook>);

impl QuietPanicHook {
    fn install() -> Self {
        let previous = std::panic::take_hook();
        // Replacing rather than just removing: with no hook at all, std falls
        // back to printing the panic itself.
        std::panic::set_hook(Box::new(|_| {}));
        Self(Some(previous))
    }
}

impl Drop for QuietPanicHook {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            std::panic::set_hook(previous);
        }
    }
}

/// Run every file in the suite and print one pass/fail line per file, plus a
/// grand total. Never asserts.
///
/// This is the shape M2-17 (#60) turns into a gate. Until the instructions are
/// implemented most files fail, so asserting here would only measure how much
/// work is left. A file whose opcode is not implemented yet counts as failed
/// with the panic's text as the reason, not as an error.
pub fn run_all_files() {
    let _quiet = QuietPanicHook::install();
    let Some(names) = all_files() else {
        eprintln!("SKIPPED: missing test data: Z80 SingleStepTests ({SUITE_DIR})");
        return;
    };
    let mut files_passed = 0;
    let mut cases_passed = 0;
    let mut cases_failed = 0;
    for name in &names {
        // One file at a time: the whole suite does not fit in memory at once
        // and there is no reason to keep a file around after reporting it.
        let cases = load_file(name).expect("suite_dir was present");
        let report = run_one_file(&cases);
        report.print(name, cases.len());
        cases_passed += report.passed;
        cases_failed += report.failed;
        if report.failed == 0 {
            files_passed += 1;
        }
    }
    println!(
        "Z80 SingleStepTests: {files_passed}/{} files pass; {} cases passed, {} \
         files failed (a failing file stops at its first bad case, so the case \
         count is checked cases, not all cases)",
        names.len(),
        cases_passed,
        cases_failed,
    );
}
