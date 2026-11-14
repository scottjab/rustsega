//! A CP/M 2 console thin enough to boot a `.com` file and thin enough to read.
//!
//! This exists for one thing: running Frank Cringle's `zexdoc`/`zexall`
//! instruction exercisers (M2-16, #59) against the core. They are CP/M programs,
//! so running them means playing the part of the operating system, which is a
//! surprisingly small part. A `.com` is loaded at [`TPA`] and jumped to; it
//! prints through BDOS at `0x0005`; and it stops by jumping to the warm-boot
//! vector at `0x0000`.
//!
//! The BDOS side is three functions, and only the two the exercisers use are
//! implemented: function 2 writes the character in `E`, function 9 writes the
//! `$`-terminated string at `DE`. Everything else ends the program, because a
//! program that asks for something else has left the ground this harness covers
//! and its output can no longer be trusted.
//!
//! # The two bytes that are not obvious
//!
//! `0x0005` holds a `RET`, so a `CALL 5` can be answered by running BDOS here
//! and then letting the return happen normally: the exercisers push and pop
//! their own registers around `CALL 5`, and a harness that skipped the `RET`
//! would corrupt the stack under them.
//!
//! `0x0006` holds the top of the stack, little-endian. Real CP/M keeps the
//! "memory top" address there and `zexdoc` starts with `LD HL,(6)` / `LD SP,HL`
//! (see `start:` in `zexdoc.src`), so a harness that only set `SP` itself would
//! have it thrown away by the program's own first instruction.

#![allow(dead_code)]

use std::path::PathBuf;

use z80::{Z80, Z80Bus};

/// Where the fetched exercisers live under `$RUSTSEGA_TEST_ROM_DIR`.
pub const SUITE_DIR: &str = "z80/zexall-cpm";

/// CP/M's transient program area: where a `.com` is loaded and where execution
/// starts.
pub const TPA: u16 = 0x0100;

/// BDOS entry point. `0x0005` holds a `RET`; see the module docs.
const BDOS: u16 = 0x0005;

/// Warm boot. A CP/M program that has finished jumps here, and no CP/M program
/// is ever fetched from here, so reaching it means the program is over.
const WARM_BOOT: u16 = 0x0000;

/// The stack top written at `0x0006`, and the initial `SP`.
///
/// Not CP/M's 23 KiB default of `0x5C00`: `zexall` runs its own machine states
/// through the stack, and the exercisers only need one that is far above their
/// own code (the larger of the two, at 8587 bytes, ends at `0x228B`).
pub const STACK_TOP: u16 = 0xF000;

/// Why a run stopped.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The program reached `0x0000`, so it finished.
    WarmBoot,
    /// The T-state budget ran out first. Always a failure: it means the program
    /// is looping, and the last line of [`Output::lines`] says where.
    Limit,
    /// The program called a BDOS function this harness does not implement.
    UnknownBdos(u8),
    /// BDOS 9 was handed a string with no `$` in the whole address space.
    /// Only a broken or crashed program does this, and printing memory for
    /// ever is not an answer to it.
    UnterminatedString,
}

/// What a run produced.
#[derive(Debug)]
pub struct Output {
    /// Everything the program wrote, as one string.
    pub text: String,
    /// The same split into lines, in order, without their terminators.
    pub lines: Vec<String>,
    /// Why the run stopped.
    pub exit: Exit,
    /// T-states executed.
    pub tstates: u64,
}

impl Output {
    /// Did the exercisers say they were done?
    pub fn completed(&self) -> bool {
        self.exit == Exit::WarmBoot && self.text.contains("Tests complete")
    }

    /// The `ERROR` lines, which is how a CP/M exerciser names what failed.
    pub fn errors(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(String::as_str)
            .filter(|line| line.contains("ERROR"))
            .collect()
    }
}

/// The 64 KiB a CP/M machine has, with the I/O space going nowhere.
struct Ram {
    ram: Vec<u8>,
}

impl Ram {
    fn new() -> Self {
        Self {
            ram: vec![0; 0x1_0000],
        }
    }
}

impl Z80Bus for Ram {
    fn read(&mut self, addr: u16) -> u8 {
        self.ram[usize::from(addr)]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.ram[usize::from(addr)] = value;
    }

    /// The exercisers do not touch a port; the `zexall` undocumented-instruction
    /// group runs `IN r,(C)`, and a floating bus reads high.
    fn io_read(&mut self, _port: u16) -> u8 {
        0xFF
    }

    fn io_write(&mut self, _port: u16, _value: u8) {}
}

/// A CP/M machine: the CPU, its memory, and the console being written to.
pub struct Machine {
    cpu: Z80,
    bus: Ram,
    out: Vec<u8>,
}

impl Machine {
    /// A machine with `program` loaded at [`TPA`], ready to run it.
    pub fn new(program: &[u8]) -> Self {
        let mut machine = Self {
            cpu: Z80::new(),
            bus: Ram::new(),
            out: Vec::new(),
        };
        assert!(
            usize::from(TPA) + program.len() <= 0x1_0000,
            "the program does not fit in the TPA"
        );
        for (offset, byte) in program.iter().enumerate() {
            machine.bus.ram[usize::from(TPA) + offset] = *byte;
        }
        machine.bus.ram[usize::from(BDOS)] = 0xC9;
        machine.bus.ram[usize::from(WARM_BOOT)] = 0xC9;
        machine.bus.ram[6] = STACK_TOP as u8;
        machine.bus.ram[7] = (STACK_TOP >> 8) as u8;
        machine.cpu.set_pc(TPA);
        machine.cpu.set_sp(STACK_TOP);
        machine
    }

    /// Run until the program stops or `limit` T-states go by, calling `on_line`
    /// for each line of console output as it completes.
    ///
    /// The callback is what makes a long run legible: an exerciser prints a line
    /// per test group, so the last line out is the group it is in, which is the
    /// difference between "it is still running" and "it is stuck in `bitx`".
    pub fn run(&mut self, limit: u64, mut on_line: impl FnMut(&str)) -> Output {
        let mut line = Vec::new();
        let mut written = 0;
        let mut tstates = 0;
        // A handled call leaves `PC` on the `RET` at `0x0005`, not past it, and
        // the next iteration would see `PC == BDOS` again and answer the same
        // call for ever. So a call is answered once per arrival at `0x0005`,
        // and the `RET` itself is left to the core: the program pushed its own
        // return address, and only the CPU's own `POP` can hand it back.
        let mut answer = true;
        let exit = loop {
            match self.cpu.pc() {
                WARM_BOOT => break Exit::WarmBoot,
                BDOS if answer => match self.bdos() {
                    Ok(()) => answer = false,
                    Err(exit) => break exit,
                },
                _ => answer = true,
            }
            tstates += u64::from(self.cpu.step(&mut self.bus));
            self.flush(&mut written, &mut line, &mut on_line);
            if tstates >= limit {
                break Exit::Limit;
            }
        };
        // Anything the program wrote without a final newline is still a line.
        if !line.is_empty() {
            on_line(&String::from_utf8_lossy(&line));
        }
        let text = String::from_utf8_lossy(&self.out).into_owned();
        Output {
            // `str::lines` already drops the `\r` these programs print.
            lines: text.lines().map(str::to_string).collect(),
            text,
            exit,
            tstates,
        }
    }

    /// Hand `on_line` every line that ended since the last call, tracked by how
    /// many console bytes have been looked at so far (`written`), and leave the
    /// unfinished tail of the last one in `line`.
    fn flush(&mut self, written: &mut usize, line: &mut Vec<u8>, on_line: &mut impl FnMut(&str)) {
        for &byte in &self.out[*written..] {
            if byte == b'\n' {
                on_line(&String::from_utf8_lossy(line));
                line.clear();
            } else if byte != b'\r' {
                line.push(byte);
            }
        }
        *written = self.out.len();
    }

    /// Answer a BDOS call. `Ok(())` is the handled case, `Err` says why the run
    /// is over instead of continuing to the `RET` at `0x0005`.
    fn bdos(&mut self) -> Result<(), Exit> {
        match self.cpu.c() {
            2 => {
                let value = self.cpu.e();
                self.out.push(value);
                Ok(())
            }
            9 => {
                let mut addr = self.cpu.de();
                for _ in 0..0x1_0000 {
                    let byte = self.bus.ram[usize::from(addr)];
                    if byte == b'$' {
                        return Ok(());
                    }
                    self.out.push(byte);
                    addr = addr.wrapping_add(1);
                }
                Err(Exit::UnterminatedString)
            }
            other => Err(Exit::UnknownBdos(other)),
        }
    }
}

/// The bytes of one of the fetched exercisers, or `None` when `just fetch` has
/// not run. Skipping rather than failing is what keeps a bare checkout green,
/// and it is what [`harness`](super::harness) does for the JSON suites.
pub fn load(name: &str) -> Option<Vec<u8>> {
    let dir = testkit::test_dir(SUITE_DIR)?;
    let path: PathBuf = dir.join(format!("{name}.com"));
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) => panic!("{} is not readable: {e}", path.display()),
    }
}

/// Run `program` as a CP/M `.com` with no line callback.
pub fn run(program: &[u8], limit: u64) -> Output {
    Machine::new(program).run(limit, |_| {})
}
