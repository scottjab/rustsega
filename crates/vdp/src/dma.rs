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
//! When they run, and how long they take, is M4-22 (#101) and is the second half
//! of this file: [`Vdp::run_dma`] and [`Vdp::run_fill`] *start* a transfer rather
//! than finishing one, [`Vdp::dma_clock`] spends the access slots of `slots` on
//! it as the beam goes past them, and [`Vdp::dma_68k_stall_mclk`] is how long the
//! 68k is frozen while one runs - the number the bus glue of M3-16 (#77) waits.
//!
//! The words of a transfer reach memory in FIFO order behind whatever the
//! port had already queued: Nemesis's `VDPFIFOTesting` sources have the DMA
//! add its source words to the same four-entry write FIFO the data port
//! feeds ("it will read a value from external memory using the DMA source
//! address register and add it to the FIFO using the current command code
//! and incremented command address registers", SpritesMind t=1291), so the
//! queued writes land first and the transfer's first source word then lands
//! at the transfer's own start address. Nothing is lost - that is what his
//! `TestFIFOFullDMATransfer` expected result reads back, and the two pinning
//! tests below carry it. `Vdp::start_running` is where the ordering is kept:
//! the queue drains before the first transfer write. Their slot cost is part
//! of the transfer's window ([`Running::Transfer`]'s `drain`): the DMA's
//! pushes need FIFO space, and "pending FIFO writes take priority over DMA
//! fills" (same post), so on hardware the queue's drain is serialised ahead
//! of the transfer, and Nemesis's own hardware note is that "when a DMA
//! transfer operation terminates, the FIFO is full of pending writes" - the
//! freeze ends at the last push, with words still in flight. This window is
//! landing-based like every other DMA window here, so it charges the drain
//! in full and stays a couple of slots long against that push reading by at
//! most the last four words: uniform in the queue, and fenced with the
//! status-bits-mid-transfer corner it belongs to (the #563 residue). The
//! fill waits the same way ("DMA fill operations will only ever run at an
//! access slot if the FIFO is empty"), its trigger write being one of the
//! entries it waits behind; the copy has no such wait - Nemesis advances it
//! with no FIFO condition at all. Not modelled either: the CRAM "dots" a
//! write during active display leaves behind (out of scope for #101). The
//! four bandwidth figures the slot costs below reproduce are the Sega
//! Genesis Software Manual's own "DMA transfer capacity" table now: #305
//! read it off the scan and every cell matched.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

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

/// Access slots a copied byte costs: one to read the source and one to write the
/// destination. It is why the Sega DMA bandwidth table's copy figures are half a
/// transfer's - 9 bytes on an active H40 line out of its 18 slots, and 102 of a
/// blank line's 205, rounding down as the manual's own "VRAM COPY" row does
/// (Sega Genesis Software Manual, "DMA transfer capacity": 9/102 H40, 8/83
/// H32).
const COPY_SLOTS: u8 = 2;

/// Access slots one unit of a DMA costs, a unit being a word of a 68k transfer
/// and a byte of a fill or a copy.
///
/// VRAM is byte wide, so its word is two writes and two slots (genvdp.txt
/// section 8); CRAM and VSRAM are word wide and cost one (sections 9 and 10),
/// which is the manual's own note: "number of words (not byte) should apply"
/// when CRAM or VSRAM is the destination of a MEMORY TO VRAM transfer (Sega
/// Genesis Software Manual, "DMA transfer capacity"). A step of a fill is
/// one byte, or one word of CRAM or VSRAM, and one slot; that a fill's line
/// holds one slot *less* than a transfer's is not this function's business, it is
/// [`Vdp::dma_slot_table`] that pays it. That a CRAM or VSRAM *fill* costs one
/// slot a word is analogy and not a citation: the manual's fill row is VRAM
/// FILL only, its words note covers MEMORY TO VRAM only, and #305 left it
/// assumed on that basis.
const fn slots_per_unit(mode: DmaMode, target: DataTarget) -> u8 {
    match mode {
        DmaMode::Mc68kTransfer => match target {
            DataTarget::Vram => 2,
            // A code register that names no memory still costs the slot, for the
            // reason `Vdp::fill_step` gives for the address still moving.
            DataTarget::Cram | DataTarget::Vsram | DataTarget::Ignored => 1,
        },
        DmaMode::Fill => 1,
        DmaMode::Copy => COPY_SLOTS,
    }
}

/// A DMA that has started and has not finished: what [`Vdp::dma_clock`] spends
/// access slots on, one unit at a time.
///
/// The three differ by what a unit of theirs is, what it costs
/// ([`slots_per_unit`]) and what it writes ([`Vdp::dma_move_unit`]).
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) enum Running {
    /// A 68k to VDP transfer.
    ///
    /// Its source words are read when the transfer starts, because
    /// [`Vdp::advance`] has no bus to read them with and the 68k is frozen for
    /// the whole of a transfer (genvdp.txt section 6: "the 68000 is frozen
    /// during 68k -> VDP transfers"), so reading them all here and reading them
    /// one a slot are the same reads in the same order. That is a promise
    /// [`DmaBus`] carries from #101 as well: a read may not do anything that the
    /// first of those reads would not have done.
    ///
    /// `words` is reversed, so that the next word to land is the one `pop`
    /// gives rather than a `remove(0)`.
    Transfer {
        /// Access slots the write FIFO still owes on the entries it held when the
        /// transfer started: a transfer feeds the same FIFO the port does, and
        /// "pending FIFO writes take priority" (Nemesis on SpritesMind t=1291),
        /// so none of its words can land until the queue is out. Spent before
        /// `slots`, and counted in [`Running::slots`] with them.
        drain: u32,
        /// Access slots still owed before the transfer is in memory.
        slots: u32,
        /// Slots each word costs, from [`slots_per_unit`].
        per_unit: u8,
        /// The source address the transfer ends at, which is what R21/R22 come
        /// back as when it does.
        ends_at: u32,
        /// The source words, the next to land last.
        words: Vec<u16>,
    },
    /// A fill: `value` repeated along the address register, one slot a step. The
    /// byte, or word, that it repeats is the data port write that started it
    /// ([`Vdp::run_fill`]). Its steps run "only ... at an access slot if the FIFO
    /// is empty" (Nemesis on SpritesMind t=1291), so a fill armed behind queued
    /// writes waits out their drain first, which `drain` counts.
    Fill { drain: u32, slots: u32, value: u16 },
    /// A copy: `source` walked a byte at a time to the address register.
    Copy { slots: u32, source: u16 },
}

impl Running {
    /// The fill, and only the fill, pays one slot of every line to the machine:
    /// see [`Vdp::dma_slot_table`].
    fn is_fill(&self) -> bool {
        matches!(self, Self::Fill { .. })
    }

    /// Access slots still owed before the last unit reaches memory: the queue's
    /// drain and the DMA's own slots together.
    fn slots(&self) -> u32 {
        match self {
            Self::Transfer { drain, slots, .. } | Self::Fill { drain, slots, .. } => drain + slots,
            Self::Copy { slots, .. } => *slots,
        }
    }

    /// Whether the next slot is still one of the queue's drain: [`Vdp::dma_take_slot`]
    /// spends these without moving a unit, because nothing of the DMA is on the
    /// memory yet.
    fn drain_pending(&self) -> bool {
        match self {
            Self::Transfer { drain, .. } | Self::Fill { drain, .. } => *drain > 0,
            Self::Copy { .. } => false,
        }
    }

    /// One access slot spent on the queue's drain. Only reached while
    /// [`Running::drain_pending`] says there is one.
    fn spend_drain(&mut self) {
        match self {
            Self::Transfer { drain, .. } | Self::Fill { drain, .. } => *drain -= 1,
            Self::Copy { .. } => {}
        }
    }

    /// One access slot spent: the transfer is a slot closer to memory.
    fn spend_slot(&mut self) {
        match self {
            Self::Transfer { slots, .. } | Self::Fill { slots, .. } | Self::Copy { slots, .. } => {
                *slots -= 1;
            }
        }
    }

    /// Whether the unit at the head of the DMA lands on this slot rather than on
    /// a later one: it lands on the last of the slots it costs, because a VRAM
    /// word is not in memory after one byte-wide slot and a copied byte is not
    /// copied before the slot that writes it.
    fn unit_is_due(&self) -> bool {
        match self {
            Self::Transfer {
                slots, per_unit, ..
            } => *slots % u32::from(*per_unit) == 0,
            Self::Fill { .. } => true,
            Self::Copy { slots, .. } => *slots % u32::from(COPY_SLOTS) == 0,
        }
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
    /// R23 says it was: the 68k transfer and the copy start here, and the fill is
    /// armed for [`Vdp::run_fill`] to start at the next data port write.
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
    ///
    /// Starting one is not finishing one: the transfer is recorded ([`Running`])
    /// and its access slots pay for it afterwards, in [`Vdp::dma_clock`]. Only
    /// the 68k words are read here, because here is the one place a DMA has a
    /// bus - see [`Running::Transfer`] for why that has to be the case.
    pub fn run_dma(&mut self, bus: &mut impl DmaBus) {
        if !self.dma_requested {
            return;
        }
        self.dma_requested = false;
        match self.dma_mode() {
            // A fill does not start here: it waits for the data port write that
            // starts it (`Vdp::run_fill`). Being armed is as far as the command
            // word gets, and it is the R1 M1 bit that allows it, the same bit
            // `transfer_allowed` reads for the 68k transfer.
            DmaMode::Fill if self.dma_enabled() => self.arm_fill(),
            DmaMode::Copy if self.dma_enabled() => self.start_copy(),
            _ if transfer_allowed(self.dma_enabled(), self.dma_mode()) => self.start_transfer(bus),
            _ => {}
        }
    }

    /// Take over the DMA in flight with `running`.
    ///
    /// Whatever the port has queued reaches memory first, and its slots are
    /// part of the DMA's window: the DMA feeds the same write FIFO the data
    /// port does ("it will read a value from external memory using the DMA
    /// source address register and add it to the FIFO using the current command
    /// code and incremented command address registers", Nemesis on SpritesMind
    /// t=1291), so a transfer started non-empty writes the queue out first and
    /// its own first source word then lands at the transfer's own start
    /// address, nothing lost - his `TestFIFOFullDMATransfer` reads back exactly
    /// that, and the pinning tests in this file carry it. Draining here,
    /// before the first transfer write, is the order; the drain's slots are
    /// what [`Running::Transfer`]'s `drain` charges, read off the queue before
    /// this empties it. And a DMA started while one is running replaces it - a
    /// corner nothing documents, because hardware has the 68k frozen for a
    /// transfer and tells it to keep off the data port for the other two
    /// (status bit 1), so nobody is supposed to get here at all.
    fn start_running(&mut self, running: Running) {
        self.fifo_drain();
        self.dma = Some(running);
        // "Bit 1 is set for the duration of a DMA operation" (genvdp.txt
        // section 6), and the duration ends at [`Vdp::finish_dma`].
        self.dma_busy = true;
    }

    /// Start the 68k to VDP transfer: [`Vdp::dma_source`] to the address
    /// register, [`words_to_transfer`] of them, [`slots_per_unit`] slots a word.
    ///
    /// The words are all read here, for the reason [`Running::Transfer`] gives,
    /// and land a unit at a time in [`Vdp::dma_move_unit`]. The destination
    /// memory, its masking and the move of the address register stay the data
    /// port rules of M4-02 (#80), applied one word per slot by
    /// [`Vdp::write_data_now`].
    ///
    /// The control port latch and a fill waiting for the port are left alone: a
    /// transfer goes to the memories without either (genvdp.txt section 7 "VDP
    /// port access"), so a command word that was half written when the DMA
    /// started is still half written while it runs.
    fn start_transfer(&mut self, bus: &mut impl DmaBus) {
        let words = words_to_transfer(self.dma_length());
        let per_unit = slots_per_unit(DmaMode::Mc68kTransfer, data_target(self.code));
        // The queue's drain goes on the transfer's clock before its own slots:
        // see [`Running::Transfer`]'s `drain`. Read before `start_running`
        // empties the queue.
        let drain = self.fifo.slots_owed();

        let mut source = self.dma_source();
        let mut staged = Vec::with_capacity(words as usize);
        for _ in 0..words {
            staged.push(bus.read_word(source));
            source = next_source(source);
        }
        // Reversed, so that the word that lands next is the one `pop` gives
        // (`Running::Transfer`).
        staged.reverse();

        self.start_running(Running::Transfer {
            drain,
            slots: words * u32::from(per_unit),
            per_unit,
            ends_at: source,
            words: staged,
        });
    }

    /// Start the VRAM to VRAM copy: [`Vdp::dma_copy_source`] to the address
    /// register, [`bytes_to_move`] of them, [`COPY_SLOTS`] slots a byte.
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
    /// the order of the writes differs; with a step of 2 they do not, and a test
    /// pins this one.
    ///
    /// Starts from [`Vdp::run_dma`], straight away, because "In the case of DMA
    /// transfer or DMA copy, the operation will start immediately after writing
    /// this value" (plutiedev "Start DMA") - unlike the fill, which waits for
    /// the data port.
    ///
    /// Only R19/R20 are written back, and only when the copy is over. Whether the
    /// source registers move during a DMA is one of genvdp.txt's own open
    /// questions - "Source and length registers being updated during DMA" is on
    /// its to-do list - and Genesis Plus GX adds the whole length to R21/R22 for
    /// every mode without convincing anybody. A game that chained copies without
    /// reloading them would want that; if one turns up, that is the follow-up.
    fn start_copy(&mut self) {
        let bytes = bytes_to_move(self.dma_length());
        self.start_running(Running::Copy {
            slots: bytes * u32::from(COPY_SLOTS),
            source: self.dma_copy_source(),
        });
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
    /// Like the other two modes this *starts* the fill rather than finishing it:
    /// [`bytes_to_move`] steps of one access slot each go onto [`Running`] and
    /// [`Vdp::dma_move_unit`] does them as the slots come round.
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

        // One access slot a step, [`Vdp::fill_step`] being the step; the busy bit
        // that armed the fill came from the command word and stays up until the
        // last of those slots has been spent (`Vdp::finish_dma`).
        let steps = bytes_to_move(self.dma_length());
        // The trigger write itself is on the queue it is about to wait behind:
        // its entry plus whatever the port had still standing are the FIFO the
        // fill's steps only run once it is empty (see [`Running::Fill`]). Read
        // before `start_running` empties it.
        let drain = self.fifo.slots_owed();
        self.start_running(Running::Fill {
            drain,
            slots: steps,
            value,
        });
    }

    /// One step of a fill: the repeated byte, or word, one address further on.
    ///
    /// `addr ^ 1` is the byte on the other side of the pair the address sits in,
    /// and the address register then moves on by R15: the two lines of the
    /// genvdp.txt pseudocode [`Vdp::run_fill`] quotes.
    ///
    /// CRAM and VSRAM take the whole word every step, for the reasons that
    /// function gives. A code register that names no memory still costs the step:
    /// the address moves on whatever the fill wrote (Genesis Plus GX's default
    /// case: "invalid destination does nothing ... address is still
    /// incremented").
    fn fill_step(&mut self, value: u16) {
        match data_target(self.code) {
            DataTarget::Vram => {
                let byte = (value >> 8) as u8;
                self.write_vram_byte(usize::from(self.addr ^ 1), byte);
                self.addr = increment(self.addr, self.auto_increment());
            }
            DataTarget::Cram | DataTarget::Vsram => self.write_target_word(value),
            DataTarget::Ignored => {
                self.addr = increment(self.addr, self.auto_increment());
            }
        }
    }

    /// The DMA's share of the beam moving on: every access slot the line gives a
    /// DMA between `from` and `to` moves one unit of the transfer in flight.
    ///
    /// Called by [`Vdp::advance`] for each segment of the line it walks, so the
    /// slots are counted against the line the beam is standing on rather than the
    /// one the call started in: a transfer that outlives VBlank goes from 205
    /// slots a line to 18 part way through, which is what hardware does.
    pub(crate) fn dma_clock(&mut self, from: u32, to: u32) {
        let Some(running) = self.dma.as_ref() else {
            return;
        };
        let fill = running.is_fill();
        let slots = self.dma_slots_in(fill, from, to);
        for _ in 0..slots {
            if !self.dma_take_slot() {
                break;
            }
        }
    }

    /// One access slot of the DMA in flight: the queue's drain first, and none
    /// of the DMA's own units move while it is being spent; then the unit at the
    /// head reaches memory if this was the last of the slots it costs, and the
    /// transfer is over when its slots run out. Whether another DMA still wants
    /// a slot is the answer.
    fn dma_take_slot(&mut self) -> bool {
        // Taken out rather than borrowed, so that writing memory and moving the
        // running state on are not both borrowing `self`.
        let Some(mut running) = self.dma.take() else {
            return false;
        };
        if running.drain_pending() {
            running.spend_drain();
            if running.slots() == 0 {
                self.finish_dma(&running);
                return false;
            }
            self.dma = Some(running);
            return true;
        }
        running.spend_slot();
        if running.unit_is_due() {
            self.dma_move_unit(&mut running);
        }
        if running.slots() == 0 {
            self.finish_dma(&running);
            return false;
        }
        self.dma = Some(running);
        true
    }

    /// One unit of `running` reaches memory: a word of a 68k transfer through the
    /// port rules of M4-02 (#80), one step of a fill, or one byte of a copy, from
    /// `source ^ 1` to `addr ^ 1`.
    fn dma_move_unit(&mut self, running: &mut Running) {
        match running {
            Running::Transfer { words, .. } => {
                if let Some(word) = words.pop() {
                    self.write_data_now(word);
                }
            }
            Running::Fill { value, .. } => self.fill_step(*value),
            Running::Copy { source, .. } => {
                let byte = self.vram[usize::from(*source ^ 1)];
                self.write_vram_byte(usize::from(self.addr ^ 1), byte);
                *source = source.wrapping_add(1);
                self.addr = increment(self.addr, self.auto_increment());
            }
        }
    }

    /// The last slot of `running` has been spent, so the length is spent with it
    /// and a transfer's source registers say where it got to.
    ///
    /// "After either: length regs 0" is how the ticket puts the write-back of
    /// R19/R20, which is what a game waiting for a DMA reads, and the source
    /// moving on is genvdp.txt section 11 "DMA" and plutiedev "$93xx" to
    /// "$96xx". Both happen on the last access slot rather than on the command
    /// word, which is the whole point of a DMA that takes time.
    fn finish_dma(&mut self, running: &Running) {
        self.dma_busy = false;
        self.regs[regs::R19] = 0;
        self.regs[regs::R20] = 0;
        if let Running::Transfer { ends_at, .. } = running {
            let (source_low, source_high) = source_fields(*ends_at);
            self.regs[regs::R21] = source_low;
            self.regs[regs::R22] = source_high;
        }
    }

    /// Master clocks the 68k is frozen for by the transfer in flight, `0` when
    /// nothing is holding the bus: the time left until the last of the
    /// transfer's access slots, which includes the slots the write FIFO still
    /// owed on the entries it held when the transfer started
    /// ([`Running::Transfer`]'s `drain`).
    ///
    /// "The 68000 is frozen during 68k -> VDP transfers" (genvdp.txt section 6),
    /// and this is for how long: the bus glue of M3-16 (#77) adds it to the cycle
    /// count of the command word that started the DMA. It is the same shape as
    /// [`Vdp::data_port_stall_mclk`], on the other side of the same chip.
    ///
    /// A fill and a copy answer 0. They keep the 68k off the *data port* for
    /// their duration, which is what status bit 1 is for, and leave it running:
    /// the busy bit is "only useful for fills and copies, since the 68000 is
    /// frozen during 68k -> VDP transfers" (genvdp.txt section 6).
    ///
    /// The time is counted along the DMA's own slot schedule one line at a time
    /// (`slots::Vdp::dma_slot_table`), so a transfer that started mid-active-line
    /// ends where 18 slots a line put it rather than where a flat rate would.
    pub fn dma_68k_stall_mclk(&self) -> u32 {
        match &self.dma {
            Some(running @ Running::Transfer { .. }) => {
                self.dma_mclk_to_slots(running.is_fill(), running.slots())
            }
            Some(Running::Fill { .. } | Running::Copy { .. }) | None => 0,
        }
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
        run_dma_to_end(&mut vdp);

        assert_eq!(
            &vdp.vram[0x1000..0x1008],
            [0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04],
            "the words of 0x000100 in VRAM order, at the address of the command"
        );
        assert_eq!(bus.reads, [0x000100, 0x000102, 0x000104, 0x000106]);
    }

    /// The beam on the active display and DMA allowed at once: the one state
    /// where a data port write is still on the queue when the next 68k
    /// instruction runs, which is the state Nemesis's full-FIFO transfer test
    /// sets up (his "Wait for the FIFO to be empty" / "Make sure the FIFO is
    /// full" loop around `TestFIFOFullDMATransfer`).
    fn active_display_dma(vdp: &mut Vdp) {
        vdp.write_register(1, 0x50);
        vdp.advance(200);
        assert!(
            !vdp.in_hblank && !vdp.in_vblank,
            "the beam is not on the active display"
        );
    }

    /// Nemesis's "DMA Transfer to VRAM inc=2" (`TestDMATransferToVRAM.asm`):
    /// the FIFO empty when the command word lands, and his read-back of the
    /// region is the word in front of the destination unchanged, then the
    /// seven source words in order. The first word that lands in the
    /// destination is the first source word, at the transfer's own start
    /// address; the "stale word first, first source word lost" reading that
    /// #101 deferred on is not what his sources measure.
    #[test]
    fn an_empty_fifo_starts_the_transfer_at_its_first_source_word() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000200, 7); // R15 = 2, the step every set-up here uses
        // The region around the destination, laid by port writes with the
        // display off - so every write lands the moment it is made and the
        // queue drains at once (`Fifo`, "a write with the display off drains
        // at once"). Distinct values, so "unchanged" asserts something.
        let (hi, lo) = vram_write_cmd(0x0FFE);
        vdp.write_control(hi);
        vdp.write_control(lo);
        for word in 0..16u16 {
            vdp.write_data(0x0200 + word);
        }
        assert!(vdp.fifo_empty(), "the display is off, so nothing queues");

        start_dma(&mut vdp, vram_write_cmd(0x1000));
        let mut bus = FakeBus::laid_at(
            0x000200,
            &[0x1122, 0x3344, 0x5566, 0x7788, 0x99AA, 0xBBCC, 0xDDEE],
        );
        vdp.run_dma(&mut bus);
        run_dma_to_end(&mut vdp);

        assert_eq!(
            &vdp.vram[0x0FFE..0x1010],
            [
                0x02, 0x00, // the word in front of the destination: prior content
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
                0x02, 0x08, // the word behind the transfer: prior content
            ],
            "his expected result: the source words from the start address, nothing stale ahead"
        );
    }

    /// Nemesis's "FIFO Full Before DMA Transfer" (`TestFIFOFullDMATransfer.asm`):
    /// four queued port writes for 0x8000..0x8006 when the command word lands,
    /// the transfer's destination 0x8002 one word in. His expected result reads
    /// back $1234 - the first queued word, at its own address - and then the
    /// seven source words from 0x8002 on: the queue lands before the transfer,
    /// in FIFO order, and no source word is lost. The transfer does not
    /// overtake the writes made before its command word (`Vdp::start_running`).
    #[test]
    fn a_transfer_behind_a_full_fifo_writes_the_queue_first_and_loses_nothing() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000300, 7);
        active_display_dma(&mut vdp);

        // Nemesis's prior content: 16 zeros at 0x8000, drained before the
        // interesting writes, like his "Wait for the FIFO to be empty" loop.
        let (hi, lo) = vram_write_cmd(0x8000);
        vdp.write_control(hi);
        vdp.write_control(lo);
        for _ in 0..16 {
            vdp.write_data(0x0000);
        }
        for _ in 0..6 {
            if vdp.fifo_empty() {
                break;
            }
            vdp.advance(MCLKS_PER_LINE);
        }
        assert!(
            vdp.fifo_empty(),
            "the prior content drained like his loop waits for"
        );

        // The four stale writes, all four still queued when the command word
        // comes: what his `btst #0` gate checks before starting the transfer.
        vdp.write_control(hi);
        vdp.write_control(lo);
        for word in [0x1234u16, 0x5678, 0x9ABC, 0xDEF0] {
            vdp.write_data(word);
        }
        assert!(
            vdp.fifo_full(),
            "the four writes queue behind the active display's slots"
        );

        start_dma(&mut vdp, vram_write_cmd(0x8002));
        let mut bus = FakeBus::laid_at(
            0x000300,
            &[0x1122, 0x3344, 0x5566, 0x7788, 0x99AA, 0xBBCC, 0xDDEE],
        );
        vdp.run_dma(&mut bus);
        run_dma_to_end(&mut vdp);

        assert_eq!(
            &vdp.vram[0x8000..0x8010],
            [
                0x12, 0x34, // the queued word, at its own address
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
            ],
            "his expected result: the queue first, then the transfer's own words from 0x8002"
        );
        assert!(
            vdp.vram[0x8010..0x8020].iter().all(|&byte| byte == 0),
            "his prior zeros behind the transfer, untouched by any of it"
        );
    }

    /// And the queue is not dropped either: Nemesis fills his FIFO from
    /// `TestDMATransferUsingFIFO.asm` ("This test demonstrates that a DMA
    /// transfer operation moves its data through the FIFO") and reads the
    /// queued words back out. Here two queued writes for another region
    /// survive a transfer started behind them, and land at their own
    /// addresses, which is what his result block shows for the words it reads.
    #[test]
    fn a_transfer_started_behind_queued_writes_drops_neither_side() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000400, 3);
        active_display_dma(&mut vdp);

        // Two writes queued for CRAM, a region the transfer does not touch.
        let (hi, lo) = cram_write_cmd(0x0004);
        vdp.write_control(hi);
        vdp.write_control(lo);
        vdp.write_data(0x0CAFE);
        vdp.write_data(0x0BEEF);
        assert!(
            !vdp.fifo_empty(),
            "the two writes queue behind the active display"
        );

        start_dma(&mut vdp, vram_write_cmd(0x2000));
        let mut bus = FakeBus::laid_at(0x000400, &[0x1111, 0x2222, 0x3333]);
        vdp.run_dma(&mut bus);
        run_dma_to_end(&mut vdp);

        assert_eq!(
            &vdp.cram[2..4],
            [0x0AEE, 0x0EEE],
            "the queued CRAM words landed at their own addresses (----bbb0 ggg0 rrr0)"
        );
        assert_eq!(
            &vdp.vram[0x2000..0x2006],
            [0x11, 0x11, 0x22, 0x22, 0x33, 0x33],
            "and the transfer's words landed at theirs"
        );
    }

    #[test]
    fn a_dma_to_cram_is_masked_like_a_data_port_write() {
        let mut vdp = Vdp::new(false);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, cram_write_cmd(0x0000));

        let mut bus = FakeBus::laid_at(0x000100, &[0x0EEE, 0xFFFF, 0x1234, 0x000F]);
        vdp.run_dma(&mut bus);
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
    /// allowed, and R23 holding `mode` ([`FILL`] or [`COPY`]).
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

    /// Give the beam the clocks the DMA it has just started is made of, so that a
    /// test can look at the memory it finished in.
    ///
    /// Since M4-22 (#101) a DMA runs one access slot at a time rather than inside
    /// the call that started it, so the instant-DMA tests of #85 and #86 have to
    /// advance. These are all a handful of words: a line of slots with the
    /// display off, and four of them is more than enough of a margin.
    fn run_dma_to_end(vdp: &mut Vdp) {
        for _ in 0..4 {
            if vdp.dma.is_none() {
                return;
            }
            vdp.advance(MCLKS_PER_LINE);
        }
        assert!(
            vdp.dma.is_none(),
            "these tests move a handful of words, not four lines of slots"
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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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
        run_dma_to_end(&mut vdp);

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

    // ---- timing, busy and the 68k stall (M4-22, #101) ----

    use crate::hv::MCLKS_PER_LINE;

    /// The beam at the head of an active H40 line, display on and DMA allowed,
    /// with a `words`-word 68k-to-VRAM transfer armed and the command word
    /// written: the state a mid-frame transfer is started in, which is the one
    /// the DMA bandwidth table measures against the 18 slots an active line
    /// leaves free.
    ///
    /// The bus is `laid_at` the source with 0x1000, 0x1001, ... so a word of the
    /// transfer can be recognised in VRAM by its own value.
    fn transfer_on_the_active_h40_display(words: u16) -> (Vdp, FakeBus) {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x81);
        dma_from(&mut vdp, 0x000100, words);
        // R1 again, with bit 6 (DISP) up alongside the M1 `dma_from` set: with
        // the display off the transfer would be timed against a blank line.
        vdp.write_register(1, 0x50);
        vdp.in_hblank = false;
        vdp.in_vblank = false;
        start_dma(&mut vdp, vram_write_cmd(0x2000));

        let source: Vec<u16> = (0..words).map(|index| 0x1000 + index).collect();
        let bus = FakeBus::laid_at(0x000100, &source);
        (vdp, bus)
    }

    /// The beam at the head of a line in VBlank in H40, display on: the 205
    /// slots of a blank line, which is what a VBlank transfer is timed against.
    fn h40_in_vblank() -> Vdp {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x81);
        vdp.in_vblank = true;
        vdp
    }

    /// A 68k-to-VRAM transfer is 2 access slots a word over the 18 an active H40
    /// line leaves free, so 100 words are 200 slots and eighteen of them a line
    /// is [`ceil(200 / 18)`] = twelve lines: the ticket's first timing test, and
    /// the reason `run_dma` no longer finishes anything.
    #[test]
    fn a_hundred_word_transfer_over_the_active_display_is_twelve_lines() {
        let (mut vdp, mut bus) = transfer_on_the_active_h40_display(100);
        vdp.run_dma(&mut bus);

        assert_eq!(
            bus.reads.len(),
            100,
            "the source is read once, at the moment the transfer starts"
        );
        assert!(
            vdp.vram[0x2000..0x20C8].iter().all(|&byte| byte == 0),
            "and nothing has been written yet: the words are on the DMA, waiting for slots"
        );
        assert!(
            vdp.dma_68k_stall_mclk() > 0,
            "and the 68k is frozen for the whole of it"
        );

        // Five lines is 90 of the slots, which is 45 of the words: the transfer
        // is slow, and "some games depend on the DMA being slow" (genvdp.txt
        // section 11) is about VRAM being half-written while it runs.
        for _ in 0..5 {
            vdp.advance(MCLKS_PER_LINE);
        }
        assert_eq!(
            &vdp.vram[0x2058..0x205A],
            [0x10, 0x2C],
            "the 45th word, 0x102C, is in"
        );
        assert_eq!(
            &vdp.vram[0x205A..0x205C],
            [0, 0],
            "and the 46th is not: it is the next two slots that put it there"
        );

        let mut lines = 5;
        while vdp.dma_68k_stall_mclk() != 0 {
            vdp.advance(MCLKS_PER_LINE);
            lines += 1;
            assert!(lines < 40, "twelve was expected, not {}", lines);
        }

        assert_eq!(
            lines, 12,
            "ceil(200 / 18): the twelfth line counting the one it started on"
        );
        assert!(!vdp.dma_busy, "and the busy bit comes down with it");
        assert_eq!(
            &vdp.vram[0x20C6..0x20C8],
            [0x10, 0x63],
            "the last of the hundred words, at the end of 200 slots"
        );
        assert_eq!(
            (vdp.reg(19), vdp.reg(20)),
            (0, 0),
            "the length is spent when the last slot is, not when the command word is"
        );
    }

    /// The same hundred words in VBlank: a blank H40 line offers 205 of the 200
    /// slots the transfer owes, so it is over inside the line it started on.
    #[test]
    fn a_hundred_word_transfer_in_vblank_fits_inside_one_line() {
        let mut vdp = h40_in_vblank();
        dma_from(&mut vdp, 0x000100, 100);
        vdp.write_register(1, 0x50);
        start_dma(&mut vdp, vram_write_cmd(0x2000));

        let source: Vec<u16> = (0..100).map(|index| 0x1000 + index).collect();
        let mut bus = FakeBus::laid_at(0x000100, &source);
        vdp.run_dma(&mut bus);

        let stall = vdp.dma_68k_stall_mclk();
        assert!(
            stall > 0 && stall < MCLKS_PER_LINE,
            "one line of blanking, not more: {stall} of {MCLKS_PER_LINE}"
        );
        vdp.advance(stall - 1);
        assert!(
            vdp.dma_busy,
            "one clock short of the last slot of the last word"
        );
        vdp.advance(1);
        assert!(!vdp.dma_busy, "and on that clock the transfer is done");
        assert_eq!(vdp.line, 0, "inside the line it started on");
        assert_eq!(
            &vdp.vram[0x20C6..0x20C8],
            [0x10, 0x63],
            "every word of it in VRAM"
        );
    }

    /// The freeze a 68k -> VDP transfer puts on the 68000 is the instant the last
    /// of its access slots arrives, so the transfer cannot still be running when
    /// the freeze is paid - and a transfer that outlives VBlank runs the rest of
    /// its slots at the 18 an active line leaves free, not the 205 of the blank
    /// one it started on (#536).
    ///
    /// The 200 words here are 400 slots. Line 260 - the last one `in_vblank`
    /// covers - gives 205 of them, so 195 are owed over the active display at 18
    /// a line: about twelve lines in all. Charged the blank line's rate for every
    /// one of them, the freeze came out near two, the 68000 woke with the
    /// transfer in mid air, and its next control write (a register write clears
    /// the code register, genvdp.txt section 7) silently abandoned what was left.
    #[test]
    fn a_transfer_that_outlives_vblank_is_frozen_for_the_whole_of_it() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x81);
        dma_from(&mut vdp, 0x000100, 200);
        // R1 bit 6 both enables the DMA and is the display, so from line 261 the
        // transfer is timed against a line that is being drawn.
        vdp.write_register(1, 0x50);
        vdp.in_vblank = true;
        vdp.set_position(260, 0);
        start_dma(&mut vdp, vram_write_cmd(0x2000));

        let source: Vec<u16> = (0..200).map(|index| 0x1000 + index).collect();
        let mut bus = FakeBus::laid_at(0x000100, &source);
        vdp.run_dma(&mut bus);

        let stall = vdp.dma_68k_stall_mclk();
        assert!(
            stall > 5 * MCLKS_PER_LINE,
            "195 slots over the active display is not a couple of lines: {stall}"
        );

        vdp.advance(stall - 1);
        assert!(
            vdp.dma_busy,
            "one clock short of the last slot of the last word"
        );
        vdp.advance(1);
        assert!(
            !vdp.dma_busy,
            "the freeze is the transfer's last slot, so it is over with the freeze"
        );
        assert_eq!(
            &vdp.vram[0x218E..0x2190],
            [0x10, 0xC7],
            "and the 200th word is in VRAM, not abandoned with the freeze"
        );
    }

    /// Status bit 1 is "set for the duration of a DMA operation ... only useful
    /// for fills and copies" (genvdp.txt section 6), and a duration is what a
    /// slot-timed DMA has: 300 bytes of fill is two lines of a blank H32 line's
    /// 166, so the bit is up between them and down after.
    #[test]
    fn the_fill_is_busy_for_as_long_as_its_slots_take() {
        let mut vdp = Vdp::new(false);
        dma_set_up(&mut vdp, FILL, 300, 1);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));
        assert!(vdp.dma_busy, "armed by the command word, as before");

        vdp.write_data(0xAB00);
        assert!(vdp.dma_busy, "the fill has started, not finished");

        vdp.advance(MCLKS_PER_LINE);
        assert!(
            vdp.dma_busy,
            "166 slots of a blank H32 line is not 300 bytes of fill"
        );
        vdp.advance(MCLKS_PER_LINE);
        assert!(!vdp.dma_busy, "and two lines of them is");
        assert_eq!(
            (vdp.reg(19), vdp.reg(20)),
            (0, 0),
            "the count is spent on the last slot, not on the write that started it"
        );
        assert_eq!(
            vdp.read_status() & (1 << 1),
            0,
            "and status bit 1 follows it down"
        );
    }

    /// "The 68000 is frozen during 68k -> VDP transfers" (genvdp.txt section 6),
    /// which is what [`Vdp::dma_68k_stall_mclk`] reports and which a fill and a
    /// copy do not do: they hold the data port through the busy bit and nobody
    /// else at all.
    #[test]
    fn only_a_transfer_holds_the_68k() {
        let (mut vdp, mut bus) = transfer_on_the_active_h40_display(4);
        vdp.run_dma(&mut bus);
        assert!(
            vdp.dma_68k_stall_mclk() > 0,
            "right after the command word the bus is taken"
        );

        let mut fill = Vdp::new(false);
        dma_set_up(&mut fill, FILL, 4, 1);
        start_and_run(&mut fill, vram_write_cmd(0x2000));
        fill.write_data(0xAB00);
        assert!(fill.dma_busy, "the fill is running,");
        assert_eq!(
            fill.dma_68k_stall_mclk(),
            0,
            "but it is not the 68k's to wait for"
        );

        let mut copy = Vdp::new(false);
        dma_set_up(&mut copy, COPY, 4, 1);
        copy_source(&mut copy, 0x0100);
        start_and_run(&mut copy, copy_write_cmd(0x0200));
        assert!(copy.dma_busy, "and a copy runs, busy,");
        assert_eq!(
            copy.dma_68k_stall_mclk(),
            0,
            "and holds the 68k for nothing"
        );
    }

    /// A copy moves one byte per *two* slots - one to read the source and one to
    /// write the destination - so the Sega table's 102 bytes are the 204 of the
    /// 205 a blank H40 line offers, and fit inside it exactly.
    #[test]
    fn a_copy_of_102_bytes_in_h40_vblank_is_one_line() {
        let mut vdp = h40_in_vblank();
        for (offset, byte) in vdp.vram[0x0100..0x0166].iter_mut().enumerate() {
            *byte = offset as u8 + 1;
        }
        dma_set_up(&mut vdp, COPY, 102, 1);
        copy_source(&mut vdp, 0x0100);
        start_and_run(&mut vdp, copy_write_cmd(0x0200));
        assert!(vdp.dma_busy, "the copy is running");

        vdp.advance(MCLKS_PER_LINE);

        assert!(
            !vdp.dma_busy,
            "102 bytes x 2 slots is 204 of a blank H40 line's 205"
        );
        assert_eq!(vdp.vram[0x0200], 1, "the first byte across");
        assert_eq!(
            vdp.vram[0x0200 + 101],
            102,
            "and the last, all of it inside the one line"
        );
    }

    /// The other half of the same measurement: one byte more is 206 slots, which
    /// does not fit a blank H40 line, so the copy runs into the next one.
    #[test]
    fn a_copy_of_103_bytes_does_not_fit_in_that_line() {
        let mut vdp = h40_in_vblank();
        dma_set_up(&mut vdp, COPY, 103, 1);
        copy_source(&mut vdp, 0x0100);
        start_and_run(&mut vdp, copy_write_cmd(0x0200));

        vdp.advance(MCLKS_PER_LINE);
        assert!(vdp.dma_busy, "206 slots is two lines of 205");
        vdp.advance(MCLKS_PER_LINE);
        assert!(!vdp.dma_busy, "and two lines of them is enough");
    }

    /// The manual's fill row as a line budget: "VRAM FILL ... H40CELL ... DURING
    /// V BLANK 204 Bytes" (Sega Genesis Software Manual, "DMA transfer
    /// capacity", read off the scan on #305). A fill pays one slot of every
    /// blank H40 line to the machine, which is what puts it one byte short of
    /// the transfer's 205, so 204 bytes of fill are 204 steps.
    ///
    /// The steps do not start on the write that triggered the fill: the write
    /// is itself a FIFO entry, and the fill only runs "at an access slot if the
    /// FIFO is empty" (Nemesis on SpritesMind t=1291) - its two slots pass
    /// first, and the 204 steps end two slots into the next line.
    #[test]
    fn a_fill_of_204_bytes_in_h40_vblank_is_one_line() {
        let mut vdp = h40_in_vblank();
        dma_set_up(&mut vdp, FILL, 204, 1);
        vdp.write_register(1, 0x50);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));
        vdp.write_data(0xAB00);
        assert!(vdp.dma_busy, "the fill is running");

        vdp.advance(MCLKS_PER_LINE);

        assert!(
            vdp.dma_busy,
            "204 steps began after the trigger write's two drain slots, \
             so two of them are still owed"
        );
        // The two steps land on the next line's fill slots, wherever on that
        // line's grid they sit: any slot of the line is more than enough.
        vdp.advance(MCLKS_PER_LINE);
        assert!(!vdp.dma_busy, "and with those the line budget is spent");
        assert_eq!(
            (vdp.reg(19), vdp.reg(20)),
            (0, 0),
            "the count is spent when the last slot is"
        );
    }

    /// The other half of the same row: one byte more is 205 slots against 204,
    /// so the fill runs into the next line.
    #[test]
    fn a_fill_of_205_bytes_does_not_fit_in_that_line() {
        let mut vdp = h40_in_vblank();
        dma_set_up(&mut vdp, FILL, 205, 1);
        vdp.write_register(1, 0x50);
        start_and_run(&mut vdp, vram_write_cmd(0x2000));
        vdp.write_data(0xAB00);
        assert!(vdp.dma_busy, "the fill is running");

        vdp.advance(MCLKS_PER_LINE);
        assert!(vdp.dma_busy, "205 slots against 204: one byte short");
        vdp.advance(MCLKS_PER_LINE);
        assert!(!vdp.dma_busy, "and the second line finishes it");
    }

    /// The manual's words note as a cost: "In the MEMORY TO VRAM, in the case
    /// where CRAM and VSRAM are the dessinations, number of words (not byte)
    /// should apply" (Sega Genesis Software Manual, "DMA transfer capacity";
    /// the typo is the scan's). One slot a word ([`slots_per_unit`]) over the
    /// 205 of a blank H40 line is 205 words, so a CRAM transfer of exactly that
    /// fits inside the line it starts on. Every source word is 0x0EEE, the one
    /// colour word the CRAM's write masking leaves alone, so the whole CRAM
    /// reads back as it once the transfer has been through it, wrap and all.
    #[test]
    fn a_cram_transfer_of_205_words_in_h40_vblank_is_one_line() {
        let mut vdp = h40_in_vblank();
        vdp.write_register(1, 0x50);
        dma_from(&mut vdp, 0x000100, 205);
        start_dma(&mut vdp, cram_write_cmd(0x0000));

        let source: Vec<u16> = vec![0x0EEE; 205];
        let mut bus = FakeBus::laid_at(0x000100, &source);
        vdp.run_dma(&mut bus);

        vdp.advance(MCLKS_PER_LINE);
        assert!(
            !vdp.dma_busy,
            "205 words x 1 slot is the blank H40 line's 205 exactly"
        );
        assert!(
            vdp.cram.iter().all(|&word| word == 0x0EEE),
            "the whole CRAM is the transferred word"
        );
    }

    /// One word more is 206 slots against 205, so the transfer runs into the
    /// next line: the words note makes the unit of a CRAM transfer a word, not
    /// the transfer free.
    #[test]
    fn a_cram_transfer_of_206_words_does_not_fit_in_that_line() {
        let mut vdp = h40_in_vblank();
        vdp.write_register(1, 0x50);
        dma_from(&mut vdp, 0x000100, 206);
        start_dma(&mut vdp, cram_write_cmd(0x0000));

        let source: Vec<u16> = vec![0x0EEE; 206];
        let mut bus = FakeBus::laid_at(0x000100, &source);
        vdp.run_dma(&mut bus);

        vdp.advance(MCLKS_PER_LINE);
        assert!(vdp.dma_busy, "206 slots against 205: one word short");
        assert_eq!(vdp.cram[0], 0x0EEE, "words are landing while it runs");
        vdp.advance(MCLKS_PER_LINE);
        assert!(!vdp.dma_busy, "and the second line finishes it");
    }

    /// The copy row of an active line: "VRAM COPY ... H40CELL DURING EFFECTIVE
    /// SCREEN 9 Bytes" (Sega Genesis Software Manual, "DMA transfer capacity",
    /// on #305). Two slots a copied byte ([`COPY_SLOTS`]) halves the 18 free
    /// slots of an active H40 line, so 9 bytes fit inside the line exactly.
    /// A copy moves byte lanes, from `source ^ 1` to `addr ^ 1` (genvdp.txt
    /// section 11), so the source is planted lane-swapped and the ninth byte
    /// of the sequence lands at 0x0209.
    #[test]
    fn a_copy_of_9_bytes_over_the_active_h40_display_is_one_line() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x81);
        dma_set_up(&mut vdp, COPY, 9, 1);
        vdp.write_register(1, 0x50);
        copy_source(&mut vdp, 0x0100);
        vdp.in_hblank = false;
        vdp.in_vblank = false;
        for offset in 0u16..10 {
            vdp.vram[usize::from((0x0100 + offset) ^ 1)] = offset as u8 + 1;
        }
        start_and_run(&mut vdp, copy_write_cmd(0x0200));
        assert!(vdp.dma_busy, "the copy is running");

        vdp.advance(MCLKS_PER_LINE);

        assert!(
            !vdp.dma_busy,
            "9 bytes x 2 slots is the active H40 line's 18 exactly"
        );
        for byte in 1u16..=9 {
            assert_eq!(
                vdp.vram[usize::from((0x0200 + byte - 1) ^ 1)],
                byte as u8,
                "byte {byte} of the sequence, at its lane-swapped slot"
            );
        }
    }

    /// One byte more is 20 slots against 18: the copy runs into the next line.
    #[test]
    fn a_copy_of_10_bytes_does_not_fit_in_that_line() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x81);
        dma_set_up(&mut vdp, COPY, 10, 1);
        vdp.write_register(1, 0x50);
        copy_source(&mut vdp, 0x0100);
        vdp.in_hblank = false;
        vdp.in_vblank = false;
        for offset in 0u16..11 {
            vdp.vram[usize::from((0x0100 + offset) ^ 1)] = offset as u8 + 1;
        }
        start_and_run(&mut vdp, copy_write_cmd(0x0200));
        assert!(vdp.dma_busy, "the copy is running");

        vdp.advance(MCLKS_PER_LINE);
        assert!(vdp.dma_busy, "20 slots against 18: one byte short");
        vdp.advance(MCLKS_PER_LINE);
        assert!(!vdp.dma_busy, "and the second line finishes it");
        assert_eq!(vdp.vram[0x0208], 10, "the tenth byte, on the next line");
    }

    /// The slot budget of a line is the Sega DMA bandwidth table read in slots
    /// rather than in bytes, and every cell of it has a page now: the Sega
    /// Genesis Software Manual's "DMA transfer capacity" table (read off the
    /// scan on #305, and transcribed cell for cell in genvdp.txt "Transfer
    /// capacity") quotes MEMORY TO VRAM 18/205 H40 and 16/167 H32, VRAM FILL
    /// 17/204 and 15/166, VRAM COPY 9/102 and 8/83, bytes for VRAM and words
    /// for a CRAM or VSRAM destination. With a unit costing 2 slots for a VRAM
    /// word and a copied byte and 1 for a CRAM or VSRAM word
    /// ([`slots_per_unit`]), those are exactly the numbers the four tables
    /// make: 18 / 205, 17 / 204, 9 / 102 and 16 / 167, 15 / 166, 8 / 83.
    #[test]
    fn the_dma_line_budget_is_the_bandwidth_table() {
        let mut active = Vdp::new(false);
        active.write_register(1, 0x40);
        active.write_register(12, 0x81);
        assert_eq!(active.dma_slot_table(false).len(), 18, "active H40");
        assert_eq!(
            active.dma_slot_table(true).len(),
            17,
            "and a fill, one less"
        );

        active.write_register(12, 0x00);
        assert_eq!(active.dma_slot_table(false).len(), 16, "active H32");
        assert_eq!(
            active.dma_slot_table(true).len(),
            15,
            "and a fill, one less"
        );

        let mut blank = h40_in_vblank();
        assert_eq!(blank.dma_slot_table(false).len(), 205, "blank H40");
        assert_eq!(
            blank.dma_slot_table(true).len(),
            204,
            "and a fill, one less"
        );

        blank.write_register(12, 0x00);
        assert_eq!(blank.dma_slot_table(false).len(), 167, "blank H32");
        assert_eq!(
            blank.dma_slot_table(true).len(),
            166,
            "and a fill, one less"
        );

        assert_eq!(
            Vdp::new(false).dma_slot_table(false).len(),
            167,
            "a line with the display off is a blank one: nothing is being fetched"
        );
    }

    /// Two slots for the VRAM word of a 68k transfer, because VRAM is byte wide
    /// (genvdp.txt section 8), and two for a copied byte, because a copy reads
    /// as well as writes. One for a CRAM or VSRAM word - "number of words (not
    /// byte) should apply" when they are the destination of a MEMORY TO VRAM
    /// transfer (Sega Genesis Software Manual, "DMA transfer capacity") - and
    /// one for a fill step, which for a CRAM or VSRAM fill the manual does not
    /// state and is analogy with this row (#305).
    #[test]
    fn a_unit_costs_what_its_memory_costs() {
        assert_eq!(
            slots_per_unit(DmaMode::Mc68kTransfer, DataTarget::Vram),
            2,
            "a 68k transfer of VRAM words: 9 of them an active H40 line, which is the table's 18 bytes"
        );
        assert_eq!(
            slots_per_unit(DmaMode::Mc68kTransfer, DataTarget::Cram),
            1,
            "CRAM and VSRAM are the same slot counts in words"
        );
        assert_eq!(slots_per_unit(DmaMode::Mc68kTransfer, DataTarget::Vsram), 1);
        assert_eq!(slots_per_unit(DmaMode::Fill, DataTarget::Vram), 1);
        assert_eq!(
            slots_per_unit(DmaMode::Copy, DataTarget::Vram),
            2,
            "read and write: the table's 9 bytes an active H40 line out of 18 slots"
        );
    }

    // A DMA writes VRAM the way the data port does, so a write that lands in the
    // sprite attribute table updates the VDP's internal cache of it (#103) as
    // well. The three modes reach memory by three different routes, so each is
    // tried: the transfer through `Vdp::write_code_word`, the fill and the copy
    // through their own byte writes.

    /// The SAT base of the three tests below: R5 = `0x78` is 0xF000 (genvdp.txt
    /// section 17 "$05"), away from the source data they copy from.
    const SPRITE_REG: u8 = 0x78;

    /// A 68k transfer is how a game loads a sprite table, and it writes the
    /// memory through the port rules of M4-02 rather than the port itself.
    #[test]
    fn a_68k_transfer_into_the_sprite_table_updates_the_sprite_cache() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(5, SPRITE_REG);
        dma_from(&mut vdp, 0x000100, 4);
        start_dma(&mut vdp, vram_write_cmd(0xF000));

        let mut bus = FakeBus::laid_at(0x000100, &[0x0080, 0x0102, 0x1234, 0x0ABC]);
        vdp.run_dma(&mut bus);
        run_dma_to_end(&mut vdp);

        assert_eq!(
            &vdp.vram[0xF000..0xF008],
            [0x00, 0x80, 0x01, 0x02, 0x12, 0x34, 0x0A, 0xBC],
            "the four words of the source, big endian"
        );
        assert_eq!(
            vdp.sprite_cache.line(0).copied(),
            Some([0x00, 0x80, 0x01, 0x02]),
            "the y word and the size/link word of entry 0, which is what the cache holds"
        );
    }

    /// A fill writes its repeated byte with `Vdp::fill_step`, which is no part of
    /// `Vdp::write_code_word`, and still has to reach the cache.
    #[test]
    fn a_fill_of_the_sprite_table_updates_the_sprite_cache() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(5, SPRITE_REG);
        dma_set_up(&mut vdp, FILL, 4, 1);
        start_and_run(&mut vdp, vram_write_cmd(0xF000));

        vdp.write_data(0xABCD);
        run_dma_to_end(&mut vdp);

        assert_eq!(
            &vdp.vram[0xF000..0xF006],
            [0xAB, 0xCD, 0xAB, 0xAB, 0x00, 0xAB],
            "the word of the port write that starts the fill, then four steps of 0xAB at R15 = 1"
        );
        assert_eq!(
            vdp.sprite_cache.line(0).copied(),
            Some([0xAB, 0xCD, 0xAB, 0xAB]),
            "every one of entry 0's four cached bytes was written here or by the port"
        );
        assert_eq!(
            vdp.sprite_cache.line(1).copied(),
            Some([0; 4]),
            "0xF005 is byte 5 of entry 1, and the last four bytes of an entry are not cached"
        );
    }

    /// And a copy, which writes one byte at a time out of `Vdp::dma_move_unit`.
    #[test]
    fn a_copy_into_the_sprite_table_updates_the_sprite_cache() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(5, SPRITE_REG);
        vdp.vram[0x0100..0x0104].copy_from_slice(&[0x00, 0x8A, 0x01, 0x02]);
        copy_source(&mut vdp, 0x0100);
        dma_set_up(&mut vdp, COPY, 4, 1);
        start_and_run(&mut vdp, vram_write_cmd(0xF000));
        run_dma_to_end(&mut vdp);

        assert_eq!(&vdp.vram[0xF000..0xF004], [0x00, 0x8A, 0x01, 0x02]);
        assert_eq!(
            vdp.sprite_cache.line(0).copied(),
            Some([0x00, 0x8A, 0x01, 0x02]),
            "the four bytes the copy landed on top of, cached as they went in"
        );
    }

    /// Queue `n` VRAM writes to 0x1000 on a display-on H40 VDP in VBlank, at
    /// beam (line 0, clock 0). No `advance` between them, so all `n` sit on
    /// the queue when the test starts its DMA.
    fn queued_writes(vdp: &mut Vdp, n: usize) {
        vdp.write_register(1, 0x50); // display on + DMA enable
        let (first, second) = vram_write_cmd(0x1000);
        vdp.write_control(first);
        vdp.write_control(second);
        for i in 0..n {
            vdp.write_data(0x5100 + i as u16);
        }
    }

    /// Start a `words`-word VRAM transfer to 0x2000 and walk the beam until it
    /// is over, reporting `(stall at start, when the first word landed, when
    /// the busy bit came down)` in master clocks from the start.
    fn probe_transfer(vdp: &mut Vdp, words: u16) -> (u32, u32, u32) {
        dma_from(vdp, 0x000100, words);
        start_dma(vdp, vram_write_cmd(0x2000));
        let source: Vec<u16> = (0..words).map(|i| 0x1000 + i).collect();
        let mut bus = FakeBus::laid_at(0x000100, &source);
        vdp.run_dma(&mut bus);
        let stall = vdp.dma_68k_stall_mclk();
        let mut clocks = 0u32;
        while vdp.vram[0x2000] == 0 {
            vdp.advance(1);
            clocks += 1;
        }
        let first_lands = clocks;
        while vdp.dma_busy {
            vdp.advance(1);
            clocks += 1;
        }
        (stall, first_lands, clocks)
    }

    /// The freeze a transfer starts behind is the queue's drain on top of the
    /// transfer's own slots: a transfer pushes its source words into the same
    /// four-entry write FIFO the data port feeds, and "pending FIFO writes take
    /// priority" (Nemesis on SpritesMind t=1291), so behind `n` queued entries
    /// its first word cannot land until they have, and `dma_68k_stall_mclk`
    /// covers their slots. Two queued VRAM words are four slots: the figure the
    /// empty-queue transfer reports, plus the four slots it takes to drain
    /// them, both read off the same grid.
    #[test]
    fn a_transfer_behind_queued_writes_costs_their_drain() {
        let mut without = h40_in_vblank();
        without.write_register(1, 0x50);
        let bare = without.dma_mclk_to_slots(false, 2);
        let (stall_without, ..) = probe_transfer(&mut without, 1);

        let mut with = h40_in_vblank();
        queued_writes(&mut with, 2);
        assert_eq!(with.fifo_len(), 2, "the queue is still holding both writes");
        // The window is the four drain slots and the word's own two on the same
        // grid, against the bare word's two - read before the probe moves the
        // beam.
        let window = with.dma_mclk_to_slots(false, 4 + 2);
        let (stall_with, ..) = probe_transfer(&mut with, 1);

        assert_eq!(
            stall_with, window,
            "four drain slots plus the word's own two: {stall_with}"
        );
        assert_eq!(stall_without, bare);
        assert_eq!(stall_with, stall_without + (window - bare));
    }

    /// And a CRAM queue costs one slot an entry, the width of the write.
    #[test]
    fn a_transfer_behind_a_cram_queue_costs_one_slot_an_entry() {
        let mut without = h40_in_vblank();
        without.write_register(1, 0x50);
        let (stall_without, ..) = probe_transfer(&mut without, 1);

        let mut with = h40_in_vblank();
        with.write_register(1, 0x50);
        let (first, second) = cram_write_cmd(0x0020);
        with.write_control(first);
        with.write_control(second);
        with.write_data(0x0E20);
        let (stall_with, ..) = probe_transfer(&mut with, 1);

        let one_slot = with.dma_mclk_to_slots(false, 3) - with.dma_mclk_to_slots(false, 2);
        assert_eq!(stall_with, stall_without + one_slot);
    }

    /// A fill's steps run "only ... at an access slot if the FIFO is empty"
    /// (Nemesis on SpritesMind t=1291), so a fill armed behind queued writes
    /// starts its steps after their drain, which its busy window covers.
    #[test]
    fn a_fill_behind_queued_writes_starts_after_their_drain() {
        let mut without = h40_in_vblank();
        without.write_register(1, 0x50);
        vdp_write_fill_setup(&mut without, 0x3000);
        start_and_run(&mut without, vram_write_cmd(0x3000));
        without.write_data(0xAB00);
        let mut clocks_without = 0;
        while without.vram[0x3003] != 0xAB {
            without.advance(1);
            clocks_without += 1;
        }

        let mut with = h40_in_vblank();
        queued_writes(&mut with, 2);
        vdp_write_fill_setup(&mut with, 0x3000);
        // The first step runs once the FIFO is empty (see [`Running::Fill`]),
        // and the queue the write that triggered the fill waits behind is the
        // two queued words plus the trigger write itself: the seventh slot on
        // the fill's grid, read before the loop moves the beam.
        let expected = with.dma_mclk_to_slots(true, 2 * 2 + 2 + 1);
        start_and_run(&mut with, vram_write_cmd(0x3000));
        with.write_data(0xAB00);
        let mut clocks_with = 0;
        while with.vram[0x3003] != 0xAB {
            with.advance(1);
            clocks_with += 1;
        }

        assert_eq!(
            clocks_with, expected,
            "the fill waits out the two queued words first: {clocks_with}"
        );
        assert!(clocks_with > clocks_without);
    }

    /// Register setup for a VRAM fill without touching R1, so the display state
    /// the test queued writes under stays as it is.
    fn vdp_write_fill_setup(vdp: &mut Vdp, _addr: u16) {
        vdp.write_register(19, 8);
        vdp.write_register(15, 2);
        vdp.write_register(23, FILL);
    }
}
