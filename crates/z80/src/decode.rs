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
        self.q = false;
        self.p = false;
        self.ei_pending = false;
        T_M1
    }

    /// Dispatch the `CB` space. The prefix was an M1 fetch, and so is the byte
    /// after it: the suite's `cb 00` cases show `R` up by 2 and `PC` up by 2.
    fn exec_cb(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let op = self.fetch_opcode(bus);
        self.unimplemented(&format!(
            "CB {op:02X} (x{} y{} z{}, rot {} {})",
            x(op),
            y(op),
            z(op),
            z(op) >> 3,
            REG_TABLE[z(op) as usize]
        ))
    }

    /// Dispatch the `ED` space.
    ///
    /// Defined-but-not-yet-implemented opcodes panic. Undefined ones are not an
    /// error at all: they behave as an 8 T-state NOP, and the byte after `ED` is
    /// still fetched, which is why `R` counts 2.
    fn exec_ed(&mut self, bus: &mut impl Z80Bus) -> u32 {
        let op = self.fetch_opcode(bus);
        if ed_is_defined(op) {
            self.unimplemented(&format!("ED {op:02X} (x{} y{} z{})", x(op), y(op), z(op)))
        } else {
            T_M1 + self.nop()
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
        self.unimplemented(&format!(
            "{} CB {disp:02X} {op:02X} (x{} y{} z{}, rot, {})",
            mode.prefix(),
            x(op),
            y(op),
            z(op),
            REG_TABLE[z(op) as usize]
        ))
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

    /// A bus that reads back a fixed program, so a test can name the exact bytes
    /// an instruction is made of and see what `step()` did with them.
    struct Program {
        ram: Vec<u8>,
        reads: Vec<u16>,
    }

    impl Program {
        fn new(bytes: &[u8]) -> Self {
            let mut ram = vec![0; 0x1_0000];
            ram[..bytes.len()].copy_from_slice(bytes);
            Self {
                ram,
                reads: Vec::new(),
            }
        }
    }

    impl Z80Bus for Program {
        fn read(&mut self, addr: u16) -> u8 {
            self.reads.push(addr);
            self.ram[usize::from(addr)]
        }

        fn write(&mut self, _addr: u16, _value: u8) {
            panic!("no instruction implemented so far should write");
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

        // Last prefix wins, in both orders: `DD FD 21` is `LD IY,nn` and
        // `FD DD 21` is `LD IX,nn`, whichever way the prefixes are stacked.
        for (first, second, winner) in [(0xDD, 0xFD, "FD"), (0xFD, 0xDD, "DD")] {
            let mut cpu = Z80::new();
            let mut bus = Program::new(&[first, second, 0x21]);
            let message =
                caught(|| cpu.step(&mut bus)).expect_err("{second:02X} 21 is not implemented");
            assert!(message.contains(&format!("{winner} 21")), "{message}");
        }
    }

    #[test]
    fn r_counts_prefixes() {
        // Every prefix space increments R for the prefix and for the byte after
        // it, and never for operands.
        for prefix in [0xCB, 0xED, 0xDD, 0xFD] {
            let mut cpu = Z80::new();
            let mut bus = Program::new(&[prefix, 0x00]);
            // CB 00 is a real (unimplemented) opcode; the others rot to NOP. What
            // matters is that R was counted for both bytes before the dispatch
            // gave up.
            let _ = caught(|| cpu.step(&mut bus));
            assert_eq!(cpu.r(), 2, "{prefix:02X} should have counted 2 fetches");
            assert_eq!(cpu.pc(), 2, "{prefix:02X} should have consumed 2 bytes");
        }
    }

    #[test]
    fn dd_cb_counts_the_prefix_and_cb_but_not_the_operands() {
        // `DD CB d op` is 4 bytes but only 2 M1 fetches, which is the R total of
        // 2 the suite's `dd cb __ 00` files expect. The displacement is read
        // before the opcode.
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0xDD, 0xCB, 0x7F, 0x06]);
        let message = caught(|| cpu.step(&mut bus)).expect_err("DD CB is not implemented");
        assert_eq!(cpu.r(), 2);
        assert_eq!(cpu.pc(), 4);
        assert_eq!(bus.reads, vec![0, 1, 2, 3]);
        assert!(message.contains("DD CB 7F 06"), "{message}");
    }

    #[test]
    fn main_space_gives_a_useful_panic() {
        let mut cpu = Z80::new();
        let mut bus = Program::new(&[0x01]); // LD BC,nn
        let message = caught(|| cpu.step(&mut bus)).expect_err("LD BC,nn is not implemented");
        assert!(
            message.contains("main 01 (x0 y0 z1)"),
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
