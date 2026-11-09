//! A RAM-backed bus for unit tests: enough of [`Z80Bus`] to run an instruction
//! and see what it did.
//!
//! Test-only (`#[cfg(test)]`), shared between the unit tests in this crate and
//! the integration harness, and deliberately records rather than judges: an
//! instruction that should not write is caught by asserting `writes` is empty,
//! which points at the instruction instead of at the bus.

use std::collections::VecDeque;

use crate::Z80Bus;

/// 64 KiB of RAM with the program placed where the test's `PC` starts, logging
/// every access.
pub(crate) struct RamBus {
    pub ram: Vec<u8>,
    /// Memory read addresses, in order.
    pub reads: Vec<u16>,
    /// Memory writes, in order.
    pub writes: Vec<(u16, u8)>,
    /// Port reads, in order, each with the value the bus handed back.
    pub port_reads: Vec<(u16, u8)>,
    /// Port writes, in order.
    pub port_writes: Vec<(u16, u8)>,
    /// Values still to hand out on the next `IN`, in order.
    pending_reads: VecDeque<u8>,
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
            pending_reads: VecDeque::new(),
        }
    }

    /// Set one memory byte before the test runs.
    #[must_use]
    pub fn with_byte(mut self, addr: u16, value: u8) -> Self {
        self.ram[usize::from(addr)] = value;
        self
    }

    /// Queue the value of the next `IN`, in the order the test expects its port
    /// reads.
    ///
    /// The same scheme as the conformance harness, which serves a case's
    /// `ports` array in order: the value arrives whoever asked for it, so a
    /// wrong port shows up as a wrong recorded read instead of a panic.
    #[must_use]
    pub fn with_port(mut self, value: u8) -> Self {
        self.pending_reads.push_back(value);
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
        let value = self.pending_reads.pop_front().unwrap_or(0);
        self.port_reads.push((port, value));
        value
    }

    fn io_write(&mut self, port: u16, value: u8) {
        self.port_writes.push((port, value));
    }
}
