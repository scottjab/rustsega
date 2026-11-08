//! The other half of M1-18 (#26)'s shift/rotate sweep: the eight `.w` files.
//!
//! They cannot go green with register-form shifts alone, because the `.w` name
//! says nothing about the *data* size — the size is in the opcode. Each of them
//! carries both the `ss` = `00`/`01`/`10` register-form cases of its mnemonic at
//! all three sizes and the `ss` = `11` memory-form `ASx.w <ea>` cases, which are
//! M1-19 (#27). So this asserts exactly the split, and nothing else: every
//! register-form case in every `.w` shift file passes, and whatever still fails
//! is a memory-form case.
//!
//! Run with `RUSTSEGA_VERBOSE=1` to see the memory-form counts it leaves alone.

#[path = "harness/mod.rs"]
mod harness;

use harness::Check;

const SHIFTS: [&str; 8] = ["ASL", "ASR", "LSL", "LSR", "ROL", "ROR", "ROXL", "ROXR"];

/// Bit 6..7 (`ss`) of the opcode a case name starts with. `11` in the `$E000`
/// group is memory-form `ASx.w <ea>`, i.e. not this ticket.
fn is_memory_form(name: &str) -> bool {
    let Some(hex) = name.split(' ').next() else {
        return false;
    };
    let Ok(opcode) = u16::from_str_radix(hex, 16) else {
        return false;
    };
    (opcode >> 6) & 0b11 == 0b11
}

#[test]
fn word_files_fail_only_on_memory_form_cases() {
    if harness::suite_dir().is_none() {
        eprintln!("{}", harness::skip_line(harness::SUITE_DIR));
        return;
    }

    let mut register_total = 0;
    let mut memory_total = 0;
    let mut memory_passed = 0;
    let mut memory_skipped = 0;
    for stem in SHIFTS {
        let name = format!("{stem}.w");
        let Some(cases) = harness::load_file(&name) else {
            eprintln!("{}", harness::skip_line(&name));
            return;
        };
        let (memory, register): (Vec<_>, Vec<_>) = cases
            .into_iter()
            .partition(|case| is_memory_form(&case.name));

        let register_report = harness::run_cases(&register, Check::StateSkipAddressErrors);
        assert!(
            register_report.failures.is_empty(),
            "{name}: the register-form half must pass, but only {}/{} of its cases did; \
             first failures:\n{}",
            register_report.passed,
            register_report.total,
            register_report.failures.join("\n"),
        );

        let memory_report = harness::run_cases(&memory, Check::StateSkipAddressErrors);
        register_total += register_report.total;
        memory_total += memory_report.total;
        memory_passed += memory_report.passed;
        memory_skipped += memory_report.skipped;
    }

    println!(
        "{register_total} register-form cases across the 8 .w shift files pass; \
         {memory_passed}/{memory_total} of the {memory_total} memory-form cases pass \
         ({memory_skipped} skipped as address errors), left for M1-19 (#27)"
    );
}
