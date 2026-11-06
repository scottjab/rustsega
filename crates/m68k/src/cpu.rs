//! The 68000 CPU state: register file, status register helpers, bus accessors.
//!
//! No instruction execution yet: `M68k::step` is a stub until the decode and
//! instruction tickets (M1-05 onward). Everything else here is the contract
//! those tickets build on.

use crate::bus::{ADDR_MASK, Bus};
use crate::sr::{CCR_MASK, SR_C, SR_INT_MASK, SR_MASK, SR_N, SR_RESET, SR_S, SR_V, SR_X, SR_Z};

/// Address of the initial stack pointer in the reset vector table (M68000UM
/// section 5 "Reset": the processor fetches the initial SSP from $000000).
const RESET_VECTOR_SP: u32 = 0x0000_0000;
/// Address of the reset program counter in the reset vector table.
const RESET_VECTOR_PC: u32 = 0x0000_0004;

/// Cost of the reset sequence itself in 68k clocks, excluding the two long
/// vector fetches (the bus helpers charge those). M68000UM section 5 "Reset"
/// gives the reset sequence a fixed length of 132 clock cycles.
pub const RESET_CYCLES: u32 = 132;

/// One 68000 bus cycle is 4 clock cycles (M68000UM, "Bus Cycles": S0–S3, four
/// clock phases per access). Every bus access charged by the helpers below is
/// one bus cycle, so 4 clocks; a long is two word accesses, so 8.
const BUS_CYCLE_CLOCKS: u64 = 4;

/// The 68000 register file and CPU state.
///
/// Fields are plain data (no `Box<dyn _>`, no references) so the save-state
/// milestone can serialise them, and are public so tests and the `genesis`
/// crate can poke them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M68k {
    /// Data registers D0–D7.
    pub d: [u32; 8],
    /// Address registers A0–A7. `a[7]` is always the *active* stack pointer:
    /// the SSP in supervisor mode (S = 1), the USP in user mode (S = 0).
    pub a: [u32; 8],
    /// User stack pointer. Holds the USP while it is not the active pointer
    /// (i.e. while S = 1); see [`M68k::set_sr`].
    pub usp: u32,
    /// Supervisor stack pointer. Holds the SSP while it is not the active
    /// pointer (i.e. while S = 0); see [`M68k::set_sr`].
    pub ssp: u32,
    /// Program counter. Always even.
    pub pc: u32,
    /// Status register. Only the bits in [`SR_MASK`] exist; write it through
    /// [`M68k::set_sr`] so the mask and the stack pointer swap happen.
    pub sr: u16,
    /// Prefetch queue, the IRC/IRD model the SingleStepTests expect. Filled by
    /// M1-04 (#12); unused until then.
    pub prefetch: [u16; 2],
    /// Interrupt level currently asserted on the IPL pins, 0..=7. 0 = none.
    pub ipl: u8,
    /// True while the CPU is stopped by the `STOP` instruction.
    pub stopped: bool,
    /// True while the HALT pin is asserted (the CPU stops accessing the bus).
    pub halted: bool,
    /// Running total of 68k clocks consumed since the last [`M68k::reset`].
    /// Bus accesses charge themselves (see [`M68k::read_w`]), and
    /// [`M68k::step`] will add instruction cycles as they land; the scheduler
    /// in `genesis` drains this the same way it drains the return value of
    /// `step`.
    pub cycles: u64,
}

impl Default for M68k {
    fn default() -> Self {
        Self::new()
    }
}

impl M68k {
    /// A CPU in a defined but un-reset state. Call [`M68k::reset`] before
    /// running it: the 68000 only initialises itself on a reset.
    pub fn new() -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            usp: 0,
            ssp: 0,
            pc: 0,
            sr: 0,
            prefetch: [0; 2],
            ipl: 0,
            stopped: false,
            halted: false,
            cycles: 0,
        }
    }

    /// Hardware reset (M68000UM section 5 "Reset"). Loads the initial stack
    /// pointer from address 0 and the program counter from address 4, then
    /// leaves the CPU in supervisor mode with interrupts masked.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        // Clear everything before touching SR so the stack-pointer swap that
        // `set_sr` performs below moves only zeroes: on real hardware both
        // stack pointers are uninitialised until the vector fetch.
        self.d = [0; 8];
        self.a = [0; 8];
        self.usp = 0;
        self.ssp = 0;
        self.pc = 0;
        self.prefetch = [0; 2];
        self.ipl = 0;
        self.stopped = false;
        self.halted = false;
        self.cycles = 0;
        self.set_sr(SR_RESET);

        // M68000UM section 5 "Reset": SSP from $000000, PC from $000004, in
        // that order, each one long read (high word first).
        let ssp = self.read_l(bus, RESET_VECTOR_SP);
        let pc = self.read_l(bus, RESET_VECTOR_PC);
        self.ssp = ssp;
        // SR.S is already 1, so `a[7]` is the active SSP.
        self.a[7] = ssp;
        self.pc = pc;

        self.cycles += u64::from(RESET_CYCLES);
    }

    /// Run one instruction (or one exception). Returns the 68k clocks used.
    ///
    /// Stub until the decode/instruction tickets; instruction fetch, decode
    /// and exception handling are explicitly out of scope for M1-01.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        let _ = bus;
        0
    }

    /// Mask `value` with [`SR_MASK`] and store it in SR, swapping the stack
    /// pointers if the S bit changed.
    ///
    /// Every SR change in the core goes through here — that is what keeps the
    /// nonexistent SR bits at zero and keeps `a[7]` pointing at the stack
    /// pointer that matches the new mode (M68000UM, "Status Register": the S
    /// bit selects which stack pointer is active).
    pub fn set_sr(&mut self, value: u16) {
        let value = value & SR_MASK;
        if (self.sr ^ value) & SR_S != 0 {
            self.swap_stack_pointers();
        }
        self.sr = value;
    }

    /// Move the active stack pointer between `a[7]` and the inactive slot.
    /// Called by [`M68k::set_sr`] while `self.sr` still holds the *old* value,
    /// so `S` says which pointer `a[7]` currently holds.
    fn swap_stack_pointers(&mut self) {
        if self.sr & SR_S != 0 {
            self.ssp = self.a[7];
            self.a[7] = self.usp;
        } else {
            self.usp = self.a[7];
            self.a[7] = self.ssp;
        }
    }

    /// The CCR: the low byte of SR, masked with [`CCR_MASK`].
    #[inline]
    pub fn ccr(&self) -> u16 {
        self.sr & CCR_MASK
    }

    /// Write the CCR (low byte of SR) without touching T, S or the interrupt
    /// mask, so no stack pointer swap can happen. Bits above [`CCR_MASK`] are
    /// dropped. This is what `ANDI/ORI/EORI` to CCR will use.
    pub fn set_ccr(&mut self, value: u16) {
        // Go through `set_sr` so the masking invariant holds; S cannot change
        // (it is outside `CCR_MASK`) so the stack pointers cannot move.
        let upper = self.sr & !CCR_MASK;
        self.set_sr(upper | (value & CCR_MASK));
    }

    /// Carry flag C.
    #[inline]
    pub fn flag_c(&self) -> bool {
        self.sr & SR_C != 0
    }

    /// Overflow flag V.
    #[inline]
    pub fn flag_v(&self) -> bool {
        self.sr & SR_V != 0
    }

    /// Zero flag Z.
    #[inline]
    pub fn flag_z(&self) -> bool {
        self.sr & SR_Z != 0
    }

    /// Negative flag N.
    #[inline]
    pub fn flag_n(&self) -> bool {
        self.sr & SR_N != 0
    }

    /// Extend flag X.
    #[inline]
    pub fn flag_x(&self) -> bool {
        self.sr & SR_X != 0
    }

    /// Supervisor mode (SR.S = 1).
    #[inline]
    pub fn is_supervisor(&self) -> bool {
        self.sr & SR_S != 0
    }

    /// The current interrupt mask (SR.I2–I0) as a level, 0..=7.
    #[inline]
    pub fn int_mask(&self) -> u8 {
        ((self.sr & SR_INT_MASK) >> 8) as u8
    }

    /// Set or clear a single SR bit. `mask` is one of the `SR_*` constants.
    #[inline]
    pub fn set_flag(&mut self, mask: u16, on: bool) {
        self.set_sr(if on { self.sr | mask } else { self.sr & !mask });
    }

    /// Read the byte at `addr`, masked to 24 bits, and charge one bus cycle.
    #[inline]
    pub fn read_b(&mut self, bus: &mut impl Bus, addr: u32) -> u8 {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.read_byte(addr & ADDR_MASK)
    }

    /// Read the big endian word at `addr`, masked to 24 bits, and charge one
    /// bus cycle. `addr` is even: the 68000 never performs an odd word access
    /// (an unaligned one is an address error exception).
    #[inline]
    pub fn read_w(&mut self, bus: &mut impl Bus, addr: u32) -> u16 {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.read_word(addr & ADDR_MASK)
    }

    /// Read the big endian long at `addr`: high word at `addr`, low word at
    /// `addr + 2`, two bus cycles charged (8 clocks).
    #[inline]
    pub fn read_l(&mut self, bus: &mut impl Bus, addr: u32) -> u32 {
        let addr = addr & ADDR_MASK;
        let high = u32::from(self.read_w(bus, addr));
        let low = u32::from(self.read_w(bus, (addr + 2) & ADDR_MASK));
        (high << 16) | low
    }

    /// Write a byte, masked to 24 bits, and charge one bus cycle.
    #[inline]
    pub fn write_b(&mut self, bus: &mut impl Bus, addr: u32, value: u8) {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.write_byte(addr & ADDR_MASK, value);
    }

    /// Write a big endian word at even `addr`, masked to 24 bits, and charge
    /// one bus cycle.
    #[inline]
    pub fn write_w(&mut self, bus: &mut impl Bus, addr: u32, value: u16) {
        self.cycles += BUS_CYCLE_CLOCKS;
        bus.write_word(addr & ADDR_MASK, value);
    }

    /// Write a big endian long: high word at `addr`, low word at `addr + 2`,
    /// two bus cycles charged (8 clocks).
    #[inline]
    pub fn write_l(&mut self, bus: &mut impl Bus, addr: u32, value: u32) {
        let addr = addr & ADDR_MASK;
        self.write_w(bus, addr, (value >> 16) as u16);
        self.write_w(bus, (addr + 2) & ADDR_MASK, value as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sr::{SR_C, SR_INT_MASK, SR_N, SR_T, SR_V, SR_X};

    /// Minimal 64 KB RAM bus for these unit tests. M1-02 (#10) adds the real
    /// `TestBus` with bus-transaction recording for the conformance suites;
    /// this one only needs to remember the addresses it was asked about.
    struct Ram64k {
        mem: Vec<u8>,
        /// Every address the CPU asked the bus about, in order.
        accesses: Vec<u32>,
    }

    impl Ram64k {
        fn new() -> Self {
            Self {
                mem: vec![0; 1 << 16],
                accesses: Vec::new(),
            }
        }

        /// Mirror of `M68k::read_*`: only ever called with 24 bit addresses in
        /// these tests, so no masking here — that asymmetry is the point of
        /// `address_is_masked_to_24_bits`.
        fn peek_byte(&self, addr: u32) -> u8 {
            self.mem[addr as usize]
        }

        fn peek_word(&self, addr: u32) -> u16 {
            let i = addr as usize;
            u16::from_be_bytes([self.mem[i], self.mem[i + 1]])
        }

        fn peek_long(&self, addr: u32) -> u32 {
            let i = addr as usize;
            u32::from_be_bytes([
                self.mem[i],
                self.mem[i + 1],
                self.mem[i + 2],
                self.mem[i + 3],
            ])
        }

        fn poke_long(&mut self, addr: u32, value: u32) {
            let i = addr as usize;
            self.mem[i..i + 4].copy_from_slice(&value.to_be_bytes());
        }
    }

    impl Bus for Ram64k {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.accesses.push(addr);
            self.peek_byte(addr)
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.accesses.push(addr);
            self.peek_word(addr)
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.accesses.push(addr);
            let i = addr as usize;
            self.mem[i] = value;
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.accesses.push(addr);
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }
    }

    /// Bus with the reset vector table: SSP = 0x00FF_0000, PC = 0x0000_0200.
    fn bus_with_reset_vector() -> Ram64k {
        let mut bus = Ram64k::new();
        bus.poke_long(0x0000_0000, 0x00FF_0000);
        bus.poke_long(0x0000_0004, 0x0000_0200);
        bus
    }

    #[test]
    fn reset_loads_ssp_and_pc() {
        let mut bus = bus_with_reset_vector();
        let mut cpu = M68k::new();

        cpu.reset(&mut bus);

        assert_eq!(cpu.a[7], 0x00FF_0000, "SSP comes from address 0");
        assert_eq!(cpu.pc, 0x0000_0200, "PC comes from address 4");
        assert_eq!(cpu.sr, 0x2700, "S=1, T=0, interrupt mask 7");
        assert!(cpu.is_supervisor());
        assert_eq!(cpu.int_mask(), 7);
        // The reset vector fetches are two long accesses, high word first.
        assert_eq!(
            bus.accesses,
            [0x0000_0000, 0x0000_0002, 0x0000_0004, 0x0000_0006]
        );
    }

    #[test]
    fn set_sr_masks_unused_bits() {
        let mut cpu = M68k::new();

        cpu.set_sr(0xFFFF);

        assert_eq!(cpu.sr, 0xA71F);
        assert_eq!(cpu.sr & !SR_MASK, 0);
    }

    #[test]
    fn switching_to_user_mode_swaps_stack_pointers() {
        let mut cpu = M68k::new();
        cpu.set_sr(SR_S);
        cpu.a[7] = 0x1000; // active SSP
        cpu.usp = 0x2000; // stored USP

        cpu.set_sr(0x0000); // -> user mode

        assert_eq!(cpu.a[7], 0x2000, "a[7] becomes the USP");
        assert_eq!(cpu.ssp, 0x1000, "the old a[7] is now the stored SSP");
        assert_eq!(cpu.usp, 0x2000);

        cpu.set_sr(SR_S); // -> supervisor again

        assert_eq!(cpu.a[7], 0x1000);
        assert_eq!(cpu.ssp, 0x1000);
        assert_eq!(cpu.usp, 0x2000, "a[7] moved back into the USP slot");
    }

    #[test]
    fn ccr_helpers() {
        let mut cpu = M68k::new();
        cpu.set_sr(0);

        cpu.set_flag(SR_X, true);
        assert!(cpu.flag_x(), "X set");
        cpu.set_flag(SR_N, true);
        assert!(cpu.flag_n(), "N set");
        cpu.set_flag(SR_Z, true);
        assert!(cpu.flag_z(), "Z set");
        cpu.set_flag(SR_V, true);
        assert!(cpu.flag_v(), "V set");
        cpu.set_flag(SR_C, true);
        assert!(cpu.flag_c(), "C set");
        assert_eq!(cpu.sr, SR_X | SR_N | SR_Z | SR_V | SR_C);
        assert_eq!(cpu.ccr(), SR_X | SR_N | SR_Z | SR_V | SR_C);

        cpu.set_flag(SR_X, false);
        cpu.set_flag(SR_N, false);
        cpu.set_flag(SR_Z, false);
        cpu.set_flag(SR_V, false);
        cpu.set_flag(SR_C, false);
        assert_eq!(cpu.sr, 0);
        assert!(!cpu.flag_x());
        assert!(!cpu.flag_n());
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_v());
        assert!(!cpu.flag_c());
        assert_eq!(cpu.ccr(), 0);

        // set_ccr writes only the low byte: T, S and the interrupt mask stay.
        cpu.set_sr(SR_T | SR_S | SR_INT_MASK | SR_C);
        cpu.set_ccr(SR_Z);
        assert_eq!(cpu.sr, SR_T | SR_S | SR_INT_MASK | SR_Z);
        assert!(cpu.flag_z());
        assert!(!cpu.flag_c());
        // Bits outside the CCR cannot leak in.
        cpu.set_ccr(0xFFFF);
        assert_eq!(cpu.ccr(), CCR_MASK);
        // S is untouched, so the stack pointers do not move.
        cpu.set_sr(SR_S);
        cpu.a[7] = 0x1000;
        cpu.usp = 0x2000;
        cpu.set_ccr(SR_C);
        assert!(cpu.is_supervisor());
        assert_eq!(cpu.a[7], 0x1000);
        assert_eq!(cpu.usp, 0x2000);
    }

    #[test]
    fn address_is_masked_to_24_bits() {
        let mut bus = Ram64k::new();
        bus.poke_long(0x0000_0004, 0xDEAD_BEEF);
        let mut cpu = M68k::new();

        assert_eq!(cpu.read_w(&mut bus, 0xFF00_0004), 0xDEAD);
        assert_eq!(bus.accesses, [0x0000_0004]);

        assert_eq!(cpu.read_l(&mut bus, 0xFF00_0004), 0xDEAD_BEEF);
        assert_eq!(
            bus.accesses,
            [0x0000_0004, 0x0000_0004, 0x0000_0006],
            "a long is two word accesses, high word first"
        );

        cpu.write_b(&mut bus, 0xFF00_0008, 0x12);
        cpu.write_w(&mut bus, 0xFF00_000A, 0x3456);
        cpu.write_l(&mut bus, 0xFF00_000C, 0x789A_BCDE);
        assert_eq!(bus.accesses, [4, 4, 6, 8, 0x0A, 0x0C, 0x0E]);
        assert_eq!(bus.peek_byte(0x0008), 0x12);
        assert_eq!(bus.peek_word(0x000A), 0x3456);
        assert_eq!(bus.peek_long(0x000C), 0x789A_BCDE);
    }

    #[test]
    fn bus_helpers_charge_four_clocks_per_access() {
        let mut bus = Ram64k::new();
        let mut cpu = M68k::new();
        assert_eq!(cpu.cycles, 0);

        cpu.read_b(&mut bus, 0x0000_0010);
        cpu.read_w(&mut bus, 0x0000_0010);
        cpu.write_b(&mut bus, 0x0000_0010, 1);
        cpu.write_w(&mut bus, 0x0000_0010, 2);
        assert_eq!(cpu.cycles, 16, "four single bus cycles");

        cpu.read_l(&mut bus, 0x0000_0010);
        assert_eq!(cpu.cycles, 24, "a long is two bus cycles");

        cpu.write_l(&mut bus, 0x0000_0010, 3);
        assert_eq!(cpu.cycles, 32, "a long is two bus cycles");
    }

    #[test]
    fn reset_charges_132_clocks() {
        let mut bus = bus_with_reset_vector();
        let mut cpu = M68k::new();

        cpu.reset(&mut bus);

        // The reset sequence itself, plus the two long vector fetches.
        assert_eq!(cpu.cycles, u64::from(RESET_CYCLES) + 16);
    }
}
