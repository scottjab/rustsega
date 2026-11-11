//! The three DMA modes: the 68k transfer, the fill and the copy.
//!
//! A command word with CD5 set asks for one of them (`ports`, genvdp.txt
//! section 7 "VDP ports": "Setting CD5 will trigger a DMA operation"), and it
//! also sets up the destination, so what each mode writes arrives at the
//! address register through the data port rules of M4-02 (#80). Which mode it
//! asked for is R23 bits 7-6 ([`DmaMode`]), and the length and the source come
//! from R19 to R23 (genvdp.txt section 11 "DMA"; plutiedev "$93xx" to "$97xx").
//!
//! [`Vdp::run_dma`] is the turn the bus glue gives the request after every
//! control port write, and it is where the 68k transfer ([`DmaMode::Mc68kTransfer`],
//! M4-07 #85) and the copy ([`DmaMode::Copy`], M4-08 #86) run. The fill
//! ([`DmaMode::Fill`], also #86) is the odd one out: the command word only arms
//! it, and the next word to the data port is what runs it, so [`Vdp::run_fill`]
//! is called from the data port instead.
//!
//! Not modelled: when any of the three runs and how long it takes, the 68k bus
//! stall that goes with the transfer, and the read FIFO quirk that makes the
//! first word of a 68k-to-VRAM transfer wrong on real hardware. All three are
//! M4-22 (#101).

use crate::DmaMode;
use crate::Vdp;
use crate::ports::{DataTarget, data_target, increment};
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

/// Bytes to fill or copy for a length field of `length`.
///
/// The same field that is a word count for a 68k transfer is a **byte** count
/// for the other two modes: "Registers 19, 20, specify how many 8-bit bytes to
/// fill" and "... to copy" (genvdp.txt section 11 "VRAM fill", "VRAM copy").
///
/// Zero is [`DMA_FULL_LENGTH`] here too. genvdp.txt section 11 says only
/// "When the length field is set to zero, the length is treated as FFFFh", but
/// its own fill pseudocode counts with `while (--length)`, which walks the
/// counter through 0 to FFFFh and stops after FFFFh + 1 steps; Genesis Plus GX
/// spells the same thing out as `dma_length = ...; if (!dma_length) dma_length
/// = 0x10000;` for the fill and the copy alike.
const fn bytes_to_move(length: u16) -> u32 {
    match length as u32 {
        0 => DMA_FULL_LENGTH,
        bytes => bytes,
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
    /// Run the DMA that the last control port write asked for, whatever mode
    /// R23 says it was: the 68k transfer and the copy run here, and the fill is
    /// armed for [`Vdp::run_fill`] to run at the next data port write.
    ///
    /// Called by the bus glue after every control port write, which only
    /// records the request ([`Vdp::dma_requested`]) because it has no bus to
    /// hand over; so this is a no-op, touching nothing, whenever the write
    /// asked for no DMA.
    ///
    /// The request is consumed either way. A DMA that is not allowed, because
    /// R1 M1 went clear between the command word and here, stops at the
    /// consuming: no bus read, no write, no register write-back. Leaving the
    /// request standing would let a later, permitted command carry out a
    /// transfer the game never asked for.
    pub fn run_dma(&mut self, bus: &mut impl DmaBus) {
        if !self.dma_requested {
            return;
        }
        self.dma_requested = false;
        match self.dma_mode() {
            // A fill does not run here: it waits for the data port write that
            // starts it (`Vdp::run_fill`). Being armed is as far as the command
            // word gets, and it is the R1 M1 bit that allows it, the same bit
            // `transfer_allowed` reads for the 68k transfer.
            DmaMode::Fill if self.dma_enabled() => self.arm_fill(),
            DmaMode::Copy if self.dma_enabled() => self.run_copy(),
            _ if transfer_allowed(self.dma_enabled(), self.dma_mode()) => self.run_transfer(bus),
            _ => {}
        }
    }

    /// Arm the DMA fill that the next data port write starts.
    fn arm_fill(&mut self) {
        self.dma_fill_pending = true;
        // "Bit 1 is set for the duration of a DMA operation. This is only
        // useful for fills and copies" (genvdp.txt section 6), and a fill is
        // running from the command word: it has been started, it is only
        // waiting for its data. Genesis Plus GX sets its DMA busy the same way
        // when it sees the fill mode in the command word.
        self.dma_busy = true;
    }

    /// Run the fill that [`Vdp::arm_fill`] armed, if there is one, with `value`
    /// as the word the data port has just been written.
    ///
    /// The reference for the whole of this is genvdp.txt section 11 "VRAM
    /// fill", which gives the fill as "C" pseudocode. Quoted in full, because
    /// the exact byte sequence is the part everyone gets wrong:
    ///
    /// ```text
    ///  void vram_fill(int data)
    ///  {
    ///     /* Write lower byte to address specified */
    ///     vram[address] = (data >> 0) & 0xFF;
    ///
    ///     do {
    ///         /* Write upper byte to adjacent address */
    ///         vram[address ^ 1] = (data >> 8) & 0xFF;
    ///
    ///         /* Increment address register */
    ///         address += vdp_reg[15];
    ///     } while(--length)
    ///  }
    /// ```
    ///
    /// Read against the port rules of M4-02 (#80), the first line of that
    /// pseudocode is the data port write that got us here: `Vdp::write_data`
    /// has already written the whole word, swapped for an odd address, by the
    /// time this runs. So the loop starts from where that write left the
    /// address register - the address of the write plus one step of R15 - and
    /// the MSB is what it repeats, `length` times, on top of a word that has
    /// already been written in the ordinary way. Genesis Plus GX's
    /// `vdp_dma_fill` is called the same way, after the port write has moved
    /// the register on, and its loop is `WRITE_BYTE(vram, addr ^ 1, data)` then
    /// `addr += reg[15]`.
    ///
    /// The first line of that pseudocode is the one place the doc contradicts
    /// itself: it puts the LSB at `address`, where genvdp.txt section 8 has the
    /// MSB at the even address ("When writing 16-bit data to VRAM, having
    /// address bit 0 set will swap the upper and lower bytes of the data
    /// written") and where the port rules of #80 were settled. A fill does not
    /// reverse a data port write, so the word goes in the ordinary way and only
    /// the repeated byte is the fill's own.
    ///
    /// The MSB is also what makes an 8-bit write to the data port a byte fill:
    /// "a distinction between 8-bit and 16-bit VRAM fills do not exist. There
    /// is only one type of fill, and depending on how you write to the data port
    /// will determine the kind of data used in the fill" (genvdp.txt section 7
    /// "8-bit port access"), and a byte write arrives here as that byte
    /// duplicated in both halves.
    ///
    /// Two things about the counts, both of which the tests pin:
    ///
    /// - The length is a **byte** count here, not the word count of a 68k
    ///   transfer: "Registers 19, 20, specify how many 8-bit bytes to fill"
    ///   (genvdp.txt section 11), so a length of N is N writes of one byte on
    ///   top of the word that starts the fill - which is what every game that
    ///   fills relies on.
    /// - One byte per step means R15 is the stride: R15 = 1 walks a contiguous
    ///   run of the MSB, R15 = 2 leaves the other bytes of VRAM alone.
    ///
    /// CRAM and VSRAM take the whole word every step instead of a byte, because
    /// they are word-wide memories and a fill to them is "the same as normal ...
    /// data port access" (genvdp.txt section 11). That is the doc's own "I'd
    /// assume" - the one fill rule genvdp.txt admits it never verified - with
    /// Genesis Plus GX's `vdp_dma_fill` behind it, which writes `fifo[fifo_idx]`,
    /// the whole of the last word, at every step of a CRAM or VSRAM fill.
    pub(crate) fn run_fill(&mut self, value: u16) {
        if !self.dma_fill_pending {
            return;
        }
        self.dma_fill_pending = false;
        // M4-22 (#101) is what will make a fill take time; until then the write
        // that starts it is the write that finishes it.
        self.dma_busy = false;

        let steps = bytes_to_move(self.dma_length());
        match data_target(self.code) {
            DataTarget::Vram => {
                let byte = (value >> 8) as u8;
                for _ in 0..steps {
                    // `addr ^ 1` is the byte on the other side of the pair the
                    // address sits in, and the address register then moves on by
                    // R15: the two lines of the pseudocode quoted above.
                    self.vram[usize::from(self.addr ^ 1)] = byte;
                    self.addr = increment(self.addr, self.auto_increment());
                }
            }
            DataTarget::Cram | DataTarget::Vsram => {
                for _ in 0..steps {
                    self.write_target_word(value);
                }
            }
            // A code register that names no memory still costs the step: the
            // address moves on whatever the fill wrote (Genesis Plus GX's
            // default case: "invalid destination does nothing ... address is
            // still incremented").
            DataTarget::Ignored => {
                for _ in 0..steps {
                    self.addr = increment(self.addr, self.auto_increment());
                }
            }
        }

        // The count is spent, as it is after a 68k transfer: "After either:
        // length regs 0" is how the ticket puts it, and the same write-back of
        // R19/R20 is what a game that polls the DMA sees.
        self.regs[regs::R19] = 0;
        self.regs[regs::R20] = 0;
    }

    /// Copy VRAM to VRAM: [`Vdp::dma_copy_source`] to the address register,
    /// [`bytes_to_move`] of them, one byte per step.
    ///
    /// genvdp.txt section 11 "VRAM copy" is short: "The VDP will read a byte
    /// from the source address which is then incremented by one. The data will
    /// then be written to the destination address, which is incremented by
    /// register #15."
    ///
    /// What it does not say is that both addresses are used one byte off: the
    /// copy reads `src ^ 1` and writes `dst ^ 1`, the same adjacent-byte rule
    /// as the fill. Nothing in genvdp.txt covers this, so it is **inferred** -
    /// from the ticket and from Genesis Plus GX's `vdp_dma_copy`, which reads
    /// `READ_BYTE(vram, source ^ 1)` and writes `WRITE_BYTE(vram, addr ^ 1,
    /// data)`. For the common set-up, where the step is 1 and the source and
    /// destination parities match, the two readings give the same VRAM and only
    /// the order of the writes differs; with a step of 2 they do not, and a
    /// test pins this one.
    ///
    /// Runs from [`Vdp::run_dma`], straight out, because "In the case of DMA
    /// transfer or DMA copy, the operation will start immediately after writing
    /// this value" (plutiedev "Start DMA") - unlike the fill, which waits for
    /// the data port.
    ///
    /// Only R19/R20 are written back. Whether the source registers move during a
    /// DMA is one of genvdp.txt's own open questions - "Source and length
    /// registers being updated during DMA" is on its to-do list - and Genesis
    /// Plus GX adds the whole length to R21/R22 for every mode without
    /// convincing anybody. A game that chained copies without reloading them
    /// would want that; if one turns up, that is the follow-up.
    fn run_copy(&mut self) {
        let steps = bytes_to_move(self.dma_length());
        let mut src = self.dma_copy_source();
        for _ in 0..steps {
            let byte = self.vram[usize::from(src ^ 1)];
            self.vram[usize::from(self.addr ^ 1)] = byte;
            src = src.wrapping_add(1);
            self.addr = increment(self.addr, self.auto_increment());
        }

        self.regs[regs::R19] = 0;
        self.regs[regs::R20] = 0;
        self.dma_busy = false;
    }

    /// The 68k to VDP transfer itself: [`Vdp::dma_source`] to the address
    /// register, [`words_to_transfer`] of them.
    ///
    /// The words arrive through [`Vdp::write_data`], so the destination memory,
    /// its masking and the move of the address register are the data port rules
    /// of M4-02 (#80) and are not restated here.
    fn run_transfer(&mut self, bus: &mut impl DmaBus) {
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
        // Nor is a fill left waiting by a transfer. Only a data port write
        // starts one (genvdp.txt section 11 "VRAM fill") and these words come
        // from the bus, so the transfer has to leave the pending fill for the
        // next port write to find - which matters because the two modes are
        // the same CD5 command read with a different R23, so a game can ask
        // for one and then the other without a write in between.
        let fill_pending = self.dma_fill_pending;
        self.dma_fill_pending = false;
        for _ in 0..words {
            let value = bus.read_word(src);
            // TODO(M4-22 #101): the first word of a 68k-to-VRAM transfer is
            // often wrong on real hardware, because of the read FIFO, and the
            // transfer is not instant either. Neither is modelled here.
            self.write_data(value);
            src = next_source(src);
        }
        self.pending = pending;
        self.dma_fill_pending = fill_pending;

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
    use crate::ports::testutil::{copy_write_cmd, cram_write_cmd, vram_write_cmd, vsram_write_cmd};

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

    /// R23 bits 7-6 for a fill, and for a copy (genvdp.txt section 11).
    const FILL: u8 = 0b10 << 6;
    const COPY: u8 = 0b11 << 6;

    /// Set up a fill or a copy: `length` in bytes (R19 low, R20 high), the
    /// address register stepping by `step` (R15), R1 M1 set so the DMA is
    /// allowed, and R23 holding `mode` ( [`FILL`] or [`COPY`]).
    ///
    /// All of this has to happen before the command word, because a register
    /// write clears the code register ([`Vdp::write_control`]).
    fn dma_set_up(vdp: &mut Vdp, mode: u8, length: u16, step: u8) {
        vdp.write_register(19, length as u8);
        vdp.write_register(20, (length >> 8) as u8);
        vdp.write_register(15, step);
        vdp.write_register(1, 0x10);
        vdp.write_register(23, mode);
    }

    /// The source registers of a copy: R21 is bits 7-0 and R22 bits 15-8 of
    /// the VRAM byte address (genvdp.txt section 11 "VRAM copy"), which is
    /// byte addressing rather than the `S08-S01` of a 68k transfer.
    fn copy_source(vdp: &mut Vdp, src: u16) {
        vdp.write_register(21, src as u8);
        vdp.write_register(22, (src >> 8) as u8);
    }

    /// Write the command word and let the glue have its turn at the DMA.
    ///
    /// Neither a fill nor a copy reads the 68k bus, so the fake handed to
    /// `run_dma` is an empty one; the assertions on it are that it stayed that
    /// way.
    fn start_and_run(vdp: &mut Vdp, cmd: (u16, u16)) {
        start_dma(vdp, cmd);
        let mut bus = FakeBus::default();
        vdp.run_dma(&mut bus);
        assert!(
            bus.reads.is_empty(),
            "neither a fill nor a copy is on the 68k bus"
        );
    }

    /// A 68k transfer writes the memories, not the data port, so it does not
    /// run a fill that is already waiting for a port write: "Any write to the
    /// data port will then start a VRAM fill" (genvdp.txt section 11 "VRAM
    /// fill"). Genesis Plus GX agrees: its transfer path goes to `vdp_bus_w`
    /// and leaves its `dmafill` flag standing.
    #[test]
    fn a_transfer_does_not_run_a_fill_that_is_already_waiting() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, FILL, 4, 1);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));
        assert!(
            vdp.dma_fill_pending,
            "the fill is waiting for the data port"
        );

        // A 68k transfer set up over the top of it, with the fill never given
        // its write: two words to VRAM 0x3000, and the fill still waiting after
        // them.
        dma_from(&mut vdp, 0x000100, 2);
        start_dma(&mut vdp, vram_write_cmd(0x3000));
        let mut bus = FakeBus::laid_at(0x000100, &[0x1111, 0x2222]);
        vdp.run_dma(&mut bus);

        assert_eq!(
            &vdp.vram[0x3000..0x3004],
            [0x11, 0x11, 0x22, 0x22],
            "the transfer lands clean: a fill run part-way through it would leave its byte at 0x3003"
        );
        assert!(vdp.dma_fill_pending, "and the fill is still waiting");
    }

    #[test]
    fn a_fill_waits_for_the_data_port_write() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, FILL, 4, 1);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));

        assert!(
            vdp.vram[0x2000..0x2006].iter().all(|&byte| byte == 0),
            "the command word alone fills nothing: \"Any write to the data port will then start a VRAM fill\" (genvdp.txt section 11)"
        );
        assert!(
            vdp.dma_busy,
            "and the busy bit is set for the wait, which is why it is \"only useful for fills and copies\" (genvdp.txt section 6)"
        );
    }

    #[test]
    fn a_fill_repeats_the_high_byte_along_the_address_register() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, FILL, 4, 1);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));

        vdp.write_data(0xAB00);

        assert_eq!(
            &vdp.vram[0x2000..0x2006],
            [0xAB, 0x00, 0xAB, 0xAB, 0x00, 0xAB],
            "the word first (0xAB00 big-endian at 0x2000), then 0xAB at 0x2000, 0x2003, 0x2002 and 0x2005: four steps of `addr ^= 1` and `addr += 1` from the 0x2001 the data port write left behind. See the quote on `Vdp::run_fill`."
        );
        assert_eq!(
            vdp.addr, 0x2005,
            "the address register ends where the last step pointed"
        );
    }

    #[test]
    fn a_fill_with_a_step_of_two_writes_every_other_byte() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, FILL, 4, 2);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));

        vdp.write_data(0xAB00);

        assert_eq!(
            &vdp.vram[0x2000..0x200A],
            [0xAB, 0x00, 0x00, 0xAB, 0x00, 0xAB, 0x00, 0xAB, 0x00, 0xAB],
            "one byte per step, so a step of 2 leaves the even bytes of VRAM alone: the fill writes bytes, not words (genvdp.txt section 11 \"VRAM fill\")"
        );
    }

    #[test]
    fn a_fill_spends_its_length_and_clears_busy() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, FILL, 4, 1);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));
        assert_eq!((vdp.reg(19), vdp.reg(20)), (4, 0));

        vdp.write_data(0xAB00);

        assert_eq!(
            (vdp.reg(19), vdp.reg(20)),
            (0, 0),
            "the count is spent, as it is after a 68k transfer"
        );
        assert!(!vdp.dma_busy, "and the fill is over");
    }

    #[test]
    fn a_cram_fill_writes_the_whole_word_every_step() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, FILL, 4, 2);
        start_and_run(&mut vdp, cram_write_cmd(0x0000));

        vdp.write_data(0x1234);

        assert_eq!(
            &vdp.cram[0..5],
            [0x0224, 0x0224, 0x0224, 0x0224, 0x0224],
            "CRAM is word-wide, so a fill to it repeats the whole word - masked to ----bbb0 ggg0 rrr0 like any other CRAM write - rather than one byte of it (genvdp.txt section 11: CRAM fills are \"the same as normal ... data port access\")"
        );
    }

    #[test]
    fn a_copy_moves_bytes_to_the_address_register() {
        let mut vdp = Vdp::new(false);
        vdp.vram[0x0100..0x0104].copy_from_slice(&[1, 2, 3, 4]);
        dma_set_up(&mut vdp, COPY, 4, 1);
        copy_source(&mut vdp, 0x0100);

        start_and_run(&mut vdp, copy_write_cmd(0x0200));

        assert_eq!(
            &vdp.vram[0x0200..0x0204],
            [1, 2, 3, 4],
            "four bytes of 0x0100 at 0x0200, in order, with nothing else of VRAM touched"
        );
        assert!(vdp.vram[0x0204..0x0210].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn a_copy_with_a_step_of_two_spreads_bytes() {
        let mut vdp = Vdp::new(false);
        vdp.vram[0x0100..0x0104].copy_from_slice(&[1, 2, 3, 4]);
        dma_set_up(&mut vdp, COPY, 4, 2);
        copy_source(&mut vdp, 0x0100);

        start_and_run(&mut vdp, copy_write_cmd(0x0200));

        assert_eq!(
            &vdp.vram[0x0200..0x0208],
            [0x00, 2, 0x00, 1, 0x00, 4, 0x00, 3],
            "one byte per step, and the copy works a byte off the address it names on both sides (see `Vdp::run_copy`), so the pairs arrive swapped"
        );
    }

    #[test]
    fn a_copy_spends_its_length_and_clears_busy() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, COPY, 4, 1);
        copy_source(&mut vdp, 0x0100);
        assert_eq!((vdp.reg(19), vdp.reg(20)), (4, 0));

        start_and_run(&mut vdp, copy_write_cmd(0x0200));

        assert_eq!(
            (vdp.reg(19), vdp.reg(20)),
            (0, 0),
            "the count is spent by a copy as it is by a fill"
        );
        assert!(!vdp.dma_busy, "and the copy is over");
    }

    #[test]
    fn a_zero_length_is_all_of_them() {
        assert_eq!(bytes_to_move(0), 0x1_0000);
        assert_eq!(bytes_to_move(1), 1);
        assert_eq!(bytes_to_move(4), 4);
        assert_eq!(bytes_to_move(0xFFFF), 0xFFFF);
    }
}
