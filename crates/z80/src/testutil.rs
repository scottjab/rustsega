//! A RAM-backed bus for unit tests: enough of [`Z80Bus`] to run an instruction
//! and see what it did.
//!
//! Test-only (`#[cfg(test)]`), shared between the unit tests in this crate and
//! the integration harness, and deliberately records rather than judges: an
//! instruction that should not write is caught by asserting `writes` is empty,
//! which points at the instruction instead of at the bus.

use crate::Z80Bus;

/// 64 KiB of RAM with the program placed where the test's `PC` starts, logging
/// every access.
pub(crate) struct RamBus {
    pub ram: Vec<u8>,
    /// Memory read addresses, in order.
    pub reads: Vec<u16>,
    /// Memory writes, in order.
    pub writes: Vec<(u16, u8)>,
    /// Port reads, in order.
    pub port_reads: Vec<(u16, u8)>,
    /// Port writes, in order.
    pub port_writes: Vec<(u16, u8)>,
}

impl RamBus {
    /// A zeroed bus with `program` placed at `pc`, which is where a test that
    /// starts at a non-zero `PC` needs it: bytes fetched from anywhere else are
    /// `00` and the test would be exercising `NOP`.
    pub fn at(pc: u16, program: &[u8]) -> Self {
        let mut ram = vec![0; 0x1_0000];
        let start = usize::from(pc);
        ram[start..start + program.len()].copy_from_slice(program);
        Self::blank(ram)
    }

    fn blank(ram: Vec<u8>) -> Self {
        Self {
            ram,
            reads: Vec::new(),
            writes: Vec::new(),
            port_reads: Vec::new(),
            port_writes: Vec::new(),
        }
    }

    /// Set one memory byte before the test runs.
    #[must_use]
    pub fn with_byte(mut self, addr: u16, value: u8) -> Self {
        self.ram[usize::from(addr)] = value;
        self
    }

    /// The byte at `addr`, for asserting on results.
    pub fn peek(&self, addr: u16) -> u8 {
        self.ram[usize::from(addr)]
    }
}

impl Z80Bus for RamBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.reads.push(addr);
        self.ram[usize::from(addr)]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.writes.push((addr, value));
        self.ram[usize::from(addr)] = value;
    }

    fn io_read(&mut self, port: u16) -> u8 {
        self.port_reads.push((port, 0));
        0
    }

    fn io_write(&mut self, port: u16, value: u8) {
        self.port_writes.push((port, value));
    }
}
