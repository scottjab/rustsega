//! The status register: the one word the control port answers a read with.
//!
//! "Reading the control port returns a 16-bit word that allows you to observe
//! various states of the VDP and physical display" (genvdp.txt section 6
//! "Status Register"). The other half of that port is [`Vdp::write_control`]
//! (`ports`), and a read of it is also one of the four things that clear the
//! command-word latch (genvdp.txt section 7: "It is cleared when the control
//! port is read").
//!
//! Every flag here is plain state that the timing tickets set: M4-05 (#83) the
//! blanking and frame bits, M4-06 (#84) the interrupt pending bit, M4-16 (#93)
//! the sprite flags, M4-21a (#99) the FIFO the empty/full pair reports, M4-22
//! (#101) the DMA busy bit. Nothing in this file drives them: the read packs
//! what it is given and clears the two bits reading clears on real hardware.

use crate::Vdp;

/// d15-d10 of the status word: `001101`.
///
/// These are driven by the VDP rather than left floating: "d15 - Always 0,
/// d14 - Always 0, d13 - Always 1, d12 - Always 1, d11 - Always 0, d10 - Always
/// 1" (genvdp.txt section 6). So a status read that never reached the chip is
/// distinguishable in a trace, which is worth something: the word of an idle
/// NTSC VDP with an empty FIFO is `$3600`, not the `$FFFF` of the undecoded bus.
const STATUS_FIXED: u16 = 0x3400;

/// d9, FIFO empty.
///
/// The empty/full pair reports the four-entry FIFO (genvdp.txt section 6 quotes
/// Flavio Morsoletto: "EMPTY only goes 1 when there is nothing on the stack. The
/// intermediate state is signaled by both of them showing 0"), and there is no
/// FIFO yet, so there is no intermediate state to report and d8 is never set.
/// Reporting empty always is what genvdp.txt recommends on its own reading of
/// the field: "I've noticed most emulators keep the EMPTY flag set, so it
/// appears as if the FIFO was always empty instead of being in the neutral
/// state. This is probably for games that would normally check and find the FIFO
/// in a neutral state, then write data and expect to poll the FULL flag
/// afterwards." TODO(M4-21a #99): the FIFO that decides both bits.
const STATUS_FIFO_EMPTY: u16 = 1 << 9;

/// Bit numbers of genvdp.txt section 6, for [`flag`].
const BIT_PAL: u8 = 0;
const BIT_DMA_BUSY: u8 = 1;
const BIT_HBLANK: u8 = 2;
const BIT_VBLANK: u8 = 3;
const BIT_ODD_FRAME: u8 = 4;
const BIT_SPRITE_COLLISION: u8 = 5;
const BIT_SPRITE_OVERFLOW: u8 = 6;
const BIT_VINT: u8 = 7;

/// `bit` when `set`, nothing when it is not, for OR-ing the flag bits together.
const fn flag(bit: u8, set: bool) -> u16 {
    if set { 1 << bit } else { 0 }
}

impl Vdp {
    /// Read the status register, which is what reading the control port
    /// (`$C00004`, and its mirror at `$C00006`) does.
    ///
    /// The word is genvdp.txt section 6's bit list, and only two of the bits are
    /// the read's own doing:
    ///
    /// * d6 SOVR and d5 C, the sprite flags, are cleared by the read that
    ///   reports them. genvdp.txt does not say so outright - it says only that
    ///   each "is most likely cleared at the end of the frame" - but it does
    ///   stop to say of d7 that "reading the control port does not clear this
    ///   bit", which is a thing to point out only if the others are; BlastEm and
    ///   Genesis Plus GX both clear them here, and a VBL handler that polls for
    ///   overflow in the next frame depends on it.
    /// * the write-pending flag goes with it: "It is cleared when the control
    ///   port is read" (genvdp.txt section 7), so a status read in the middle of
    ///   a command word abandons that command. [`Vdp::write_control`] is the
    ///   other side of that rule.
    ///
    /// `vint_pending` is explicitly not cleared: only the acknowledge of
    /// interrupt level 6 clears it (genvdp.txt section 6, and [`Vdp::ack_irq`]
    /// in `timing`). The blanking, frame and DMA bits are read-only snapshots of
    /// state that `timing` drives.
    pub fn read_status(&mut self) -> u16 {
        self.pending = false;

        let word = STATUS_FIXED
            | STATUS_FIFO_EMPTY
            | flag(BIT_VINT, self.vint_pending)
            | flag(BIT_SPRITE_OVERFLOW, self.sprite_overflow)
            | flag(BIT_SPRITE_COLLISION, self.sprite_collision)
            | flag(BIT_ODD_FRAME, self.odd_frame)
            // A display that has been switched off is blank for the whole frame:
            // "'Blanking' is either the vertical blanking period or when the
            // display is forcibly blanked via register #1" (genvdp.txt section
            // 11). d3 is the vertical blanking signal, so it reads 1 there
            // whatever `in_vblank` says.
            | flag(BIT_VBLANK, self.in_vblank || !self.display_enabled())
            | flag(BIT_HBLANK, self.in_hblank)
            | flag(BIT_DMA_BUSY, self.dma_busy)
            | flag(BIT_PAL, self.pal);

        self.sprite_overflow = false;
        self.sprite_collision = false;

        word
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::testutil::vram_write_cmd;

    /// The word of a VDP that has been reset and never drawn a frame: the
    /// constants of genvdp.txt section 6, FIFO empty, and VB, which a display
    /// that is switched off reads as (see [`display_off_reads_vertical_blank`]).
    const FRESH_NTSC: u16 = 0x3608;

    /// The same word with the display on and nothing pending: VB is the only bit
    /// the display switch moves.
    const DISPLAY_ON_NTSC: u16 = 0x3600;

    fn set_reg(vdp: &mut Vdp, reg: u8, value: u8) {
        vdp.write_control(0x8000 | ((u16::from(reg) & 0x1F) << 8) | u16::from(value));
    }

    /// A fresh NTSC VDP: FIFO empty set, PAL clear, and none of the flag bits
    /// the timing tickets have not reached yet.
    #[test]
    fn fresh_ntsc_status_is_fifo_empty_over_the_fixed_bits() {
        let mut vdp = Vdp::new(false);
        let status = vdp.read_status();

        assert_eq!(
            status & 0xFC00,
            0x3400,
            "d15-d10 are driven by the VDP as 0,0,1,1,0,1 (genvdp.txt section 6)"
        );
        assert_ne!(status & (1 << 9), 0, "d9 FIFO empty");
        assert_eq!(status & (1 << 8), 0, "d8 FIFO full");
        assert_eq!(
            status & 0x76,
            0,
            "no DMA, no sprite flag, no HBlank; bit 3 is the display being off"
        );
        assert_eq!(status & 1, 0, "d0 PAL is clear on an NTSC chip");
        assert_eq!(status, FRESH_NTSC, "the whole word at reset");
    }

    /// d0 is the `PAL` pin the chip was built with, the same fact
    /// [`Vdp::active_height`] reads R1 bit 3 against.
    #[test]
    fn pal_vdp_sets_the_pal_bit() {
        let mut vdp = Vdp::new(true);
        assert_ne!(vdp.read_status() & 1, 0, "d0 PAL mode flag");

        let mut ntsc = Vdp::new(false);
        assert_eq!(ntsc.read_status() & 1, 0);
    }

    /// d6 and d5 are gone after the read that showed them, which is how a VBL
    /// handler can leave them set and still tell whether the next frame overflowed.
    #[test]
    fn sprite_overflow_and_collision_read_once_and_clear() {
        let mut vdp = Vdp::new(false);
        vdp.sprite_overflow = true;
        vdp.sprite_collision = true;

        let status = vdp.read_status();
        assert_ne!(status & (1 << 6), 0, "d6 sprite overflow");
        assert_ne!(status & (1 << 5), 0, "d5 sprite collision");

        let status = vdp.read_status();
        assert_eq!(
            status & 0x60,
            0,
            "reading the status register is what clears SOVR and C, and it did"
        );
        assert!(
            !vdp.sprite_overflow && !vdp.sprite_collision,
            "the flags themselves went too, not just the word"
        );
    }

    /// d7 is the pending VINT, and reading it is not acknowledging it: "Reading
    /// the control port does not clear this bit" (genvdp.txt section 6).
    #[test]
    fn vint_pending_is_not_cleared_by_reading_it() {
        let mut vdp = Vdp::new(false);
        vdp.vint_pending = true;

        assert_ne!(vdp.read_status() & (1 << 7), 0, "d7 VINT pending");
        assert_ne!(
            vdp.read_status() & (1 << 7),
            0,
            "and again: only the interrupt acknowledge clears it"
        );
        assert!(vdp.vint_pending, "the flag survives the reads");
    }

    /// d3 is the vertical blanking signal, and a display that has been switched
    /// off is blanking all the time: R1 bit 6 clear forces blank (genvdp.txt
    /// section 11 says of its transfer table that "'Blanking' is either the
    /// vertical blanking period or when the display is forcibly blanked via
    /// register #1").
    #[test]
    fn display_off_reads_vertical_blank() {
        let mut vdp = Vdp::new(false);
        assert!(!vdp.in_vblank, "no timing here has put the chip in VBlank");
        assert!(
            !vdp.display_enabled(),
            "R1 bit 6 clear is the state a reset VDP is in"
        );
        assert_ne!(
            vdp.read_status() & (1 << 3),
            0,
            "so d3 VB reads 1 while the display is off"
        );

        set_reg(&mut vdp, 1, 0x40);
        assert_eq!(
            vdp.read_status() & (1 << 3),
            0,
            "with the display on, d3 is the real blanking signal again"
        );

        vdp.in_vblank = true;
        assert_ne!(vdp.read_status() & (1 << 3), 0, "and that is d3");
    }

    /// d4, d3, d2 and d1 are the four remaining flags, each in its own bit, and
    /// none of them is cleared by the read that showed them.
    #[test]
    fn every_flag_sits_in_its_own_bit() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 1, 0x40);
        vdp.odd_frame = true;
        vdp.in_hblank = true;
        vdp.dma_busy = true;
        assert_eq!(
            vdp.read_status(),
            DISPLAY_ON_NTSC | (1 << 4) | (1 << 2) | (1 << 1),
            "d4 odd frame, d2 horizontal blank, d1 DMA busy"
        );

        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 1, 0x40);
        vdp.in_vblank = true;
        assert_eq!(
            vdp.read_status(),
            DISPLAY_ON_NTSC | (1 << 3),
            "d3 is the only blanking bit VBlank itself sets"
        );
    }

    /// Reading the status register is one of the four things that clear the
    /// write-pending flag: "It is cleared when the control port is read"
    /// (genvdp.txt section 7).
    #[test]
    fn reading_status_clears_the_command_latch() {
        let mut vdp = Vdp::new(false);
        let (first, _second) = vram_write_cmd(0x0004);
        vdp.write_control(first);
        assert!(vdp.pending, "half a command word is latched");

        vdp.read_status();
        assert!(!vdp.pending, "the status read cleared it");

        // The port-side proof: a register-looking word is a register write
        // again, not the second half of the command ("You cannot write to a VDP
        // register if the pending flag is set to one").
        vdp.write_control(0x8F04);
        assert_eq!(vdp.reg(15), 4, "0x8F04 was taken as R15 = 4");
    }
}
