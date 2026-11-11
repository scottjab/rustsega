//! 68k to VDP DMA: the transfer a command word starts and the 68k bus feeds.
//!
//! A command word with CD5 set asks for the transfer (`ports`, genvdp.txt
//! section 7 "VDP ports": "Setting CD5 will trigger a DMA operation"), and it
//! also sets up the destination, so every word arrives at the address register
//! through the data port rules of M4-02 (#80). The source and the word count
//! come from R19 to R23 (genvdp.txt section 11 "DMA"; plutiedev "$93xx" to
//! "$97xx").
//!
//! Only the 68k transfer mode is here. R23 bit 7 set is a fill or a copy
//! ([`DmaMode::Fill`], [`DmaMode::Copy`]), which is M4-08 (#86); a request for
//! one is dropped unrun, as a request is whenever the transfer is not allowed.
//!
//! Not modelled: when the transfer runs and how long it takes, the 68k bus
//! stall that goes with it, and the read FIFO quirk that makes the first word
//! of a 68k-to-VRAM transfer wrong on real hardware. All three are M4-22 (#101).

use crate::DmaMode;
use crate::Vdp;
use crate::regs;

/// The 68k-side memory a 68k to VDP DMA reads its source from.
///
/// The VDP only ever wants one word at a time from a bus it takes over to
/// make the transfer; what is behind the address (ROM, RAM, a mapper) is the
/// `genesis` crate's business, and its 68k window implements this for the
/// address space as it has it mapped (M3-09, #69).
pub trait DmaBus {
    /// Read the 16-bit word at byte address `addr` of the 68k address space.
    fn read_word(&mut self, addr: u32) -> u16;
}

/// The block half of a DMA source address.
///
/// The source is a 128 KiB block of the 68k map plus an offset inside it: R23
/// bits 6-0 are address bits 23-17, R22 bits 16-9, and R21 bits 8-1
/// (plutiedev "$95xx"/"$96xx"/"$97xx", where `DMD0 becomes SA23`). Each step
/// of a 68k transfer adds 2 to the offset and leaves the block alone, so the
/// source walks its own 128 KiB block and never crosses into the next one
/// (genvdp.txt section 11 "DMA").
const DMA_BLOCK_MASK: u32 = 0x00FE_0000;

/// The offset half of a DMA source address: the complement of
/// [`DMA_BLOCK_MASK`], one 128 KiB block wide (plutiedev "$95xx"/"$96xx").
const DMA_OFFSET_MASK: u32 = 0x0001_FFFF;

/// Words transferred by a length field of zero: R19/R20 are a 16-bit word
/// count, and the value that would mean "no words" means all of them
/// (genvdp.txt section 11 "DMA"; plutiedev "$93xx"/"$94xx").
const DMA_FULL_LENGTH: u32 = 0x1_0000;

/// Can a command word with CD5 run a 68k-to-VDP transfer right now?
///
/// R1 M1 is the switch that makes it "1 to allow DMA operations" (plutiedev
/// "$81xx"; genvdp.txt section 11 "DMA"), and R23 bit 7 set is a fill or a
/// copy rather than this transfer: the 68k mode is `0x` in the two mode bits
/// (genvdp.txt section 11; plutiedev "$97xx"). CD5 itself is the request that
/// [`Vdp::run_dma`] consumes rather than a register to re-read.
const fn transfer_allowed(dma_enabled: bool, dma_mode: DmaMode) -> bool {
    dma_enabled && matches!(dma_mode, DmaMode::Mc68kTransfer)
}

/// Words to transfer for a length field of `length`: R19 low and R20 high as
/// a word count, with 0 meaning [`DMA_FULL_LENGTH`] (see that constant).
const fn words_to_transfer(length: u16) -> u32 {
    match length as u32 {
        0 => DMA_FULL_LENGTH,
        words => words,
    }
}

/// The source address of the next word in a 68k transfer.
///
/// Add 2 to the offset, keep the block: see [`DMA_BLOCK_MASK`] and
/// [`DMA_OFFSET_MASK`] (genvdp.txt section 11 "DMA").
const fn next_source(src: u32) -> u32 {
    (src & DMA_BLOCK_MASK) | ((src.wrapping_add(2)) & DMA_OFFSET_MASK)
}

/// The source address registers a transfer leaves behind: R21 is bits 8-1 and
/// R22 is bits 16-9 of the address the next word would come from (plutiedev
/// "$95xx"/"$96xx").
///
/// R23 is deliberately not part of this: its bits 6-0 are the block the source
/// is in, which [`next_source`] holds fixed, and bit 7 is the mode bit, which
/// a transfer has no business changing (plutiedev "$97xx").
const fn source_fields(src: u32) -> (u8, u8) {
    ((src >> 1) as u8, (src >> 9) as u8)
}

impl Vdp {
    /// Run the 68k to VDP DMA that the last control port write asked for.
    ///
    /// Called by the bus glue after every control port write, which only
    /// records the request ([`Vdp::dma_requested`]) because it has no bus to
    /// hand over; so this is a no-op, touching nothing, whenever the write
    /// asked for no DMA.
    ///
    /// The request is consumed either way. A DMA that is not allowed, because
    /// R1 M1 went clear or R23 bit 7 went set between the command word and
    /// here, stops at the consuming: no bus read, no write, no register
    /// write-back. Leaving the request standing would let a later, permitted
    /// command carry out a transfer the game never asked for.
    ///
    /// The words arrive through [`Vdp::write_data`], so the destination
    /// memory, its masking and the move of the address register are the data
    /// port rules of M4-02 (#80) and are not restated here.
    pub fn run_dma(&mut self, bus: &mut impl DmaBus) {
        if !self.dma_requested {
            return;
        }
        self.dma_requested = false;
        if !transfer_allowed(self.dma_enabled(), self.dma_mode()) {
            return;
        }

        let words = words_to_transfer(self.dma_length());
        let mut src = self.dma_source();

        // The control port latch is not part of a DMA. Its flag is set and
        // cleared by command port and data port accesses (genvdp.txt section 7
        // "VDP port access"), and a transfer goes to the memories without
        // either, so it keeps whatever it held: a command word that was half
        // written when the DMA ran is still half written when it ends. In the
        // flow the glue uses, a call straight after the command word, that
        // word has just cleared the flag, so this is the same value twice.
        let pending = self.pending;
        for _ in 0..words {
            let value = bus.read_word(src);
            // TODO(M4-22 #101): the first word of a 68k-to-VRAM transfer is
            // often wrong on real hardware, because of the read FIFO, and the
            // transfer is not instant either. Neither is modelled here.
            self.write_data(value);
            src = next_source(src);
        }
        self.pending = pending;

        // The count is spent and the source moved on, both of which software
        // can see (genvdp.txt section 11 "DMA"; plutiedev "$93xx" to "$96xx").
        let (source_low, source_high) = source_fields(src);
        self.regs[regs::R19] = 0;
        self.regs[regs::R20] = 0;
        self.regs[regs::R21] = source_low;
        self.regs[regs::R22] = source_high;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::testutil::{cram_write_cmd, vram_write_cmd, vsram_write_cmd};

    /// A 68k bus for these tests: a handful of words laid down at byte
    /// addresses, and every read recorded.
    ///
    /// The recording is what makes the block rule testable, because what
    /// matters there is not the value that came back but the address that was
    /// asked for. And a bus that holds nothing at all is open bus, which lets
    /// the length-of-0 test count 0x10000 reads without holding 0x10000 words.
    #[derive(Default)]
    struct FakeBus {
        /// Words, as the byte address each one is laid at.
        words: Vec<(u32, u16)>,
        /// The addresses read, in order.
        reads: Vec<u32>,
    }

    impl FakeBus {
        /// A bus with `words` laid from byte address `addr` on up, first word
        /// at `addr` itself.
        fn laid_at(addr: u32, words: &[u16]) -> Self {
            Self {
                words: words
                    .iter()
                    .enumerate()
                    .map(|(index, &word)| (addr + 2 * index as u32, word))
                    .collect(),
                reads: Vec::new(),
            }
        }
    }

    impl DmaBus for FakeBus {
        fn read_word(&mut self, addr: u32) -> u16 {
            self.reads.push(addr);
            self.words
                .iter()
                .find(|(at, _)| *at == addr)
                .map_or(addr as u16, |&(_, word)| word)
        }
    }

    /// Set up a 68k transfer of `words` words starting at byte address `src`,
    /// with R1 M1 set so that it is allowed (plutiedev "$81xx") and R15 stepping
    /// the address register by a word like a real transfer set-up does.
    fn dma_from(vdp: &mut Vdp, src: u32, words: u16) {
        vdp.write_register(19, words as u8);
        vdp.write_register(20, (words >> 8) as u8);
        vdp.write_register(21, (src >> 1) as u8);
        vdp.write_register(22, (src >> 9) as u8);
        vdp.write_register(23, ((src >> 17) as u8) & 0x7F);
        vdp.write_register(15, 2);
        vdp.write_register(1, 0x10);
    }

    /// The command word that starts a DMA to the memory `cmd` points at: CD5
    /// in the second half (genvdp.txt section 7).
    fn start_dma(vdp: &mut Vdp, cmd: (u16, u16)) {
        vdp.write_control(cmd.0);
        vdp.write_control(cmd.1 | 0x0080);
        assert!(vdp.dma_requested(), "a command word with CD5 asked for it");
    }

    #[test]
    fn four_words_land_in_vram_at_the_address_register() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, vram_write_cmd(0x1000));

        let mut bus = FakeBus::laid_at(0x000100, &[1, 2, 3, 4]);
        vdp.run_dma(&mut bus);

        assert_eq!(
            &vdp.vram[0x1000..0x1008],
            [0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04],
            "the words of 0x000100 in VRAM order, at the address of the command"
        );
        assert_eq!(bus.reads, [0x000100, 0x000102, 0x000104, 0x000106]);
    }

    #[test]
    fn a_dma_to_cram_is_masked_like_a_data_port_write() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, cram_write_cmd(0x0000));

        let mut bus = FakeBus::laid_at(0x000100, &[0x0EEE, 0xFFFF, 0x1234, 0x000F]);
        vdp.run_dma(&mut bus);

        assert_eq!(
            &vdp.cram[0..4],
            [0x0EEE, 0x0EEE, 0x0224, 0x000E],
            "----bbb0 ggg0 rrr0 for every word (genvdp.txt section 9)"
        );
    }

    #[test]
    fn a_dma_to_vsram_works_like_the_other_two() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 2);
        start_dma(&mut vdp, vsram_write_cmd(0x0000));

        let mut bus = FakeBus::laid_at(0x000100, &[0x0102, 0x03FF]);
        vdp.run_dma(&mut bus);

        assert_eq!(&vdp.vsram[0..2], [0x0102, 0x03FF]);
    }

    #[test]
    fn the_source_wraps_inside_its_own_128kib_block() {
        let mut vdp = Vdp::new(false);
        // Block 0x20000, offset 0x1FFFE: the last word of the block, so the
        // next one has to wrap to the start of the same block.
        dma_from(&mut vdp, 0x0003_FFFE, 2);
        start_dma(&mut vdp, vram_write_cmd(0x0000));

        let mut bus = FakeBus::default();
        bus.words.push((0x0003_FFFE, 0x1111));
        bus.words.push((0x0002_0000, 0x2222));
        vdp.run_dma(&mut bus);

        assert_eq!(
            bus.reads,
            [0x0003_FFFE, 0x0002_0000],
            "the low 17 bits of the source wrap, bit 17 and above stay"
        );
        assert_eq!(&vdp.vram[0..4], [0x11, 0x11, 0x22, 0x22]);
    }

    #[test]
    fn a_length_of_zero_transfers_0x10000_words() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x0000_0000, 0);
        start_dma(&mut vdp, vram_write_cmd(0x0000));

        // Open bus: an empty fake, so the 0x10000 words cost nothing but the
        // reads themselves, and the transfer still has to end.
        let mut bus = FakeBus::default();
        vdp.run_dma(&mut bus);

        assert_eq!(bus.reads.len(), 0x1_0000);
        assert_eq!(bus.reads.first(), Some(&0x0000_0000));
        assert_eq!(
            bus.reads.last(),
            Some(&0x0001_FFFE),
            "the last word of the block, so all of it went"
        );
    }

    #[test]
    fn a_disabled_dma_never_touches_the_bus() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, vram_write_cmd(0x1000));
        vdp.write_register(1, 0x00);

        let mut bus = FakeBus::laid_at(0x000100, &[1, 2, 3, 4]);
        vdp.run_dma(&mut bus);

        assert!(bus.reads.is_empty(), "R1 M1 clear: the bus is left alone");
        assert!(
            !vdp.dma_requested(),
            "and the request is consumed all the same"
        );
        assert!(
            vdp.vram[0x1000..0x1008].iter().all(|&byte| byte == 0),
            "nothing transferred"
        );
        assert_eq!(
            (vdp.reg(19), vdp.reg(20)),
            (4, 0),
            "a transfer that did not run spends nothing"
        );
    }

    #[test]
    fn a_fill_or_copy_request_is_left_for_m4_08() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        // R23 bit 7 set is a fill, which is not this transfer and must not run
        // as one (genvdp.txt section 11; plutiedev "$97xx").
        vdp.write_register(23, 0x80);
        start_dma(&mut vdp, vram_write_cmd(0x1000));

        let mut bus = FakeBus::laid_at(0x000100, &[1, 2, 3, 4]);
        vdp.run_dma(&mut bus);

        assert!(bus.reads.is_empty(), "R23 bit 7 set: the bus is left alone");
        assert!(!vdp.dma_requested(), "and the request is consumed");
    }

    #[test]
    fn a_transfer_spends_the_count() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, vram_write_cmd(0x1000));

        let mut bus = FakeBus::laid_at(0x000100, &[1, 2, 3, 4]);
        vdp.run_dma(&mut bus);

        assert_eq!(
            (vdp.reg(19), vdp.reg(20)),
            (0, 0),
            "R19/R20 read back as zero, which is how a game knows"
        );
    }

    #[test]
    fn a_transfer_leaves_the_source_at_the_next_word() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, vram_write_cmd(0x1000));

        let mut bus = FakeBus::laid_at(0x000100, &[1, 2, 3, 4]);
        vdp.run_dma(&mut bus);

        assert_eq!(
            (vdp.reg(21), vdp.reg(22), vdp.reg(23)),
            (0x84, 0x00, 0x00),
            "0x000108, the address the fifth word would have come from"
        );
    }

    #[test]
    fn the_source_registers_follow_a_wrapped_transfer_and_r23_does_not_move() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x0003_FFFE, 2);
        start_dma(&mut vdp, vram_write_cmd(0x0000));

        let mut bus = FakeBus::default();
        vdp.run_dma(&mut bus);

        assert_eq!(
            (vdp.reg(21), vdp.reg(22)),
            (0x01, 0x00),
            "the wrapped offset 0x020002 in the 16 bits R21/R22 hold"
        );
        assert_eq!(
            vdp.reg(23),
            0x01,
            "R23 keeps the block and the mode: it is not written back"
        );
    }

    #[test]
    fn calling_with_nothing_requested_does_nothing() {
        // The glue calls `run_dma` after every control port write, so a write
        // that was not a DMA command word has to leave the VDP and the bus
        // exactly as they were.
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        let cmd = vram_write_cmd(0x1000);
        vdp.write_control(cmd.0);
        vdp.write_control(cmd.1);
        assert!(!vdp.dma_requested(), "no CD5, no request");

        let mut bus = FakeBus::laid_at(0x000100, &[1, 2, 3, 4]);
        vdp.run_dma(&mut bus);

        assert!(bus.reads.is_empty());
        assert_eq!((vdp.reg(19), vdp.reg(21)), (4, 0x80));
    }

    #[test]
    fn a_dma_leaves_a_half_written_command_word_alone() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, vram_write_cmd(0x1000));
        // Half of another command word before the glue gets to the DMA: the
        // transfer must neither finish it nor undo it.
        vdp.write_control(vram_write_cmd(0x2000).0);
        assert!(vdp.pending);

        let mut bus = FakeBus::laid_at(0x000100, &[1, 2, 3, 4]);
        vdp.run_dma(&mut bus);

        assert_eq!(
            &vdp.vram[0x2000..0x2008],
            [0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04],
            "the address of the half-written command is the live one"
        );
        assert!(
            vdp.pending,
            "the second half is still owed to the control port"
        );
    }

    #[test]
    fn a_68k_transfer_needs_m1_and_the_68k_mode() {
        assert!(transfer_allowed(true, DmaMode::Mc68kTransfer));
        assert!(
            !transfer_allowed(false, DmaMode::Mc68kTransfer),
            "R1 M1 clear"
        );
        assert!(
            !transfer_allowed(true, DmaMode::Fill),
            "R23 bit 7 set is a fill"
        );
        assert!(
            !transfer_allowed(true, DmaMode::Copy),
            "R23 bit 7 set is a copy"
        );
    }

    #[test]
    fn a_length_field_of_zero_is_the_whole_count() {
        assert_eq!(words_to_transfer(0), 0x1_0000);
        assert_eq!(words_to_transfer(1), 1);
        assert_eq!(words_to_transfer(4), 4);
        assert_eq!(words_to_transfer(0xFFFF), 0xFFFF);
    }

    #[test]
    fn the_next_source_moves_the_offset_and_not_the_block() {
        assert_eq!(next_source(0x000100), 0x000102);
        assert_eq!(next_source(0x0000_01FE), 0x000200);
        assert_eq!(
            next_source(0x0003_FFFE),
            0x0002_0000,
            "offset 0x1FFFE of block 0x20000 wraps to offset 0 of the same block"
        );
        assert_eq!(
            next_source(0x00FF_FFFE),
            0x00FE_0000,
            "block 0xFE0000, offset 0x1FFFE: the top of the map wraps back to the top block"
        );
    }

    #[test]
    fn the_source_fields_are_the_low_two_registers() {
        assert_eq!(source_fields(0x000108), (0x84, 0x00));
        assert_eq!(source_fields(0x0002_0002), (0x01, 0x00));
        assert_eq!(source_fields(0x00FE_FFFE), (0xFF, 0x7F));
    }
}
