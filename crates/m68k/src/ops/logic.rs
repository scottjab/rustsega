//! Logic instructions: `AND`, `OR`, `EOR` and `NOT`, plus the immediate forms
//! `ANDI`, `ORI` and `EORI` (M1-13, #21).
//!
//! Opcodes (M68000UM section 8, "Instruction Formats"):
//!
//! | Instruction | Encoding | Operation |
//! |---|---|---|
//! | `AND` | `1100 ddd ooo mmm rrr` | `ooo` 000/001/010: `<ea>` & Dd → Dd; 100/101/110: Dd & `<ea>` → `<ea>`; `oo` = size |
//! | `OR` | `1000 ddd ooo mmm rrr` | as `AND`, with `\|` |
//! | `EOR` | `1011 ddd 1ss mmm rrr` | Dd ^ `<ea>` → `<ea>` |
//! | `NOT` | `0100 0110 ss mmm rrr` | ~`<ea>` → `<ea>` |
//! | `ANDI` | `0000 0010 ss mmm rrr` | `<#data>` & `<ea>` → `<ea>` |
//! | `ORI` | `0000 0000 ss mmm rrr` | `<#data>` \| `<ea>` → `<ea>` |
//! | `EORI` | `0000 1010 ss mmm rrr` | `<#data>` ^ `<ea>` → `<ea>` |
//!
//! Four shapes cover all seven: [`M68k::logic_alu`] is AND and OR (which differ
//! only in the operation and the group they live in), [`M68k::logic_to_ea`] is
//! the read-modify-write side those two share and the whole of EOR,
//! [`M68k::logic_imm`] is the three immediates, and [`M68k::op_not`] is the one
//! operand read, inverted and written back. That is why there is no
//! `op_and`/`op_or`/`op_eor` triple of near-identical functions: the operations
//! are one [`LogicOp`] each and everything else is shared.
//!
//! Flags (M68000UM, "68000 CCR Affected by Instruction Set"): every one of these
//! sets `N` and `Z` from the result, clears `V` and `C`, and leaves `X` alone —
//! which is [`M68k::set_logic_flags`]'s rule, the same one `TST` uses. Nothing
//! here therefore computes a carry or an overflow; the helpers below only mask and
//! call through.
//!
//! The to-CCR and to-SR forms of the three immediates are *not* here: `ORI
//! #imm,CCR` (`$003C`), `ANDI #imm,CCR` (`$023C`), `EORI #imm,CCR` (`$0A3C`) and
//! the `ss` = `11` move-to-SR encodings are M1-26 (#34)'s, and
//! [`crate::decode`] leaves them [`crate::decode::Op::Unimplemented`] so no
//! handler here can be reached with them.
//!
//! Illegal operands are rejected by [`crate::decode`], not here: an address
//! register as AND's or OR's byte source, an address register or an immediate as
//! any destination, and EOR's mode `001`, which CMPM took first.

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::ea::{Ea, Size};
use crate::ops::field;

/// Shift of the register field of AND's `1100`, OR's `1000` and EOR's `1011`
/// groups, bits 11..9 — `arith::ALU_REG_SHIFT` under another name because these
/// handlers read it independently.
const LOGIC_REG_SHIFT: u32 = 9;
/// Shift of the operation field: `ooo` for AND and OR, `1ss` for EOR, `ss` for
/// NOT and the three immediates. Only the low two bits are the size, which is
/// what [`Size::from_bits`] takes.
const LOGIC_OP_SHIFT: u32 = 6;
/// Shift of an EA mode, bits 5..3; the EA register is bits 2..0.
const EA_MODE_SHIFT: u32 = 3;
/// Bit 8 of AND's and OR's operation field: with bit 6 or 7 set it selects the
/// `Dn op <ea> → <ea>` direction, exactly as `arith::ALU_MODE_TO_EA` does for
/// ADD and SUB.
const LOGIC_MODE_TO_EA: u8 = 0b100;
/// EA mode `111`, the mode whose register picks absolute/PC-relative/immediate.
const EA_MODE_111: u8 = 0b111;
/// EA register `100` inside mode `111`: immediate.
const EA_REG_IMMEDIATE: u8 = 0b100;

/// The operation one of the logic handlers performs. AND, OR and EOR are one
/// shape apart — see [`M68k::logic_alu`] and [`M68k::logic_to_ea`] — so naming
/// the operation is enough to cover three instructions with two functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogicOp {
    And,
    Or,
    Eor,
}

impl LogicOp {
    /// Combine two operands. All three operations are commutative, which is what
    /// lets AND's two directions pass their operands in whichever order they
    /// fell to hand.
    const fn apply(self, lhs: u32, rhs: u32) -> u32 {
        match self {
            LogicOp::And => lhs & rhs,
            LogicOp::Or => lhs | rhs,
            LogicOp::Eor => lhs ^ rhs,
        }
    }
}

impl M68k {
    /// `AND`: `1100 ddd ooo mmm rrr`.
    ///
    /// `ooo` 000/001/010 ANDs the effective address into data register `ddd`;
    /// 100/101/110 ANDs data register `ddd` *into* the effective address, a
    /// read-modify-write of that one location (M68000UM section 8, "AND", #21).
    pub(crate) fn op_and(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.logic_alu(bus, opcode, LogicOp::And)
    }

    /// `OR`: `1000 ddd ooo mmm rrr`, [`M68k::op_and`] with `|` (M68000UM section
    /// 8, "OR", #21).
    pub(crate) fn op_or(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.logic_alu(bus, opcode, LogicOp::Or)
    }

    /// `EOR`: `1011 ddd 1ss mmm rrr` — `Dd ^ <ea> → <ea>` (M68000UM section 8,
    /// "EOR", #21). EOR has no `<ea>,Dn` direction, which is why its operation
    /// field is `1ss` throughout and why CMPM could borrow mode `001` from it.
    pub(crate) fn op_eor(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.logic_to_ea(bus, opcode, LogicOp::Eor)
    }

    /// `ANDI #imm,<ea>`: `$0200 + ss<<6 + <ea>` (M68000UM section 8, "ANDI",
    /// #21).
    pub(crate) fn op_andi(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.logic_imm(bus, opcode, LogicOp::And)
    }

    /// `ORI #imm,<ea>`: `$0000 + ss<<6 + <ea>` (M68000UM section 8, "ORI", #21).
    pub(crate) fn op_ori(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.logic_imm(bus, opcode, LogicOp::Or)
    }

    /// `EORI #imm,<ea>`: `$0A00 + ss<<6 + <ea>` (M68000UM section 8, "EORI",
    /// #21).
    pub(crate) fn op_eori(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        self.logic_imm(bus, opcode, LogicOp::Eor)
    }

    /// `NOT`: `0100 0110 ss mmm rrr` — one operand, inverted in place
    /// (M68000UM section 8, "NOT", #21).
    pub(crate) fn op_not(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = logic_size(opcode) else {
            // `ss` = `11` is MOVE to CCR and MOVE to SR's sixth of `$46xx`
            // (#34), which `classify` keeps out of this handler.
            return self.op_illegal(bus);
        };

        let Some(dest) = self.resolve_logic_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        // `!value` inverts all 32 bits, so the mask in `logic_flags` is what
        // keeps a byte or word NOT from reaching above its own width — the bits
        // a byte NOT must leave alone in a data register are restored by
        // `write_ea`, which writes one size's worth of the register.
        let result = self.logic_flags(!value, size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        0
    }

    /// The `1100`/`1000` shape: bit 8 of the operation field picks the
    /// direction, the low two bits the size (M68000UM "AND", "OR", #21).
    fn logic_alu(&mut self, bus: &mut impl Bus, opcode: u16, op: LogicOp) -> u32 {
        let op_mode = field(opcode, LOGIC_OP_SHIFT);
        if op_mode & LOGIC_MODE_TO_EA == 0 {
            let Some(size) = logic_size(opcode) else {
                // `011` and `111` are MUL and DIV, taken by `classify` before it
                // routes anything here.
                return self.op_illegal(bus);
            };
            // `AND <ea>,Dn`: the source first, so `(A0)+` steps exactly once.
            let reg = field(opcode, LOGIC_REG_SHIFT);
            let mode = field(opcode, EA_MODE_SHIFT);
            let ea_reg = field(opcode, 0);
            let Some(src) = self.resolve_logic_ea(bus, mode, ea_reg, size) else {
                return self.op_illegal(bus);
            };
            let value = self.read_ea(bus, src, size);
            let result = self.logic_flags(op.apply(self.d[usize::from(reg)], value), size);
            self.write_ea(bus, Ea::DataReg(reg), size, result);

            self.advance_opcode(bus);
            return 0;
        }

        self.logic_to_ea(bus, opcode, op)
    }

    /// `Dn op <ea> → <ea>`: the read-modify-write half of AND and OR and the
    /// whole of EOR. One resolve, one read, one write, so `AND D0,(A0)+` steps
    /// A0 once (M68000UM "AND", "OR", "EOR", #21).
    fn logic_to_ea(&mut self, bus: &mut impl Bus, opcode: u16, op: LogicOp) -> u32 {
        let reg = field(opcode, LOGIC_REG_SHIFT);
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = logic_size(opcode) else {
            // MUL and DIV again; `classify_and`/`classify_or`/`classify_cmp`
            // never route them here.
            return self.op_illegal(bus);
        };

        let source = self.d[usize::from(reg)];
        let Some(dest) = self.resolve_logic_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.logic_flags(op.apply(source, value), size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        0
    }

    /// The three `#imm,<ea>` immediates. `op` is the operation; the base opcode
    /// said which of ANDI/ORI/EORI this is, and nothing else differs.
    ///
    /// The immediate is fetched before the destination is resolved, exactly as
    /// `arith::op_addi` does it, because the immediate's extension words precede
    /// the destination's in the encoding (M68000UM "ORI", "ANDI", "EORI", #21).
    fn logic_imm(&mut self, bus: &mut impl Bus, opcode: u16, op: LogicOp) -> u32 {
        let mode = field(opcode, EA_MODE_SHIFT);
        let ea_reg = field(opcode, 0);
        let Some(size) = logic_size(opcode) else {
            // `ss` = `11` is MOVE to SR's space (#34).
            return self.op_illegal(bus);
        };

        let Some(immediate) = self.resolve_logic_ea(bus, EA_MODE_111, EA_REG_IMMEDIATE, size)
        else {
            return self.op_illegal(bus);
        };
        let operand = self.read_ea(bus, immediate, size);

        let Some(dest) = self.resolve_logic_ea(bus, mode, ea_reg, size) else {
            return self.op_illegal(bus);
        };
        let value = self.read_ea(bus, dest, size);
        let result = self.logic_flags(op.apply(operand, value), size);
        self.write_ea(bus, dest, size, result);

        self.advance_opcode(bus);
        0
    }

    /// Mask `result` to `size`, set `N` and `Z` from it, clear `V` and `C`, leave
    /// `X` alone, and return the masked value (M68000UM, "68000 CCR Affected by
    /// Instruction Set"; see [`M68k::set_logic_flags`]).
    fn logic_flags(&mut self, result: u32, size: Size) -> u32 {
        let result = result & size.mask();
        self.set_logic_flags(result, size);
        result
    }

    /// [`crate::ea::Ea`] resolution for the logic class: the same rules as
    /// arithmetic's, with the debug assertion naming this module so a decode
    /// mistake is traceable to it.
    fn resolve_logic_ea(
        &mut self,
        bus: &mut impl Bus,
        mode: u8,
        reg: u8,
        size: Size,
    ) -> Option<Ea> {
        let ea = self.resolve_ea(bus, mode, reg, size);
        debug_assert!(
            ea.is_some(),
            "decode let an invalid logic operand through: mode {mode:b} reg {reg:b} {size:?}"
        );
        ea
    }
}

/// Size from the two `ss`/`oo` bits at 7..6 of a logic opcode, `None` for `11`
/// (M68000UM "AND"/"OR"/"EOR"/"NOT"/"ANDI"/"ORI"/"EORI" all leave `11` out; in
/// the `$0000`/`$0200`/`$0A00` groups it is MOVE to SR and in `$46xx` it is MOVE
/// to CCR/SR — #34 either way).
const fn logic_size(opcode: u16) -> Option<Size> {
    Size::from_bits((opcode >> LOGIC_OP_SHIFT) & 0b11)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::cpu_with_program;

    /// Where [`cpu_with_program`] loads the program, so a test can name the
    /// address `pc` should reach.
    const PROGRAM: u32 = 0x0000_1000;
    /// A data address every memory test can use.
    const MEM: u32 = 0x0000_2000;

    /// Every condition code as one CCR value, for "this instruction leaves the
    /// flags alone" tests.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as the
    /// following words.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, crate::testbus::TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    /// `AND.W D1,D0` (`$C041` = `1100 ddd 001 mmm rrr`, operation `001` = word
    /// `<ea> & Dd → Dd`): the low word is masked and the upper word of the
    /// destination long is untouched (M68000UM "AND").
    #[test]
    fn and_w_masks() {
        let (mut cpu, mut bus) = run(0xC041, &[0x4E71]);
        cpu.d[0] = 0xDEAD_BEEF;
        cpu.d[1] = 0x1111_00F0;

        cpu.step(&mut bus);

        assert_eq!(
            cpu.d[0], 0xDEAD_00E0,
            "$BEEF & $00F0, and the upper word is left alone"
        );
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_n(), "N is bit 15 of the word, which is clear");
    }

    /// `AND.B D1,D0` (`$C001`) to a zero byte: Z is set from the byte, and only
    /// the byte changes (M68000UM "AND").
    #[test]
    fn and_b_to_zero() {
        let (mut cpu, mut bus) = run(0xC001, &[0x4E71]);
        cpu.d[0] = 0xDEAD_BEEF;
        cpu.d[1] = 0x0000_0100;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_BE00, "only the low byte changed");
        assert!(cpu.flag_z(), "the masked byte is zero");
    }

    /// `OR.B D1,(A0)` (`$8310`, operation `100` = byte written back to the EA):
    /// one byte of memory is written and `N` comes from bit 7 of that byte, not
    /// from bit 31 of the long that happens to hold it (M68000UM "OR"). The
    /// opcode is `1000 ddd 100 010 000` — `$8210`, one bit lower in operation
    /// mode `000`, is the opposite direction, `OR.B (A0),D1`, which leaves memory
    /// alone (the suite names both: `8310 [OR.b D1, (A0)]` and
    /// `8210 [OR.b (A0), D1]`).
    #[test]
    fn or_b_sets_n() {
        let (mut cpu, mut bus) = run(0x8310, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[1] = 0x0000_0080;
        // The byte `(A0)` names is the byte at `MEM`, i.e. the high byte of the
        // big-endian long the test reads back: `$0F | $80 = $8F`.
        bus.write_long(MEM, 0x0FAD_BE7F);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0x8FAD_BE7F);
        assert!(cpu.flag_n(), "the byte result has bit 7 set");
        assert!(!cpu.flag_z());
        assert_eq!(cpu.pc, PROGRAM + 2);
    }

    /// `OR.W D1,D0` (`$8041`) with every flag preset: `X` survives, and `Z` is
    /// cleared by a non-zero word even though the register's other half is
    /// unchanged (M68000UM "OR").
    #[test]
    fn or_w_keeps_x_and_sets_n() {
        let (mut cpu, mut bus) = run(0x8041, &[0x4E71]);
        cpu.d[0] = 0xDEAD_FFFF;
        cpu.d[1] = 0x0000_0000;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_FFFF);
        assert!(cpu.flag_n(), "bit 15 of the word result is set");
        assert!(!cpu.flag_z());
        assert!(cpu.flag_x(), "X is unaffected by a logic operation");
    }

    /// `EOR.L D0,(A0)+` (`$B198` = `1011 ddd 110 mmm rrr`): one
    /// read-modify-write of one location, and the post-increment steps A0 by the
    /// operand size exactly once (M68000UM "EOR").
    #[test]
    fn eor_l_to_memory() {
        let (mut cpu, mut bus) = run(0xB198, &[0x4E71]);
        cpu.a[0] = MEM;
        cpu.d[0] = 0xAAAA_55AA;
        bus.write_long(MEM, 0xFFFF_00FF);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0x5555_5555);
        assert_eq!(cpu.a[0], MEM + 4, "A0 steps once, by four bytes");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_n());
    }

    /// `EOR.B D0,D0` (`$B100`): a value XOR itself is zero. This is the opcode
    /// that decodes to [`crate::decode::Op::Eor`] rather than to CMP, and EOR's
    /// flags are the reason the suite's `EOR.*` files need it.
    #[test]
    fn eor_b_with_itself_is_zero() {
        let (mut cpu, mut bus) = run(0xB100, &[0x4E71]);
        cpu.d[0] = 0xDEAD_BEEF;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_BE00, "the low byte XORed with itself");
        assert!(cpu.flag_z());
    }

    /// `NOT.B D0` (`$4600`) inverts one byte and leaves the other three alone —
    /// the byte-lattice property AND and OR cannot show, because they have no
    /// one-operand form (M68000UM "NOT").
    #[test]
    fn not_b_keeps_upper_bits() {
        let (mut cpu, mut bus) = run(0x4600, &[0x4E71]);
        cpu.d[0] = 0xDEAD_BEEF;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_BE10, "only the low byte flipped");
        assert!(!cpu.flag_n(), "$10 has bit 7 clear");
        assert!(!cpu.flag_z());
    }

    /// `NOT.B (A0)` (`$4610`) and `NOT.W D0` (`$4640`): N and Z come from the
    /// operand at its own size, so a word NOT of `$0000` sets N without setting Z
    /// even though the register's upper half is still zero. `(A0)` holds the byte
    /// at `MEM`, which is the *high* byte of the big-endian long a test reads back
    /// from that address.
    #[test]
    fn not_sets_n_and_z_at_its_size() {
        let (mut cpu, mut bus) = run(0x4610, &[0x4E71]);
        cpu.a[0] = MEM;
        bus.write_long(MEM, 0x7FAD_BE7F);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_long(MEM),
            0x80AD_BE7F,
            "the byte at (A0) flipped, the three above it did not"
        );
        assert!(cpu.flag_n(), "the inverted byte has bit 7 set");

        let (mut cpu, mut bus) = run(0x4640, &[0x4E71]);
        cpu.d[0] = 0xDEAD_0000;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xDEAD_FFFF, "the word flipped, the long did not");
        assert!(cpu.flag_n());
        assert!(!cpu.flag_z(), "the word is not zero");
    }

    /// `ANDI.L #$12345678,($2000).W` (`$02B8`): two extension words for the
    /// immediate, high word first, *before* the destination's own address word —
    /// the same order `ADDI` and `CMPI` use, and the mask only comes out right if
    /// the words were read in that order (M68000UM "ANDI", #21).
    #[test]
    fn andi_l_immediate() {
        let (mut cpu, mut bus) = run(0x02B8, &[0x1234, 0x5678, MEM as u16, 0x4E71]);
        bus.write_long(MEM, 0xFFFF_FF00);

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0x1234_5600);
        assert_eq!(cpu.pc, PROGRAM + 8, "opcode plus three extension words");
        assert!(!cpu.flag_z());
        assert!(!cpu.flag_n());
    }

    /// `ORI.B #$34,D0` (`$0000`, the `0000 000 0 00 mmm rrr` base with the EA
    /// `000 000`): a byte immediate takes the low half of one extension word and
    /// consumes exactly that word (M68000UM "ORI").
    #[test]
    fn ori_b_immediate_uses_the_operand_width() {
        let (mut cpu, mut bus) = run(0x0000, &[0x1234, 0x4E71]);
        cpu.d[0] = 0x0000_000F;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x0000_003F, "only the byte $34 is ORed in");
        assert_eq!(cpu.pc, PROGRAM + 4, "one extension word");
        assert!(!cpu.flag_z());
    }

    /// `ORI.W #$1234,($2000).W` (`$0078`): the word *at* the address — the top two
    /// bytes of the long a test reads back from there, this bus being big-endian —
    /// changes and the word above it does not (M68000UM "ORI").
    #[test]
    fn ori_w_to_memory() {
        let (mut cpu, mut bus) = run(0x0078, &[0x1234, MEM as u16, 0x4E71]);
        bus.write_long(MEM, 0xDEAD_BEEF);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_long(MEM),
            0xDEBD_BEEF,
            "$DEAD | $1234, and the word at $2002 is untouched"
        );
        assert!(!cpu.flag_z());
        assert!(cpu.flag_n(), "bit 15 of the word result $DEBD is set");
    }

    /// `EORI.W #$5678,($2000).W` (`$0A78`) with the value the addressed word
    /// already holds: that word becomes zero and Z is set from it, while the long
    /// in memory keeps the word EORI never touched (M68000UM "EORI").
    #[test]
    fn eori_w_clears_by_xor() {
        let (mut cpu, mut bus) = run(0x0A78, &[0x5678, MEM as u16, 0x4E71]);
        bus.write_long(MEM, 0x5678_BEEF);

        cpu.step(&mut bus);

        assert_eq!(
            bus.read_long(MEM),
            0x0000_BEEF,
            "the word at $2000 XORed itself away, the word at $2002 did not"
        );
        assert!(cpu.flag_z(), "the word result is zero");
    }

    /// The flag rule for the whole class: `N` and `Z` from the result, `V` and
    /// `C` cleared, `X` left alone (M68000UM, "68000 CCR Affected by Instruction
    /// Set"). Measured with all five preset on an AND that cannot carry or
    /// overflow by construction.
    #[test]
    fn logic_clears_v_c_keeps_x() {
        let (mut cpu, mut bus) = run(0xC041, &[0x4E71]);
        cpu.d[0] = 0xFFFF_F0F0;
        cpu.d[1] = 0x0000_0F0F;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0x0000_FFFF, 0x0000, "the AND is zero");
        assert!(cpu.flag_z());
        assert!(!cpu.flag_v(), "V is cleared");
        assert!(!cpu.flag_c(), "C is cleared");
        assert!(cpu.flag_x(), "X is unaffected");
    }

    /// `NOT` follows the same rule, which the shared flag helper gets wrong if
    /// NOT is given a carry-preserving path of its own.
    #[test]
    fn not_clears_v_c_keeps_x() {
        let (mut cpu, mut bus) = run(0x4640, &[0x4E71]);
        cpu.d[0] = 0x0000_FFFF;
        cpu.set_ccr(ALL_FLAGS);

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0] & 0x0000_FFFF, 0x0000);
        assert!(cpu.flag_z());
        assert!(!cpu.flag_v());
        assert!(!cpu.flag_c());
        assert!(cpu.flag_x());
    }
}
