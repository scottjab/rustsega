//! Harness for the SingleStepTests 68000 suite: parse one instruction file, run
//! each of its ~8000 cases through [`m68k::M68k::step`], and compare the machine
//! state afterwards.
//!
//! This is the piece every later m68k instruction ticket reuses. Turning on a
//! group means adding `crates/m68k/tests/<group>.rs` that calls
//! [`assert_file_passes`] with the file names that group covers — e.g.
//! `assert_file_passes("ADD.b", Check::StateSkipAddressErrors)` — and watching
//! it go green. Name is the file name without `.json` or `.json.gz`, exactly as
//! it appears in `ls $RUSTSEGA_TEST_ROM_DIR/680x0/68000/v1/`.
//!
//! # What is and is not compared
//!
//! Compared: every data and address register, both stored stack pointers, the
//! active one (`a[7]`), SR, PC, both prefetch words, and every byte listed in
//! the case's `final.ram`. Not compared: the per-access bus trace in the case's
//! `transactions` (explicitly out of scope for M1-03, and skipping it is what
//! keeps a 1.4 GB suite, 195 MB gzipped on disk, cheap to read). The clock
//! count (`length`) is compared only by the modes where [`Check::checks_cycles`] is set.
//!
//! # Layout
//!
//! `scripts/fetch-test-data.sh` puts the suite in
//! `$RUSTSEGA_TEST_ROM_DIR/680x0/68000/v1/`, one `.json.gz` per instruction and
//! size: `ADD.b.json.gz`, `MOVE.w.json.gz`, `ANDItoSR.json.gz`. Both `.json.gz`
//! and plain `.json` are read, so a future pin can drop the compression.
//!
//! The names are not one uniform grammar, so nothing here guesses at them:
//! `TAS`, `CHK`, `EXG`, `NOP` and friends carry no size suffix, byte-wide move
//! is `MOVE.q` rather than `MOVEQ`, and `Bcc`, `Scc` and `DBcc` are one file
//! each covering every condition. [`all_files`] enumerates the directory and
//! every file in the pinned suite parses with the same schema.
//!
//! Missing data is a skip, never a failure: `just check` has to be green with
//! or without `just fetch`. That extends to a file inside a fetched suite that
//! does not exist — the ticket asks for `SKIPPED: <file> not found` rather than
//! an error, which is deliberately more forgiving than the z80 harness, where a
//! misnamed opcode inside a fetched suite panics. Copy names from `ls`, not from
//! memory.
//!
//! # Reuse
//!
//! The suite is ~1 000 000 cases over 124 files, and [`m68k::testbus::TestBus`]
//! is 16 MiB (the whole 24 bit address space). Building one per case would zero
//! ~16 TB of memory over a sweep, so a [`Machine`] owns one CPU and one bus for
//! a whole file and zeroes only the bytes the previous case touched.
//!
//! # Dead code
//!
//! Each integration-test binary compiles its own copy of this module and only
//! sees the items it uses, so an item used solely by a later ticket's test file
//! looks unused here. That is the point of a shared harness, so the lint is off
//! for the whole module rather than sprinkled through it.

#![allow(dead_code)]

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use m68k::testbus::TestBus;
use m68k::{Bus, M68k, SR_S};
use serde::Deserialize;

/// Where the suite lives under `$RUSTSEGA_TEST_ROM_DIR`, for skip messages.
pub const SUITE_DIR: &str = "680x0/68000/v1";

/// Failing cases kept in a [`Report`] before it stops collecting. Enough to see
/// a pattern, short enough that one bad file does not bury the rest of a sweep.
const MAX_REPORTED_FAILURES: usize = 10;

/// Differences listed for one failing case. A wrong instruction differs in
/// almost every register, and the first few say what is wrong.
const MAX_REPORTED_MISMATCHES: usize = 5;

/// The suite's exception vector slot: the handler long at $00000C..=$00000F,
/// which a case that takes an exception fetches and jumps to.
const EXCEPTION_VECTOR: u32 = 0x0000_000C;

/// One `[address, value]` pair from a case's `ram` array. Address is a full
/// 24 bit 68000 address (`u32`, not the z80 suite's `u16`).
type RamByte = (u32, u8);

/// How strictly to compare a case.
///
/// The variant names are the API ticket #11 published for later tickets, so
/// they stay even though the common `State` prefix is repetitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names, reason = "published API of ticket #11")]
pub enum Check {
    /// Registers, SR, PC, prefetch and the listed RAM bytes; not the cycles.
    State,
    /// [`Check::State`] plus the clock count `step` returned against the case's
    /// `length`.
    StateAndCycles,
    /// [`Check::State`] with the exception cases left out, because they need
    /// exception handling the core does not have yet (address errors are
    /// M1-29, #37). Instruction tickets use this until then.
    StateSkipAddressErrors,
    /// [`Check::StateAndCycles`] with the exception cases left out: the two
    /// independent axes of the variants above, combined. Added by M1-31a (#39),
    /// whose cycle work lands on files that #223 has not yet moved to full
    /// state checking — a cycle ticket must not have to take exception
    /// handling with it, and a timing ticket must not weaken a file by dropping
    /// back to [`Check::StateSkipAddressErrors`].
    StateAndCyclesSkipAddressErrors,
}

impl Check {
    /// Whether this mode compares the clock count `step` returned against the
    /// case's `length`.
    fn checks_cycles(self) -> bool {
        matches!(
            self,
            Self::StateAndCycles | Self::StateAndCyclesSkipAddressErrors
        )
    }

    /// Whether this mode leaves the exception cases out (see
    /// [`is_address_error_case`]).
    fn skips_address_errors(self) -> bool {
        matches!(
            self,
            Self::StateSkipAddressErrors | Self::StateAndCyclesSkipAddressErrors
        )
    }
}

/// One test case: a starting state, one instruction, the state expected after
/// it, and the 68k clock count it should have taken.
#[derive(Debug, Deserialize)]
pub struct TestCase {
    /// Human-readable label like `30dd [MOVE.w (A5)+, (A0)+] 1802` (opcode,
    /// mnemonic, case index). Failure reports quote it back.
    pub name: String,
    pub initial: CpuState,
    /// The state expected after the instruction, spelled `final` in the JSON.
    #[serde(rename = "final")]
    pub final_state: CpuState,
    /// Clocks the instruction takes, in 68k clock cycles — four per bus cycle,
    /// so `NOP` is 4, `RTS` 16..58. Not bus cycles.
    pub length: u32,
    // `transactions` (the per-access bus trace) is deliberately not a field:
    // comparing it is out of scope, and leaving it undeclared means serde_json
    // discards it without building values, which matters over 1.4 GB of JSON.
}

/// Register file plus the sparse RAM contents, as one JSON `initial`/`final`
/// object. The field list is exactly the suite's: `d0`..`d7`, `a0`..`a6`,
/// `usp`, `ssp`, `sr`, `pc`, `prefetch`, `ram`.
///
/// Every field is required on purpose: a case that did not spell out a register
/// would silently compare against whatever the harness guessed, which is how a
/// conformance suite stops meaning anything. If a future version of the suite
/// starts omitting fields, this should fail loudly.
///
/// There is no `a7` field. `a[7]` is not stored separately on the 68000: it is
/// the SSP while SR.S is 1 and the USP while it is 0 (M68000UM, "Status
/// Register"), so [`CpuState::active_sp`] derives it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CpuState {
    pub d0: u32,
    pub d1: u32,
    pub d2: u32,
    pub d3: u32,
    pub d4: u32,
    pub d5: u32,
    pub d6: u32,
    pub d7: u32,
    pub a0: u32,
    pub a1: u32,
    pub a2: u32,
    pub a3: u32,
    pub a4: u32,
    pub a5: u32,
    pub a6: u32,
    /// The user stack pointer, stored while it is not the active pointer.
    pub usp: u32,
    /// The supervisor stack pointer, stored while it is not the active pointer.
    pub ssp: u32,
    /// Status register. It goes in through [`M68k::set_sr`], which masks with
    /// `SR_MASK`; a suite value carrying a bit outside that mask would come back
    /// as an `sr` mismatch in the sweep rather than being hidden by the mask.
    pub sr: u16,
    pub pc: u32,
    /// The prefetch queue, IRC then IRD — the pair the suite models and the
    /// core has to reproduce to pass (M1-04, #12).
    pub prefetch: [u16; 2],
    /// Sparse RAM: only the addresses the case cares about are listed, and they
    /// are listed byte by byte, so odd addresses are fine.
    pub ram: Vec<RamByte>,
}

impl CpuState {
    /// The stack pointer `a[7]` holds in this state: the SSP while SR.S is 1,
    /// the USP while it is 0 (M68000UM, "Status Register").
    pub fn active_sp(&self) -> u32 {
        if self.sr & SR_S != 0 {
            self.ssp
        } else {
            self.usp
        }
    }
}

/// A single difference between what the core did and what the suite expects.
#[derive(Debug, Clone)]
pub struct Mismatch {
    /// Name to report the field under, e.g. `d3`, `prefetch[1]`,
    /// `ram[0x00000C04]`.
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

/// The line printed whenever data is missing, so a skip reads the same
/// everywhere: `SKIPPED: <file> not found`.
pub fn skip_line(name: &str) -> String {
    format!("SKIPPED: {name} not found")
}

/// The data root: `$RUSTSEGA_TEST_ROM_DIR`, else `<workspace>/test-roms`.
///
/// From `testkit`, which is also what resolves it for every other suite, so the
/// two CPU harnesses cannot disagree about where the data is.
fn data_root() -> PathBuf {
    testkit::test_rom_dir()
}

/// The suite directory under an explicit data root, or `None` when it is not
/// there. The root is a parameter rather than read from the environment so that
/// the harness's own tests can cover a missing or synthetic suite;
/// `std::env::set_var` is `unsafe` in edition 2024 and the workspace forbids
/// `unsafe`. Same reason `testkit::test_rom_in` exists.
pub fn suite_dir_in(base: &Path) -> Option<PathBuf> {
    let dir = base.join(SUITE_DIR);
    dir.is_dir().then_some(dir)
}

/// [`suite_dir_in`] against the real data root.
pub fn suite_dir() -> Option<PathBuf> {
    suite_dir_in(&data_root())
}

/// The instruction a suite file is for: `MOVE.w.json.gz` and `MOVE.w.json` both
/// give `MOVE.w`, and anything else (the suite ships a `README.md`) gives
/// `None`.
fn instruction_name(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let name = match path.extension().and_then(|ext| ext.to_str())? {
        // `.json.gz`: the stem is `MOVE.w.json`, so the `.json` goes too.
        "gz" => stem.strip_suffix(".json")?,
        // `.json`: the stem is already the instruction name.
        "json" => stem,
        _ => return None,
    };
    Some(name.to_string())
}

/// Names of every instruction file in the suite under `base`, sorted, without
/// their extensions. `None` when the suite is not fetched.
pub fn all_files_in(base: &Path) -> Option<Vec<String>> {
    let dir = suite_dir_in(base)?;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let entry = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        if let Some(name) = instruction_name(&entry.path()) {
            names.push(name);
        }
    }
    // `read_dir` order is arbitrary; the sweep is only diffable if the report
    // is not.
    names.sort();
    Some(names)
}

/// [`all_files_in`] against the real data root.
pub fn all_files() -> Option<Vec<String>> {
    all_files_in(&data_root())
}

/// The on-disk file for one instruction name, gzip preferred. `None` means this
/// suite has no such file, which every caller turns into a skip.
fn file_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let gzipped = dir.join(format!("{name}.json.gz"));
    if gzipped.is_file() {
        return Some(gzipped);
    }
    let plain = dir.join(format!("{name}.json"));
    plain.is_file().then_some(plain)
}

/// Read one suite file, inflating it when it is gzipped.
fn read_text(path: &Path) -> String {
    let file = File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut reader = BufReader::new(file);
    let mut text = String::new();
    if path.extension().is_some_and(|ext| ext == "gz") {
        GzDecoder::new(&mut reader)
            .read_to_string(&mut text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    } else {
        reader
            .read_to_string(&mut text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
    text
}

/// Parse one instruction file from an explicit data root. `None` means the suite
/// or that file is not there — a skip, per ticket #11. A file that is there but
/// unreadable or unparseable is a hard error, because that is a broken pin or a
/// schema change and a skip would hide it.
pub fn load_file_in(base: &Path, name: &str) -> Option<Vec<TestCase>> {
    let path = file_path(&suite_dir_in(base)?, name)?;
    let text = read_text(&path);
    match serde_json::from_str::<Vec<TestCase>>(&text) {
        Ok(cases) => Some(cases),
        Err(e) => panic!("{} does not parse: {e}", path.display()),
    }
}

/// [`load_file_in`] against the real data root.
pub fn load_file(name: &str) -> Option<Vec<TestCase>> {
    load_file_in(&data_root(), name)
}

/// One CPU and one bus, kept alive across a whole file so the 16 MiB bus is
/// built once per file rather than once per case.
#[derive(Debug)]
pub struct Machine {
    cpu: M68k,
    bus: TestBus,
    /// Bytes poked into `bus` for the case that just ran, so the next case can
    /// zero exactly those instead of reallocating or clearing 16 MiB.
    touched: Vec<u32>,
}

impl Machine {
    /// A fresh CPU and an empty [`TestBus`].
    pub fn new() -> Self {
        Self {
            cpu: M68k::new(),
            bus: TestBus::new(),
            touched: Vec::new(),
        }
    }

    /// Load `case`'s initial state, run exactly one `step`, and return every
    /// difference from its final state. Empty means the case passed.
    pub fn run(&mut self, case: &TestCase, check: Check) -> Vec<Mismatch> {
        self.clear();
        // `M68k::new`, not `M68k::reset`: the suite spells out the whole machine
        // state, including the PC and stack pointers, so there is nothing for a
        // reset to fetch. Resetting would also charge the 132 + 16 clocks the
        // reset sequence takes (see `RESET_CYCLES`), which no case expects.
        self.cpu = M68k::new();
        apply(&case.initial, &mut self.cpu);
        self.poke(&case.initial.ram);

        let outcome = {
            let cpu = &mut self.cpu;
            let bus = &mut self.bus;
            // Caught rather than allowed to unwind: an unimplemented instruction
            // panics on purpose, and a sweep has to get past it to the next case
            // instead of dying 40 files in.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cpu.step(bus)))
        };

        let mut mismatches = Vec::new();
        compare_state(&self.cpu, &case.final_state, &mut mismatches);
        compare_ram(&mut self.bus, &case.final_state.ram, &mut mismatches);
        match outcome {
            Ok(cycles) => {
                if check.checks_cycles() {
                    check_number(
                        &mut mismatches,
                        "cycles",
                        u64::from(case.length),
                        u64::from(cycles),
                    );
                }
            }
            Err(payload) => {
                mismatches.push(mismatch("panic", "none".to_string(), panic_text(&payload)))
            }
        }

        // The bytes a correct instruction is expected to write are the ones a
        // wrong instruction will have written, so clear those too.
        self.touched
            .extend(case.final_state.ram.iter().map(|&(addr, _)| addr));
        mismatches
    }

    /// Poke the case's sparse RAM in, remembering what was touched.
    fn poke(&mut self, ram: &[RamByte]) {
        for &(addr, value) in ram {
            self.bus.write_byte(addr, value);
            self.touched.push(addr);
        }
    }

    /// Zero the bytes the previous case touched, so cases cannot see each
    /// other's memory.
    ///
    /// Only the touched bytes need clearing: the suite lists in `initial.ram`
    /// every byte its model reads, so a byte no case poked is a byte no case
    /// can read. Repeated addresses in `touched` just write zero twice.
    fn clear(&mut self) {
        for addr in self.touched.iter().copied() {
            self.bus.write_byte(addr, 0);
        }
        self.touched.clear();
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

/// Push the suite's state into a CPU.
///
/// SR goes in first, through [`M68k::set_sr`], while both stack pointers are
/// still zero: `set_sr` swaps them when SR.S changes, and doing that swap after
/// loading `a[7]` would move the loaded value into the wrong slot.
pub fn apply(state: &CpuState, cpu: &mut M68k) {
    cpu.set_sr(state.sr);
    cpu.d = [
        state.d0, state.d1, state.d2, state.d3, state.d4, state.d5, state.d6, state.d7,
    ];
    // `a[7]` last: it is the pointer SR.S selects, derived from the two stored
    // ones, and nothing below can move it now.
    cpu.a = [
        state.a0,
        state.a1,
        state.a2,
        state.a3,
        state.a4,
        state.a5,
        state.a6,
        state.active_sp(),
    ];
    cpu.usp = state.usp;
    cpu.ssp = state.ssp;
    cpu.pc = state.pc;
    cpu.prefetch = state.prefetch;
}

/// Read a CPU back out into a [`CpuState`], the inverse of [`apply`].
///
/// `ram` comes back empty because RAM belongs to the bus, not the CPU, and
/// `a[7]` is not a field at all: the round trip that uses this asserts `a[7]`
/// against [`CpuState::active_sp`] separately.
pub fn snapshot(cpu: &M68k) -> CpuState {
    CpuState {
        d0: cpu.d[0],
        d1: cpu.d[1],
        d2: cpu.d[2],
        d3: cpu.d[3],
        d4: cpu.d[4],
        d5: cpu.d[5],
        d6: cpu.d[6],
        d7: cpu.d[7],
        a0: cpu.a[0],
        a1: cpu.a[1],
        a2: cpu.a[2],
        a3: cpu.a[3],
        a4: cpu.a[4],
        a5: cpu.a[5],
        a6: cpu.a[6],
        usp: cpu.usp,
        ssp: cpu.ssp,
        sr: cpu.sr,
        pc: cpu.pc,
        prefetch: cpu.prefetch,
        ram: Vec::new(),
    }
}

/// Compare every register, both stack pointers, SR, PC and the prefetch queue.
fn compare_state(cpu: &M68k, want: &CpuState, out: &mut Vec<Mismatch>) {
    let d = [
        want.d0, want.d1, want.d2, want.d3, want.d4, want.d5, want.d6, want.d7,
    ];
    for (i, &want) in d.iter().enumerate() {
        check_long(out, &format!("d{i}"), want, cpu.d[i]);
    }
    let a = [
        want.a0, want.a1, want.a2, want.a3, want.a4, want.a5, want.a6,
    ];
    for (i, &want) in a.iter().enumerate() {
        check_long(out, &format!("a{i}"), want, cpu.a[i]);
    }
    // `a[7]` is reported as its own field even though it is derived: when it
    // disagrees with the stored pointer that just got swapped, that is the bug
    // worth naming.
    check_long(out, "a7", want.active_sp(), cpu.a[7]);
    check_long(out, "usp", want.usp, cpu.usp);
    check_long(out, "ssp", want.ssp, cpu.ssp);
    check_word(out, "sr", want.sr, cpu.sr);
    check_long(out, "pc", want.pc, cpu.pc);
    for (i, &want) in want.prefetch.iter().enumerate() {
        check_word(out, &format!("prefetch[{i}]"), want, cpu.prefetch[i]);
    }
}

/// Compare the bytes the case says it cares about.
///
/// Only the listed addresses are checked, which is what the suite specifies, so
/// a stray write to an address the case never mentions goes unnoticed. Anything
/// that walks off into memory normally trips a register or cycle difference as
/// well, so this is a known hole rather than a suspected one.
fn compare_ram(bus: &mut TestBus, want: &[RamByte], out: &mut Vec<Mismatch>) {
    for &(addr, value) in want {
        check_byte(
            out,
            &format!("ram[{addr:#010X}]"),
            value,
            bus.read_byte(addr),
        );
    }
}

/// True when `case` takes an exception rather than running its instruction, and
/// so needs exception handling the core does not have yet (M1-29, #37).
///
/// The suite spells it out: the handler long is present in `initial.ram` at
/// $00000C..=$00000F and `final.pc` is that long. Measured over the whole pinned
/// suite with this harness (`all_instructions`): 178 087 of 1 000 060 cases, over
/// 62 of the 124 files, and every one of those 62 files reaches `final.pc`
/// $00001400, the single handler the suite installs in its vectors.
///
/// One thing to know when reading the name: the vector caught here is the one at
/// $00000C, which on the 68000 is *illegal instruction*; address error is the
/// long at $000008 (M68000UM, "Exception Vector Memory Map"). So what this
/// skips is "the case ends in the suite's exception handler", which is not
/// exactly address errors. Issue #185 carries the decision that owes M1-29 (#37).
pub fn is_address_error_case(case: &TestCase) -> bool {
    let mut bytes = [None; 4];
    for (i, slot) in bytes.iter_mut().enumerate() {
        let addr = EXCEPTION_VECTOR + i as u32;
        if let Some(&(_, value)) = case.initial.ram.iter().find(|&&(a, _)| a == addr) {
            *slot = Some(value);
        }
    }
    let [Some(b0), Some(b1), Some(b2), Some(b3)] = bytes else {
        return false;
    };
    case.final_state.pc == u32::from_be_bytes([b0, b1, b2, b3])
}

/// How one file went.
#[derive(Debug)]
pub struct Report {
    /// Cases in the file, including any skipped.
    pub total: usize,
    /// Cases whose state matched. With [`Check::StateSkipAddressErrors`] a clean
    /// file has `passed + skipped == total`.
    pub passed: usize,
    /// Cases left out by [`Check::StateSkipAddressErrors`].
    pub skipped: usize,
    /// The first [`MAX_REPORTED_FAILURES`] failures, one readable line each.
    /// Empty means every case that ran passed.
    pub failures: Vec<String>,
}

impl Report {
    /// Cases that ran and did not match: `total - passed - skipped`.
    ///
    /// Kept as a subtraction rather than a fourth counter so the three
    /// invariants a reader checks by hand (`passed + failed + skipped == total`)
    /// cannot drift apart from each other.
    pub fn failed(&self) -> usize {
        self.total - self.passed - self.skipped
    }

    /// Print this file's line, plus the lines of the failures it kept.
    fn print(&self, name: &str) {
        let skipped = if self.skipped == 0 {
            String::new()
        } else {
            format!(" ({} skipped)", self.skipped)
        };
        if self.failures.is_empty() {
            println!("{name}: PASS {}/{}{skipped}", self.passed, self.total);
            return;
        }
        println!("{name}: FAIL {}/{}{skipped}", self.passed, self.total);
        for failure in &self.failures {
            println!("  {failure}");
        }
    }
}

/// Run every case in `cases` on a reusable [`Machine`].
fn run_cases_on(machine: &mut Machine, cases: &[TestCase], check: Check) -> Report {
    let mut report = Report {
        total: cases.len(),
        passed: 0,
        skipped: 0,
        failures: Vec::new(),
    };
    for case in cases {
        if check.skips_address_errors() && is_address_error_case(case) {
            report.skipped += 1;
            continue;
        }
        let mismatches = machine.run(case, check);
        if mismatches.is_empty() {
            report.passed += 1;
            continue;
        }
        if report.failures.len() < MAX_REPORTED_FAILURES {
            report.failures.push(failure_line(case, &mismatches));
        }
    }
    report
}

/// One failing case on one line: `case "ADD.b 1234": d3: expected ..., got ...;
/// sr: expected ..., got ...`.
fn failure_line(case: &TestCase, mismatches: &[Mismatch]) -> String {
    let shown: Vec<String> = mismatches
        .iter()
        .take(MAX_REPORTED_MISMATCHES)
        .map(Mismatch::line)
        .collect();
    let extra = mismatches.len().saturating_sub(MAX_REPORTED_MISMATCHES);
    let joined = shown.join("; ");
    if extra == 0 {
        format!("case {:?}: {joined}", case.name)
    } else {
        format!("case {:?}: {joined}; +{extra} more", case.name)
    }
}

/// Run every case in one file of an explicit data root. `None` when the suite or
/// that file is missing, which callers turn into a skip.
pub fn run_file_in(base: &Path, name: &str, check: Check) -> Option<Report> {
    let cases = load_file_in(base, name)?;
    Some(run_cases_on(&mut Machine::new(), &cases, check))
}

/// [`run_file_in`] against the real data root.
pub fn run_file(name: &str, check: Check) -> Option<Report> {
    run_file_in(&data_root(), name, check)
}

/// Run the cases in `cases` and report, without touching the filesystem. Public
/// for the harness's own tests, which build cases by hand.
pub fn run_cases(cases: &[TestCase], check: Check) -> Report {
    run_cases_on(&mut Machine::new(), cases, check)
}

/// Run every instruction file in the suite under `base` and return
/// `(name, report)` for each, sorted by name. Empty when the suite is not
/// fetched.
///
/// Neither printing nor asserting is this function's job: the sweep in
/// `singlestep.rs` (M1-32, #42) owns both, and a report here would duplicate the
/// table that gate prints.
pub fn run_all_files_in(base: &Path, check: Check) -> Vec<(String, Report)> {
    let Some(names) = all_files_in(base) else {
        eprintln!("{}", skip_line(SUITE_DIR));
        return Vec::new();
    };
    // A case whose instruction has no handler panics on purpose, and the panic
    // text is already in the report line; a whole sweep's worth of default panic
    // output would bury the rest of the log.
    let _quiet = QuietPanicHook::install();
    // One machine for the whole sweep: the bus is 16 MiB and the files are 124.
    let mut machine = Machine::new();
    let mut reports = Vec::with_capacity(names.len());
    for name in names {
        let cases = load_file_in(base, &name).expect("the suite was listed");
        reports.push((name, run_cases_on(&mut machine, &cases, check)));
    }
    reports
}

/// [`run_all_files_in`] against the real data root.
pub fn run_all_files(check: Check) -> Vec<(String, Report)> {
    run_all_files_in(&data_root(), check)
}

/// Run one file of the real suite and fail with the kept failures if any case
/// of it failed. Skips (prints [`skip_line`]) when the data is missing.
///
/// This is what an instruction ticket's own test calls.
pub fn assert_file_passes(name: &str, check: Check) {
    let Some(report) = run_file(name, check) else {
        eprintln!("{}", skip_line(name));
        return;
    };
    report.print(name);
    assert!(
        report.failures.is_empty(),
        "{name}: {}/{} cases passed, {} skipped; first {} failing cases \
         (up to {} differences each, and a file's later failures are not kept):\n{}",
        report.passed,
        report.total,
        report.skipped,
        report.failures.len(),
        MAX_REPORTED_MISMATCHES,
        report.failures.join("\n"),
    );
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

/// Record a mismatch unless two long-sized things agree.
fn check_long(out: &mut Vec<Mismatch>, field: &str, want: u32, got: u32) {
    if want != got {
        out.push(mismatch(
            field,
            format!("{want:#010X}"),
            format!("{got:#010X}"),
        ));
    }
}

/// Record a mismatch unless two plain numbers agree.
fn check_number(out: &mut Vec<Mismatch>, field: &str, want: u64, got: u64) {
    if want != got {
        out.push(mismatch(field, want.to_string(), got.to_string()));
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

/// Silence the default panic hook while a sweep walks into thousands of
/// unimplemented instructions on purpose.
///
/// The hook is process-global, which is why only the sweep installs this and not
/// [`run_file_in`]: a test panicking in another thread while it is installed
/// would lose its message.
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
