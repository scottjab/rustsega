//! VDP register numbers, and the pure decodes of the fields that are more than
//! one bit.
//!
//! Register numbers are the values the control port puts on its address lines:
//! R0 is the register a command word with `$80` in the high byte selects, R23
//! the one `$97` selects (genvdp.txt section 7 "VDP ports", section 17 "VDP
//! registers"; plutiedev, "VDP register reference").
//!
//! Nothing in here holds state. Each helper takes the raw register byte and
//! returns the decoded value, which is what lets the field edge cases be tested
//! without a whole VDP.

/// Number of VDP registers: R0 to R23, i.e. 0x00 to 0x17 (genvdp.txt section 17).
pub(crate) const NUM_REGISTERS: usize = 24;

pub(crate) const R0: usize = 0x00;
pub(crate) const R1: usize = 0x01;
pub(crate) const R2: usize = 0x02;
pub(crate) const R3: usize = 0x03;
pub(crate) const R4: usize = 0x04;
pub(crate) const R5: usize = 0x05;
pub(crate) const R7: usize = 0x07;
pub(crate) const R10: usize = 0x0A;
pub(crate) const R11: usize = 0x0B;
pub(crate) const R12: usize = 0x0C;
pub(crate) const R13: usize = 0x0D;
pub(crate) const R15: usize = 0x0F;
pub(crate) const R16: usize = 0x10;
pub(crate) const R17: usize = 0x11;
pub(crate) const R18: usize = 0x12;
pub(crate) const R19: usize = 0x13;
pub(crate) const R20: usize = 0x14;
pub(crate) const R21: usize = 0x15;
pub(crate) const R22: usize = 0x16;
pub(crate) const R23: usize = 0x17;

/// R12: the two resolution bits, RS0 (bit 7) and RS1 (bit 0). genvdp.txt
/// section 17 "$0C" lists `01` and `11` as a 40-cell display and `00` as a
/// 32-cell one; `10` is invalid, and is read here as the 40 cells its RS0 says.
pub(crate) const R12_RES_MASK: u8 = 0x81;

/// How the VDP walks the horizontal scroll table (R11 bits 1-0, plutiedev
/// "$8Bxx": "HSCR/LSCR: horizontal scroll mode").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HScrollMode {
    /// `00`: one scroll value for the whole plane.
    Full,
    /// `01`: scroll the first eight lines, then repeat that for the rest.
    EightLines,
    /// `10`: a new scroll value for every cell column.
    PerCell,
    /// `11`: a new scroll value for every display line.
    PerLine,
}

/// Interlace mode, R12 bits 2-1 "LSM" (genvdp.txt section 17 "$0C"; plutiedev
/// "$8Cxx").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterlaceMode {
    /// No interlacing. `10` is documented as "no interlace" as well, so it maps
    /// here too.
    Off,
    /// Interlaced, same vertical resolution as progressive.
    Mode1,
    /// Interlaced with double vertical resolution (480 lines out).
    Mode2,
}

/// The DMA operation R23 bits 7-6 asks for (genvdp.txt section 11; plutiedev
/// "$97xx", "DMD1-0").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaMode {
    /// `0x`: 68000 to VDP transfer. R23 bit 6 doubles as the top source bit.
    Mc68kTransfer,
    /// `10`: VDP VRAM/CRAM/VSRAM fill.
    Fill,
    /// `11`: VRAM copy.
    Copy,
}

/// Base address a table-address register points at.
///
/// Every table register (R2 to R5, R13) holds a base address in its top bits
/// with the low bits unused, and every table has its own granularity, so the
/// mask and shift come from the caller. Calculated wide and truncated to the
/// 16-bit VRAM address space the value lives in (genvdp.txt section 17).
#[inline]
pub(crate) const fn table_base(value: u8, mask: u8, shift: u32) -> u16 {
    (((value & mask) as u32) << shift) as u16
}

/// Width or height in cells of one of R16's two size fields (genvdp.txt
/// section 17 "$10", plutiedev "$90xx").
///
/// `0b10` is not a valid setting: genvdp.txt records that the first row of the
/// name table is then shown for every line of the display. Nothing sane can be
/// derived from it, so it decodes to 32 cells here; if a later ticket models
/// the real behaviour it changes this one function.
#[inline]
pub(crate) const fn plane_extent(field: u8) -> u8 {
    match field & 0x03 {
        0b01 => 64,
        0b11 => 128,
        _ => 32,
    }
}

/// R11 bits 1-0 as a [`HScrollMode`] (plutiedev "$8Bxx").
#[inline]
pub(crate) const fn decode_hscroll_mode(reg: u8) -> HScrollMode {
    match reg & 0x03 {
        0b01 => HScrollMode::EightLines,
        0b10 => HScrollMode::PerCell,
        0b11 => HScrollMode::PerLine,
        _ => HScrollMode::Full,
    }
}

/// R12 bits 2-1 as an [`InterlaceMode`] (genvdp.txt section 17 "$0C").
#[inline]
pub(crate) const fn decode_interlace_mode(reg: u8) -> InterlaceMode {
    match (reg >> 1) & 0x03 {
        0b01 => InterlaceMode::Mode1,
        0b11 => InterlaceMode::Mode2,
        _ => InterlaceMode::Off,
    }
}

/// R23 bits 7-6 as a [`DmaMode`]: any `0x` is a 68000 transfer (plutiedev
/// "$97xx").
#[inline]
pub(crate) const fn decode_dma_mode(reg: u8) -> DmaMode {
    match reg >> 6 {
        0b10 => DmaMode::Fill,
        0b11 => DmaMode::Copy,
        _ => DmaMode::Mc68kTransfer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_extent_maps_the_r16_size_field() {
        assert_eq!(plane_extent(0b00), 32);
        assert_eq!(plane_extent(0b01), 64);
        assert_eq!(plane_extent(0b11), 128);
        assert_eq!(plane_extent(0b10), 32, "10 is the invalid setting");
        assert_eq!(
            plane_extent(0b1111_1100),
            32,
            "only the low two bits are the field"
        );
    }

    #[test]
    fn interlace_mode_10_behaves_like_off() {
        assert_eq!(decode_interlace_mode(0b0000_0000), InterlaceMode::Off);
        assert_eq!(decode_interlace_mode(0b0000_0010), InterlaceMode::Mode1);
        assert_eq!(decode_interlace_mode(0b0000_0100), InterlaceMode::Off);
        assert_eq!(decode_interlace_mode(0b0000_0110), InterlaceMode::Mode2);
    }

    #[test]
    fn hscroll_modes_are_the_two_bit_field() {
        assert_eq!(decode_hscroll_mode(0b0000_0000), HScrollMode::Full);
        assert_eq!(decode_hscroll_mode(0b0000_0001), HScrollMode::EightLines);
        assert_eq!(decode_hscroll_mode(0b0000_0010), HScrollMode::PerCell);
        assert_eq!(decode_hscroll_mode(0b0000_0011), HScrollMode::PerLine);
    }

    #[test]
    fn dma_mode_is_the_top_two_bits_of_r23() {
        assert_eq!(decode_dma_mode(0b0000_0000), DmaMode::Mc68kTransfer);
        assert_eq!(decode_dma_mode(0b0100_0000), DmaMode::Mc68kTransfer);
        assert_eq!(decode_dma_mode(0b1000_0000), DmaMode::Fill);
        assert_eq!(decode_dma_mode(0b1100_0000), DmaMode::Copy);
    }

    #[test]
    fn table_base_masks_then_shifts() {
        assert_eq!(
            table_base(0x3F, 0x38, 10),
            0xE000,
            "bits outside the mask do not reach the address"
        );
        assert_eq!(table_base(0xFF, 0x07, 13), 0xE000);
        assert_eq!(table_base(0x79, 0x7E, 9), 0xF000);
        assert_eq!(table_base(0x79, 0x7F, 9), 0xF200);
    }
}
