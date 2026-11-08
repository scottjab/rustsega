//! Shifts and rotates, register form: `ASL`, `ASR`, `LSL`, `LSR`, `ROL`, `ROR`,
//! `ROXL`, `ROXR` (M68000UM "ASL, ASR, LSL, LSR", "ROL, ROR", "ROXL, ROXR";
//! M1-18, #26).
//!
//! One encoding covers the eight mnemonics in three sizes, `1110 ccc d ss i tt
//! rrr`:
//!
//! | Field | Bits | Meaning |
//! |---|---|---|
//! | `ccc` | 11..9 | shift count, or the register holding it |
//! | `d` | 8 | direction: `1` left, `0` right |
//! | `ss` | 7..6 | size: `00` byte, `01` word, `10` long; `11` is *not* a size |
//! | `i` | 5 | count source: `0` immediate `ccc`, `1` `D(ccc)` modulo 64 |
//! | `tt` | 4..3 | `00` AS, `01` LS, `10` ROX, `11` RO |
//! | `rrr` | 2..0 | the data register shifted |
//!
//! `ss` = `11` is not a fourth size: it is Format IX, memory-form `ASx.w <ea>`, a
//! different instruction that shares these top four bits. That one is M1-19
//! (#27), and [`crate::decode`] leaves it
//! [`Op::Unimplemented`](crate::decode::Op::Unimplemented), so this handler only
//! ever sees the 3072 register-form encodings and never has to ask.
//!
//! # The shape of the handler
//!
//! [`Shift::read`] turns the opcode into a [`Shift`], [`shift_operand`] performs
//! it as a pure function of that, the operand and `X`, and [`M68k::op_shift`]
//! writes the result and the five condition codes back. One bit-at-a-time loop
//! covers all
//! eight mnemonics and all three sizes because that is what the hardware does —
//! M68000PM section 6 prices the shift group at `2` clocks *per place* — and
//! because three of the flag rules below only come out of the loop's
//! intermediate states.
//!
//! # Flags, and the three rules that are not obvious
//!
//! From M68000UM's "Operation"/"Flags" lines for the three pairs, and confirmed
//! against all 193,560 register-form cases of the 24 suite files:
//!
//! 1. **A count of zero still moves `C`** ([`Shift::zero_count_carry`]). `ROXL`
//!    and `ROXR` copy `X` into `C`; the other six clear it. `X` itself is
//!    untouched, `N`/`Z` describe the operand as it was, `V` clears.
//! 2. **An immediate `ccc` of `000` is 8, not 0** ([`IMMEDIATE_EIGHT`]), so only
//!    the register form can ask for a shift of zero places at all.
//! 3. **A shift saturates; a rotate wraps** ([`Shift::steps`]). `LSL`/`ASL` of
//!    `width` or more is zero; `ASR` of `width` or more is the sign. `C` and `X`
//!    are *the last bit pushed out of the field*, and past the width what gets
//!    pushed out is the zero that follows the operand, not the operand's last
//!    significant bit. So `ASR` by 8 of `$80` is `$FF` with `C` = `X` = `1` — the
//!    sign, one place too far — while `ASR` by 9 is `$FF` too (it never stopped
//!    being sign-filled) but with `C` = `X` = `0`. Neither the manual's prose nor
//!    a shift-and-mask shortcut gives that pair; the `ASR` suite does, 3,736
//!    cases across its three sizes, and that is what this loop is built around.
//!
//! `V` is `ASL`'s alone: set if the sign bit changed *at any place* of the shift,
//! which is why the test is per place rather than between the two end values
//! (`ASL.B #2` of `$40` passes through negative on the way to zero). `ASR`,
//! `LSL`, `LSR` and all four rotates clear `V`.
//!
//! # Cycles
//!
//! `0`, like every other handler: the queue rotation in [`M68k::advance_opcode`]
//! charges what the core models today, and the `6 + 2n` / `8 + 2n` of M68000PM
//! section 6 are M1-31 (#39/#40)'s — where `n` is exactly what
//! [`Shift::steps`] already works out.

use crate::bus::Bus;
use crate::cpu::M68k;
use crate::ea::Size;
use crate::ops::field;
use crate::sr::{SR_C, SR_V, SR_X};

/// Bit 8 `d`: direction, set for the four left-hand mnemonics.
const LEFT_BIT: u16 = 0x0100;
/// Bit 5 `i`: the count comes from data register `ccc`, not from `ccc` itself.
const COUNT_IS_REGISTER: u16 = 0x0020;
/// Shifts of `ccc` (3 bits), `ss` and `tt` (2 bits each).
const COUNT_SHIFT: u32 = 9;
const SIZE_SHIFT: u32 = 6;
const TYPE_SHIFT: u32 = 3;
/// Width of the `ss` and `tt` fields.
const FIELD_2_BITS: u16 = 0b11;

/// An immediate `ccc` of `000` means 8 (M68000UM "ASL, ASR, LSL, LSR": "if the
/// shift count is zero ... a shift count of 8"). Only `i` = 1 can shift zero.
const IMMEDIATE_EIGHT: u32 = 8;

/// A register-sourced count is used modulo 64 (M68000UM "ASL, ASR, LSL, LSR":
/// "the shift count ... is masked with #$3F"). 64 is above every width, so a
/// count of 63 saturates a shift and takes a rotate seven places past a full
/// turn.
const REGISTER_COUNT_MASK: u32 = 63;

/// Which pair of mnemonics `tt` selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `ASL`/`ASR`: the sign bit goes out one end and comes back in the other,
    /// and `ASL` alone can overflow.
    Arithmetic,
    /// `LSL`/`LSR`: zero in at one end, carry out of the other.
    Logical,
    /// `ROXL`/`ROXR`: the width + 1 bit rotate through `X`.
    RotateExtend,
    /// `ROL`/`ROR`: the width bit rotate, with `X` outside the ring.
    Rotate,
}

/// One register-form shift or rotate, read out of the opcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shift {
    kind: Kind,
    left: bool,
    size: Size,
    /// The count as the opcode gives it, before [`Shift::steps`] applies the
    /// saturating and wrapping rules.
    count: u32,
}

impl Shift {
    /// Read `1110 ccc d ss i tt rrr`. `register_count` is `D(ccc)` when `i` = 1
    /// and is ignored otherwise. `None` is `ss` = `11` alone — memory-form
    /// `ASx.w <ea>`, M1-19 (#27), which [`crate::decode`] never routes here, so
    /// the `None` keeps this a total function rather than being a route.
    fn read(opcode: u16, register_count: u32) -> Option<Self> {
        let size = Size::from_bits((opcode >> SIZE_SHIFT) & FIELD_2_BITS)?;
        let immediate = u32::from(field(opcode, COUNT_SHIFT));
        Some(Self {
            kind: match (opcode >> TYPE_SHIFT) & FIELD_2_BITS {
                0b00 => Kind::Arithmetic,
                0b01 => Kind::Logical,
                0b10 => Kind::RotateExtend,
                _ => Kind::Rotate,
            },
            left: opcode & LEFT_BIT != 0,
            size,
            count: if opcode & COUNT_IS_REGISTER != 0 {
                register_count & REGISTER_COUNT_MASK
            } else if immediate == 0 {
                IMMEDIATE_EIGHT
            } else {
                immediate
            },
        })
    }

    /// Width of the operation in bits: 8, 16 or 32.
    const fn width(self) -> u32 {
        match self.size {
            Size::Byte => 8,
            Size::Word => 16,
            Size::Long => 32,
        }
    }

    /// How many places the operand moves.
    ///
    /// A shift stops at the width because the remaining places move nothing but
    /// zero (rule 3); a rotate wraps at its own period, which is the width for
    /// `ROx` and the width + 1 for `ROXx`. A whole number of turns is *not*
    /// skipped: rotating a byte by 8 leaves it alone but carries bit 0 out last,
    /// and rotating a byte through `X` by 9 leaves it alone with `C` = `X`. Those
    /// two facts are what tell `ROx` from `ROXx` at that count, and only running
    /// the full ring shows them.
    fn steps(self) -> u32 {
        match self.kind {
            Kind::Arithmetic | Kind::Logical => self.count.min(self.width()),
            Kind::Rotate => self.places_within(self.width()),
            Kind::RotateExtend => self.places_within(self.width() + 1),
        }
    }

    /// `count` modulo `period`, keeping a whole number of periods as `period`
    /// rather than folding it to zero (see [`Shift::steps`]).
    fn places_within(self, period: u32) -> u32 {
        if self.count == 0 {
            return 0;
        }
        match self.count % period {
            0 => period,
            rest => rest,
        }
    }

    /// `C` for a count of zero: `X` for the two rotate-through-extend forms,
    /// clear for the other six (M68000UM "ROXL, ROXR": "if the shift count is
    /// zero ... the extend bit is copied into the carry bit"; the other six say
    /// the carry is cleared). `X` itself is never written at a count of zero, so
    /// this answers only about `C`.
    fn zero_count_carry(self) -> bool {
        self.kind == Kind::RotateExtend
    }

    /// Whether `X` is written at all: a `ROx` never writes it, and neither does
    /// any mnemonic at a count of zero.
    fn writes_extend(self) -> bool {
        self.count != 0 && self.kind != Kind::Rotate
    }
}

/// What one shift or rotate produced. `extend` is the new `X`, or `None` when
/// the operation leaves `X` alone (a `ROx`, or anything at a count of zero);
/// `carry` is the new `C` in both cases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shifted {
    result: u32,
    carry: bool,
    extend: Option<bool>,
    overflow: bool,
}

/// Move `operand` by `shift`, starting with `extend` as `X`.
///
/// The loop is the definition, not an optimisation of it: `ASL`'s `V` needs
/// every intermediate sign bit, and `C`/`X` need the last bit out of a loop that
/// keeps running after the operand has left the field. Keeping `value` masked to
/// `size` is what makes "the bit that comes in from outside the field is zero"
/// free, and running [`Shift::steps`] rather than `count` is what stops a shift
/// of 63 from costing 63 iterations.
fn shift_operand(shift: Shift, operand: u32, extend: bool) -> Shifted {
    let mask = shift.size.mask();
    let msb = shift.size.msb();
    let mut value = operand & mask;
    // The bit that just left the field: `X` before the first place of a
    // rotate-through-extend, and the previous place's `out` from then on.
    let mut carry = extend;
    let mut overflow = false;

    for _ in 0..shift.steps() {
        let sign = value & msb != 0;
        let out = if shift.left { sign } else { value & 1 != 0 };
        let mut next = if shift.left { value << 1 } else { value >> 1 };
        // The bit that comes back in at the far end: nothing for the shifts,
        // `out` for a rotate, and the outgoing `X` for a rotate through it.
        let fill = match shift.kind {
            Kind::Arithmetic => !shift.left && sign,
            Kind::Logical => false,
            Kind::Rotate => out,
            Kind::RotateExtend => carry,
        };
        if fill {
            next |= if shift.left { 1 } else { msb };
        }
        if shift.left && shift.kind == Kind::Arithmetic && (next & msb != 0) != sign {
            overflow = true;
        }
        value = next & mask;
        carry = out;
    }

    if shift.count == 0 {
        carry = shift.zero_count_carry() && extend;
    } else if shift.count > shift.width() && matches!(shift.kind, Kind::Arithmetic | Kind::Logical)
    {
        // Every place past the width moves a zero. `LSL`/`ASL` get here with a
        // zero in `carry` already; `ASR` does not — its last in-width place
        // pushed the sign out — and this is the rule that makes `ASR` by 9 of
        // `$80` carry zero anyway.
        carry = false;
    }

    Shifted {
        result: value,
        carry,
        extend: shift.writes_extend().then_some(carry),
        overflow,
    }
}

impl M68k {
    /// `ASL`/`ASR`/`LSL`/`LSR`/`ROL`/`ROR`/`ROXL`/`ROXR` on a data register
    /// (M68000UM "ASL, ASR, LSL, LSR", "ROL, ROR", "ROXL, ROXR"; M1-18, #26).
    ///
    /// The register is read and written at the instruction's own size, so a byte
    /// shift leaves the upper three bytes alone and `Z` comes from the byte, not
    /// from the register. `N`/`Z` are the result's; the other three come from
    /// [`shift_operand`], which is the only place they are computed.
    pub(crate) fn op_shift(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let count_field = field(opcode, COUNT_SHIFT);
        let count = if opcode & COUNT_IS_REGISTER != 0 {
            self.d[usize::from(count_field)]
        } else {
            0
        };
        let Some(shift) = Shift::read(opcode, count) else {
            // `ss` = `11`: memory-form `ASx.w <ea>`, which `decode` holds back
            // for M1-19 (#27). Unreachable by construction, and loud rather than
            // silently wrong if that ever stops being true.
            return self.op_illegal(bus);
        };
        let reg = usize::from(field(opcode, 0));

        let shifted = shift_operand(shift, self.d[reg], self.sr & SR_X != 0);

        self.set_flag(SR_C, shifted.carry);
        if let Some(extend) = shifted.extend {
            self.set_flag(SR_X, extend);
        }
        self.set_flag(SR_V, shifted.overflow);
        self.set_nz(shifted.result, shift.size);
        self.d[reg] = (self.d[reg] & !shift.size.mask()) | shifted.result;

        self.advance_opcode(bus);
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sr::{SR_N, SR_Z};
    use crate::testbus::TestBus;

    /// The five condition code bits.
    const CC: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// `1110 ccc d ss i tt 010` in pieces, for the two helpers below. `D2` is the
    /// destination so that `D1` is free to hold a count.
    fn encoding(count_field: u32, left: bool, size: u8, kind: u8, from_register: bool) -> u16 {
        let i = u16::from(from_register) * COUNT_IS_REGISTER;
        (u16::try_from(count_field).unwrap_or(0xff) << COUNT_SHIFT)
            | (u16::from(left) << 8)
            | (u16::from(size) << SIZE_SHIFT)
            | i
            | (u16::from(kind) << TYPE_SHIFT)
            | 0b1110_0000_0000_0010
    }

    /// `1110 ccc d ss 0 tt 010`: shift `D2` by `count` places, `count` in `1..=8`.
    /// Eight is the interesting one: the field cannot hold it, so `000` means it.
    fn by(count: u32, left: bool, size: u8, kind: u8) -> u16 {
        encoding(if count == 8 { 0 } else { count }, left, size, kind, false)
    }

    /// `1110 001 d ss 1 tt 010`: shift `D2` by however many places `D1` holds,
    /// which is how a test asks for a count wider than a byte.
    fn by_d1(left: bool, size: u8, kind: u8) -> u16 {
        encoding(1, left, size, kind, true)
    }

    /// Run one shift: `D1` starts at `count` (the register-count source), `D2` at
    /// `value`, `SR` at `sr`. Returns the half of `D2` the instruction writes and
    /// the five condition codes, so a test can check the result and the flags it
    /// is really about rather than a whole `SR` in hex.
    fn run(opcode: u16, count: u32, value: u32, sr: u16) -> (u32, u16) {
        let mut cpu = M68k::new();
        cpu.d[1] = count;
        cpu.d[2] = value;
        cpu.sr = sr;
        cpu.op_shift(&mut TestBus::new(), opcode);
        (cpu.d[2] & wide_mask(opcode), cpu.sr & CC)
    }

    /// The half of `D2` an instruction of this `ss` is allowed to write.
    fn wide_mask(opcode: u16) -> u32 {
        Size::from_bits((opcode >> SIZE_SHIFT) & FIELD_2_BITS).map_or(0xFFFF_FFFF, Size::mask)
    }

    /// `ASL`'s `V` is "the sign changed at some place", not "the signs of the two
    /// ends differ": `$40` by 1 turns negative, and by 2 it is back to zero with
    /// `V` still set from the place that went through the sign.
    #[test]
    fn asl_sets_v_for_any_place_that_crossed_the_sign() {
        let (value, flags) = run(by(1, true, 0b00, 0b00), 0, 0x40, 0);
        assert_eq!(value, 0x80);
        assert_eq!(flags, SR_N | SR_V, "N and V, no carry");

        let (value, flags) = run(by(1, true, 0b00, 0b00), 0, 0x21, 0);
        assert_eq!(value, 0x42);
        assert_eq!(flags, 0, "$21 by 1 loses nothing");

        let (value, flags) = run(by(2, true, 0b00, 0b00), 0, 0x40, 0);
        assert_eq!(value, 0x00);
        assert_eq!(
            flags,
            SR_Z | SR_V | SR_C | SR_X,
            "$40 by 2 is zero, but V is still set from the place that went negative"
        );
    }

    /// `ASR` takes its own sign back in, and carries the bit it pushed out.
    #[test]
    fn asr_fills_with_the_sign_and_carries_the_bit_it_pushed_out() {
        let (value, flags) = run(by(1, false, 0b00, 0b00), 0, 0x81, 0);
        assert_eq!(value, 0xC0);
        assert_eq!(flags, SR_N | SR_C | SR_X, "N, C and X from $81 by 1");

        let (value, flags) = run(by(1, false, 0b00, 0b00), 0, 0x01, 0);
        assert_eq!(value, 0x00);
        assert_eq!(
            flags,
            SR_Z | SR_C | SR_X,
            "a positive operand fills with zeros"
        );
    }

    /// The width itself is the boundary. By the width, the last bit out is the
    /// operand's own; one place further it is the zero that follows the operand,
    /// and the result is still sign-filled, because shifting never undoes what
    /// the earlier places did.
    #[test]
    fn a_shift_past_its_width_carries_zero() {
        let (value, flags) = run(by(8, false, 0b00, 0b00), 0, 0x80, 0);
        assert_eq!(value, 0xFF, "ASR.B #8 of $80 is all sign");
        assert_eq!(flags, SR_N | SR_C | SR_X, "...and it carries the sign out");

        let (value, flags) = run(by(8, false, 0b00, 0b00), 0, 0x40, 0);
        assert_eq!(value, 0x00, "ASR.B #8 of $40 is zero");
        assert_eq!(flags, SR_Z, "and a positive operand carries zero");

        let (value, flags) = run(by_d1(false, 0b00, 0b00), 9, 0x80, 0);
        assert_eq!(value, 0xFF, "ASR.B #9 of $80 is still all sign");
        assert_eq!(
            flags, SR_N,
            "but the ninth place pushed out a zero, and that is the carry it ends on"
        );
    }

    /// `LSR` is the logical half of the same width boundary, and the right-hand
    /// half: `LSx` of more than the width is zero with `C` and `X` clear, while
    /// `LSR.B` by exactly the width still carries the operand's own top bit out.
    #[test]
    fn a_logical_right_shift_past_its_width_is_zero_with_no_carry() {
        let (value, flags) = run(by(8, false, 0b00, 0b01), 0, 0x80, 0);
        assert_eq!(value, 0x00, "LSR.B #8 of $80 is zero");
        assert_eq!(
            flags,
            SR_Z | SR_C | SR_X,
            "and its own top bit is the last thing out"
        );

        let (value, flags) = run(by_d1(false, 0b00, 0b01), 9, 0x80, SR_X);
        assert_eq!(value, 0x00, "LSR.B #9 of $80 is zero too");
        assert_eq!(
            flags, SR_Z,
            "but the ninth place moved a zero, and that clears C and X"
        );

        let (value, flags) = run(by_d1(false, 0b10, 0b01), 40, 0x8000_0001, SR_X);
        assert_eq!(value, 0, "LSR.L #40 of $80000001 is zero");
        assert_eq!(flags, SR_Z, "with both C and X clear");
    }

    /// A left shift past the width is zero with no carry; `V` keeps whatever the
    /// places that *did* move signed did.
    #[test]
    fn a_left_shift_past_its_width_is_zero() {
        let (value, flags) = run(by(8, true, 0b00, 0b00), 0, 0xFF, 0);
        assert_eq!(value, 0x00, "ASL.B #8 of $FF");
        assert_eq!(
            flags,
            SR_Z | SR_V | SR_C | SR_X,
            "$FF lost its sign at once"
        );

        let (value, flags) = run(by_d1(true, 0b00, 0b00), 9, 0xFF, 0);
        assert_eq!(value, 0x00, "ASL.B #9 of $FF");
        assert_eq!(
            flags,
            SR_Z | SR_V,
            "and the ninth place cleared C and X again"
        );

        let (value, flags) = run(by_d1(true, 0b10, 0b01), 40, 0xFFFF_FFFF, 0);
        assert_eq!(value, 0, "LSL.L #40 of -$1 is zero");
        assert_eq!(flags, SR_Z);

        let (value, flags) = run(by_d1(true, 0b10, 0b01), 32, 0x8000_0001, 0);
        assert_eq!(value, 0, "LSL.L #32 of $80000001 is zero");
        assert_eq!(flags, SR_Z | SR_C | SR_X, "and bit 0 is the last thing out");
    }

    /// `ROL`/`ROR` are the width-bit rotate: the bit that leaves one end enters
    /// the other, and `X` is not part of the ring at all.
    #[test]
    fn rolx_rotates_without_x() {
        let (value, flags) = run(by(1, true, 0b01, 0b11), 0, 0x7FFF, SR_X);
        assert_eq!(value, 0xFFFE, "ROL.W #1 of $7FFF");
        assert_eq!(
            flags,
            SR_N | SR_X,
            "a zero rotated out clears C and leaves X alone"
        );

        let (value, flags) = run(by_d1(true, 0b01, 0b11), 16, 0x8001, SR_X);
        assert_eq!(value, 0x8001, "a whole turn of the register is a no-op");
        assert_eq!(
            flags,
            SR_N | SR_C | SR_X,
            "but not for C: bit 0 left last. X, which ROL never touches, stayed set"
        );
    }

    /// `ROXL`/`ROXR` are the width + 1 bit rotate: `X` goes in at one end, and
    /// the bit that leaves the other end becomes the new `X`.
    #[test]
    fn roxlx_rotates_through_x() {
        let (value, flags) = run(by(1, true, 0b01, 0b10), 0, 0x8001, SR_X);
        assert_eq!(value, 0x0003, "ROXL.W #1 of $8001 with X set");
        assert_eq!(flags, SR_C | SR_X, "X and C both take the sign that left");

        let (value, flags) = run(by(1, false, 0b00, 0b10), 0, 0x00, SR_X);
        assert_eq!(
            value, 0x80,
            "ROXR.B #1 of $00 with X set pulls X into the sign"
        );
        assert_eq!(flags, SR_N, "and X and C both take the zero that left");

        let (value, flags) = run(by_d1(true, 0b01, 0b10), 17, 0x8001, SR_X);
        assert_eq!(
            value, 0x8001,
            "17 places of a 17-bit ring is nothing at all"
        );
        assert_eq!(
            flags,
            SR_N | SR_C | SR_X,
            "with the ring ending where it started: C and X both $1"
        );
    }

    /// A register count of zero moves nothing, but it still writes `C`: from `X`
    /// for `ROXL`/`ROXR`, to zero for the other six. `X` itself is never touched,
    /// and `N`/`Z` describe the operand.
    #[test]
    fn a_zero_count_still_writes_carry() {
        for kind in [0b00, 0b01, 0b11] {
            let (value, flags) = run(by_d1(true, 0b00, kind), 0, 0x81, SR_X);
            assert_eq!(value, 0x81, "kind {kind:#04b} must not move the operand");
            assert_eq!(
                flags,
                SR_N | SR_X,
                "kind {kind:#04b}: X untouched, C clear, N from the operand"
            );
        }
        let (value, flags) = run(by_d1(true, 0b00, 0b10), 0, 0x81, SR_X);
        assert_eq!(value, 0x81);
        assert_eq!(flags, SR_N | SR_C | SR_X, "ROXx copies X into C");

        let (value, flags) = run(by_d1(true, 0b00, 0b10), 0, 0x00, 0);
        assert_eq!(value, 0x00);
        assert_eq!(
            flags, SR_Z,
            "...which is zero when X was clear, and Z is set"
        );
    }

    /// `ccc` = `000` in the immediate form is 8, and a register count is used
    /// modulo 64; both are rules about the *field*, so they are checked on the
    /// decoded [`Shift`] rather than through an operation.
    #[test]
    fn counts_are_read_the_way_the_manual_reads_them() {
        let count = |opcode, register| Shift::read(opcode, register).map(|shift| shift.count);
        assert_eq!(count(by(8, true, 0b00, 0b00), 0), Some(8));
        assert_eq!(count(by(3, false, 0b01, 0b01), 0), Some(3));
        let d1 = by_d1(false, 0b01, 0b01);
        assert_eq!(count(d1, 0x1FF), Some(63));
        assert_eq!(count(d1, 64), Some(0));
        assert_eq!(count(d1, 0), Some(0));
    }

    /// `ss` = `11` is memory-form `ASx.w <ea>` (M1-19, #27), not a fourth size:
    /// it is the one `ss` with no register shift in it.
    #[test]
    fn the_memory_form_encoding_is_not_a_register_shift() {
        assert_eq!(
            Shift::read(encoding(1, true, 0b11, 0b00, false), 0),
            None,
            "ASx.W <ea> shares these bits but is not a register shift"
        );
        assert!(Shift::read(encoding(1, true, 0b10, 0b00, false), 0).is_some());
    }

    /// The place count is where the saturating and wrapping rules live, so it is
    /// worth pinning directly: a shift never exceeds its width, `ROx` wraps at
    /// it, and `ROXx` wraps one place later.
    #[test]
    fn steps_saturate_shifts_and_wrap_rotates() {
        let steps = |count, kind, size| {
            Shift {
                kind,
                left: true,
                size,
                count,
            }
            .steps()
        };
        assert_eq!(steps(63, Kind::Arithmetic, Size::Byte), 8);
        assert_eq!(steps(63, Kind::Logical, Size::Long), 32);
        assert_eq!(steps(9, Kind::Rotate, Size::Byte), 1);
        assert_eq!(steps(8, Kind::Rotate, Size::Byte), 8);
        assert_eq!(steps(9, Kind::RotateExtend, Size::Byte), 9);
        assert_eq!(steps(18, Kind::RotateExtend, Size::Byte), 9);
        assert_eq!(steps(17, Kind::RotateExtend, Size::Word), 17);
        assert_eq!(steps(0, Kind::RotateExtend, Size::Byte), 0);
    }

    /// A byte or word shift writes one size's worth of the destination: the
    /// register's other bytes survive untouched, in both directions.
    #[test]
    fn a_narrow_shift_leaves_the_rest_of_the_register_alone() {
        let mut cpu = M68k::new();
        cpu.d[2] = 0xC0FF_EE11;
        cpu.op_shift(&mut TestBus::new(), by(1, true, 0b00, 0b00));
        assert_eq!(cpu.d[2], 0xC0FF_EE22, "ASL.B #1 of $11 is $22");

        let mut cpu = M68k::new();
        cpu.d[5] = 0x1234_5678;
        let lsr_w_d5 = by(4, false, 0b01, 0b01) & !0b111 | 0b101;
        cpu.op_shift(&mut TestBus::new(), lsr_w_d5);
        assert_eq!(cpu.d[5], 0x1234_0567, "LSR.W #4 of $5678 is $567");
    }
}
