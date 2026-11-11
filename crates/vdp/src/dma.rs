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
//! Not modelled: the read FIFO quirk that makes the first word of a
//! 68k-to-VRAM transfer wrong on real hardware (#306), and the CRAM "dots" a
//! write during active display leaves behind (out of scope for #101). The four
//! bandwidth figures the slot costs below reproduce are ticket-stated rather
//! than quoted from the Sega manual, and #305 is checking them.

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
/// blank line's 205.
const COPY_SLOTS: u8 = 2;

/// Access slots one unit of a DMA costs, a unit being a word of a 68k transfer
/// and a byte of a fill or a copy.
///
/// VRAM is byte wide, so its word is two writes and two slots (genvdp.txt
/// section 8); CRAM and VSRAM are word wide and cost one (sections 9 and 10),
/// which is the table's "the same slot counts but in words". A step of a fill is
/// one byte, or one word of CRAM or VSRAM, and one slot; that a fill's line
/// holds one slot *less* than a transfer's is not this function's business, it is
/// [`Vdp::dma_slot_table`] that pays it.
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
    /// ([`Vdp::run_fill`]).
    Fill { slots: u32, value: u16 },
    /// A copy: `source` walked a byte at a time to the address register.
    Copy { slots: u32, source: u16 },
}

impl Running {
    /// The fill, and only the fill, pays one slot of every line to the machine:
    /// see [`Vdp::dma_slot_table`].
    fn is_fill(&self) -> bool {
        matches!(self, Self::Fill { .. })
    }

    /// Access slots still owed before the last unit reaches memory.
    fn slots(&self) -> u32 {
        match self {
            Self::Transfer { slots, .. } | Self::Fill { slots, .. } | Self::Copy { slots, .. } => {
                *slots
            }
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
    /// Whatever the port has queued reaches memory first: a DMA writes the same
    /// memories through the same rules of M4-02 (#80) and must not overtake the
    /// writes that were made before its own command word. And a DMA started
    /// while one is running replaces it - a corner nothing documents, because
    /// hardware has the 68k frozen for a transfer and tells it to keep off the
    /// data port for the other two (status bit 1), so nobody is supposed to get
    /// here at all.
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
        self.start_running(Running::Fill {
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

    /// One access slot of the DMA in flight: the unit at the head reaches memory
    /// if this was the last of the slots it costs, and the transfer is over when
    /// its slots run out. Whether another DMA still wants a slot is the answer.
    fn dma_take_slot(&mut self) -> bool {
        // Taken out rather than borrowed, so that writing memory and moving the
        // running state on are not both borrowing `self`.
        let Some(mut running) = self.dma.take() else {
            return false;
        };
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
    /// transfer's access slots.
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

    /// The slot budget of a line is the Sega DMA bandwidth table read in slots
    /// rather than in bytes: 18 of them on an active H40 line and 205 on a blank
    /// one, 16 and 167 in H32, and one less in every column for a fill - 17,
    /// 204, 15 and 166. With a unit costing 2 slots for a VRAM word and a
    /// copied byte and 1 for a CRAM or VSRAM word ([`slots_per_unit`]), those
    /// are the table's 18 / 205, 17 / 204, 9 / 102 and 16 / 167, 15 / 166,
    /// 8 / 83.
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
    /// as well as writes. One for a CRAM or VSRAM word, which are word wide
    /// (sections 9 and 10), and one for a fill step.
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
}
