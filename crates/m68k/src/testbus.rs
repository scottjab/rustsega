//! Flat RAM bus for 68000 tests: the whole 24 bit address space, zeroed.
//!
//! Every later m68k ticket builds CPU tests on [`TestBus`] plus
//! [`cpu_with_program`], which is why this module is always compiled rather
//! than `#[cfg(test)]`: the integration suites under `crates/m68k/tests/` and
//! other crates need it too.
//!
//! It is a test bus, not a device: no I/O and no side effects beyond RAM and
//! [`TestBus::accesses`]. Byte, word and long access is plain big endian RAM,
//! so a test can put an opcode at an address, run the CPU and read the result
//! back at the same address.
//!
//! # Access recording
//!
//! There is one rule, and it is the whole of it: every [`Bus`] read and write
//! appends its address to [`TestBus::accesses`], in the order the bus received
//! them, with the address the caller asked for rather than the masked one —
//! so a test can assert the CPU masked an address by asserting what the bus
//! was handed (#184, which retired the 64 KB `Ram64k` that used to be this
//! log's only home). `int_ack` and `reset_devices` are not accesses and are
//! not recorded.

use crate::bus::{ADDR_MASK, Bus};
use crate::cpu::M68k;

/// Full size of the flat memory: every address the 24 bit address bus can
/// reach, $000000–$FFFFFF (M68000UM, "Address Definition": only A23–A1 are
/// decoded, so nothing above $FFFFFF exists to talk to).
const MEM_SIZE: usize = 0x100_0000;

/// [`ADDR_MASK`] in `usize` form, for indexing [`TestBus::mem`]. Written with
/// `as` rather than the crate's usual `From`/`.into()` because `From` is not
/// stable as a const trait yet, so `const MEM_SIZE: usize = ADDR_MASK.into();`
/// does not compile.
const MEM_MASK: usize = ADDR_MASK as usize;

/// Reset vector slot holding the initial stack pointer (M68000UM section 5
/// "Reset": the processor reads the SSP from $000000).
const VECTOR_SP: u32 = 0x0000_0000;
/// Reset vector slot holding the initial program counter.
const VECTOR_PC: u32 = 0x0000_0004;

/// Where [`cpu_with_program`] loads the program words, and the value it puts
/// in the PC vector, so `PC` lands on the first instruction.
const PROGRAM_ADDR: u32 = 0x0000_1000;
/// Initial stack pointer given by [`cpu_with_program`]: one page above the
/// program, so a `MOVE.L D0,-(A7)` in a short test cannot clobber it.
const STACK_TOP: u32 = 0x0001_0000;

/// Zeroed RAM covering the whole 24 bit space, as a [`Bus`].
///
/// Fields are public so tests can poke memory directly and assert on it.
/// `Debug` prints all 16 MiB, so assert on `&bus.mem[a..b]` slices rather than
/// on the whole bus.
///
/// Records every access into [`TestBus::accesses`] under the one rule at the
/// top of this module. The rule has no switch, and the inherent helpers
/// ([`TestBus::load_words`], [`TestBus::write_long`], [`TestBus::read_long`])
/// go through the [`Bus`] accessors, so they are recorded too: a test that
/// sets memory up before running the CPU clears the log in between to keep its
/// assertion about what the *CPU* asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestBus {
    /// Flat memory, indexed by the low 24 bits of the address.
    pub mem: Vec<u8>,
    /// Every address the bus was asked about, in order — the recording rule is
    /// on [the module docs](self#access-recording).
    pub accesses: Vec<u32>,
    /// Vector number returned from [`Bus::int_ack`]. `None` (the default) is
    /// autovectoring, which is what a real Genesis always does.
    pub int_vector: Option<u8>,
    /// How many times [`Bus::reset_devices`] ran, for tests of `RESET`.
    pub reset_count: u32,
}

impl TestBus {
    /// 16 MiB of zeroed memory, an empty [`TestBus::accesses`] log,
    /// autovectoring, no resets seen yet.
    pub fn new() -> Self {
        Self {
            mem: vec![0; MEM_SIZE],
            accesses: Vec::new(),
            int_vector: None,
            reset_count: 0,
        }
    }

    /// Load big endian `words` at `addr`, one word every 2 bytes — the way a
    /// test writes a program or a data table.
    ///
    /// Deliberately an inherent method, not part of [`Bus`]: the real
    /// `genesis` bus has no notion of "load a block", and adding it here would
    /// put it in every implementor. Like the [`Bus`] accessors it masks the
    /// address rather than bounds-checking it, mirroring the CPU.
    pub fn load_words(&mut self, addr: u32, words: &[u16]) {
        let mut addr = addr & ADDR_MASK;
        for &word in words {
            self.write_word(addr, word);
            addr = (addr + 2) & ADDR_MASK;
        }
    }

    /// Write a big endian long: high word at `addr`, low word at `addr + 2`.
    ///
    /// Inherent like [`TestBus::load_words`], because [`Bus`] is byte/word
    /// only: the 68000 builds every long access from two word cycles, so a
    /// real bus never sees a long. Goes through [`Bus::write_word`] so there is
    /// one place that decides byte order and masking.
    pub fn write_long(&mut self, addr: u32, value: u32) {
        let addr = addr & ADDR_MASK;
        self.write_word(addr, (value >> 16) as u16);
        self.write_word((addr + 2) & ADDR_MASK, value as u16);
    }

    /// Read a big endian long: high word at `addr`, low word at `addr + 2`.
    pub fn read_long(&mut self, addr: u32) -> u32 {
        let addr = addr & ADDR_MASK;
        let high = u32::from(self.read_word(addr));
        let low = u32::from(self.read_word((addr + 2) & ADDR_MASK));
        (high << 16) | low
    }

    /// The low 24 bits of `addr` as an index into [`TestBus::mem`].
    #[inline]
    fn index(addr: u32) -> usize {
        addr as usize & MEM_MASK
    }
}

impl Default for TestBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for TestBus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.accesses.push(addr);
        self.mem[Self::index(addr)]
    }

    /// `(mem[addr] << 8) | mem[addr + 1]`: the 68000 is big endian, the byte at
    /// the lower address is the high byte (M68000UM, "Data Organization").
    ///
    /// Word addresses are even on real hardware and the CPU only ever issues
    /// them even; an odd or top-of-space address here is a bug in the test and
    /// panics with an out-of-range index instead of silently wrapping.
    fn read_word(&mut self, addr: u32) -> u16 {
        self.accesses.push(addr);
        let i = Self::index(addr);
        u16::from_be_bytes([self.mem[i], self.mem[i + 1]])
    }

    fn write_byte(&mut self, addr: u32, value: u8) {
        self.accesses.push(addr);
        self.mem[Self::index(addr)] = value;
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        self.accesses.push(addr);
        let i = Self::index(addr);
        self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn int_ack(&mut self, _level: u8) -> Option<u8> {
        self.int_vector
    }

    fn reset_devices(&mut self) {
        self.reset_count += 1;
    }
}

/// A CPU reset onto `program`, ready to step.
///
/// Writes the reset vector table (SSP = [`STACK_TOP`] at $000000, PC =
/// [`PROGRAM_ADDR`] at $000004), loads `program` at [`PROGRAM_ADDR`], then
/// calls [`M68k::reset`] — a real reset, so the CPU comes back in supervisor
/// mode with interrupts masked, with the prefetch queue already holding
/// `program`'s first two words, and with `cycles` charged for the reset
/// sequence, the two long vector fetches and the two prefetch fetches
/// (`RESET_CYCLES + 24`, i.e. 156).
///
/// ```
/// use m68k::testbus::cpu_with_program;
///
/// let (cpu, _bus) = cpu_with_program(&[0x4E71, 0x4E75]); // NOP, RTE
/// assert_eq!(cpu.pc, 0x0000_1000);
/// assert_eq!(cpu.prefetch, [0x4E71, 0x4E75]);
/// ```
pub fn cpu_with_program(program: &[u16]) -> (M68k, TestBus) {
    let mut bus = TestBus::new();
    bus.write_long(VECTOR_SP, STACK_TOP);
    bus.write_long(VECTOR_PC, PROGRAM_ADDR);
    bus.load_words(PROGRAM_ADDR, program);

    let mut cpu = M68k::new();
    cpu.reset(&mut bus);
    (cpu, bus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RESET_CYCLES;

    #[test]
    fn word_access_is_big_endian() {
        let mut bus = TestBus::new();

        bus.write_word(0x0000_0010, 0x1234);

        assert_eq!(bus.read_byte(0x0000_0010), 0x12, "high byte first");
        assert_eq!(bus.read_byte(0x0000_0011), 0x34, "low byte second");
        assert_eq!(bus.read_word(0x0000_0010), 0x1234, "round trips");
    }

    #[test]
    fn memory_covers_full_24_bit_space() {
        let mut bus = TestBus::new();
        assert_eq!(bus.mem.len(), 0x100_0000, "every 24 bit address");

        bus.write_word(0x00FF_FFFE, 0xBEEF);

        assert_eq!(bus.read_byte(0x00FF_FFFE), 0xBE);
        assert_eq!(bus.read_byte(0x00FF_FFFF), 0xEF);
        assert_eq!(bus.read_word(0x00FF_FFFE), 0xBEEF);

        // Bits above A23 do not exist, so they alias like the masked CPU.
        bus.write_byte(0xFF00_0010, 0x5A);
        assert_eq!(bus.read_byte(0x0000_0010), 0x5A);
    }

    #[test]
    fn load_program_places_words() {
        let mut bus = TestBus::new();

        bus.load_words(0x0000_1000, &[0x4E71, 0x4E75]); // NOP, RTE

        assert_eq!(bus.read_word(0x0000_1000), 0x4E71);
        assert_eq!(bus.read_word(0x0000_1002), 0x4E75);
        assert_eq!(bus.read_byte(0x0000_1000), 0x4E, "big endian, again");
        assert_eq!(bus.read_byte(0x0000_1003), 0x75);
        assert_eq!(bus.read_byte(0x0000_1004), 0, "nothing past the program");
    }

    #[test]
    fn long_access_is_big_endian() {
        let mut bus = TestBus::new();

        bus.write_long(0x0000_2000, 0xDEAD_BEEF);

        assert_eq!(bus.read_long(0x0000_2000), 0xDEAD_BEEF);
        assert_eq!(bus.read_word(0x0000_2000), 0xDEAD, "high word first");
        assert_eq!(bus.read_word(0x0000_2002), 0xBEEF);
    }

    #[test]
    fn int_ack_autovectors_unless_told_otherwise() {
        let mut bus = TestBus::new();

        assert_eq!(bus.int_ack(2), None, "default is autovector");
        assert_eq!(bus.reset_count, 0);

        bus.int_vector = Some(0x42);
        assert_eq!(bus.int_ack(5), Some(0x42));

        bus.reset_devices();
        assert_eq!(bus.reset_count, 1, "`RESET` instruction bumps it");
    }

    /// The recording rule in one test: every [`Bus`] read and write, in the
    /// order the bus received them, with the address the caller asked for
    /// rather than the masked one, reads and writes in the one list. The
    /// inherent helpers appear in it too, because they go through the trait
    /// accessors.
    #[test]
    fn accesses_are_recorded_in_order_as_they_were_asked_for() {
        let mut bus = TestBus::new();

        bus.write_word(0x0000_1000, 0x1234);
        bus.write_long(0x0000_2000, 0xDEAD_BEEF);
        assert_eq!(bus.read_long(0x0000_2000), 0xDEAD_BEEF);
        bus.write_byte(0xFF00_0010, 0x5A);
        assert_eq!(bus.read_byte(0x0000_0010), 0x5A, "aliased like the CPU");

        assert_eq!(
            bus.accesses,
            [
                0x0000_1000, // write_word
                0x0000_2000, // write_long, high word
                0x0000_2002, // write_long, low word
                0x0000_2000, // read_long, high word
                0x0000_2002, // read_long, low word
                0xFF00_0010, // write_byte, as asked for, not masked
                0x0000_0010, // read_byte
            ],
            "one list, in bus order, addresses as the caller asked for them"
        );
    }

    #[test]
    fn cpu_with_program_resets_to_program() {
        let (cpu, mut bus) = cpu_with_program(&[0x4E71, 0x4E75]); // NOP, RTE

        assert_eq!(cpu.pc, 0x0000_1000, "PC from the long at $4");
        assert_eq!(cpu.a[7], 0x0001_0000, "SSP from the long at $0");
        assert_eq!(cpu.ssp, 0x0001_0000);
        assert!(cpu.is_supervisor());
        assert_eq!(bus.read_word(cpu.pc), 0x4E71, "program is at PC");
        assert_eq!(bus.read_word(cpu.pc + 2), 0x4E75);
        // The helper runs a real `M68k::reset`, not a register poke: the 132
        // clock reset sequence, the two long vector fetches, and the two word
        // fetches that fill the prefetch queue (M1-04, #12).
        assert_eq!(cpu.cycles, u64::from(RESET_CYCLES) + 16 + 8);
        assert_eq!(
            cpu.prefetch,
            [0x4E71, 0x4E75],
            "reset leaves the queue holding the first two words of the program"
        );
    }
}
