//! `zexdoc` / `zexall`: Frank Cringle's Z80 instruction exercisers, run as CP/M
//! programs (M2-16, #59).
//!
//! These are the second opinion the SingleStepTests cannot give. Where the JSON
//! suite pins one instruction from one starting state, a `.com` exerciser runs a
//! real program that happens to consist of nothing but instructions, computing a
//! 32-bit CRC of the machine state after each one and comparing it against
//! values taken from a real Z80. A core that passes both has been checked against
//! hardware state it was never tuned on.
//!
//! `zexdoc` covers the documented instruction set; `zexall` adds the
//! undocumented half, which is most of what this crate went to the trouble of
//! modelling (`wz`, `q`, the `X`/`Y` flag latch, the `IXH`/`IXL` registers), so
//! it is the harder of the two by a wide margin and by a wide margin of runtime.
//!
//! Both are `#[ignore]`d: they are minutes, not milliseconds, and they are only
//! interesting as a whole. Run them with
//! `cargo nextest run -p z80 --run-ignored all zex`.

mod cpm;

/// A budget for `zexdoc`, measured rather than guessed: this core runs the whole
/// exerciser in 46_734_960_693 T-states (about a minute of wall clock on the
/// `test` profile), and 1.88 million test cases to get there. The budget is a
/// little over twice that, because an exerciser that cannot finish has to stop
/// and say so, and the last line it printed says where.
const ZEXDOC_TSTATE_LIMIT: u64 = 100_000_000_000;

/// The same for `zexall`. It is not the bigger number one would expect: the two
/// binaries differ in 193 bytes of masks and expected CRCs, not in how many
/// cases they enumerate, so `zexall` measured the same 46_734_960_693.
const ZEXALL_TSTATE_LIMIT: u64 = 100_000_000_000;

/// Run one exerciser and fail with its output if anything in it failed.
///
/// Every line is echoed as it arrives: an exerciser prints one line per test
/// group, and when a run goes wrong the group it died in is the whole
/// diagnostic. A run that fails says which group and what CRC it got; a run that
/// hits [`ZEXDOC_TSTATE_LIMIT`] says the same thing one level down.
fn run_exerciser(name: &str, limit: u64) {
    let Some(image) = cpm::load(name) else {
        eprintln!(
            "SKIPPED: missing test data: CP/M exercisers ({})",
            cpm::SUITE_DIR
        );
        return;
    };

    let output = cpm::Machine::new(&image).run(limit, |line| eprintln!("{name}| {line}"));

    let errors = output.errors();
    assert!(
        errors.is_empty(),
        "{name} reported {} error(s), last line {:?} after {} T-states:\n{}",
        errors.len(),
        output.lines.last(),
        output.tstates,
        errors.join("\n")
    );
    assert!(
        output.completed(),
        "{name} did not finish: {output:?} (last line {:?}, {} T-states)",
        output.lines.last(),
        output.tstates
    );
    eprintln!("{name}: OK in {} T-states", output.tstates);
}

/// The harness's own self-check, and the only one that runs in a normal test
/// run: a hand-assembled program that calls BDOS 9 to print `hi`, then BDOS 2 to
/// add `!`, then warm-boot.
///
/// Written in machine code because there is no assembler in the toolchain, and
/// annotated because the bytes are the point: `LD DE,string` / `LD C,9` /
/// `CALL 5` is all `zexdoc` ever does to talk to the host.
#[test]
fn bdos_print_string() {
    // The string goes past the code rather than at a round offset: 18 bytes of
    // program start at 0, and a literal at 0x10 would land inside `JP 0`.
    let string_at: u16 = 0x20;
    let addr = cpm::TPA + string_at;
    let code: &[u8] = &[
        0x11u8,
        addr as u8,
        (addr >> 8) as u8, // LD DE,string
        0x0E,
        9, //                             LD C,9
        0xCD,
        0x05,
        0x00, //                    CALL 5
        0x0E,
        2, //                             LD C,2
        0x1E,
        b'!', //                          LD E,'!'
        0xCD,
        0x05,
        0x00, //                    CALL 5
        0xC3,
        0x00,
        0x00, //                    JP 0 (warm boot)
    ];
    let mut image = vec![0u8; 0x40];
    image[..code.len()].copy_from_slice(code);
    image[string_at as usize..string_at as usize + 3].copy_from_slice(b"hi$");

    let output = cpm::run(&image, 1000);

    assert_eq!(
        output.text, "hi!",
        "the whole point of a BDOS, and this is it"
    );
    assert_eq!(output.exit, cpm::Exit::WarmBoot);
    assert!(output.tstates < 1000, "it should not have run long");
}

/// A program that asks for a BDOS call this harness does not have must say so,
/// not run on with a silently wrong answer.
#[test]
fn bdos_unknown_function_ends_the_run() {
    let mut image = vec![0u8; 4];
    image[0..2].copy_from_slice(&[0x0E, 6]); // LD C,6 (console input)
    image[2..4].copy_from_slice(&[0xCD, 0x05]); // CALL 5 - and stop

    let output = cpm::run(&image, 1000);

    assert_eq!(output.exit, cpm::Exit::UnknownBdos(6));
    assert!(output.text.is_empty());
}

/// And a program that never comes back has to be stopped and reported, which is
/// what keeps a hung exerciser a failure rather than a timeout.
#[test]
fn run_limit_reports_a_hang() {
    let mut image = vec![0u8; 0x20];
    image[0] = 0x10; // DJNZ -3: the shortest loop there is
    image[1] = 0xFE;

    let output = cpm::run(&image, 10_000);

    assert_eq!(output.exit, cpm::Exit::Limit);
    assert!(output.tstates >= 10_000);
}

/// `zexdoc`, the documented instruction set.
///
/// As of M2-16 this fails on one of its 67 groups, `ld <bcdexya>,<bcdexya>`,
/// because the `DD`/`FD`-prefixed `LD r,r` moves are not implemented and the
/// core panics on them mid-group: see #247. Everything else reports `OK`.
#[test]
#[ignore = "runs a whole CP/M exerciser; minutes, not milliseconds"]
fn zexdoc() {
    run_exerciser("zexdoc", ZEXDOC_TSTATE_LIMIT);
}

/// `zexall`, which adds the undocumented instructions.
#[test]
#[ignore = "runs a whole CP/M exerciser; minutes, not milliseconds"]
fn zexall() {
    run_exerciser("zexall", ZEXALL_TSTATE_LIMIT);
}
