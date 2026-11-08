//! Effective address (EA) calculation and operand access (M1-05 #13, M1-06 #14).
//!
//! The EA field of an opcode is 6 bits: mode in bits 5..3, register in bits
//! 2..0 (M68000UM, section 3 "Effective Address Computation"). This module
//! covers all of them — `000` Dn, `001` An, `010` (An), `011` (An)+, `100`
//! -(An), `101` d16(An), `110` d8(An,Xn), and `111` abs.W, abs.L, d16(PC),
//! d8(PC,Xn), #imm — except mode `111` with register `101`–`111`, which is not
//! an addressing mode at all and becomes an illegal instruction in M1-07 (#15).
//!
//! The three entry points are used in this order, and the order matters for
//! read-modify-write instructions:
//!
//! 1. [`M68k::resolve_ea`] computes the address *and* applies the
//!    `(An)+` / `-(An)` register update. Call it exactly once per operand.
//! 2. [`M68k::read_ea`] / [`M68k::write_ea`] then access that [`Ea`]. An
//!    `ADD.W #x, (A0)+`-style instruction reads and writes the same `Ea`;
//!    resolving twice would increment A0 twice.
//!
//! Modes `000`–`100` read no memory at all. The rest take a bus for exactly
//! one reason: pulling their extension words out of the prefetch queue, which
//! refills itself from `pc + 4` on every pop. Extension words — displacements,
//! absolute addresses, immediate data — therefore come from
//! [`M68k::fetch_ext_word`] / [`M68k::fetch_ext_long`], never from
//! `bus.read_word(pc)`; see [`M68k::pc`] for the queue convention and
//! [`M68k::ext_addr`] for the address an extension word has while it is still
//! queued. PC-relative modes measure from that address.
//!
//! The 24-bit rule: an [`Ea::Mem`] carries the address already masked with
//! [`ADDR_MASK`], which is the address the bus is asked about. Masking there is
//! the same address as masking late at the bus, because addition commutes with
//! the modulo — pinned by `tests::masking_wraps_at_24_bits` — but it is not the
//! address an address-*as-data* instruction wants: `LEA` keeps the unmasked
//! 32-bit sum in its destination register (`LEA.json.gz` case
//! `41fa [LEA (d16, PC), A0] 59` ends with `a0 = 0xFFFFAFEB`), so those
//! instructions need the sum itself rather than an [`Ea::Mem`] — see #193.

use crate::bus::{ADDR_MASK, Bus};
use crate::cpu::M68k;

/// Operand width of an instruction or an EA access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Byte (8 bit).
    Byte,
    /// Word (16 bit).
    Word,
    /// Long (32 bit).
    Long,
}

impl Size {
    /// Width in bytes: 1, 2, 4.
    #[inline]
    pub const fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }

    /// Bit mask covering this width: `0xFF`, `0xFFFF`, `0xFFFF_FFFF`.
    #[inline]
    pub const fn mask(self) -> u32 {
        match self {
            Size::Byte => 0x0000_00FF,
            Size::Word => 0x0000_FFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }

    /// Most significant bit of this width: `0x80`, `0x8000`, `0x8000_0000`.
    /// Used by the flag rules (N is the MSB of the result, V is signed overflow).
    #[inline]
    pub const fn msb(self) -> u32 {
        match self {
            Size::Byte => 0x0000_0080,
            Size::Word => 0x0000_8000,
            Size::Long => 0x8000_0000,
        }
    }

    /// Decode the common 2-bit size field (`00` byte, `01` word, `10` long).
    /// `11` — and anything wider than the field — is invalid, so this returns
    /// `None` rather than guessing.
    #[inline]
    pub const fn from_bits(bits: u16) -> Option<Size> {
        match bits {
            0 => Some(Size::Byte),
            1 => Some(Size::Word),
            2 => Some(Size::Long),
            _ => None,
        }
    }

    /// Decode the **move class's** size field (bits 13..12 of `MOVE`, `MOVEA`
    /// and the `00ss` groups): `01` byte, `11` word, `10` long.
    ///
    /// Deliberately not [`Size::from_bits`]: the move class numbers its sizes
    /// differently (M68000UM, "Instruction Formats", Format III — `00ss RRR MMM
    /// mmm rrr`), and `00` in bits 13..12 with bits 15..14 clear is not a MOVE
    /// at all but the `$0000` group (`ORI`, `BTST`, `MOVEP`), so `None` means
    /// "this opcode is not a move" rather than "byte".
    #[inline]
    pub const fn from_move_bits(bits: u16) -> Option<Size> {
        match bits & 0b11 {
            0b01 => Some(Size::Byte),
            0b11 => Some(Size::Word),
            0b10 => Some(Size::Long),
            _ => None,
        }
    }
}

/// A resolved effective address: where an operand lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ea {
    /// Data register direct (mode 000).
    DataReg(u8),
    /// Address register direct (mode 001).
    AddrReg(u8),
    /// Memory, at an address already computed and masked to 24 bits.
    Mem(u32),
    /// Immediate data (`#imm`), already fetched from the prefetch queue at the
    /// operand's width: a byte immediate is the low byte of one extension word,
    /// a long immediate is two. Source only — [`M68k::write_ea`] refuses it.
    Imm(u32),
}

impl Ea {
    /// True when `size` is a legal access width for this EA.
    ///
    /// Byte access to an address register is the only illegal combination:
    /// mode 001 is only listed for byte-free operand types, so no 68000
    /// instruction encodes it (M68000UM, section 3 "Effective Address
    /// Computation", mode 001, and section 1 "Address Registers", which gives
    /// An a word or long operand width). [`M68k::resolve_ea`] rejects it, so
    /// `read_ea` / `write_ea` never see it.
    #[inline]
    pub fn accepts_size(self, size: Size) -> bool {
        !matches!(self, Ea::AddrReg(_) if size == Size::Byte)
    }
}

/// Extra internal clocks charged by the `d8(An,Xn)` / `d8(PC,Xn)` address
/// calculation.
///
/// M68000UM, Appendix C "Instruction Timing": the indexed forms carry 2
/// internal clock cycles beyond the access itself, because the index register
/// has to be read and added before the bus cycle can start. TODO(M1-31a, #39):
/// confirm against the SingleStepTests cycle counts.
const INDEX_CALC_CLOCKS: u64 = 2;

/// Sign extend a 16-bit value to 32 bits (`0x8000` -> `0xFFFF_8000`).
///
/// Written with `as` rather than `i32::from` because `From` is not a const
/// trait, so `#[rustfmt::skip]`-style `const fn` helpers cannot use it.
pub(crate) const fn sign_extend_word(value: u16) -> u32 {
    (value as i16) as u32
}

/// Sign extend an 8-bit value to 32 bits (`0xFE` -> `0xFFFF_FFFE`). `as` for
/// the same const-fn reason as [`sign_extend_word`].
pub(crate) const fn sign_extend_byte(value: u8) -> u32 {
    (value as i8) as u32
}

/// Extra internal clocks charged by the `-(An)` address calculation.
///
/// M68000UM, Appendix C "Instruction Timing": predecrement EAs carry 2 internal
/// clock cycles beyond the access itself, because the decrement has to complete
/// before the bus cycle can start. TODO(M1-31a, #39): confirm against the
/// SingleStepTests cycle counts and add the remaining internal-clock
/// differences (`(An)+` with A7 and the register indirect pair).
const PREDEC_CALC_CLOCKS: u64 = 2;

/// Increment or decrement [`M68k::a`] applies for `(An)+` / `-(An)`.
///
/// The step is the operand size, except for a byte access through A7, which
/// steps by 2 so the stack pointer stays word aligned even for byte operands
/// (M68000UM, section 3 "Address Register Indirect" and "Address Register
/// Indirect with Postdecrement": A7 used as a stack pointer steps by 2 for a
/// byte operand, while A0–A6 step by 1).
/// The stack pointer register: `a[7]`, the address register with the two quirks
/// — a byte push or pop steps it by two ([`ea_step`]), and it mirrors into the
/// stored `ssp`/`usp` slot ([`M68k::set_an`]).
pub(crate) const SP_REG: u8 = 7;

const fn ea_step(reg: u8, size: Size) -> u32 {
    if reg == SP_REG && matches!(size, Size::Byte) {
        2
    } else {
        size.bytes()
    }
}

/// Which operand class an EA field belongs to, from the `(mode, reg)` pair.
///
/// The classification behind [`is_data`], [`is_memory`], [`is_control`] and the
/// alterable predicates, so the six of them cannot drift apart: each is one set
/// of these, exactly as M68000PM/AD section 2.2.12 "Effective Addressing Mode
/// Summary" tabulates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EaClass {
    /// `000` Dn.
    DataReg,
    /// `001` An — data-class for word and long only, which is what
    /// [`Ea::accepts_size`] enforces at access time.
    AddrReg,
    /// `010` (An).
    Indirect,
    /// `011` (An)+.
    PostIncrement,
    /// `100` -(An).
    PreDecrement,
    /// `101` d16(An).
    Displacement,
    /// `110` d8(An,Xn).
    Indexed,
    /// `111` `000`/`001`: abs.W, abs.L.
    Absolute,
    /// `111` `010`/`011`: d16(PC), d8(PC,Xn).
    PcRelative,
    /// `111` `100`: #immediate.
    Immediate,
    /// `111` `101`-`111`: not an addressing mode on the 68000.
    Invalid,
}

/// The class of an EA field: `mode` is bits 5..3, `reg` bits 2..0.
///
/// In mode `111` the register field *is* the sub-mode (M68000UM, "Effective
/// Address Calculation"), which is why `reg` matters for that mode alone and is
/// ignored for every other mode.
const fn ea_class(mode: u8, reg: u8) -> EaClass {
    match mode {
        0b000 => EaClass::DataReg,
        0b001 => EaClass::AddrReg,
        0b010 => EaClass::Indirect,
        0b011 => EaClass::PostIncrement,
        0b100 => EaClass::PreDecrement,
        0b101 => EaClass::Displacement,
        0b110 => EaClass::Indexed,
        0b111 => match reg {
            0b000 | 0b001 => EaClass::Absolute,
            0b010 | 0b011 => EaClass::PcRelative,
            0b100 => EaClass::Immediate,
            // `101` is the 68010+ `(d8,PC,Xn)`, `110`/`111` the 68020+
            // bracketed forms. On the 68000 none of them is a mode, which is
            // also why [`M68k::resolve_ea`] declines them.
            _ => EaClass::Invalid,
        },
        // `mode` is 3 bits, so this is unreachable; naming it beats a `todo!()`
        // on a path a caller could one day reach with a bad mask.
        _ => EaClass::Invalid,
    }
}

/// Is `(mode, reg)` a real addressing mode on the 68000 at all?
///
/// False only for mode `111` with register `101`-`111`, which the manual
/// reserves for the 68010/68020 (M68000UM, "Effective Address Calculation");
/// an opcode carrying one is an illegal instruction (vector 3, `crate::ops`).
pub(crate) const fn ea_mode_valid(mode: u8, reg: u8) -> bool {
    !matches!(ea_class(mode, reg), EaClass::Invalid)
}

/// Can `(mode, reg)` be a *data* operand (source or destination of a
/// move/ALU-class instruction)?
///
/// Every valid mode qualifies (M68000PM/AD 2.2.12: the Data column is marked
/// for all of them), with one size caveat: mode `001` An is data-class for word
/// and long only, and [`Ea::accepts_size`] rejects the byte case.
pub(crate) const fn is_data(mode: u8, reg: u8) -> bool {
    ea_mode_valid(mode, reg)
}

/// Can `(mode, reg)` name *memory* — i.e. is the operand read or written
/// through the address bus?
///
/// The address register indirect group, the two absolute forms and the two
/// PC-relative ones. Not Dn or An (registers), not #immediate (its value is in
/// the instruction stream, not in memory at the EA), and not an invalid mode
/// (M68000PM/AD 2.2.12).
pub(crate) const fn is_memory(mode: u8, reg: u8) -> bool {
    matches!(
        ea_class(mode, reg),
        EaClass::Indirect
            | EaClass::PostIncrement
            | EaClass::PreDecrement
            | EaClass::Displacement
            | EaClass::Indexed
            | EaClass::Absolute
            | EaClass::PcRelative
    )
}

/// Can `(mode, reg)` be a *control* operand — the address a `JMP`, `JSR` or
/// `LEA` computes?
///
/// The address register indirect group minus `(An)+` and `-(An)`, plus the
/// absolute and PC-relative forms (M68000PM/AD section 8, the addressing-mode
/// tables of those instructions: postincrement and predecrement are excluded
/// because a control operand is only read, never stepped over or into). The
/// 68010 adds `(An)+` and `-(An)` to `LEA`; this core is a 68000.
///
/// `LEA` and `PEA` are the callers so far (M1-15, #23); `JMP` and `JSR` add two
/// more when M1-16 lands.
pub(crate) const fn is_control(mode: u8, reg: u8) -> bool {
    matches!(
        ea_class(mode, reg),
        EaClass::Indirect
            | EaClass::Displacement
            | EaClass::Indexed
            | EaClass::Absolute
            | EaClass::PcRelative
    )
}

/// Can an instruction *write* an operand at `(mode, reg)`?
///
/// True for Dn, An, the address register indirect group and the absolute forms.
/// False for the PC-relative modes and #immediate: `d16(PC)` names a spot in
/// the instruction stream and an immediate *is* its own value, so neither is
/// somewhere a result can go (M68000PM/AD 2.2.12, and the "destination operand"
/// wording of the section 8 instruction descriptions).
pub(crate) const fn is_alterable(mode: u8, reg: u8) -> bool {
    matches!(
        ea_class(mode, reg),
        EaClass::DataReg
            | EaClass::AddrReg
            | EaClass::Indirect
            | EaClass::PostIncrement
            | EaClass::PreDecrement
            | EaClass::Displacement
            | EaClass::Indexed
            | EaClass::Absolute
    )
}

/// A valid *data destination* — what `MOVE`, `ADD`, `CLR`, `NEG` and the rest
/// of that family may write.
///
/// Not `is_data` and `is_alterable` together, because the two columns of
/// M68000PM/AD 2.2.12 differ on one mode: `001` An passes both (`ADDQ.W #1,A0`
/// really does write an An through an EA field, so An is alterable, and An is a
/// data-class source), yet no data-class instruction may name an An as its
/// destination — `MOVE.W D0,A0` does not exist, `MOVEA`/`LEA`/`ADDA` are what
/// writes an An from data, and their destination is the address register in
/// their extension field, not an EA field. So the set is Dn plus the writable
/// memory modes, and An is in it neither way.
pub(crate) const fn is_data_alterable(mode: u8, reg: u8) -> bool {
    is_memory_alterable(mode, reg) || matches!(ea_class(mode, reg), EaClass::DataReg)
}

/// Memory operand *and* writable — the destination of a memory write, e.g.
/// what `CLR` and `NEG` require.
pub(crate) const fn is_memory_alterable(mode: u8, reg: u8) -> bool {
    is_memory(mode, reg) && is_alterable(mode, reg)
}

impl M68k {
    /// Resolve an EA to an [`Ea`], applying the `(An)+` / `-(An)` register
    /// update as a side effect and consuming the mode's extension words.
    ///
    /// `mode` is bits 5..3 of the EA field and `reg` bits 2..0 (for MOVE's
    /// destination field the two are swapped — handled by M1-09, #17). In mode
    /// `111` the `reg` field *is* the sub-mode, which is why it is matched
    /// against here instead of used as a register number.
    ///
    /// `bus` is only there to feed the prefetch queue (see the module docs):
    /// no addressing mode reads memory for its operand here, and none writes.
    ///
    /// Returns `None` for a mode this layer cannot resolve — mode `111` with
    /// register `101`–`111`, an illegal instruction that M1-07 (#15) decodes,
    /// and byte size with An direct, which no instruction encodes (see
    /// [`Ea::accepts_size`]). Callers turn `None` into an illegal instruction or
    /// an unimplemented-mode path; it never panics, and it consumes nothing when
    /// it declines, so the instruction stream stays where it was.
    ///
    /// The address registers stay full 32-bit values — the suite keeps nonzero
    /// upper bytes in them (`MOVEA.w.json.gz` case `364a [MOVEA.w A2, A3] 3`
    /// ends with `a3 = 0xFFFF_E780`) — and only [`Ea::Mem`] is masked to
    /// [`ADDR_MASK`], which is the address that actually reaches the bus.
    pub(crate) fn resolve_ea(
        &mut self,
        bus: &mut impl Bus,
        mode: u8,
        reg: u8,
        size: Size,
    ) -> Option<Ea> {
        let r = usize::from(reg);
        match mode {
            0b000 => Some(Ea::DataReg(reg)),
            0b001 => Ea::AddrReg(reg)
                .accepts_size(size)
                .then_some(Ea::AddrReg(reg)),
            0b010 => Some(Ea::Mem(self.a[r] & ADDR_MASK)),
            0b011 => {
                // Postincrement: the current value is the address, then the
                // register moves past the operand.
                let addr = self.a[r] & ADDR_MASK;
                self.set_an(reg, self.a[r].wrapping_add(ea_step(reg, size)));
                Some(Ea::Mem(addr))
            }
            0b100 => {
                // Predecrement: the register moves first, then the new value is
                // the address, and the calculation itself costs 2 clocks.
                self.set_an(reg, self.a[r].wrapping_sub(ea_step(reg, size)));
                self.cycles += PREDEC_CALC_CLOCKS;
                Some(Ea::Mem(self.a[r] & ADDR_MASK))
            }
            // 101 d16(An): one extension word, sign extended, added to An
            // (M68000UM, "Address Register Displacement Addressing Mode").
            0b101 => {
                let disp = sign_extend_word(self.fetch_ext_word(bus));
                Some(Ea::Mem(self.a[r].wrapping_add(disp) & ADDR_MASK))
            }
            // 110 d8(An,Xn): one brief extension word, An + Xn + d8.
            0b110 => Some(Ea::Mem(self.indexed_addr(bus, self.a[r]) & ADDR_MASK)),
            // 111: the sub-mode comes from the register field of the EA
            // (M68000UM, "Effective Address Calculation", mode 111).
            0b111 => match reg {
                // abs.W: one extension word, sign extended to 32 bits, so
                // 0x8000 is 0xFFFF_8000 and reaches the bus as 0x00FF_8000.
                0b000 => {
                    let addr = sign_extend_word(self.fetch_ext_word(bus));
                    Some(Ea::Mem(addr & ADDR_MASK))
                }
                // abs.L: two extension words, high word first.
                0b001 => {
                    let addr = self.fetch_ext_long(bus);
                    Some(Ea::Mem(addr & ADDR_MASK))
                }
                // d16(PC): measured from the extension word's own address, not
                // from `pc` — see [`M68k::ext_addr`] and
                // `tests::pc_relative_uses_ext_word_address` for the evidence.
                0b010 => {
                    let base = self.ext_addr();
                    let disp = sign_extend_word(self.fetch_ext_word(bus));
                    Some(Ea::Mem(base.wrapping_add(disp) & ADDR_MASK))
                }
                // d8(PC,Xn): the indexed form, same base.
                0b011 => {
                    let base = self.ext_addr();
                    Some(Ea::Mem(self.indexed_addr(bus, base) & ADDR_MASK))
                }
                // #imm: the operand *is* the extension word(s). A byte
                // immediate occupies the low byte of one word, with the high
                // byte unused (M68000UM, "Immediate Addressing Mode").
                0b100 => Some(Ea::Imm(match size {
                    Size::Byte => u32::from(self.fetch_ext_word(bus) as u8),
                    Size::Word => u32::from(self.fetch_ext_word(bus)),
                    Size::Long => self.fetch_ext_long(bus),
                })),
                // 101, 110, 111 are not addressing modes: illegal instruction,
                // which M1-07 (#15) decodes. Decline without touching anything.
                _ => None,
            },
            // `mode` is 3 bits, so this is unreachable; naming it beats a
            // `todo!()` on a path an instruction could one day reach.
            _ => None,
        }
    }

    /// The address an EA field *computes*, without reading or writing anything
    /// there: `LEA` and `PEA` take the address itself as their operand
    /// (M68000UM, "LEA — Address Register Indirect and Program" and "PEA — Push
    /// Effective Address"; M1-15, #23).
    ///
    /// Two differences from [`M68k::resolve_ea`], and both matter:
    ///
    /// * the sum is **not** masked to [`ADDR_MASK`]. [`Ea::Mem`] holds the
    ///   address that reaches the bus, so masking there is right; but `LEA` and
    ///   `PEA` put the value in an address register, where the top eight bits are
    ///   real. `LEA.json.gz` case `41fa [LEA (d16, PC), A0] 59` starts at
    ///   `pc = $000C00` with displacement `$FFFFA3E9` and expects `A0 =
    ///   $FFFFAFEB`, not the masked `$0000AFEB` — the report is #193, and 7162 of
    ///   that file's 8065 cases and 7153 of `PEA.json.gz`'s end above the mask,
    ///   so this is not an edge case.
    /// * only the *control* modes are addressable: `Dn`, `An`, `(An)+`, `-(An)`
    ///   and `#imm` have no address to hand over ([`is_control`], and the mode
    ///   list M68000UM gives both instructions). `decode` rejects those first, so
    ///   as in [`M68k::resolve_ea`] `None` here means a handler and the decoder
    ///   disagree, and the caller turns it into an illegal instruction.
    ///
    /// `mode` and `reg` are the two halves of the EA field as in
    /// [`M68k::resolve_ea`], and the extension words are consumed the same way —
    /// including `d16(PC)`/`d8(PC,Xn)` measuring from [`M68k::ext_addr`].
    pub(crate) fn ea_address(&mut self, bus: &mut impl Bus, mode: u8, reg: u8) -> Option<u32> {
        if !is_control(mode, reg) {
            return None;
        }
        let r = usize::from(reg);
        let addr = match mode {
            // (An): the register contents are the address, whole.
            0b010 => self.a[r],
            // d16(An).
            0b101 => self.a[r].wrapping_add(sign_extend_word(self.fetch_ext_word(bus))),
            // d8(An,Xn).
            0b110 => self.indexed_addr(bus, self.a[r]),
            // 111: the sub-mode is the register field, as in `resolve_ea`.
            0b111 => match reg {
                0b000 => sign_extend_word(self.fetch_ext_word(bus)),
                0b001 => self.fetch_ext_long(bus),
                0b010 => {
                    let base = self.ext_addr();
                    base.wrapping_add(sign_extend_word(self.fetch_ext_word(bus)))
                }
                0b011 => {
                    let base = self.ext_addr();
                    self.indexed_addr(bus, base)
                }
                // `is_control` excludes #imm and the three invalid sub-modes.
                _ => return None,
            },
            // `is_control` excludes Dn, An, (An)+ and -(An).
            _ => return None,
        };
        Some(addr)
    }

    /// The `d8(An,Xn)` / `d8(PC,Xn)` brief extension word: `An` (or the PC
    /// relative base) + index register + 8-bit displacement, plus the 2 internal
    /// clocks the index calculation costs (M68000UM, "Index Displacement
    /// Addressing Mode").
    ///
    /// Returns the unmasked sum: [`M68k::resolve_ea`] masks it on the way into
    /// [`Ea::Mem`] and [`M68k::ea_address`] hands it over whole.
    ///
    /// Bit layout of the brief extension word (M68000UM, "Index Registers"):
    /// bit 15 index register type (0 = Dn, 1 = An), bits 14..12 index register
    /// number, bit 11 index size (0 = sign-extended low word, 1 = full long),
    /// bits 7..0 signed displacement. Bits 10..8 are the scale field of the
    /// 68020 and later and are ignored here — silently, because the 68000 reads
    /// them as part of the word and acts on nothing: 2339 of the 2729
    /// `d8(...,Xn)` cases in `LEA.json.gz` carry nonzero bits there and all of
    /// them still match the sum with the bits dropped.
    fn indexed_addr(&mut self, bus: &mut impl Bus, base: u32) -> u32 {
        let brief = self.fetch_ext_word(bus);
        self.cycles += INDEX_CALC_CLOCKS;

        let index_reg = ((brief >> 12) & 0b111) as usize;
        let raw = if brief & 0x8000 != 0 {
            self.a[index_reg]
        } else {
            self.d[index_reg]
        };
        // Bit 11 selects the size of the index, including for an An index: the
        // suite's An-index cases with bit 11 = 0 use the low word only
        // (`4dfb [LEA (d8, PC, Xn), A6] 14` adds 0x56BE, A0's low word).
        let index = if brief & 0x0800 != 0 {
            raw
        } else {
            sign_extend_word(raw as u16)
        };
        let disp = sign_extend_byte(brief as u8);

        base.wrapping_add(index).wrapping_add(disp)
    }

    /// Address of the extension word that the next [`M68k::fetch_ext_word`] will
    /// hand over, for the modes whose operand address is measured from it.
    ///
    /// `prefetch == [word(pc), word(pc + 2)]` (see [`M68k::pc`]), so the word at
    /// the back of the queue — the first extension word of the instruction — is
    /// the one at `pc + 2`. Call this *before* popping, or it names the wrong
    /// word; that is the whole reason it is a helper instead of a `+ 2` at each
    /// call site.
    #[inline]
    pub(crate) fn ext_addr(&self) -> u32 {
        (self.pc + 2) & ADDR_MASK
    }

    /// Read the operand at `ea`: `size` bits, zero extended into the returned
    /// `u32` (sign extension is the instruction's business, not the EA layer's).
    ///
    /// Memory goes through [`M68k::read_b`] / [`M68k::read_w`] / [`M68k::read_l`]
    /// so every access charges its bus cycles and is masked to 24 bits; a
    /// register access costs nothing.
    pub(crate) fn read_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: Size) -> u32 {
        debug_assert!(
            ea.accepts_size(size),
            "illegal {size:?} access to {ea:?}: resolve_ea is the gate"
        );
        match ea {
            Ea::DataReg(r) => self.d[usize::from(r)] & size.mask(),
            Ea::AddrReg(r) => self.a[usize::from(r)] & size.mask(),
            Ea::Mem(addr) => match size {
                Size::Byte => u32::from(self.read_b(bus, addr)),
                Size::Word => u32::from(self.read_w(bus, addr)),
                Size::Long => self.read_l(bus, addr),
            },
            // Already fetched, at the right width, by `resolve_ea`; costs no bus
            // time, which is why an immediate operand is 4 clocks cheaper than
            // a memory one.
            Ea::Imm(value) => value & size.mask(),
        }
    }

    /// Write an address register, keeping the stack pointer's stored mirror in
    /// step.
    ///
    /// `a[7]` is only the active half of the stack pointer: the other half lives
    /// in `ssp`/`usp` for the current mode, and a `MOVEA` to A7 or an `(A7)+` /
    /// `-(A7)` moves it, so both places have to agree ([`M68k::set_active_sp`]).
    /// A save state and the conformance harness read `ssp`/`usp` back, which is
    /// how a core that only updated `a[7]` was caught — `MOVEA.w (A7)+,A0`
    /// (case `305f [MOVEA.w (A7)+, A0] 117`).
    #[inline]
    fn set_an(&mut self, reg: u8, value: u32) {
        if reg == SP_REG {
            self.set_active_sp(value);
        } else {
            self.a[usize::from(reg)] = value;
        }
    }

    /// Write `value` to the operand at `ea`, affecting only `size` bits.
    ///
    /// A byte or word write to Dn keeps the upper bits (M68000UM, section 3
    /// "Data Registers": narrower operands occupy the least significant bits).
    /// A word write to An is sign extended to 32 bits, so `MOVEA.W` of a
    /// negative value lands at `0xFFFF_xxxx`. Byte access to An cannot reach
    /// here: [`M68k::resolve_ea`] rejects it.
    ///
    /// TODO(M1-29, #37): a word or long access at an odd address is an address
    /// error (vector 2) before anything is read or written; this layer just
    /// passes the address on and `Ea::Mem` is where that check will hang.
    pub(crate) fn write_ea(&mut self, bus: &mut impl Bus, ea: Ea, size: Size, value: u32) {
        debug_assert!(
            ea.accepts_size(size),
            "illegal {size:?} access to {ea:?}: resolve_ea is the gate"
        );
        debug_assert!(
            !matches!(ea, Ea::Imm(_)),
            "immediates are source-only operands: nothing can be written to {ea:?}"
        );
        match ea {
            Ea::DataReg(r) => {
                let i = usize::from(r);
                self.d[i] = (self.d[i] & !size.mask()) | (value & size.mask());
            }
            Ea::AddrReg(r) => {
                let i = usize::from(r);
                let written = match size {
                    // Not a legal encoding (see `resolve_ea`); the debug_assert
                    // above fires on any path that gets here, and the low-byte
                    // merge keeps an unreachable release build well defined
                    // instead of panicking inside an instruction.
                    Size::Byte => (self.a[i] & !0xFF) | (value & 0xFF),
                    // M68000UM, section 1 "Address Registers": word operands
                    // written to an address register are sign extended.
                    Size::Word => (value as i16) as i32 as u32,
                    Size::Long => value,
                };
                // Only a MOVEA can write an An this way — a plain MOVE's
                // destination may not be an An (see `crate::decode`).
                self.set_an(r, written);
            }
            Ea::Mem(addr) => match size {
                Size::Byte => self.write_b(bus, addr, value as u8),
                Size::Word => self.write_w(bus, addr, value as u16),
                Size::Long => self.write_l(bus, addr, value),
            },
            // Unreachable in a debug build (the debug_assert above). No 68000
            // instruction encodes a write to an immediate, and a release build
            // must not panic inside an instruction, so the value is dropped.
            Ea::Imm(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testbus::cpu_with_program;

    /// Addressing mode numbers (M68000UM, "Effective Address Calculation").
    const DN: u8 = 0b000;
    const AN: u8 = 0b001;
    const AN_IND: u8 = 0b010;
    const AN_POST: u8 = 0b011;
    const AN_PRE: u8 = 0b100;
    const D16_AN: u8 = 0b101;
    const D8_AN_XN: u8 = 0b110;
    /// Mode 111 selects its sub-mode from the *register* field of the EA.
    const ABSOLUTE: u8 = 0b111;
    const ABS_W: u8 = 0b000;
    const ABS_L: u8 = 0b001;
    const D16_PC: u8 = 0b010;
    const D8_PC_XN: u8 = 0b011;
    const IMMEDIATE: u8 = 0b100;

    const SIZES: [Size; 3] = [Size::Byte, Size::Word, Size::Long];

    /// Where [`crate::testbus::cpu_with_program`] and [`ram_with_program`] load
    /// their program words (its `PROGRAM_ADDR`, which is private to `testbus`).
    const PROGRAM_BASE: u32 = 0x0000_1000;

    /// Test-LOCAL RAM that records every address it was asked about, copied
    /// from the pattern in `cpu.rs`'s test module: `testbus::TestBus`'s public
    /// shape is deliberately fixed (no access recorder, see #10 / #183), so the
    /// EA tests that need to count bus accesses keep their own.
    ///
    /// It covers the whole 24 bit space, unlike the 64 KB one in `cpu.rs`,
    /// because the M1-06 modes deliberately reach the top of it: `abs.W` of
    /// 0x8000 sign extends to 0xFFFF_8000 and must arrive at the bus as
    /// 0x00FF_8000, which a 64 KB RAM cannot tell apart from a wrong address.
    struct Ram {
        mem: Vec<u8>,
        reads: Vec<u32>,
        writes: Vec<u32>,
    }

    impl Ram {
        fn new() -> Self {
            Self {
                mem: vec![0; 1 << 24],
                reads: Vec::new(),
                writes: Vec::new(),
            }
        }

        /// Forget the accesses recorded so far, so `reads` / `writes` describe
        /// only what the test does next and not the reset sequence that set the
        /// CPU up (see [`ram_with_program`]).
        fn clear_log(&mut self) {
            self.reads.clear();
            self.writes.clear();
        }

        fn poke_byte(&mut self, addr: u32, value: u8) {
            self.mem[addr as usize] = value;
        }

        fn poke_words(&mut self, addr: u32, words: &[u16]) {
            for (i, &word) in words.iter().enumerate() {
                self.poke_word(addr + 2 * u32::try_from(i).unwrap(), word);
            }
        }

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

        fn poke_word(&mut self, addr: u32, value: u16) {
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }

        fn poke_long(&mut self, addr: u32, value: u32) {
            let i = addr as usize;
            self.mem[i..i + 4].copy_from_slice(&value.to_be_bytes());
        }
    }

    /// A CPU reset onto `words`, on the recording [`Ram`]: the same setup as
    /// [`crate::testbus::cpu_with_program`] (reset vector table, then a real
    /// [`M68k::reset`], so the queue already holds `words[0..2]` and every
    /// extension word has to come out of it), but with the access log cleared
    /// so assertions describe only what the test itself does.
    fn ram_with_program(words: &[u16]) -> (M68k, Ram) {
        let mut bus = Ram::new();
        bus.poke_long(0x0000_0000, 0x0001_0000); // SSP
        bus.poke_long(0x0000_0004, PROGRAM_BASE); // PC
        bus.poke_words(PROGRAM_BASE, words);

        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        bus.clear_log();
        (cpu, bus)
    }

    impl Bus for Ram {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.reads.push(addr);
            self.peek_byte(addr)
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.reads.push(addr);
            self.peek_word(addr)
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.writes.push(addr);
            let i = addr as usize;
            self.mem[i] = value;
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.writes.push(addr);
            let i = addr as usize;
            self.mem[i..i + 2].copy_from_slice(&value.to_be_bytes());
        }
    }

    // ---- mode 000: Dn ----

    #[test]
    fn read_dn_byte_word_long() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();
        cpu.d[0] = 0x1234_5678;

        let want = [0x78u32, 0x5678, 0x1234_5678];
        for (size, want) in SIZES.iter().zip(want) {
            let ea = cpu
                .resolve_ea(&mut bus, DN, 0, *size)
                .expect("Dn always resolves");
            assert_eq!(ea, Ea::DataReg(0));
            assert_eq!(
                cpu.read_ea(&mut bus, ea, *size),
                want,
                "D0 = 0x12345678 reads {size:?} as {want:#x}"
            );
        }
        assert!(
            bus.reads.is_empty(),
            "register direct never touches the bus"
        );
        assert_eq!(cpu.cycles, 0, "nor does it charge bus cycles");
    }

    #[test]
    fn write_dn_byte_keeps_upper() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();
        cpu.d[0] = 0xFFFF_FFFF;

        let ea = cpu.resolve_ea(&mut bus, DN, 0, Size::Byte).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Byte, 0x00);
        assert_eq!(cpu.d[0], 0xFFFF_FF00, "byte write replaces only bit 7..0");

        let ea = cpu.resolve_ea(&mut bus, DN, 0, Size::Word).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Word, 0x1234_0000);
        assert_eq!(cpu.d[0], 0xFFFF_0000, "word write replaces only bit 15..0");

        let ea = cpu.resolve_ea(&mut bus, DN, 0, Size::Long).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Long, 0xA5A5_5A5A);
        assert_eq!(cpu.d[0], 0xA5A5_5A5A, "long write replaces all 32 bits");
        assert!(bus.writes.is_empty());
    }

    // ---- mode 001: An ----

    #[test]
    fn read_an_direct_truncates_to_size() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();
        cpu.a[2] = 0x1234_5678;

        // Byte is deliberately absent: byte size with An direct is rejected,
        // see `byte_addr_reg_direct_is_rejected`.
        let want = [(Size::Word, 0x5678u32), (Size::Long, 0x1234_5678)];
        for (size, want) in want {
            let ea = cpu.resolve_ea(&mut bus, AN, 2, size).expect("An resolves");
            assert_eq!(ea, Ea::AddrReg(2));
            assert_eq!(cpu.read_ea(&mut bus, ea, size), want, "{size:?} read");
        }
    }

    #[test]
    fn write_word_to_an_sign_extends() {
        let mut cpu = M68k::new();
        let mut bus = Ram::new();

        let ea = cpu.resolve_ea(&mut bus, AN, 3, Size::Word).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Word, 0xFFFF_FFFE);
        assert_eq!(cpu.a[3], 0xFFFF_FFFE, "a negative word fills the top");

        let ea = cpu.resolve_ea(&mut bus, AN, 3, Size::Word).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Word, 0x0000_1000);
        assert_eq!(cpu.a[3], 0x0000_1000, "a positive word zeroes the top");

        // A long write is the register value verbatim, top byte included: the
        // suite keeps nonzero upper bytes in An (`MOVEA.w.json.gz` case
        // `3062 [MOVEA.w -(A2), A0] 8` ends with `a2 = 0x9DB1_C21F`).
        let ea = cpu.resolve_ea(&mut bus, AN, 3, Size::Long).unwrap();
        cpu.write_ea(&mut bus, ea, Size::Long, 0x9DB1_C221);
        assert_eq!(cpu.a[3], 0x9DB1_C221);
    }

    #[test]
    fn byte_addr_reg_direct_is_rejected() {
        let mut bus = Ram::new();
        let mut cpu = M68k::new();
        cpu.a[3] = 0x1000;

        assert_eq!(
            cpu.resolve_ea(&mut bus, AN, 3, Size::Byte),
            None,
            "no 68000 instruction encodes byte size with An direct"
        );
        assert_eq!(cpu.a[3], 0x1000, "rejection changes nothing");
        assert!(
            !Ea::AddrReg(3).accepts_size(Size::Byte),
            "and the EA itself knows it"
        );
        for size in [Size::Word, Size::Long] {
            assert!(Ea::AddrReg(3).accepts_size(size));
        }
        for size in SIZES {
            assert!(Ea::DataReg(3).accepts_size(size));
            assert!(Ea::Mem(0x1000).accepts_size(size));
        }
    }

    // ---- mode 010: (An) ----

    #[test]
    fn addr_reg_indirect_reads_and_writes() {
        let mut bus = Ram::new();
        bus.poke_long(0x1000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[0] = 0x1000;

        let ea = cpu.resolve_ea(&mut bus, AN_IND, 0, Size::Long).unwrap();
        assert_eq!(ea, Ea::Mem(0x1000));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Long), 0xDEAD_BEEF);
        assert_eq!(bus.reads, [0x1000, 0x1002], "a long is two words");

        cpu.write_ea(&mut bus, ea, Size::Word, 0x1111_2222);
        assert_eq!(bus.writes, [0x1000]);
        assert_eq!(
            bus.peek_long(0x1000),
            0x2222_BEEF,
            "a word write replaces only the addressed half"
        );
        assert_eq!(cpu.a[0], 0x1000, "(An) does not move the register");
    }

    // ---- mode 011: (An)+ ----

    #[test]
    fn postinc_steps_by_size() {
        for (i, size) in SIZES.into_iter().enumerate() {
            let mut bus = Ram::new();
            bus.poke_long(0x1000, 0xDEAD_BEEF);
            let mut cpu = M68k::new();
            cpu.a[0] = 0x1000;

            let want = [0xDEu32, 0xDEAD, 0xDEAD_BEEF][i];
            let ea = cpu.resolve_ea(&mut bus, AN_POST, 0, size).unwrap();
            assert_eq!(ea, Ea::Mem(0x1000), "the old value is the address");
            assert_eq!(
                cpu.read_ea(&mut bus, ea, size),
                want,
                "big endian: a narrow read takes the {size:?} at the address"
            );
            assert_eq!(
                cpu.a[0],
                0x1000 + size.bytes(),
                "A0 steps by {} for {size:?}",
                size.bytes()
            );
        }
    }

    #[test]
    fn byte_postinc_a7_steps_by_2() {
        let mut bus = Ram::new();
        bus.poke_long(0x1000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[7] = 0x1000;

        let ea = cpu.resolve_ea(&mut bus, AN_POST, 7, Size::Byte).unwrap();
        assert_eq!(ea, Ea::Mem(0x1000));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Byte), 0xDE);
        assert_eq!(
            cpu.a[7], 0x1002,
            "A7 keeps the stack word aligned: a byte still steps by 2"
        );

        // It is the byte step that is special, not A7 always stepping by 2.
        cpu.a[7] = 0x1000;
        let ea = cpu.resolve_ea(&mut bus, AN_POST, 7, Size::Long).unwrap();
        cpu.read_ea(&mut bus, ea, Size::Long);
        assert_eq!(cpu.a[7], 0x1004);
    }

    // ---- mode 100: -(An) ----

    #[test]
    fn predec_long() {
        let mut bus = Ram::new();
        bus.poke_long(0x2000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[1] = 0x2004;

        let ea = cpu.resolve_ea(&mut bus, AN_PRE, 1, Size::Long).unwrap();
        assert_eq!(ea, Ea::Mem(0x2000), "the decremented value is the address");
        assert_eq!(cpu.a[1], 0x2000, "A1 = 0x2004 - 4");
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Long), 0xDEAD_BEEF);
        assert_eq!(bus.reads, [0x2000, 0x2002]);
    }

    #[test]
    fn byte_predec_a7_steps_by_2() {
        let mut bus = Ram::new();
        bus.poke_long(0x2000, 0xDEAD_BEEF);
        let mut cpu = M68k::new();
        cpu.a[7] = 0x2004;

        let ea = cpu.resolve_ea(&mut bus, AN_PRE, 7, Size::Byte).unwrap();
        assert_eq!(ea, Ea::Mem(0x2002));
        assert_eq!(
            cpu.a[7], 0x2002,
            "A7 keeps the stack word aligned: a byte still steps by 2"
        );
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Byte), 0xBE);

        // A0 is not special: a byte predecrement steps by 1.
        cpu.a[0] = 0x2004;
        let ea = cpu.resolve_ea(&mut bus, AN_PRE, 0, Size::Byte).unwrap();
        assert_eq!(ea, Ea::Mem(0x2003));
        assert_eq!(cpu.a[0], 0x2003);
    }

    #[test]
    fn predec_charges_two_extra_clocks() {
        let mut cpu = M68k::new();
        cpu.a[1] = 0x2004;

        let mut bus = Ram::new();
        let ea = cpu.resolve_ea(&mut bus, AN_PRE, 1, Size::Long).unwrap();
        assert_eq!(
            cpu.cycles, PREDEC_CALC_CLOCKS,
            "-(An) charges 2 internal clocks for the address calculation"
        );

        cpu.read_ea(&mut bus, ea, Size::Long);
        assert_eq!(
            cpu.cycles,
            PREDEC_CALC_CLOCKS + 8,
            "then the long read charges its own two bus cycles"
        );

        cpu.cycles = 0;
        cpu.resolve_ea(&mut bus, AN_POST, 1, Size::Long);
        cpu.resolve_ea(&mut bus, DN, 1, Size::Long);
        assert_eq!(cpu.cycles, 0, "(An)+ and Dn charge no extra clocks");
    }

    #[test]
    fn predec_wraps_below_zero() {
        let mut bus = Ram::new();
        let mut cpu = M68k::new();
        cpu.a[7] = 0;

        let ea = cpu.resolve_ea(&mut bus, AN_PRE, 7, Size::Word).unwrap();
        assert_eq!(
            cpu.a[7], 0xFFFF_FFFE,
            "the address register itself is a full 32-bit register"
        );
        assert_eq!(
            ea,
            Ea::Mem(0x00FF_FFFE),
            "the address that reaches the bus is masked to 24 bits"
        );
    }

    #[test]
    fn mem_ea_is_masked_to_24_bits() {
        let mut bus = Ram::new();
        bus.poke_word(0x1000, 0xABCD);
        let mut cpu = M68k::new();
        cpu.a[0] = 0xFF00_1000;

        let ea = cpu.resolve_ea(&mut bus, AN_IND, 0, Size::Word).unwrap();
        assert_eq!(ea, Ea::Mem(0x0000_1000));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Word), 0xABCD);
        assert_eq!(bus.reads, [0x1000]);
    }

    // ---- read-modify-write ----

    #[test]
    fn read_modify_write_resolves_postinc_once() {
        let mut bus = Ram::new();
        bus.poke_long(0x1000, 0x1111_2222);
        let mut cpu = M68k::new();
        cpu.a[0] = 0x1000;

        // What an `ADD.W #x, (A0)+` does: resolve once, then read and write the
        // same Ea. Resolving twice would increment A0 twice.
        let ea = cpu.resolve_ea(&mut bus, AN_POST, 0, Size::Word).unwrap();
        let value = cpu.read_ea(&mut bus, ea, Size::Word);
        assert_eq!(value, 0x1111, "the word at 0x1000 is the high half");
        cpu.write_ea(&mut bus, ea, Size::Word, value ^ 0xFFFF);

        assert_eq!(cpu.a[0], 0x1002, "one increment, not two");
        assert_eq!(bus.reads, [0x1000]);
        assert_eq!(bus.writes, [0x1000]);
        assert_eq!(
            bus.peek_long(0x1000),
            0xEEEE_2222,
            "only the low word moved"
        );
    }

    // ---- mode 101: d16(An) ----

    /// `MOVE.L d16(A0), D0` (0x2028): one extension word, sign extended, added
    /// to An. M68000UM, "Address Register Displacement Addressing Mode".
    #[test]
    fn d16_an_negative_displacement() {
        let (mut cpu, mut bus) = ram_with_program(&[0x2028, 0xFFFE, 0x4E71]);
        bus.poke_word(0x1FFE, 0xC0DE);
        cpu.a[0] = 0x2000;
        let cycles = cpu.cycles;

        let ea = cpu
            .resolve_ea(&mut bus, D16_AN, 0, Size::Word)
            .expect("d16(An) resolves");

        assert_eq!(ea, Ea::Mem(0x1FFE), "d16 = 0xFFFE is -2");
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Word), 0xC0DE);
        assert_eq!(
            bus.reads,
            [PROGRAM_BASE + 4, 0x1FFE],
            "the displacement came out of the queue, so the only fetch before the operand is the refill at pc + 4"
        );
        assert_eq!(cpu.a[0], 0x2000, "d16(An) never writes the address back");
        assert_eq!(cpu.pc, PROGRAM_BASE + 2, "one extension word consumed");
        assert_eq!(
            cpu.cycles - cycles,
            8,
            "queue refill (4) + operand read (4): d16(An) charges no internal clocks"
        );

        // Only bit 15 of the displacement is the sign bit.
        let (mut cpu, mut bus) = ram_with_program(&[0x2028, 0x7FFF, 0x4E71]);
        cpu.a[0] = 0x2000;
        let ea = cpu
            .resolve_ea(&mut bus, D16_AN, 0, Size::Word)
            .expect("d16(An) resolves");
        assert_eq!(ea, Ea::Mem(0x9FFF), "0x7FFF is +32767, not negative");
    }

    // ---- mode 110: d8(An,Xn) ----

    /// Bit 11 = 0 takes the index register's low word and sign extends it, so
    /// D1 = 0xFFFF indexes -1 however wide the register is.
    #[test]
    fn d8_an_xn_word_index_sign_extends() {
        // MOVE.B d8(A0,D1), D0 (0x1030); brief 0x1004 = Dn, D1, word, +4.
        let (mut cpu, mut bus) = ram_with_program(&[0x1030, 0x1004, 0x4E71]);
        bus.poke_byte(0x2003, 0x5A);
        cpu.a[0] = 0x2000;
        cpu.d[1] = 0x0000_FFFF;
        let cycles = cpu.cycles;

        let ea = cpu
            .resolve_ea(&mut bus, D8_AN_XN, 0, Size::Byte)
            .expect("d8(An,Xn) resolves");

        assert_eq!(ea, Ea::Mem(0x2003), "A0 + index(-1) + d8(+4) = 0x2003");
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Byte), 0x5A);
        assert_eq!(bus.reads, [PROGRAM_BASE + 4, 0x2003]);
        assert_eq!(
            cpu.cycles - cycles,
            4 + 4 + INDEX_CALC_CLOCKS,
            "the index calculation costs 2 internal clocks on top of the two bus cycles"
        );
    }

    /// Bit 11 = 1 uses the whole 32-bit index instead. An index whose low word
    /// disagrees with its long is the only thing that tells the two apart.
    #[test]
    fn d8_an_xn_long_index() {
        // MOVE.L d8(A0,D1), D0 (0x2030); 0x1804 is 0x1004 with bit 11 set.
        // (brief word, expected address, word parked at that address)
        for (brief_word, want, value) in [
            (0x1004u16, 0x0000_2004u32, 0x1111u32),
            (0x1804, 0x0001_2004, 0x2222),
        ] {
            let (mut cpu, mut bus) = ram_with_program(&[0x2030, brief_word, 0x4E71]);
            bus.poke_word(0x0000_2004, 0x1111);
            bus.poke_word(0x0001_2004, 0x2222);
            cpu.a[0] = 0x2000;
            cpu.d[1] = 0x0001_0000; // low word 0x0000, long 0x10000

            let ea = cpu
                .resolve_ea(&mut bus, D8_AN_XN, 0, Size::Word)
                .expect("d8(An,Xn) resolves");

            assert_eq!(
                ea,
                Ea::Mem(want),
                "brief {brief_word:#06x} (bit 11 = {:b})",
                (brief_word >> 11) & 1
            );
            assert_eq!(cpu.read_ea(&mut bus, ea, Size::Word), value);
        }
    }

    /// Bit 15 = 1 picks an address register as the index, and bits 10..8 — the
    /// scale field of later chips — are ignored on the 68000 (M68000UM,
    /// "Index Registers": they are documented as unimplemented/zero). 2339 of
    /// the 2729 `d8(...,Xn)` cases in `LEA.json.gz` carry nonzero bits there
    /// and all of them still match the index formula with the bits dropped.
    #[test]
    fn d8_an_xn_an_index_and_ignored_scale_bits() {
        // MOVE.L d8(A5,A3), D0 (0x2075); brief 0xB808 = A3, long, +8.
        for scale_bits in [0x0000u16, 0x0300] {
            let (mut cpu, mut bus) = ram_with_program(&[0x2075, 0xB808 | scale_bits, 0x4E71]);
            cpu.a[5] = 0x0000_4000;
            cpu.a[3] = 0x0002_0000; // low word 0, so a word index would give 0x4008

            let ea = cpu
                .resolve_ea(&mut bus, D8_AN_XN, 5, Size::Long)
                .expect("d8(An,Xn) resolves");

            assert_eq!(
                ea,
                Ea::Mem(0x0002_4008),
                "A5 + A3 + 8, with brief bits 10..8 = {scale_bits:#x} ignored"
            );
        }
    }

    // ---- mode 111/000: abs.W ----

    /// The sign extension of `abs.W` happens on the way in, and the address
    /// `Ea::Mem` carries — and therefore the address the bus sees — is the
    /// masked 24-bit one. See [`masking_wraps_at_24_bits`] for why masking
    /// here and masking at the bus are the same address.
    #[test]
    fn abs_w_sign_extends() {
        // MOVE.L abs.W, D0 (0x2038).
        let (mut cpu, mut bus) = ram_with_program(&[0x2038, 0x8000, 0x4E71]);
        bus.poke_word(0x00FF_8000, 0xBEEF);

        assert_eq!(
            sign_extend_word(0x8000),
            0xFFFF_8000,
            "0x8000 sign extends to 0xFFFF8000 as a 32-bit value"
        );

        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, ABS_W, Size::Word)
            .expect("abs.W resolves");
        assert_eq!(ea, Ea::Mem(0x00FF_8000));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Word), 0xBEEF);
        assert_eq!(
            bus.reads,
            [PROGRAM_BASE + 4, 0x00FF_8000],
            "the bus is asked about 0xFF8000, never 0xFFFF8000"
        );

        let (mut cpu, mut bus) = ram_with_program(&[0x2038, 0x2345, 0x4E71]);
        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, ABS_W, Size::Word)
            .expect("abs.W resolves");
        assert_eq!(ea, Ea::Mem(0x2345), "a positive abs.W is unchanged");
    }

    // ---- mode 111/001: abs.L ----

    /// Two extension words, high word first — the same order as every long in
    /// the core (M68000UM, "Absolute Addressing Mode" and "Data Organization").
    #[test]
    fn abs_l() {
        // MOVE.L abs.L, D0 (0x2039) with the long $00012345.
        let (mut cpu, mut bus) = cpu_with_program(&[0x2039, 0x0001, 0x2345, 0x4E71]);
        bus.write_long(0x0001_2345, 0xDEAD_BEEF);

        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, ABS_L, Size::Long)
            .expect("abs.L resolves");

        assert_eq!(ea, Ea::Mem(0x0001_2345));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Long), 0xDEAD_BEEF);
        assert_eq!(cpu.pc, PROGRAM_BASE + 4, "both extension words consumed");
        assert_eq!(
            cpu.prefetch,
            [0x2345, 0x4E71],
            "the queue slid two words past them, so the next instruction still starts in the right place"
        );
    }

    // ---- modes 111/010 and 111/011: PC relative ----

    /// The PC-relative base is the address of the extension word itself, which
    /// in this core's convention ([`M68k::pc`] is the queued opcode) is
    /// `pc + 2`, captured before the word is popped — [`M68k::ext_addr`].
    ///
    /// Measured off the pinned suite, not off the manual. `LEA.json.gz` case
    /// `41fa [LEA (d16, PC), A0] 59`: `initial.pc = 0x000C00`,
    /// `prefetch = [0x41FA, 0xA3E9]` — so the extension word 0xA3E9 is already
    /// in the queue, at 0x000C02, and the case's only instruction-stream reads
    /// are the refills at 0x000C04 / 0x000C06 — and `final.a0 = 0xFFFFAFEB`,
    /// which is `0x000C02 + 0xFFFFA3E9`. Neither `pc` (0xFFFFA3E9) nor the
    /// manual's "next word to be accessed", `pc + 4` (0xFFFFAFED), reproduces
    /// it. All 286 `d16(PC)` and 287 `d8(PC,Xn)` cases in that file agree with
    /// the `pc + 2` base and with nothing else.
    #[test]
    fn pc_relative_uses_ext_word_address() {
        // MOVE.L d16(PC), D0 (0x203A) with a forward displacement of +6.
        let (mut cpu, mut bus) = cpu_with_program(&[0x203A, 0x0006, 0x4E71]);
        assert_eq!(
            cpu.ext_addr(),
            PROGRAM_BASE + 2,
            "the extension word about to be popped sits at pc + 2"
        );

        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, D16_PC, Size::Long)
            .expect("d16(PC) resolves");
        assert_eq!(
            ea,
            Ea::Mem(PROGRAM_BASE + 8),
            "0x1002 + 6, the extension word's own address plus the displacement"
        );
        assert_eq!(cpu.pc, PROGRAM_BASE + 2, "one extension word consumed");

        // Backwards: -2 from the extension word is the opcode word itself.
        let (mut cpu, mut bus) = cpu_with_program(&[0x203A, 0xFFFE, 0x4E71]);
        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, D16_PC, Size::Long)
            .expect("d16(PC) resolves");
        assert_eq!(ea, Ea::Mem(PROGRAM_BASE), "0x1002 - 2 is the opcode");
    }

    /// `d8(PC,Xn)` uses the same base as `d16(PC)` — the brief word's own
    /// address — and pays the same 2 index clocks as `d8(An,Xn)`.
    #[test]
    fn pc_indexed_uses_ext_word_address() {
        // MOVE.L d8(PC,D1), D0 (0x203B); brief 0x1104 = Dn, D1, word, +4.
        let (mut cpu, mut bus) = ram_with_program(&[0x203B, 0x1104, 0x4E71]);
        cpu.d[1] = 0x0000_0010;
        let cycles = cpu.cycles;

        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, D8_PC_XN, Size::Long)
            .expect("d8(PC,Xn) resolves");

        assert_eq!(
            ea,
            Ea::Mem(PROGRAM_BASE + 2 + 0x10 + 4),
            "the brief word's own address, plus the index, plus d8"
        );
        assert_eq!(cpu.cycles - cycles, INDEX_CALC_CLOCKS + 4);
    }

    // ---- mode 111/100: #imm ----

    /// Byte immediate occupies the LOW byte of one extension word, word
    /// immediate is one whole word, long immediate two words high first
    /// (M68000UM, "Immediate Addressing Mode").
    #[test]
    fn immediate_byte_word_long() {
        // MOVE.B #imm, D0 (0x003C).
        let (mut cpu, mut bus) = cpu_with_program(&[0x003C, 0x1234, 0x4E71]);
        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, IMMEDIATE, Size::Byte)
            .expect("#imm resolves");
        assert_eq!(ea, Ea::Imm(0x34), "the low byte of 0x1234, not the high");
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Byte), 0x34);
        assert_eq!(cpu.pc, PROGRAM_BASE + 2, "one extension word");

        // MOVE.W #imm, D0 (0x303C). No sign extension: like every other mode,
        // the operand comes back zero extended at its own width.
        let (mut cpu, mut bus) = cpu_with_program(&[0x303C, 0x1234, 0x4E71]);
        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, IMMEDIATE, Size::Word)
            .expect("#imm resolves");
        assert_eq!(ea, Ea::Imm(0x1234));
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Word), 0x1234);
        assert_eq!(cpu.pc, PROGRAM_BASE + 2);

        // MOVE.L #imm, D0 (0x203C).
        let (mut cpu, mut bus) = cpu_with_program(&[0x203C, 0x1234, 0x5678, 0x4E71]);
        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, IMMEDIATE, Size::Long)
            .expect("#imm resolves");
        assert_eq!(ea, Ea::Imm(0x1234_5678), "two words, high first");
        assert_eq!(cpu.read_ea(&mut bus, ea, Size::Long), 0x1234_5678);
        assert_eq!(cpu.pc, PROGRAM_BASE + 4, "two extension words");
    }

    /// An immediate is a source operand: no 68000 instruction encodes a write
    /// to one, so a caller that tries has a decode bug. Debug builds say so;
    /// release builds must stay defined (no panic inside an instruction).
    #[test]
    #[should_panic(expected = "immediates are source-only")]
    fn write_to_immediate_is_a_bug() {
        let (mut cpu, mut bus) = cpu_with_program(&[0x303C, 0x1234, 0x4E71]);
        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, IMMEDIATE, Size::Word)
            .unwrap();
        cpu.write_ea(&mut bus, ea, Size::Word, 0xFFFF);
    }

    // ---- masking ----

    /// Masking inside [`M68k::resolve_ea`] (what [`Ea::Mem`] carries) and
    /// masking late, at the bus, give the same address for every mode here,
    /// because masking is a ring homomorphism for addition:
    /// `(a + b) mod 2^24 == (a mod 2^24 + b mod 2^24) mod 2^24`. Pinned at the
    /// wrap points, where any other rule would show up.
    #[test]
    fn masking_wraps_at_24_bits() {
        // (An, d16, expected masked address)
        let cases = [
            (0x00FF_FFFCu32, 0x0006u16, 0x0000_0002u32), // off the top, wraps to 2
            (0x0000_0000, 0xFFFE, 0x00FF_FFFE),          // below 0, wraps to the top
            (0xFFFF_FFFF, 0x0003, 0x0000_0002),          // An itself above 24 bits
        ];
        for (an, disp, want) in cases {
            let (mut cpu, mut bus) = ram_with_program(&[0x2028, disp, 0x4E71]);
            cpu.a[0] = an;

            let ea = cpu
                .resolve_ea(&mut bus, D16_AN, 0, Size::Long)
                .expect("d16(An) resolves");

            assert_eq!(
                ea,
                Ea::Mem(want),
                "0x{an:x} + d16 {disp:#x} wraps to 0x{want:x}"
            );
            assert_eq!(
                ea,
                Ea::Mem(an.wrapping_add(sign_extend_word(disp)) & ADDR_MASK),
                "early masking is the same address as masking the full 32-bit sum at the bus"
            );
        }

        // abs.L is truncated the same way: there is no 25th address line.
        let (mut cpu, mut bus) = ram_with_program(&[0x2039, 0xFF80, 0x0000, 0x4E71]);
        let ea = cpu
            .resolve_ea(&mut bus, ABSOLUTE, ABS_L, Size::Long)
            .expect("abs.L resolves");
        assert_eq!(ea, Ea::Mem(0x0080_0000), "0xFF800000 -> 0x800000");
    }

    // ---- the extension-word ladder ----

    /// How many extension words each mode consumes, i.e. how far `pc` moves
    /// past the opcode word. Every mode's count matters because it decides
    /// where the *next* instruction starts: a mode that consumes one word too
    /// few or too many corrupts the whole instruction stream after it
    /// (M68000UM, "Effective Address Calculation", the per-mode word counts).
    #[test]
    fn ext_words_consumed() {
        // One row: (label, program holding its extension words, mode, reg,
        // size, extension words it must consume). Named as a type because the
        // bare tuple is dense enough for clippy to object.
        type Row = (&'static str, &'static [u16], u8, u8, Size, u32);
        let cases: &[Row] = &[
            ("Dn", &[0x2007, 0x4E71], DN, 7, Size::Long, 0),
            ("An", &[0x2009, 0x4E71], AN, 1, Size::Long, 0),
            ("(A6)", &[0x2026, 0x4E71], AN_IND, 6, Size::Long, 0),
            ("(A6)+", &[0x2036, 0x4E71], AN_POST, 6, Size::Long, 0),
            ("-(A6)", &[0x2046, 0x4E71], AN_PRE, 6, Size::Long, 0),
            (
                "d16(A0)",
                &[0x2028, 0x0000, 0x4E71],
                D16_AN,
                0,
                Size::Long,
                1,
            ),
            (
                "d8(A0,Xn)",
                &[0x2030, 0x0000, 0x4E71],
                D8_AN_XN,
                0,
                Size::Long,
                1,
            ),
            (
                "abs.W",
                &[0x2038, 0x1000, 0x4E71],
                ABSOLUTE,
                ABS_W,
                Size::Long,
                1,
            ),
            (
                "abs.L",
                &[0x2039, 0x0000, 0x1000, 0x4E71],
                ABSOLUTE,
                ABS_L,
                Size::Long,
                2,
            ),
            (
                "d16(PC)",
                &[0x203A, 0x0000, 0x4E71],
                ABSOLUTE,
                D16_PC,
                Size::Long,
                1,
            ),
            (
                "d8(PC,Xn)",
                &[0x203B, 0x0000, 0x4E71],
                ABSOLUTE,
                D8_PC_XN,
                Size::Long,
                1,
            ),
            (
                "#imm byte",
                &[0x003C, 0x0000, 0x4E71],
                ABSOLUTE,
                IMMEDIATE,
                Size::Byte,
                1,
            ),
            (
                "#imm word",
                &[0x303C, 0x0000, 0x4E71],
                ABSOLUTE,
                IMMEDIATE,
                Size::Word,
                1,
            ),
            (
                "#imm long",
                &[0x203C, 0x0000, 0x1000, 0x4E71],
                ABSOLUTE,
                IMMEDIATE,
                Size::Long,
                2,
            ),
        ];

        for (label, program, mode, reg, size, words) in cases {
            let (mut cpu, mut bus) = cpu_with_program(program);
            cpu.a[6] = 0x0000_2000;

            cpu.resolve_ea(&mut bus, *mode, *reg, *size)
                .unwrap_or_else(|| panic!("{label} must resolve"));

            assert_eq!(
                cpu.pc,
                PROGRAM_BASE + 2 * words,
                "{label}: {words} extension word(s) on top of the queued opcode"
            );
        }
    }

    // ---- invalid modes ----

    /// Mode `111` with register field `101`, `110` or `111` is not an
    /// addressing mode at all: it is an illegal instruction (M68000UM,
    /// "Effective Address Calculation" mode 111, and "Illegal Instruction
    /// Format"). Turning it into the illegal-instruction exception is M1-07
    /// (#15), so this layer only has to decline cleanly — `None`, and no side
    /// effect whatsoever: no queue word popped, no cycle charged. (Modes 101,
    /// 110 and the rest of 111 were this test's territory until M1-06 (#14)
    /// implemented them; they are covered by the tests above.)
    #[test]
    fn invalid_111_101_to_111_resolves_to_none() {
        for reg in 0b101..0b1000u8 {
            for size in SIZES {
                let (mut cpu, mut bus) = ram_with_program(&[0x4E71, 0x1234, 0x5678]);
                cpu.a[7] = 0x2000;
                let before = cpu.clone();

                assert_eq!(
                    cpu.resolve_ea(&mut bus, ABSOLUTE, reg, size),
                    None,
                    "111/{reg:b} is illegal and decoded by M1-07 (#15)"
                );
                assert_eq!(cpu, before, "and must not disturb the CPU");
                assert!(bus.reads.is_empty(), "nor touch the bus");
            }
        }
    }

    // ---- Size helpers ----

    #[test]
    fn size_helpers() {
        assert_eq!(Size::Byte.bytes(), 1);
        assert_eq!(Size::Word.bytes(), 2);
        assert_eq!(Size::Long.bytes(), 4);

        assert_eq!(Size::Byte.mask(), 0xFF);
        assert_eq!(Size::Word.mask(), 0xFFFF);
        assert_eq!(Size::Long.mask(), 0xFFFF_FFFF);

        assert_eq!(Size::Byte.msb(), 0x80);
        assert_eq!(Size::Word.msb(), 0x8000);
        assert_eq!(Size::Long.msb(), 0x8000_0000);

        assert_eq!(Size::from_bits(0), Some(Size::Byte));
        assert_eq!(Size::from_bits(1), Some(Size::Word));
        assert_eq!(Size::from_bits(2), Some(Size::Long));
        assert_eq!(Size::from_bits(3), None, "11 is the invalid size code");
        assert_eq!(Size::from_bits(0xFFFF), None, "and so is anything wider");
    }

    // ---- Operand-class predicates (M68000PM/AD 2.2.12) ----

    /// `(mode, reg)` as one byte, so an expected set reads like the manual's
    /// table: high three bits mode, low three register.
    const fn enc(mode: u8, reg: u8) -> u8 {
        (mode << 3) | reg
    }

    /// All 64 EA fields, in table order.
    fn all_ea_fields() -> Vec<(u8, u8)> {
        (0..8u8)
            .flat_map(|mode| (0..8u8).map(move |reg| (mode, reg)))
            .collect()
    }

    /// The eight fields of one mode, `reg` running `000`-`111`.
    fn mode_block(mode: u8) -> Vec<u8> {
        (0..8u8).map(|reg| enc(mode, reg)).collect()
    }

    /// What a predicate accepts, in table order.
    fn accepted(pred: fn(u8, u8) -> bool) -> Vec<u8> {
        all_ea_fields()
            .into_iter()
            .filter(|&(mode, reg)| pred(mode, reg))
            .map(|(mode, reg)| enc(mode, reg))
            .collect()
    }

    /// Every mode, with the three mode-`111` sub-modes the 68000 does not have
    /// taken back out again.
    fn every_valid_field() -> Vec<u8> {
        let mut want: Vec<u8> = (0..8u8).flat_map(mode_block).collect();
        for reg in [0b101, 0b110, 0b111] {
            want.retain(|&field| field != enc(ABSOLUTE, reg));
        }
        want
    }

    #[test]
    fn ea_mode_valid_rejects_only_the_modes_the_68000_lacks() {
        // M68000UM, "Effective Address Calculation": mode `111` takes its
        // sub-mode from the register field, and `101` (68010 `(d8,PC,Xn)`) and
        // `110`/`111` (68020+ bracketed) do not exist here. These are exactly
        // the three fields `resolve_ea` declines (see `mode_111_101_to_111`).
        assert_eq!(accepted(ea_mode_valid), every_valid_field());
        for reg in [0b101, 0b110, 0b111] {
            assert!(
                !ea_mode_valid(ABSOLUTE, reg),
                "mode 111 reg {reg:03b} must not be valid"
            );
        }
    }

    #[test]
    fn is_data_accepts_every_valid_mode() {
        // M68000PM/AD 2.2.12: the Data column is marked for every mode, An
        // included (word and long only, which `Ea::accepts_size` enforces).
        assert_eq!(accepted(is_data), every_valid_field());
    }

    #[test]
    fn is_memory_is_the_addressed_modes() {
        // Memory = the address register indirect group plus the absolute and
        // PC-relative forms. Not Dn, not An, not #immediate (M68000PM/AD 2.2.12).
        let mut want = Vec::new();
        for mode in [AN_IND, AN_POST, AN_PRE, D16_AN, D8_AN_XN] {
            want.extend(mode_block(mode));
        }
        want.extend([enc(ABSOLUTE, ABS_W), enc(ABSOLUTE, ABS_L)]);
        want.extend([enc(ABSOLUTE, D16_PC), enc(ABSOLUTE, D8_PC_XN)]);
        assert_eq!(accepted(is_memory), want);
    }

    #[test]
    fn is_control_excludes_the_increment_modes() {
        // `JMP`, `JSR` and `LEA` take the address register indirect group minus
        // `(An)+` and `-(An)`, plus absolute and PC-relative (M68000PM/AD
        // section 8, their addressing-mode tables).
        let mut want = Vec::new();
        for mode in [AN_IND, D16_AN, D8_AN_XN] {
            want.extend(mode_block(mode));
        }
        want.extend([
            enc(ABSOLUTE, ABS_W),
            enc(ABSOLUTE, ABS_L),
            enc(ABSOLUTE, D16_PC),
            enc(ABSOLUTE, D8_PC_XN),
        ]);
        assert_eq!(accepted(is_control), want);
    }

    #[test]
    fn is_alterable_excludes_pc_relative_and_immediate() {
        // Writable = Dn, An, the address register indirect group, absolute.
        // `d16(PC)` / `d8(PC,Xn)` name a spot in the instruction stream and
        // #immediate is its own value, so neither can hold a result.
        let mut want = Vec::new();
        for mode in [DN, AN, AN_IND, AN_POST, AN_PRE, D16_AN, D8_AN_XN] {
            want.extend(mode_block(mode));
        }
        want.extend([enc(ABSOLUTE, ABS_W), enc(ABSOLUTE, ABS_L)]);
        assert_eq!(accepted(is_alterable), want);
    }

    /// The combination the ticket calls out: An direct and the PC-relative
    /// modes are data-class but not destinations, so no `MOVE.W D0,A0`, and no
    /// `CLR d16(PC)`.
    #[test]
    fn is_data_alterable_is_false_for_an_and_pc_relative() {
        assert!(is_data_alterable(DN, 0), "D0");
        assert!(is_data_alterable(AN_IND, 0), "(A0)");
        assert!(is_data_alterable(AN_POST, 0), "(A0)+");
        assert!(is_data_alterable(AN_PRE, 0), "-(A0)");
        assert!(is_data_alterable(D16_AN, 0), "d16(A0)");
        assert!(is_data_alterable(D8_AN_XN, 0), "d8(A0,X0)");
        assert!(is_data_alterable(ABSOLUTE, ABS_W), "abs.W");
        assert!(is_data_alterable(ABSOLUTE, ABS_L), "abs.L");

        assert!(!is_data_alterable(AN, 0), "An is not a data destination");
        assert!(!is_data_alterable(AN, 7), "not even A7");
        assert!(!is_data_alterable(ABSOLUTE, D16_PC), "d16(PC)");
        assert!(!is_data_alterable(ABSOLUTE, D8_PC_XN), "d8(PC,Xn)");
        assert!(!is_data_alterable(ABSOLUTE, IMMEDIATE), "#immediate");
        for reg in [0b101, 0b110, 0b111] {
            assert!(
                !is_data_alterable(ABSOLUTE, reg),
                "invalid sub-mode {reg:03b} is not a destination either"
            );
        }
    }

    #[test]
    fn is_memory_alterable_is_the_writable_memory_modes() {
        let mut want = Vec::new();
        for mode in [AN_IND, AN_POST, AN_PRE, D16_AN, D8_AN_XN] {
            want.extend(mode_block(mode));
        }
        want.extend([enc(ABSOLUTE, ABS_W), enc(ABSOLUTE, ABS_L)]);
        assert_eq!(accepted(is_memory_alterable), want);
    }

    /// An invalid field is in no class at all: a decode that asks one of these
    /// predicates gets `false` from every one of them, so it can route the
    /// opcode to the illegal-instruction exception without special cases.
    #[test]
    fn invalid_fields_are_in_no_class() {
        for reg in [0b101, 0b110, 0b111] {
            for (name, pred) in [
                ("is_data", is_data as fn(u8, u8) -> bool),
                ("is_memory", is_memory),
                ("is_control", is_control),
                ("is_alterable", is_alterable),
                ("is_data_alterable", is_data_alterable),
                ("is_memory_alterable", is_memory_alterable),
            ] {
                assert!(
                    !pred(ABSOLUTE, reg),
                    "{name} must reject mode 111 reg {reg:03b}"
                );
            }
        }
    }
}
