//! Zilog Z80 CPU core.
//!
//! This is the foundation layer: the bus trait, the register file with its
//! hidden state, and reset. Instructions come in later tickets, each one
//! filling in a piece of [`Z80::step`].
//!
//! The register file keeps the undocumented state the Z80 really has, because
//! the conformance suite the crate is built against compares it: every case in
//! the SingleStepTests z80 suite carries `wz`, `q` and `p` in both its initial
//! and its final state, so a core that does not model them cannot pass, no
//! matter how correct its instruction semantics are.

mod decode;
mod ld16;
mod ld8;
mod regs;
#[cfg(test)]
mod testutil;

pub use decode::IndexMode;
pub use regs::{
    FLAG_C, FLAG_H, FLAG_N, FLAG_PV, FLAG_S, FLAG_SZ, FLAG_SZXY, FLAG_X, FLAG_Y, FLAG_Z, IntMode,
    SZXY_PARITY, incremented_r, parity, szxy_flags,
};

/// The memory and I/O side of a Z80.
///
/// The Z80 has two address spaces and this keeps them separate: `IN`/`OUT` use
/// the 16-bit port space (`A` supplies the high byte for the extended forms, so
/// the port is not just 8 bits), and nothing on the Genesis side lets a Z80
/// write reach the 68k's address space by accident.
pub trait Z80Bus {
    /// Read a byte from the 64 KiB memory space.
    fn read(&mut self, addr: u16) -> u8;
    /// Write a byte to the 64 KiB memory space.
    fn write(&mut self, addr: u16, value: u8);
    /// Read a byte from the 16-bit I/O port space (`IN`).
    fn io_read(&mut self, port: u16) -> u8;
    /// Write a byte to the 16-bit I/O port space (`OUT`).
    fn io_write(&mut self, port: u16, value: u8);
}

/// The Z80 register file, hidden state, and reset.
///
/// Register pairs are stored as `u16` and the 8-bit halves are views on them.
/// That is how the chip is wired too, so no instruction can leave a half and
/// its pair out of step.
#[derive(Copy, Clone, Debug)]
pub struct Z80 {
    af: u16,
    bc: u16,
    de: u16,
    hl: u16,
    af_shadow: u16,
    bc_shadow: u16,
    de_shadow: u16,
    hl_shadow: u16,
    ix: u16,
    iy: u16,
    sp: u16,
    pc: u16,
    /// Interrupt vector high byte, for `IM 2` and `LD A,I`/`LD I,A`.
    i: u8,
    /// Refresh counter: low 7 bits count opcodes, bit 7 is only written by
    /// `LD R,A`.
    r: u8,
    iff1: bool,
    iff2: bool,
    im: IntMode,
    halted: bool,
    /// The hidden `WZ` register, called MEMPTR in most documentation. It
    /// records the last address the ALU or a memory access generated and is
    /// readable through the undocumented X/Y flags of `BIT n,(HL)`.
    wz: u16,
    /// The byte that feeds the undocumented X/Y flags when an instruction does
    /// not compute them itself. Instructions that write the flags store the new
    /// flag byte here; instructions that leave the flags alone store zero. The
    /// suite proves it: in every flag-touching opcode file `final.q` equals
    /// `final.f` in all 1000 cases, and in every LD and NOP file it is zero in
    /// all of them. It is a byte and not a bit - `INC B` ends with `q` at 160
    /// in case `04 0001`, for example - so it is not a `bool`.
    q: u8,
    /// Set when the last instruction was `LD A,I` or `LD A,R`; decides the
    /// `P/V` bit an interrupt accepted right after those instructions forces.
    p: bool,
    /// `EI` does not open the interrupt gate until the instruction after it
    /// has run, so acceptance has to be delayed by one instruction.
    ei_pending: bool,
}

impl Z80 {
    /// A CPU in its reset state.
    pub fn new() -> Self {
        let mut cpu = Self {
            af: 0,
            bc: 0,
            de: 0,
            hl: 0,
            af_shadow: 0,
            bc_shadow: 0,
            de_shadow: 0,
            hl_shadow: 0,
            ix: 0,
            iy: 0,
            sp: 0,
            pc: 0,
            i: 0,
            r: 0,
            iff1: false,
            iff2: false,
            im: IntMode::Mode0,
            halted: false,
            wz: 0,
            q: 0,
            p: false,
            ei_pending: false,
        };
        cpu.reset();
        cpu
    }

    /// Put the CPU back the way a real Z80 is after its `/RESET` line goes
    /// low.
    ///
    /// `PC` is 0 and the interrupt state is off (`IFF1` = `IFF2` = false,
    /// `IM 0`). `AF` and `SP` come up as `0xFFFF` because the data bus floats
    /// high while the chip is held in reset ("The Undocumented Z80 Documented",
    /// section 2.4). Everything else is undefined on real silicon; we use 0 so
    /// a run that depends on uninitialized registers is reproducible instead of
    /// machine-dependent.
    pub fn reset(&mut self) {
        self.pc = 0;
        self.sp = 0xFFFF;
        self.af = 0xFFFF;
        self.bc = 0;
        self.de = 0;
        self.hl = 0;
        self.af_shadow = 0;
        self.bc_shadow = 0;
        self.de_shadow = 0;
        self.hl_shadow = 0;
        self.ix = 0;
        self.iy = 0;
        self.i = 0;
        self.r = 0;
        self.iff1 = false;
        self.iff2 = false;
        self.im = IntMode::Mode0;
        self.halted = false;
        self.wz = 0;
        self.q = 0;
        self.p = false;
        self.ei_pending = false;
    }

    /// Run one instruction and return the T-states it took.
    ///
    /// Only `NOP` and the prefix fallbacks are real instructions so far; every
    /// other opcode panics as unimplemented, which is what makes a missing
    /// instruction show up as a named opcode rather than a field mismatch.
    ///
    /// The fetch and the dispatch tree live in [`decode`](crate::decode);
    /// `R` is counted there, by the opcode fetch, not here.
    pub fn step(&mut self, bus: &mut impl Z80Bus) -> u32 {
        self.exec(bus)
    }

    /// The `AF` pair, flags and accumulator together.
    pub fn af(&self) -> u16 {
        self.af
    }

    /// Load `AF`, flags included. `LD A,n` style instructions must not use
    /// this: it overwrites the flags.
    pub fn set_af(&mut self, value: u16) {
        self.af = value;
    }

    /// The `BC` pair.
    pub fn bc(&self) -> u16 {
        self.bc
    }

    /// Load the whole `BC` pair.
    pub fn set_bc(&mut self, value: u16) {
        self.bc = value;
    }

    /// The `DE` pair.
    pub fn de(&self) -> u16 {
        self.de
    }

    /// Load the whole `DE` pair.
    pub fn set_de(&mut self, value: u16) {
        self.de = value;
    }

    /// The `HL` pair.
    pub fn hl(&self) -> u16 {
        self.hl
    }

    /// Load the whole `HL` pair.
    pub fn set_hl(&mut self, value: u16) {
        self.hl = value;
    }

    /// The `AF'` shadow pair, swapped in by `EXX` and `EX AF,AF'`.
    pub fn af_shadow(&self) -> u16 {
        self.af_shadow
    }

    /// Load the `AF'` shadow pair.
    pub fn set_af_shadow(&mut self, value: u16) {
        self.af_shadow = value;
    }

    /// The `BC'` shadow pair.
    pub fn bc_shadow(&self) -> u16 {
        self.bc_shadow
    }

    /// Load the `BC'` shadow pair.
    pub fn set_bc_shadow(&mut self, value: u16) {
        self.bc_shadow = value;
    }

    /// The `DE'` shadow pair.
    pub fn de_shadow(&self) -> u16 {
        self.de_shadow
    }

    /// Load the `DE'` shadow pair.
    pub fn set_de_shadow(&mut self, value: u16) {
        self.de_shadow = value;
    }

    /// The `HL'` shadow pair.
    pub fn hl_shadow(&self) -> u16 {
        self.hl_shadow
    }

    /// Load the `HL'` shadow pair.
    pub fn set_hl_shadow(&mut self, value: u16) {
        self.hl_shadow = value;
    }

    /// The `IX` index pair.
    pub fn ix(&self) -> u16 {
        self.ix
    }

    /// Load the whole `IX` pair.
    pub fn set_ix(&mut self, value: u16) {
        self.ix = value;
    }

    /// The `IY` index pair.
    pub fn iy(&self) -> u16 {
        self.iy
    }

    /// Load the whole `IY` pair.
    pub fn set_iy(&mut self, value: u16) {
        self.iy = value;
    }

    /// The stack pointer. The Z80 stack grows down.
    pub fn sp(&self) -> u16 {
        self.sp
    }

    /// Load the stack pointer.
    pub fn set_sp(&mut self, value: u16) {
        self.sp = value;
    }

    /// The program counter, pointing at the next opcode byte.
    pub fn pc(&self) -> u16 {
        self.pc
    }

    /// Jump: load the program counter.
    pub fn set_pc(&mut self, value: u16) {
        self.pc = value;
    }

    /// The accumulator.
    pub fn a(&self) -> u8 {
        (self.af >> 8) as u8
    }

    /// Load the accumulator without touching the flags.
    pub fn set_a(&mut self, value: u8) {
        self.af = (u16::from(value) << 8) | (self.af & 0xFF);
    }

    /// The flag register.
    pub fn f(&self) -> u8 {
        self.af as u8
    }

    /// Load all eight flag bits, undocumented ones included. This is what
    /// `POP AF`, `LD A,(flags)` and the conformance harness want; an ALU op
    /// that computes only some of them wants [`Z80::update_flags`] instead.
    pub fn set_flags(&mut self, flags: u8) {
        self.af = (self.af & 0xFF00) | u16::from(flags);
    }

    /// The `B` half of `BC`.
    pub fn b(&self) -> u8 {
        (self.bc >> 8) as u8
    }

    /// Load `B` without touching `C`.
    pub fn set_b(&mut self, value: u8) {
        self.bc = (u16::from(value) << 8) | (self.bc & 0xFF);
    }

    /// The `C` half of `BC`.
    pub fn c(&self) -> u8 {
        self.bc as u8
    }

    /// Load `C` without touching `B`.
    pub fn set_c(&mut self, value: u8) {
        self.bc = (self.bc & 0xFF00) | u16::from(value);
    }

    /// The `D` half of `DE`.
    pub fn d(&self) -> u8 {
        (self.de >> 8) as u8
    }

    /// Load `D` without touching `E`.
    pub fn set_d(&mut self, value: u8) {
        self.de = (u16::from(value) << 8) | (self.de & 0xFF);
    }

    /// The `E` half of `DE`.
    pub fn e(&self) -> u8 {
        self.de as u8
    }

    /// Load `E` without touching `D`.
    pub fn set_e(&mut self, value: u8) {
        self.de = (self.de & 0xFF00) | u16::from(value);
    }

    /// The `H` half of `HL`.
    pub fn h(&self) -> u8 {
        (self.hl >> 8) as u8
    }

    /// Load `H` without touching `L`.
    pub fn set_h(&mut self, value: u8) {
        self.hl = (u16::from(value) << 8) | (self.hl & 0xFF);
    }

    /// The `L` half of `HL`.
    pub fn l(&self) -> u8 {
        self.hl as u8
    }

    /// Load `L` without touching `H`.
    pub fn set_l(&mut self, value: u8) {
        self.hl = (self.hl & 0xFF00) | u16::from(value);
    }

    /// The `H` half of `IX`.
    pub fn ix_hi(&self) -> u8 {
        (self.ix >> 8) as u8
    }

    /// Load the high half of `IX` without touching the low half.
    pub fn set_ix_hi(&mut self, value: u8) {
        self.ix = (u16::from(value) << 8) | (self.ix & 0xFF);
    }

    /// The `L` half of `IX`.
    pub fn ix_lo(&self) -> u8 {
        self.ix as u8
    }

    /// Load the low half of `IX` without touching the high half.
    pub fn set_ix_lo(&mut self, value: u8) {
        self.ix = (self.ix & 0xFF00) | u16::from(value);
    }

    /// The `H` half of `IY`.
    pub fn iy_hi(&self) -> u8 {
        (self.iy >> 8) as u8
    }

    /// Load the high half of `IY` without touching the low half.
    pub fn set_iy_hi(&mut self, value: u8) {
        self.iy = (u16::from(value) << 8) | (self.iy & 0xFF);
    }

    /// The `L` half of `IY`.
    pub fn iy_lo(&self) -> u8 {
        self.iy as u8
    }

    /// Load the low half of `IY` without touching the high half.
    pub fn set_iy_lo(&mut self, value: u8) {
        self.iy = (self.iy & 0xFF00) | u16::from(value);
    }

    /// The interrupt vector base for `IM 2`.
    pub fn i(&self) -> u8 {
        self.i
    }

    /// Load the interrupt vector base (`LD I,A`).
    pub fn set_i(&mut self, value: u8) {
        self.i = value;
    }

    /// The refresh counter as the pin sees it.
    pub fn r(&self) -> u8 {
        self.r
    }

    /// Load `R` including bit 7, which only `LD R,A` may change.
    pub fn set_r(&mut self, value: u8) {
        self.r = value;
    }

    /// Count `R` for one opcode or prefix fetch, keeping bit 7.
    pub fn inc_r(&mut self) {
        self.r = incremented_r(self.r);
    }

    /// The master interrupt flip-flop: whether an `INT` is accepted at all.
    pub fn iff1(&self) -> bool {
        self.iff1
    }

    /// Set `IFF1` (`EI`, `DI`, `LD A,I`).
    pub fn set_iff1(&mut self, asserted: bool) {
        self.iff1 = asserted;
    }

    /// The shadow flip-flop, which `LD A,I` exposes and `RETI` restores into
    /// `IFF1`.
    pub fn iff2(&self) -> bool {
        self.iff2
    }

    /// Set `IFF2`.
    pub fn set_iff2(&mut self, asserted: bool) {
        self.iff2 = asserted;
    }

    /// The current interrupt mode.
    pub fn im(&self) -> IntMode {
        self.im
    }

    /// Set the interrupt mode (`IM 0`, `IM 1`, `IM 2`).
    pub fn set_im(&mut self, mode: IntMode) {
        self.im = mode;
    }

    /// Whether `HALT` has stopped the fetch loop.
    pub fn halted(&self) -> bool {
        self.halted
    }

    /// Enter or leave the `HALT` state.
    pub fn set_halted(&mut self, halted: bool) {
        self.halted = halted;
    }

    /// The hidden `WZ` register (MEMPTR).
    pub fn wz(&self) -> u16 {
        self.wz
    }

    /// Load `WZ`, for the conformance harness and save states.
    pub fn set_wz(&mut self, value: u16) {
        self.wz = value;
    }

    /// What an instruction that computes no flags does on the way out: `F` stays
    /// exactly as it was and every latch ends clear.
    ///
    /// The suite shows `final.q == 0`, `final.p == 0` and `final.ei == 0` for all
    /// 1000 cases of every LD (#47) and every 16-bit LD, `PUSH`/`POP` and `EX`
    /// file (#48) - including `POP AF`, which does write `F`, straight out of
    /// memory. So `q` records the flags an instruction *computed*, not every
    /// write to `F`.
    pub(crate) fn clear_latches(&mut self) {
        self.q = 0;
        self.p = false;
        self.ei_pending = false;
    }

    /// The internal X/Y source byte (undocumented `q`).
    pub fn q(&self) -> u8 {
        self.q
    }

    /// Set the internal X/Y source byte (undocumented `q`).
    pub fn set_q(&mut self, value: u8) {
        self.q = value;
    }

    /// The `p` flag: whether the last instruction was `LD A,I` or `LD A,R`.
    pub fn p(&self) -> bool {
        self.p
    }

    /// Set `p`.
    pub fn set_p(&mut self, set: bool) {
        self.p = set;
    }

    /// Whether `EI` has an interrupt gate opening after the next instruction.
    pub fn ei_pending(&self) -> bool {
        self.ei_pending
    }

    /// Arm or clear the one-instruction interrupt delay `EI` imposes.
    pub fn set_ei_pending(&mut self, pending: bool) {
        self.ei_pending = pending;
    }

    /// Read one flag bit, e.g. `get_flag(FLAG_C)`.
    pub fn get_flag(&self, mask: u8) -> bool {
        self.f() & mask != 0
    }

    /// Set or clear one flag bit.
    pub fn set_flag(&mut self, mask: u8, on: bool) {
        let flags = if on {
            self.f() | mask
        } else {
            self.f() & !mask
        };
        self.set_flags(flags);
    }

    /// Merge freshly computed flags into `F`, keeping the bits the instruction
    /// did not claim.
    ///
    /// The Z80's flag semantics are per-bit: an instruction like `INC` leaves
    /// `C` alone and fakes `H` from a different pair of operands, so each op
    /// passes the mask of bits it actually established (`valid`) alongside the
    /// values for all eight (`affected`). Getting this wrong in one place shows
    /// up as a stale flag two instructions later, which is what the mask is
    /// there to prevent.
    pub fn update_flags(&mut self, valid: u8, affected: u8) {
        self.set_flags((self.f() & !valid) | (affected & valid));
    }
}

impl Default for Z80 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KiB of flat memory, enough to hold the fetches these tests make.
    #[derive(Default)]
    struct FlatBus {
        ram: Vec<u8>,
        writes: Vec<(u16, u8)>,
        reads: Vec<u16>,
    }

    impl FlatBus {
        fn new() -> Self {
            Self {
                ram: vec![0; 0x1_0000],
                writes: Vec::new(),
                reads: Vec::new(),
            }
        }
    }

    impl Z80Bus for FlatBus {
        fn read(&mut self, addr: u16) -> u8 {
            self.reads.push(addr);
            self.ram[usize::from(addr)]
        }

        fn write(&mut self, addr: u16, value: u8) {
            self.writes.push((addr, value));
            self.ram[usize::from(addr)] = value;
        }

        fn io_read(&mut self, port: u16) -> u8 {
            (port >> 8) as u8
        }

        fn io_write(&mut self, port: u16, value: u8) {
            self.writes.push((port, value));
        }
    }

    #[test]
    fn new_is_reset_state() {
        let cpu = Z80::new();
        assert_eq!(cpu.pc(), 0);
        assert_eq!(cpu.sp(), 0xFFFF);
        assert_eq!(cpu.af(), 0xFFFF);
        assert!(!cpu.iff1());
        assert!(!cpu.iff2());
        assert_eq!(cpu.im(), IntMode::Mode0);
        assert!(!cpu.halted());
    }

    #[test]
    fn reset_clears_everything_new_leaves_at_zero() {
        let mut cpu = Z80::new();
        cpu.set_pc(0x1234);
        cpu.set_sp(0x2000);
        cpu.set_af(0);
        cpu.set_bc(0xBEEF);
        cpu.set_de(0xDEAD);
        cpu.set_hl(0xF00D);
        cpu.set_af_shadow(0x1111);
        cpu.set_bc_shadow(0x2222);
        cpu.set_de_shadow(0x3333);
        cpu.set_hl_shadow(0x4444);
        cpu.set_ix(0x5555);
        cpu.set_iy(0x6666);
        cpu.set_i(0x89);
        cpu.set_r(0xAB);
        cpu.set_iff1(true);
        cpu.set_iff2(true);
        cpu.set_im(IntMode::Mode2);
        cpu.set_halted(true);
        cpu.set_wz(0x7777);
        cpu.set_q(0xC5);
        cpu.set_p(true);
        cpu.set_ei_pending(true);

        cpu.reset();

        assert_eq!(cpu.pc(), 0);
        assert_eq!(cpu.sp(), 0xFFFF);
        assert_eq!(cpu.af(), 0xFFFF);
        assert_eq!(cpu.bc(), 0);
        assert_eq!(cpu.de(), 0);
        assert_eq!(cpu.hl(), 0);
        assert_eq!(cpu.af_shadow(), 0);
        assert_eq!(cpu.bc_shadow(), 0);
        assert_eq!(cpu.de_shadow(), 0);
        assert_eq!(cpu.hl_shadow(), 0);
        assert_eq!(cpu.ix(), 0);
        assert_eq!(cpu.iy(), 0);
        assert_eq!(cpu.i(), 0);
        assert_eq!(cpu.r(), 0);
        assert!(!cpu.iff1());
        assert!(!cpu.iff2());
        assert_eq!(cpu.im(), IntMode::Mode0);
        assert!(!cpu.halted());
        assert_eq!(cpu.wz(), 0);
        assert_eq!(cpu.q(), 0);
        assert!(!cpu.p());
        assert!(!cpu.ei_pending());
    }

    #[test]
    fn pair_accessors_roundtrip() {
        let mut cpu = Z80::new();

        cpu.set_bc(0x1234);
        assert_eq!(cpu.bc(), 0x1234);
        assert_eq!(cpu.b(), 0x12);
        assert_eq!(cpu.c(), 0x34);

        cpu.set_de(0x5678);
        assert_eq!(cpu.de(), 0x5678);
        assert_eq!(cpu.d(), 0x56);
        assert_eq!(cpu.e(), 0x78);

        cpu.set_hl(0x9ABC);
        assert_eq!(cpu.hl(), 0x9ABC);
        assert_eq!(cpu.h(), 0x9A);
        assert_eq!(cpu.l(), 0xBC);

        cpu.set_af(0xDEF1);
        assert_eq!(cpu.af(), 0xDEF1);
        assert_eq!(cpu.a(), 0xDE);
        assert_eq!(cpu.f(), 0xF1);

        cpu.set_ix(0x0F0F);
        assert_eq!(cpu.ix(), 0x0F0F);
        assert_eq!(cpu.ix_hi(), 0x0F);
        assert_eq!(cpu.ix_lo(), 0x0F);

        cpu.set_iy(0xF0F0);
        assert_eq!(cpu.iy(), 0xF0F0);
        assert_eq!(cpu.iy_hi(), 0xF0);
        assert_eq!(cpu.iy_lo(), 0xF0);

        cpu.set_sp(0xFFFE);
        assert_eq!(cpu.sp(), 0xFFFE);
        cpu.set_pc(0x0001);
        assert_eq!(cpu.pc(), 0x0001);
    }

    #[test]
    fn shadow_pair_accessors_roundtrip() {
        let mut cpu = Z80::new();
        cpu.set_af_shadow(0x1234);
        cpu.set_bc_shadow(0x5678);
        cpu.set_de_shadow(0x9ABC);
        cpu.set_hl_shadow(0xDEF0);
        assert_eq!(cpu.af_shadow(), 0x1234);
        assert_eq!(cpu.bc_shadow(), 0x5678);
        assert_eq!(cpu.de_shadow(), 0x9ABC);
        assert_eq!(cpu.hl_shadow(), 0xDEF0);
        // Shadows are a separate bank: EXX has nothing to disturb here.
        assert_eq!(cpu.af(), 0xFFFF);
        assert_eq!(cpu.bc(), 0);
        assert_eq!(cpu.de(), 0);
        assert_eq!(cpu.hl(), 0);
    }

    #[test]
    fn writing_a_half_keeps_the_other_half() {
        let mut cpu = Z80::new();
        cpu.set_hl(0x1234);
        cpu.set_h(0x9F);
        assert_eq!(cpu.hl(), 0x9F34);
        cpu.set_l(0x7E);
        assert_eq!(cpu.hl(), 0x9F7E);

        cpu.set_a(0x42);
        assert_eq!(cpu.af(), 0x42FF, "the flags must survive LD A,n");

        cpu.set_flags(0x01);
        assert_eq!(cpu.af(), 0x4201, "and LD F must not disturb A");
    }

    #[test]
    fn flag_helpers() {
        let mut cpu = Z80::new();
        cpu.set_flags(0);
        assert!(!cpu.get_flag(FLAG_C));

        cpu.set_flag(FLAG_C, true);
        assert!(cpu.get_flag(FLAG_C));
        assert!(!cpu.get_flag(FLAG_Z));
        assert_eq!(cpu.f(), FLAG_C);

        cpu.set_flag(FLAG_Z, true);
        cpu.set_flag(FLAG_C, false);
        assert_eq!(cpu.f(), FLAG_Z);

        // Every documented and undocumented bit is reachable.
        for mask in [
            FLAG_S, FLAG_Z, FLAG_Y, FLAG_H, FLAG_X, FLAG_PV, FLAG_N, FLAG_C,
        ] {
            cpu.set_flags(0);
            cpu.set_flag(mask, true);
            assert!(cpu.get_flag(mask), "mask {mask:#04X}");
            assert_eq!(cpu.f(), mask);
        }
    }

    #[test]
    fn update_flags_only_touches_claimed_bits() {
        let mut cpu = Z80::new();
        // Carry set, everything else clear: the state `ADC` cares about.
        cpu.set_flags(FLAG_C);

        // An op that establishes S, Z and P/V but says nothing about H, N, C.
        cpu.update_flags(FLAG_S | FLAG_Z | FLAG_PV, FLAG_S | FLAG_PV);

        assert_eq!(cpu.f(), FLAG_C | FLAG_S | FLAG_PV);
        // And a second op that claims C clears it without disturbing the rest.
        cpu.update_flags(FLAG_C, 0);
        assert_eq!(cpu.f(), FLAG_S | FLAG_PV);
    }

    #[test]
    fn hidden_state_accessors() {
        let mut cpu = Z80::new();
        assert_eq!(cpu.wz(), 0);
        assert_eq!(cpu.q(), 0);
        assert!(!cpu.p());
        assert!(!cpu.ei_pending());

        cpu.set_wz(0x1A2B);
        cpu.set_q(0xC5);
        cpu.set_p(true);
        cpu.set_ei_pending(true);

        assert_eq!(cpu.wz(), 0x1A2B);
        assert_eq!(cpu.q(), 0xC5);
        assert!(cpu.p());
        assert!(cpu.ei_pending());
    }

    #[test]
    fn refresh_counter_helpers() {
        let mut cpu = Z80::new();
        cpu.set_r(0xFF);
        cpu.inc_r();
        assert_eq!(cpu.r(), 0x80);
        cpu.set_r(0x7F);
        cpu.inc_r();
        assert_eq!(cpu.r(), 0x00);
        cpu.set_r(0x80);
        cpu.inc_r();
        assert_eq!(cpu.r(), 0x81, "bit 7 survives the refresh");
    }

    #[test]
    fn step_stub_behaves_like_nop() {
        let mut cpu = Z80::new();
        let mut bus = FlatBus::new();
        cpu.set_pc(0x0100);
        cpu.set_r(0x8F);
        cpu.set_q(0xC5);
        cpu.set_p(true);
        cpu.set_ei_pending(true);
        cpu.set_wz(0x2345);
        bus.ram[0x0100] = 0x00;

        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(cpu.pc(), 0x0101);
        assert_eq!(bus.reads, vec![0x0100]);
        // A stub must not smuggle in side effects the instruction tickets will
        // have to reproduce.
        assert!(bus.writes.is_empty());

        // What the suite demands of NOP itself: R counts once, the latches are
        // consumed, and nothing else moves.
        assert_eq!(cpu.r(), 0x90);
        assert_eq!(cpu.q(), 0);
        assert!(!cpu.p());
        assert!(!cpu.ei_pending());
        assert_eq!(cpu.wz(), 0x2345);
        assert_eq!(cpu.f(), 0xFF, "NOP leaves the flags alone");
    }
}
