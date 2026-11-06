//! Z80 register file support: flag bit positions, the flag helpers every
//! instruction ticket reuses, and the interrupt mode enum.
//!
//! The flag layout is the documented one (Z80 CPU Family UG, "Programmer's
//! Guide", flag register F):
//!
//! ```text
//! bit   7  6  5  4  3  2  1  0
//! flag  S  Z  Y  H  X  P  N  C
//! ```
//!
//! Y (bit 5) and X (bit 3) are undocumented on the Z80 but real on the chip,
//! and the Z80's undocumented behaviour is part of what the conformance suite
//! checks, so they are first-class bits here, not padding.

/// Sign: set when the result is negative (bit 7 of the result).
pub const FLAG_S: u8 = 0x80;
/// Zero: set when the result is zero.
pub const FLAG_Z: u8 = 0x40;
/// Undocumented copy of bit 5 of the result (also called `5`).
pub const FLAG_Y: u8 = 0x20;
/// Half-carry: carry out of bit 3, used by DAA.
pub const FLAG_H: u8 = 0x10;
/// Undocumented copy of bit 3 of the result (also called `3`).
pub const FLAG_X: u8 = 0x08;
/// Parity/Overflow: parity of the result for logic ops, overflow for ADD/SUB.
pub const FLAG_PV: u8 = 0x04;
/// Add/Subtract: set by SUB and CP, clear by ADD and ADC.
pub const FLAG_N: u8 = 0x02;
/// Carry.
pub const FLAG_C: u8 = 0x01;

/// The S and Z bits, which most ALU ops derive from the result alone.
pub const FLAG_SZ: u8 = FLAG_S | FLAG_Z;
/// The four flag bits an ALU op gets straight out of the result byte:
/// S, Z (inverted) and the undocumented X and Y copies.
pub const FLAG_SZXY: u8 = FLAG_S | FLAG_Z | FLAG_Y | FLAG_X;

/// Flags a byte result sets on its own: `S`, `Z`, and the undocumented `Y`/`X`
/// copies of result bits 5 and 3.
///
/// Result bits 5 and 3 are copied verbatim for most ALU and load ops, so this
/// is the common case. Where the Z80 instead copies them from an *operand*
/// (`INC`/`DEC` keep the old X/Y, `BIT n,(HL)` takes them from the register
/// number) the caller masks these bits out.
pub const fn szxy_flags(value: u8) -> u8 {
    let mut f = value & (FLAG_Y | FLAG_X);
    if value & FLAG_S != 0 {
        f |= FLAG_S;
    }
    if value == 0 {
        f |= FLAG_Z;
    }
    f
}

/// Even parity of a byte: `true` when the number of set bits is even, which is
/// what sets `P/V` for logic instructions.
pub const fn parity(value: u8) -> bool {
    value.count_ones().is_multiple_of(2)
}

/// Lookup of `szxy_flags` plus `FLAG_PV`, indexed by result byte.
///
/// Every ALU op needs S/Z/X/Y and the parity of the same byte, and `RLC`-style
/// rotates need the parity of their result immediately, so the pair is
/// precomputed once at compile time. The caller still owns `H`, `N` and `C`,
/// which depend on the operands and not just the result: mask with
/// `!FLAG_PV` if only parity was wanted.
pub const SZXY_PARITY: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < table.len() {
        let value = i as u8;
        let mut flags = szxy_flags(value);
        if parity(value) {
            flags |= FLAG_PV;
        }
        table[i] = flags;
        i += 1;
    }
    table
};

/// Interrupt mode selected by `IM 0`, `IM 1`, `IM 2`.
///
/// An enum rather than a `u8` because the three modes do genuinely different
/// things on interrupt acceptance, and a mode that cannot be named cannot be
/// silently misspelled by a later instruction ticket.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IntMode {
    /// Vectored interrupt: the device puts a restart instruction on the bus.
    Mode0,
    /// The CPU executes `RST $38` internally.
    Mode1,
    /// Vector is read from `$I:$data`, low byte first.
    Mode2,
}

impl IntMode {
    /// The `IM n` encoding, where anything but 1 and 2 is mode 0.
    pub const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Mode1,
            2 => Self::Mode2,
            _ => Self::Mode0,
        }
    }

    /// The `IM n` encoding as a number, for the test harness and save states.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Mode0 => 0,
            Self::Mode1 => 1,
            Self::Mode2 => 2,
        }
    }
}

/// Bit 7 of `R` survives the per-opcode refresh and is only changed by
/// `LD R,A`, so the increment masks it back in.
///
/// Real hardware counts the low 7 bits and leaves bit 7 alone: `0xFF` wraps to
/// `0x80`, not to `0x00` or back to `0xFF`.
pub const fn incremented_r(r: u8) -> u8 {
    (r & 0x80) | (r.wrapping_add(1) & 0x7F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_bit_positions() {
        // The bit layout the whole crate assumes; a typo here silently breaks
        // every flag assertion in the conformance suite.
        assert_eq!(FLAG_S, 0x80);
        assert_eq!(FLAG_Z, 0x40);
        assert_eq!(FLAG_Y, 0x20);
        assert_eq!(FLAG_H, 0x10);
        assert_eq!(FLAG_X, 0x08);
        assert_eq!(FLAG_PV, 0x04);
        assert_eq!(FLAG_N, 0x02);
        assert_eq!(FLAG_C, 0x01);
        assert_eq!(FLAG_SZ, 0xC0);
    }

    #[test]
    fn szxy_flags_from_result() {
        assert_eq!(szxy_flags(0x00) & FLAG_Z, FLAG_Z);
        assert_eq!(szxy_flags(0x01) & FLAG_Z, 0);
        assert_eq!(szxy_flags(0x80) & FLAG_S, FLAG_S);
        assert_eq!(szxy_flags(0x7F) & FLAG_S, 0);
        // X and Y are plain copies of result bits 3 and 5.
        assert_eq!(szxy_flags(0x28) & (FLAG_X | FLAG_Y), FLAG_X | FLAG_Y);
        assert_eq!(szxy_flags(0x08) & (FLAG_X | FLAG_Y), FLAG_X);
        assert_eq!(szxy_flags(0x20) & (FLAG_X | FLAG_Y), FLAG_Y);
        assert_eq!(szxy_flags(0x11) & (FLAG_X | FLAG_Y), 0);
        // It only speaks for the bits it owns: 0xFF is negative, has both
        // undocumented bits set, and is not zero, so no Z.
        assert_eq!(szxy_flags(0xFF) & FLAG_SZXY, FLAG_S | FLAG_Y | FLAG_X);
        assert_eq!(szxy_flags(0xFF) & !FLAG_SZXY, 0);
    }

    #[test]
    fn parity_table() {
        assert!(parity(0x00));
        assert!(!parity(0x01));
        assert!(parity(0xFF));
        assert!(!parity(0x80));
        assert!(parity(0x03));
    }

    #[test]
    fn szxy_parity_table_matches_helpers() {
        // The table is a cache of the two helpers; if they ever disagree the
        // conformance suite fails somewhere nobody wants to bisect.
        for value in 0u16..=255 {
            let value = value as u8;
            let expected = szxy_flags(value) | (u8::from(parity(value)) * FLAG_PV);
            assert_eq!(
                SZXY_PARITY[usize::from(value)],
                expected,
                "value {value:#04X}"
            );
            assert_eq!(
                SZXY_PARITY[usize::from(value)] & FLAG_PV != 0,
                parity(value)
            );
        }
        // Spot checks against hand-computed values.
        assert_eq!(SZXY_PARITY[0x00], FLAG_Z | FLAG_PV);
        assert_eq!(SZXY_PARITY[0x01], 0, "one bit set is odd parity");
        assert_eq!(SZXY_PARITY[0x03], FLAG_PV, "two bits set is even parity");
        assert_eq!(SZXY_PARITY[0x80], FLAG_S, "one bit set is odd parity");
        assert_eq!(SZXY_PARITY[0xFF], FLAG_S | FLAG_Y | FLAG_X | FLAG_PV);
    }

    #[test]
    fn inc_r_keeps_bit7() {
        assert_eq!(incremented_r(0xFF), 0x80);
        assert_eq!(incremented_r(0x7F), 0x00);
        assert_eq!(incremented_r(0x00), 0x01);
        assert_eq!(incremented_r(0x80), 0x81);
        // Counting up 128 times comes back to the same bit 7 and a zero count.
        let mut r = 0x80;
        for _ in 0..128 {
            r = incremented_r(r);
        }
        assert_eq!(r, 0x80);
    }

    #[test]
    fn int_mode_roundtrip() {
        assert_eq!(IntMode::from_u8(0), IntMode::Mode0);
        assert_eq!(IntMode::from_u8(1), IntMode::Mode1);
        assert_eq!(IntMode::from_u8(2), IntMode::Mode2);
        // IM only has three encodings; anything else behaves as mode 0.
        assert_eq!(IntMode::from_u8(3), IntMode::Mode0);
        assert_eq!(IntMode::Mode0.as_u8(), 0);
        assert_eq!(IntMode::Mode1.as_u8(), 1);
        assert_eq!(IntMode::Mode2.as_u8(), 2);
    }
}
