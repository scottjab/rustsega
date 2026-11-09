//! Opcode fetch and the dispatch tree for every Z80 opcode space.
//!
//! Every instruction ticket from here on fills in a match arm here rather than
//! touching `step()`. The spaces are the unprefixed main space, `CB`, `ED`,
//! `DD`/`FD` (which make `HL` mean `IX`/`IY`), and `DD CB d op`/`FD CB d op`,
//! where the displacement comes *before* the opcode byte that picks the
//! operation.
//!
//! The recommended decomposition for the arms is the x/y/z/p/q split from
//! "Decoding Z80 Opcodes" (<https://www.z80.info/decoding.htm>): x = `op >> 6`,
//! y = `(op >> 3) & 7`, z = `op & 7`, p = `y >> 1`, q = `y & 1`. Most of the
//! space is a table lookup once you have those five numbers, and the register
//! index `r[]` is `B, C, D, E, H, L, (HL), A` - which is why [`IndexMode`]
//! rather than a match on `7` is threaded through the index helpers.

use crate::{Z80, Z80Bus};

/// Prefix bytes: they switch opcode space instead of executing.
const OP_CB: u8 = 0xCB;
const OP_ED: u8 = 0xED;
/// `HALT`, which sits in the middle of the `LD r,r'` block. M2-13 (#56)
/// implements it; until then it must panic rather than be read as a load.
const OP_HALT: u8 = 0x76;
const OP_DD: u8 = 0xDD;
const OP_FD: u8 = 0xFD;

/// `NOP`, and the shape an undefined or rotted-out opcode falls back to.
const OP_NOP: u8 = 0x00;

/// T-states charged for one M1 opcode fetch.
const T_M1: u32 = 4;

/// What `HL` means for the instruction being decoded.
///
/// `DD` and `FD` swap `H`/`L`/`(HL)` for `IXH`/`IXL`/`(IX+d)` and the IY
/// equivalents, so the load/ALU helpers take one of these instead of matching
/// on the opcode everywhere, and `(HL)` and `(IX+d)` share one code path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexMode {
    /// No prefix.
    Hl,
    /// `DD` prefix: `HL` means `IX`.
    Ix,
    /// `FD` prefix: `HL` means `IY`.
    Iy,
}

impl IndexMode {
    /// Which prefix byte selects this mode, for panic messages.
    fn prefix(self) -> &'static str {
        match self {
            Self::Hl => "",
            Self::Ix => "DD",
            Self::Iy => "FD",
        }
    }
}

/// `x` from the x/y/z decomposition: the top two bits, the major group.
pub fn x(op: u8) -> u8 {
    op >> 6
}

/// `y`: the middle three bits.
pub fn y(op: u8) -> u8 {
    (op >> 3) & 7
}

/// `z`: the low three bits.
pub fn z(op: u8) -> u8 {
    op & 7
}

/// `p`: `y` with its low bit removed, the register-pair half of `y`.
//
// `p`, `q` and `fetch_word` are here for the instruction tickets to use (the
// 16-bit group at `x=0` is all `p`/`q` and a word operand), not dead weight:
// nothing calls them until M2-04 and M2-05 (#47, #48).
#[allow(dead_code)]
pub fn p(op: u8) -> u8 {
    y(op) >> 1
}

/// `q`: the low bit of `y`, which splits each register-pair group in two.
#[allow(dead_code)]
pub fn q(op: u8) -> u8 {
    y(op) & 1
}

/// The register index in the `r[]` table: `B, C, D, E, H, L, (HL), A`.
///
/// Index 6 is `(HL)`, so callers must handle that one as a memory access.
//
// The two panics that named operands with this table were the `CB` and
// `DD CB` ones, and both are real instructions as of M2-09 (#52). The
// main-space panic (M2-13, #56) is the next caller; until then only
// `decomposition_matches_the_reference` below reads it.
#[allow(dead_code)]
pub const REG_TABLE: [&str; 8] = ["B", "C", "D", "E", "H", "L", "(HL)", "A"];

/// Is this an `ED` opcode that is actually defined?
///
/// The `ED` space is mostly empty, and the empty part is not an error: an
/// undefined `ED` opcode behaves as an 8 T-state NOP, two M1 fetches worth.
/// The ranges here are the union of what the hardware defines and what the
/// SingleStepTests suite exercises: it ships `ED 40..=7F` and `ED A0..=A3`,
/// `A8..=AB`, `B0..=B3`, `B8..=BB` and nothing else (`v1` has 80 `ED` files,
/// and there is no `ed 00.json`).
fn ed_is_defined(op: u8) -> bool {
    matches!(op, 0x40..=0x7F | 0xA0..=0xA3 | 0xA8..=0xAB | 0xB0..=0xB3 | 0xB8..=0xBB)
}

impl Z80 {
    /// Fetch the opcode at `PC` and execute one instruction.
    pub(crate) fn exec(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let op = self.fetch_opcode(bus);
        self.exec_main(bus, op)
    }

    /// Fetch the next opcode byte.
    ///
    /// This is an M1 fetch, so it is the one place the `R` increment belongs:
    /// `R` counts opcode fetches, not bytes read. Getting this wrong is how a
    /// core ends up two cycles out on every prefixed instruction.
    pub(crate) fn fetch_opcode(&mut self, bus: &mut impl Z80Bus) -> u8 {
        let op = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        self.inc_r();
        op
    }

    /// Fetch an operand byte. Not an M1 fetch, so `R` does not move.
    pub(crate) fn fetch_byte(&mut self, bus: &mut impl Z80Bus) -> u8 {
        let value = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    /// Fetch a 16-bit operand, low byte first, as every `LD rr,nn`-style
    /// instruction stores it.
    #[allow(dead_code)] // first caller is M2-04 (#47)
    pub(crate) fn fetch_word(&mut self, bus: &mut impl Z80Bus) -> u16 {
        let low = u16::from(self.fetch_byte(bus));
        let high = u16::from(self.fetch_byte(bus));
        (high << 8) | low
    }

    /// Dispatch the main opcode space. `op` has already been fetched, so `R` is
    /// already counted for it.
    fn exec_main(&mut self, bus: &mut impl Z80Bus, op: u8) -> u32 {
        match op {
            OP_NOP => self.nop(),
            OP_CB => self.exec_cb(bus),
            OP_ED => self.exec_ed(bus),
            OP_DD => self.exec_index(bus, IndexMode::Ix),
            OP_FD => self.exec_index(bus, IndexMode::Iy),
            // The 8-bit loads (M2-04, #47). `02`/`12` store A into the pair's
            // address, `0A`/`1A` read it back.
            0x02 => {
                let addr = self.bc();
                self.ld_ind_rr_a(bus, addr)
            }
            0x12 => {
                let addr = self.de();
                self.ld_ind_rr_a(bus, addr)
            }
            0x0A => {
                let addr = self.bc();
                self.ld_a_ind_rr(bus, addr)
            }
            0x1A => {
                let addr = self.de();
                self.ld_a_ind_rr(bus, addr)
            }
            0x32 => self.ld_ind_nn_a(bus),
            0x3A => self.ld_a_ind_nn(bus),
            0x36 => self.ld_hl_ind_n(bus),
            // `LD r,r'` is the whole `40`-`7F` block except `76`, which is HALT
            // and belongs to M2-13 (#56).
            OP_HALT => self.unimplemented(&format!("main {op:02X} (HALT)")),
            0x40..=0x7F => self.ld_rr(bus, op),
            // `LD r,n`: the immediate loads, named out rather than derived from
            // `x`/`z` so the arm does not depend on what is matched above it.
            // `y(op)` is which register gets the byte. `36` (`LD (HL),n`) is the
            // eighth of these and is matched above.
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x3E => self.ld_r_n(bus, op),
            // The 16-bit loads, the stack and the exchanges (M2-05, #48). Bits
            // 5-4 of `LD rr,nn`, `PUSH qq` and `POP qq` pick the pair; see
            // `Z80::set_pair_y` for the mapping, which differs between the
            // groups (`11` is `SP` for the load, `AF` for the stack).
            0x01 | 0x11 | 0x21 | 0x31 => self.ld_rr_nn(bus, op),
            0xC1 | 0xD1 | 0xE1 | 0xF1 => self.pop_qq(bus, op),
            0xC5 | 0xD5 | 0xE5 | 0xF5 => self.push_qq(bus, op),
            0x22 => self.ld_ind_nn_hl(bus),
            0x2A => self.ld_hl_ind_nn(bus),
            0xF9 => self.ld_sp_hl(),
            // The 16-bit arithmetic (M2-07, #50). Bits 5-4 pick the operand
            // pair in `ADD HL,rr` and the pair to bump in `INC`/`DEC rr`, so
            // one helper each covers all four opcodes.
            0x09 | 0x19 | 0x29 | 0x39 => self.add_hl_rr(op, IndexMode::Hl),
            0x03 | 0x13 | 0x23 | 0x33 => self.inc_rr(op, IndexMode::Hl),
            0x0b | 0x1b | 0x2b | 0x3b => self.dec_rr(op, IndexMode::Hl),
            // The 8-bit ALU (M2-06, #49). `y` is the operation in the ALU's own
            // order (ADD, ADC, SUB, SBC, AND, XOR, OR, CP) and `z` the operand
            // slot, so `80`-`BF` and the eight immediates share one helper.
            0x80..=0xBF => self.alu_a_r(bus, op),
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => self.alu_a_n(bus, op),
            // 8-bit `INC`/`DEC`: `y` is again the register slot, with `z` 4 for
            // `INC` and 5 for `DEC`.
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x3C => self.inc_reg(op),
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x3D => self.dec_reg(op),
            0x34 => self.inc_hl_ind(bus),
            0x35 => self.dec_hl_ind(bus),
            // The accumulator rotates (M2-08, #51). `y(op)` is the rotate in the
            // `CB` shift table's order (RLC, RRC, RL, RR), so `07 0F 17 1F` are
            // the table's first four entries and need no opcode list of their own.
            0x07 | 0x0F | 0x17 | 0x1F => self.rotate_accumulator(op),
            0x08 => self.ex_af_af(),
            0xD9 => self.exx(),
            0xEB => self.ex_de_hl(),
            0xE3 => self.ex_sp_index(bus, IndexMode::Hl),
            _ => self.unimplemented(&format!("main {op:02X} (x{} y{} z{})", x(op), y(op), z(op))),
        }
    }

    /// `NOP`: nothing moves except the side effects every instruction gets.
    ///
    /// The `R` increment and the `PC` advance happened in the fetch. The three
    /// latches are cleared here because that is what the suite says: across all
    /// 1000 randomised `00.json` cases, `q`, `p` and the EI latch all end clear
    /// whatever they started as. `NOP` does *not* apply the EI latch to `IFF1`,
    /// even in the 261 cases that start with the latch set and `IFF1` clear, so
    /// whoever wires interrupt acceptance (M2-14, #57) owns that step.
    fn nop(&mut self) -> u32 {
        self.q = 0;
        self.p = false;
        self.ei_pending = false;
        T_M1
    }

    /// Dispatch the `CB` space. The prefix was an M1 fetch, and so is the byte
    /// after it: the suite's `cb 00` cases show `R` up by 2 and `PC` up by 2.
    fn exec_cb(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let op = self.fetch_opcode(bus);
        match op {
            // Rotates and shifts (M2-08, #51): `y` is the operation, `z` the
            // operand slot with 6 meaning the memory at `HL`.
            0x00..=0x3F => self.rot_shift(bus, op),
            // `BIT`/`RES`/`SET` (M2-09, #52): `y` is the bit number, `z` the
            // operand slot, and `x` says which of the three it is.
            0x40..=0x7F => self.bit_test(bus, op),
            0x80..=0xFF => self.res_set(bus, op),
        }
    }

    /// Dispatch the `ED` space.
    ///
    /// The 16-bit pair loads (M2-05, #48) are dispatched here. Anything else
    /// that is defined but not implemented yet panics. Undefined opcodes are
    /// not an error at all: they behave as an 8 T-state NOP, and the byte after
    /// `ED` is still fetched, which is why `R` counts 2.
    fn exec_ed(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let op = self.fetch_opcode(bus);
        match op {
            // `LD rr,(nn)` and `LD (nn),rr` for the four main pairs
            // (M2-05, #48). These return the whole 20 T-states, this fetch of
            // `ED` included, so the timing is checkable against the suite.
            0x4B | 0x5B | 0x6B | 0x7B => self.ld_rr_ind_nn(bus, op),
            0x43 | 0x53 | 0x63 | 0x73 => self.ld_ind_nn_rr(bus, op),
            // `ADC HL,rr` and `SBC HL,rr` (M2-07, #50): 15 T-states, this fetch
            // of `ED` included. Bit 3 says which way round they are, `0` being
            // `SBC`; `ed 42` is `SBC HL,BC` and `ed 4a` is `ADC HL,BC`.
            0x42 | 0x4a | 0x52 | 0x5a | 0x62 | 0x6a | 0x72 | 0x7a => self.adc_sbc_hl_rr(op),
            _ if ed_is_defined(op) => {
                self.unimplemented(&format!("ED {op:02X} (x{} y{} z{})", x(op), y(op), z(op)))
            }
            _ => T_M1 + self.nop(),
        }
    }

    /// Dispatch `DD`/`FD`: read the next byte and work out whether it is an
    /// indexed instruction, another prefix, or something the prefix cannot touch.
    ///
    /// A prefix that turns out not to apply is not dropped: it decays into a 4
    /// T-state `NOP` and the following byte starts a fresh instruction. That is
    /// why `DD 00` is 8 T-states with `PC` up by 2 and `R` up by 2 (the suite's
    /// `dd 00`/`fd 00` agree), and why chained prefixes cost 4 each with the
    /// last one winning.
    fn exec_index(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let op = self.fetch_opcode(bus);
        match op {
            // The indexed bit-shift space: the displacement comes first, then
            // the opcode that says what to do.
            OP_CB => self.exec_index_cb(bus, mode),
            // Chained prefixes: this one rots, the next one takes over.
            OP_DD => T_M1 + self.exec_index(bus, IndexMode::Ix),
            OP_FD => T_M1 + self.exec_index(bus, IndexMode::Iy),
            // `DD ED ...`: the prefix rots and `ED` executes normally, since `ED`
            // instructions never mention `HL`.
            OP_ED => T_M1 + self.exec_ed(bus),
            // Same reasoning: `NOP` does not mention `HL`, so the prefix is
            // wasted and the `NOP` runs.
            OP_NOP => T_M1 + self.nop(),
            // The 8-bit indexed loads (M2-04, #47). The rest of this space is
            // either a later ticket or the undocumented IXH/IXL forms
            // (M2-15, #58), which stay a panic.
            0x36 => self.ld_index_ind_n(bus, mode),
            0x46 | 0x4E | 0x56 | 0x5E | 0x66 | 0x6E | 0x7E => self.ld_r_ind_index(bus, mode, op),
            // `70`-`77` in this space is `LD (IX+d),r` - except `76`, which the
            // suite insists is still HALT: `dd 76` takes 8 T-states with `PC` up
            // by 2 and no displacement fetched, i.e. a rotted prefix followed by
            // a real HALT, not `LD (IX+d),(HL)`. So it falls through to the
            // prefix fallback and HALT (M2-13, #56) picks it up there.
            0x70..=0x75 | 0x77 => self.ld_index_ind_r(bus, mode, op),
            // The 16-bit loads, stack and exchanges in this space (M2-05, #48).
            // `22`/`2A` move the index pair through memory rather than `HL`; see
            // `Z80::ld_index_ind_nn` for the traces that settled that.
            // The 8-bit ALU and `INC`/`DEC` against `(IX+d)` (M2-06, #49). The
            // ALU needs `z = 6`, which is the only indexed ALU encoding; the
            // rest of `DD 80`-`DD BF` is the undocumented `IXH`/`IXL` set
            // (M2-15, #58) and stays a panic.
            0x86 | 0x8E | 0x96 | 0x9E | 0xA6 | 0xAE | 0xB6 | 0xBE => {
                self.alu_a_index(bus, mode, op)
            }
            0x34 => self.inc_index_ind(bus, mode),
            0x35 => self.dec_index_ind(bus, mode),
            0x21 => self.ld_index_nn(bus, mode),
            0x22 => self.ld_ind_nn_index(bus, mode),
            0x2A => self.ld_index_ind_nn(bus, mode),
            0xE1 => self.pop_index(bus, mode),
            0xE3 => self.ex_sp_index(bus, mode),
            0xE5 => self.push_index(bus, mode),
            0xF9 => self.ld_sp_index(mode),
            // The 16-bit arithmetic that mentions `HL` (M2-07, #50). Pair field
            // `2` is the index register here, so `DD 29` is `ADD IX,IX` and the
            // `INC`/`DEC` that reach it are `23`/`2B`.
            0x09 | 0x19 | 0x29 | 0x39 => self.add_hl_rr(op, mode),
            0x23 => self.inc_rr(op, mode),
            0x2b => self.dec_rr(op, mode),
            // `INC BC` and friends never mention `HL`, so the prefix has
            // nothing to index and rots into a `NOP` in front of the unprefixed
            // instruction (`dd 03`: 10 T-states, `PC + 2`, `R + 2`, `BC`
            // incremented, in all 1000 cases).
            0x03 | 0x13 | 0x33 | 0x0b | 0x1b | 0x3b => T_M1 + self.exec_main(bus, op),
            // `EXX` and `EX DE,HL` have nothing for the prefix to index, so it
            // rots into a NOP and the instruction runs (`dd d9` / `dd eb`: 8
            // T-states, `PC + 2`, in all 1000 cases of each).
            0xD9 | 0xEB => T_M1 + self.exec_main(bus, op),
            _ => self.unimplemented(&format!(
                "{} {op:02X} (x{} y{} z{})",
                mode.prefix(),
                x(op),
                y(op),
                z(op)
            )),
        }
    }

    /// Dispatch `DD CB d op` / `FD CB d op`.
    ///
    /// `d` and `op` are operand fetches, not M1 fetches: by the time this runs,
    /// `R` has already counted the prefix and the `CB`, which is the total of 2
    /// the suite's `dd cb __ 00` cases expect.
    fn exec_index_cb(&mut self, bus: &mut impl Z80Bus, mode: IndexMode) -> u32 {
        let disp = self.fetch_byte(bus);
        let op = self.fetch_byte(bus);
        self.index_cb(bus, mode, disp, op)
    }

    /// The single exit for a space we have not implemented yet.
    ///
    /// Panicking rather than silently doing nothing is deliberate: it turns a
    /// missing instruction into a line saying which opcode is missing, instead
    /// of a register mismatch 40 fields later. The conformance harness catches
    /// these, so a whole-suite run still reports per file instead of aborting.
    fn unimplemented(&self, opcode: &str) -> ! {
        panic!(
            "unimplemented z80 opcode {opcode}; PC is now {:#06X}",
            self.pc
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FLAG_C;

    /// A bus that reads back a fixed program, so a test can name the exact bytes
    /// an instruction is made of and see what `step()` did with them.
    struct Program {
        ram: Vec<u8>,
        reads: Vec<u16>,
        /// Memory writes, in order. `DD CB d op` on `(IX + d)` is the first
        /// instruction in this file that writes; before it the bus could afford
        /// to be read-only.
        writes: Vec<(u16, u8)>,
    }

    impl Program {
        fn new(bytes: &[u8]) -> Self {
            let mut ram = vec![0; 0x1_0000];
            ram[..bytes.len()].copy_from_slice(bytes);
            Self {
                ram,
                reads: Vec::new(),
                writes: Vec::new(),
            }
        }
    }

    impl Z80Bus for Program {
        fn read(&mut self, addr: u16) -> u8 {
            self.reads.push(addr);
            self.ram[usize::from(addr)]
        }

        fn write(&mut self, addr: u16, value: u8) {
            self.writes.push((addr, value));
            self.ram[usize::from(addr)] = value;
        }

        fn io_read(&mut self, _port: u16) -> u8 {
            panic!("no instruction implemented so far should read a port");
        }

        fn io_write(&mut self, _port: u16, _value: u8) {
            panic!("no instruction implemented so far should write a port");
        }
    }

    /// Run one step, returning the T-states and the panics the fetch may have
    /// already caused. Used by the tests that care about what happened *before*
    /// an unimplemented opcode gave up.
    fn caught(step: impl FnOnce() -> u32) -> Result<u32, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).map_err(|payload| {
            if let Some(msg) = payload.downcast_ref::<String>() {
                msg.clone()
            } else if let Some(msg) = payload.downcast_ref::<&str>() {
                (*msg).to_string()
            } else {
                "panic with no message".to_string()
            }
        })
    }

    #[test]
    fn nop_takes_4_and_bumps_r() {
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0x00]);
        assert_eq!(cpu.step(&mut bus), 4);
        assert_eq!(cpu.pc(), 1);
        assert_eq!(cpu.r(), 1);
        assert_eq!(bus.reads, vec![0]);
    }

    #[test]
    fn undefined_ed_is_8t_nop() {
        // `ED 00` is not an opcode. It is still two bytes and two M1 fetches,
        // so it is 8 T-states of nothing.
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0xED, 0x00]);
        assert_eq!(cpu.step(&mut bus), 8);
        assert_eq!(cpu.pc(), 2);
        assert_eq!(cpu.r(), 2);
    }

    #[test]
    fn ed_defined_range_panics_rather_than_nopping() {
        // The fallback above must only cover undefined opcodes. `ED 40` (LDI) is
        // real, so quietly NOPing it would hide a missing instruction.
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0xED, 0x40]);
        let message = caught(|| cpu.step(&mut bus)).expect_err("ED 40 is not implemented");
        assert!(message.contains("ED 40"), "{message}");
        for op in [0x40u8, 0x7F, 0xA0, 0xA3, 0xA8, 0xAB, 0xB0, 0xB3, 0xB8, 0xBB] {
            assert!(ed_is_defined(op), "ED {op:02X} should be defined");
        }
        for op in [
            0x00u8, 0x3F, 0x80, 0x9F, 0xA4, 0xA7, 0xAC, 0xB4, 0xBC, 0xC0, 0xFF,
        ] {
            assert!(!ed_is_defined(op), "ED {op:02X} should be undefined");
        }
    }

    #[test]
    fn dd_before_nop_is_prefix_nop() {
        // One step covers the prefix and the instruction it rotted into: 8
        // T-states, both bytes consumed, R counted for both. This is the model
        // the suite's `dd 00`/`fd 00` files use (PC +2, R +2, 8 cycles).
        for (prefix, name) in [(0xDD, "DD"), (0xFD, "FD")] {
            let mut cpu = Z80::new();
            let mut bus = Program::new(&[prefix, 0x00]);
            assert_eq!(cpu.step(&mut bus), 8, "{name} NOP");
            assert_eq!(cpu.pc(), 2, "{name} NOP");
            assert_eq!(cpu.r(), 2, "{name} NOP");
            assert_eq!(bus.reads, vec![0, 1], "{name} NOP");
        }
    }

    #[test]
    fn chained_prefixes_each_cost_a_nop_and_the_last_wins() {
        // No SingleStepTests file covers chained prefixes, so this pins the rule
        // from the ticket: each prefix is a 4 T-state NOP, the last one decides
        // the mode, and `DD ED` leaves `ED` to run on its own.
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0xDD, 0xDD, 0x00]);
        assert_eq!(cpu.step(&mut bus), 12);
        assert_eq!((cpu.pc(), cpu.r()), (3, 3));

        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0xDD, 0xFD, 0x00]);
        assert_eq!(cpu.step(&mut bus), 12);

        // `DD ED 00`: the DD rots, ED's undefined-opcode NOP takes over.
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0xDD, 0xED, 0x00]);
        assert_eq!(cpu.step(&mut bus), 12);
        assert_eq!((cpu.pc(), cpu.r()), (3, 3));

        // Last prefix wins, in both orders: `DD FD 24` is `INC IYH` and `FD DD 24`
        // is `INC IXH`, whichever way the prefixes are stacked. `24` is used
        // because the `IXH`/`IXL` half of this space is a later ticket (M2-15,
        // #58), so the panic still names the winning prefix.
        for (first, second, winner) in [(0xDD, 0xFD, "FD"), (0xFD, 0xDD, "DD")] {
            let mut cpu = Z80::new();
            let mut bus = Program::new(&[first, second, 0x24]);
            let message =
                caught(|| cpu.step(&mut bus)).expect_err("{second:02X} 24 is not implemented");
            assert!(message.contains(&format!("{winner} 24")), "{message}");
        }
    }

    #[test]
    fn r_counts_prefixes() {
        // Every prefix space increments R for the prefix and for the byte after
        // it, and never for operands.
        for prefix in [0xCB, 0xED, 0xDD, 0xFD] {
            let mut cpu = Z80::new();
            let mut bus = Program::new(&[prefix, 0x00]);
            // CB 00 is a real opcode; the others rot to NOP. What matters is
            // that R counted both bytes whichever way the dispatch went.
            let _ = caught(|| cpu.step(&mut bus));
            assert_eq!(cpu.r(), 2, "{prefix:02X} should have counted 2 fetches");
            assert_eq!(cpu.pc(), 2, "{prefix:02X} should have consumed 2 bytes");
        }
    }

    #[test]
    fn dd_cb_counts_the_prefix_and_cb_but_not_the_operands() {
        // `DD CB d op` is 4 bytes but only 2 M1 fetches, which is the R total of
        // 2 the suite's `dd cb __ 00` cases expect. The displacement is read
        // before the opcode, and both come before the operand the opcode names:
        // `0x06` is `RLC (IX + d)` with `z = 6`, and with `IX` at 0 that operand
        // is the byte at `0x0004`, the fifth byte of the program below. So the
        // read log is the whole instruction in fetch order, and the write is the
        // read-modify-write of that byte.
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0xDD, 0xCB, 0x04, 0x06, 0x81]);

        assert_eq!(cpu.step(&mut bus), 23, "RLC (IX + d) is 23 T-states");

        assert_eq!(cpu.r(), 2);
        assert_eq!(cpu.pc(), 4);
        assert_eq!(bus.reads, vec![0, 1, 2, 3, 4]);
        assert_eq!(bus.writes, vec![(4, 0x03)], "0x81 left, bit 7 back into bit 0");
        assert!(cpu.get_flag(FLAG_C), "bit 7 of 0x81 shifted out into C");
    }

    #[test]
    fn main_space_gives_a_useful_panic() {
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0x2F]); // CPL, which is M2-13 (#56)
        let message = caught(|| cpu.step(&mut bus)).expect_err("CPL is not implemented");
        assert!(
            message.contains("main 2F (x0 y5 z7)"),
            "unhelpful message: {message}"
        );
    }

    #[test]
    fn operands_are_little_endian_and_do_not_touch_r() {
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0x21, 0x34, 0x12]);
        let _ = caught(|| cpu.step(&mut bus));
        cpu.set_pc(1);
        assert_eq!(cpu.fetch_word(&mut bus), 0x1234);
        assert_eq!(cpu.pc(), 3);
        assert_eq!(cpu.r(), 1, "only the opcode fetch counted");
    }

    #[test]
    fn decomposition_matches_the_reference() {
        // z80.info/decoding.htm: the x/y/z/p/q split, and the r[] table order.
        assert_eq!((x(0x41), y(0x41), z(0x41)), (1, 0, 1));
        assert_eq!((x(0xB5), y(0xB5), z(0xB5)), (2, 6, 5));
        assert_eq!((p(0xB5), q(0xB5)), (3, 0));
        assert_eq!(REG_TABLE[6], "(HL)");
        assert_eq!(REG_TABLE[7], "A");
    }
}
