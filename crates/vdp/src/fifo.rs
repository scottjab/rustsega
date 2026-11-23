//! The four-entry write FIFO between the data port and the memories.
//!
//! A word written to the data port does not reach VRAM, CRAM or VSRAM in the
//! act of being written. It goes onto a queue - four entries of (code, addr,
//! data), from Nemesis's VDP FIFO research on SpritesMind, which is what the
//! `VDPFIFOTesting` ROMs measure - and the queue is emptied one entry at a time
//! at the moments the VDP is not using the memories itself, the external access
//! slots. genvdp.txt never describes the queue as a thing, but it documents
//! both ends of it: the bits that report it ("d9 - FIFO data empty", "d8 - FIFO
//! data full", section 6) and the widths that decide how many slots a write
//! costs, VRAM being byte wide (section 8) and CRAM and VSRAM word wide
//! (sections 9 and 10).
//!
//! So a VRAM word costs two slots and a CRAM or VSRAM word one, an entry
//! carrying the code and address its write was made with because the 68k has
//! moved both on by the time the entry lands. What a slot costs in master
//! clocks is the slot schedule's business, and the schedule is `slots`: the
//! positions in the line the memories are free at, per mode and per part of the
//! line. [`Vdp::slots_ahead`] says how many of them a run of clocks covers and
//! [`Vdp::mclk_to_slots`] how long `mclk` of waiting is in clocks, and this
//! module is the queue that runs behind them.
//!
//! The bus-facing half of this is [`Vdp::data_port_stall_mclk`]: a write that
//! finds the queue full cannot be performed, and the master clocks it is held
//! are what M3-16 (#77) adds to the 68k's cycle count. Reads go the other way
//! and are not the FIFO's business, but they do wait for what is already
//! queued, so [`Vdp::read_data`] drains it first.

use std::collections::VecDeque;

use crate::Vdp;
use crate::ports::{DataTarget, data_target};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Entries the write FIFO holds.
///
/// Four is the depth of the real thing (Nemesis on SpritesMind, and the number
/// the `VDPFIFOTesting` ROMs push against), and it is what lets a game lay down
/// a burst of palette writes and carry on without waiting for any of them.
///
/// It is also the only depth the chip's own numbers are read against: reaching it
/// raises status bit 8 ([`Fifo::is_full`]) and starts the wait of
/// [`Vdp::data_port_stall_mclk`]. How many entries the queue is *actually*
/// carrying is a separate question, and the answer is not always four - see
/// [`Fifo::queue`].
pub(crate) const FIFO_DEPTH: usize = 4;

/// One data port write, waiting its turn at the memories.
///
/// It carries the code and the address of the write rather than reading them
/// when it is written out: the 68k sets up its next command word as soon as the
/// last word is away, so both registers have moved on long before the first
/// word lands.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Entry {
    /// The code register as the write left it: which memory the word is for.
    pub(crate) code: u8,
    /// The address register as the write left it, before this write's own move.
    pub(crate) addr: u16,
    /// The word the port was driven with.
    pub(crate) value: u16,
    /// Access slots still needed before the word is in memory.
    slots: u8,
}

impl Entry {
    /// A `value` written to `addr` in the memory `code` names.
    ///
    /// VRAM is byte wide, so its word is two writes and two slots (genvdp.txt
    /// section 8); CRAM and VSRAM are word wide and cost one (sections 9 and
    /// 10); a code that names no memory has nothing to write and so takes no
    /// slot - and no entry, see [`Fifo::push`].
    pub(crate) fn new(code: u8, addr: u16, value: u16) -> Self {
        let slots = match data_target(code) {
            DataTarget::Vram => 2,
            DataTarget::Cram | DataTarget::Vsram => 1,
            DataTarget::Ignored => 0,
        };
        Self {
            code,
            addr,
            value,
            slots,
        }
    }
}

/// The four entries between the data port and the memories.
///
/// The queue keeps no clock of its own: the slots it waits for are positions in
/// the line (`slots`), so the beam is the only clock there is and
/// [`Vdp::fifo_clock`] tells this how many of them a run of master clocks
/// covered.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Fifo {
    /// The writes the port has made that the memories have not, oldest first.
    ///
    /// Four of them is the chip's FIFO. Past four is writes the 68k got away with
    /// while it should have been sitting on /DTACK, and the queue keeps them
    /// rather than losing them for two reasons.
    ///
    /// The first is that losing one is #540. The queue used to be four entries
    /// plus a latch for the fifth, so a write arriving when the latch was taken
    /// overwrote it: `Bus::vdp_data_write` is called for both words of a long to
    /// `$C00000` (the port is mirrored at `$C00002`, genvdp.txt section 3, and a
    /// long is two word cycles) with no slot between them, so every long written
    /// across a full FIFO lost the first of its two words and left a two-byte
    /// hole in VRAM. `Shinobi III - Return of the Ninja Master (USA)` and
    /// `Road Rash II (USA, Europe)` lose 3209 and 4653 bytes a frame that way, and
    /// every differing byte of theirs sits inside a word lost like this.
    ///
    /// The second is that "past four" is a state the bus reaches rather than a
    /// corner, because a write is performed and charged rather than refused: the
    /// 68k is told how long the chip would have held it, and this queue is where
    /// those writes wait while the beam runs. [`Vdp::data_port_stall_mclk`] now
    /// charges the wait for the whole of what stands in front of a new write
    /// (#545), which holds the queue to a couple of entries past `FIFO_DEPTH` on
    /// the carts of `docs/compat-vdp.md` - it was 28 before that fix. A real 68000
    /// never has a fifth word to put, because the fourth already froze it, so the
    /// depth is still a read-out on the bus rather than a limit the 68k is kept to:
    /// the two writes of a long to `$C00000` are made at one beam position and the
    /// second of them is queued before the first has been paid for.
    queue: VecDeque<Entry>,
}

impl Fifo {
    /// An empty FIFO, which is what a VDP that has never been written to has.
    pub(crate) fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Queue one data port write, which is what [`Vdp::write_data`] does.
    ///
    /// A write with nothing to write - a code register naming a read, whose
    /// write "is ignored" (genvdp.txt section 7) - takes no entry, so it cannot
    /// fill the FIFO or hold up the write behind it. Any other write joins the
    /// back of the queue, whether or not the FIFO is already full: a write that
    /// finds no room at the chip is queued rather than dropped, and still holds
    /// the port through [`Fifo::is_full`]. See [`Fifo::queue`] for why that is
    /// #540.
    pub(crate) fn push(&mut self, entry: Entry) {
        if entry.slots == 0 {
            return;
        }
        self.queue.push_back(entry);
    }

    /// One access slot has come round: `None` when the entry at the head still
    /// has slots to go, and that entry when this one was its last.
    ///
    /// A slot is a position in the line, not a width of time, so whether one has
    /// come round is `slots`' question rather than this module's - see
    /// [`Vdp::fifo_clock`], which is what calls this once for each of them.
    pub(crate) fn take_slot(&mut self) -> Option<Entry> {
        if self.queue.is_empty() {
            return None;
        }

        // The queue is not empty, and the slots the entry at the front has left
        // are what stands between it and memory: this one is spent, and only a
        // VRAM word still has another to wait for.
        if let Some(head) = self.queue.front_mut() {
            head.slots -= 1;
            if head.slots > 0 {
                return None;
            }
        }
        self.pop()
    }

    /// The next write off the queue, whatever slots it still had left.
    ///
    /// Every write the port made is in the queue, including the ones that
    /// arrived while it was full (see [`Fifo::queue`]), so a drain reaches them
    /// in the order the 68k wrote them.
    pub(crate) fn pop(&mut self) -> Option<Entry> {
        self.queue.pop_front()
    }

    /// Nothing on the queue: status bit 9.
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// All four entries taken: status bit 8.
    ///
    /// `FIFO_DEPTH` and not the length of the queue, because four is what the
    /// chip reports: writes past the fourth are ones the bus should not have had
    /// room to make yet, and they do not make a full FIFO read as any less full.
    pub(crate) fn is_full(&self) -> bool {
        self.queue.len() >= FIFO_DEPTH
    }

    /// The access slots a write arriving now is still owed before the queue has a
    /// place for it, and `0` whenever the port takes it straight away.
    ///
    /// Four entries is the whole chip, so a queue under four has a place and owes
    /// the write nothing. At four it does not, and genvdp.txt section 6 is exact
    /// about what the wait is: "Once the 68K has written the fourth word, FULL is
    /// raised. If the processor attempts to write one more time, it will be frozen
    /// (/DTACK high) until the FIFO unit manages to deliver the first stacked word
    /// to its rightful owner". The first word, and only when the queue is deeper
    /// than the chip's does "the first" stop being enough: a write arriving behind
    /// six entries is behind six of them, not one, so the wait is until there is
    /// room - `len + 1 - FIFO_DEPTH` entries delivered, each owing the slots the
    /// beam has not spent on it yet.
    ///
    /// Counting the head entry alone - which is what this did before #545 - is the
    /// same figure whenever the queue is exactly four deep and a smaller one
    /// whenever it is deeper, so it is right at the depth the chip can reach and
    /// short at every depth past it. That is what let a cart's queue run to 28.
    ///
    /// See [`Vdp::data_port_stall_mclk`], which turns this many slots into clocks.
    pub(crate) fn slots_until_accepted(&self) -> u32 {
        if !self.is_full() {
            return 0;
        }
        let clear = self.queue.len() + 1 - FIFO_DEPTH;
        self.queue
            .iter()
            .take(clear)
            .map(|entry| u32::from(entry.slots))
            .sum()
    }

    /// The access slots every entry on the queue still owes before it is in
    /// memory: what [`Vdp::start_running`] spends a DMA on before that DMA's own
    /// units can move, because "pending FIFO writes take priority" (Nemesis on
    /// SpritesMind t=1291) and they share the one slot stream. See
    /// [`Running::Transfer`]'s `drain`.
    pub(crate) fn slots_owed(&self) -> u32 {
        self.queue.iter().map(|entry| u32::from(entry.slots)).sum()
    }
}

impl Vdp {
    /// The FIFO's share of the beam moving on: `mclk` master clocks pass, and
    /// every external access slot inside them writes its entry to memory.
    ///
    /// Called from [`Vdp::advance`] and from [`Vdp::write_data`], the latter
    /// with no clocks at all: a write never writes itself out on the way in,
    /// because the slots are strictly in front of the beam
    /// ([`Vdp::slots_ahead`] of 0 clocks is no slots) - except with the display
    /// off, where nothing is being fetched and the memories are free for the
    /// port the moment it asks. That last part is #99's and is what keeps every
    /// data port test of #80 and #81 true without an `advance` in it. Real
    /// hardware still only lets the 68k have the memories
    /// [`slots::OFF_TRANSFERS_H40`](crate::slots::OFF_TRANSFERS_H40) times an H40
    /// line, 17.3 clocks apiece, and #302 decided not to wait for that grid: a
    /// display-off line drains writes faster than a 68000 can make them, which is
    /// argued in the display-off note of [`crate::slots`] and pinned by
    /// `slots::a_display_off_line_drains_faster_than_the_68k_can_write`.
    ///
    /// The schedule is read at the state the beam is in now, so a call that runs
    /// across the end of the line counts its slots against the line it started
    /// in. Nothing does that yet except a whole-frame `advance`, where it is
    /// within a slot or two of right either way.
    pub(crate) fn fifo_clock(&mut self, mclk: u32) {
        if self.fifo.is_empty() {
            return;
        }
        if !self.display_enabled() {
            self.fifo_drain();
            return;
        }
        for _ in 0..self.slots_ahead(mclk) {
            self.service_slot();
            if self.fifo.is_empty() {
                break;
            }
        }
    }

    /// One external access slot: the entry at the head of the FIFO moves a slot
    /// closer to memory, and is written out when this one was its last.
    fn service_slot(&mut self) {
        if let Some(entry) = self.fifo.take_slot() {
            self.write_code_word(entry.code, entry.addr, entry.value);
        }
    }

    /// Everything the FIFO is holding, written out without waiting for slots.
    ///
    /// This is what a read of the data port waits for: the 68k that wrote three
    /// words and then reads the port is reading the memory those words are on
    /// their way to, and it is the bus's own access rather than a clock, so the
    /// slots are spent in one go rather than counted out. It is also what the
    /// display-off case in [`Vdp::fifo_clock`] does; #302 kept that shortcut, and
    /// the rate it rests on is in [`crate::slots`]' display-off note rather than
    /// repeated here.
    pub(crate) fn fifo_drain(&mut self) {
        while let Some(entry) = self.fifo.pop() {
            self.write_code_word(entry.code, entry.addr, entry.value);
        }
    }

    /// Status bit 9 (d9), FIFO empty: nothing on the queue.
    pub(crate) fn fifo_empty(&self) -> bool {
        self.fifo.is_empty()
    }

    /// Status bit 8 (d8), FIFO full: all four entries taken.
    pub(crate) fn fifo_full(&self) -> bool {
        self.fifo.is_full()
    }

    /// How many entries the queue is holding.
    ///
    /// Not a chip report - status bit 8 is [`Vdp::fifo_full`] and that one reads
    /// against `FIFO_DEPTH` whatever the queue holds - but it is what a test of the
    /// hold has to be able to see, because the hold is what decides it.
    #[cfg(test)]
    pub(crate) fn fifo_len(&self) -> usize {
        self.fifo.queue.len()
    }

    /// Master clocks a write to the data port is held for, `0` when the port
    /// takes it straight away.
    ///
    /// The FIFO is four entries deep, and a write that finds all four taken
    /// cannot happen: on real hardware the VDP holds the 68k with /DTACK low
    /// until an access slot frees an entry (Nemesis's VDP FIFO research on
    /// SpritesMind, which is what the `VDPFIFOTesting` ROMs measure). This is
    /// the length of that hold, for the bus glue of M3-16 (#77) to add to the
    /// cycle count of the write - the same shape as
    /// [`Vdp::dma_68k_stall_mclk`], which is a stall on the other side of the
    /// same transfer.
    ///
    /// "How long" is the slots every entry in front of the arriving write still
    /// owes, counted along the slot schedule: nothing moves until its slots have
    /// come round, and a slot at the beam's own clock does not count. At the
    /// chip's four entries that is one VRAM word, two of the 18 free slots an
    /// active H40 line offers - the largest of which is the documented 32-pixel
    /// FIFO-full stall, and in blanking under two pixels. Past four it is the wait
    /// for the whole of what is in front, which is why the depth a cart leaves the
    /// queue at is a read-out on this hold and not on the queue.
    ///
    /// The write is not lost meanwhile: it joins the queue behind the entries that
    /// filled it, in order, whatever that costs the clock.
    pub fn data_port_stall_mclk(&self) -> u32 {
        let slots = self.fifo.slots_until_accepted();
        if slots == 0 {
            0
        } else {
            self.mclk_to_slots(slots)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hv::MCLKS_PER_LINE;
    use crate::ports::testutil::{cram_write_cmd, vram_read_cmd, vram_write_cmd};
    use crate::slots::slot_mclk;

    fn set_reg(vdp: &mut Vdp, reg: u8, value: u8) {
        vdp.write_control(0x8000 | ((u16::from(reg) & 0x1F) << 8) | u16::from(value));
    }

    fn vram_write_at(vdp: &mut Vdp, addr: u16) {
        let (first, second) = vram_write_cmd(addr);
        vdp.write_control(first);
        vdp.write_control(second);
    }

    fn cram_write_at(vdp: &mut Vdp, addr: u16) {
        let (first, second) = cram_write_cmd(addr);
        vdp.write_control(first);
        vdp.write_control(second);
    }

    /// The beam, in the middle of an active display line: the part of the
    /// schedule where a write has to wait for its slots.
    ///
    /// 200 master clocks is past the front porch of the H32 line
    /// (`timing::HBLANK_CLEAR_MCLK_H32`, 100) and nowhere near the back porch or
    /// the end of the line, so no event and no render falls in the setup. The
    /// first free slot of the line is at 280, so eight of them - 1520 clocks -
    /// still fit inside the line.
    fn active_display() -> Vdp {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 1, 0x40);
        vdp.advance(200);
        assert!(
            !vdp.in_hblank && !vdp.in_vblank,
            "the beam is not on the active display"
        );
        vdp
    }

    /// The beam, in VBlank with the display on, which is the state a palette
    /// load is really made in.
    fn in_vblank() -> Vdp {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 1, 0x40);
        vdp.in_vblank = true;
        vdp
    }

    /// A VRAM word write takes two access slots, a CRAM word one: VRAM is byte
    /// wide (genvdp.txt section 8) and CRAM is not (section 9).
    #[test]
    fn a_cram_write_drains_in_one_slot_and_a_vram_word_in_two() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);

        cram_write_at(&mut vdp, 0x0000);
        vdp.write_data(0x0EEE);
        let first_slot = vdp.mclk_to_slots(1);
        assert_eq!(vdp.cram[0], 0, "no slot has come round yet");
        vdp.advance(first_slot - 1);
        assert_eq!(vdp.cram[0], 0, "one clock short of the first slot");
        vdp.advance(1);
        assert_eq!(vdp.cram[0], 0x0EEE, "CRAM is word wide: one slot");

        vram_write_at(&mut vdp, 0x0100);
        vdp.write_data(0xBEEF);
        let two_slots = vdp.mclk_to_slots(2);
        vdp.advance(two_slots - 1);
        assert_eq!(
            &vdp.vram[0x0100..0x0102],
            [0, 0],
            "one slot of a VRAM word is not a written word"
        );
        vdp.advance(1);
        assert_eq!(&vdp.vram[0x0100..0x0102], [0xBE, 0xEF]);
    }

    /// Four words written over the active display with no clocks between them
    /// fill the FIFO, and none of them has reached VRAM: which is the whole
    /// point of the FIFO, and the state status bit 8 reports.
    #[test]
    fn four_words_during_active_display_fill_the_fifo_and_have_not_reached_vram() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);

        for _ in 0..4 {
            vdp.write_data(0xABCD);
        }

        assert!(vdp.fifo_full());
        assert!(
            vdp.vram[0..8].iter().all(|&byte| byte == 0),
            "the FIFO is full and nothing has been drained: {:?}",
            &vdp.vram[0..8]
        );
        let status = vdp.read_status();
        assert_ne!(status & (1 << 8), 0, "d8 FIFO full");
        assert_eq!(status & (1 << 9), 0, "d9 FIFO empty is not set");
    }

    /// The ticket's drain test: four VRAM words written at the start of an active
    /// H40 line reach memory at the slots the table says, to the master clock.
    ///
    /// Eight slots of the 18 an active H40 line offers, from the head of the
    /// line: 272, 412, 540, 796, 924, 1052, 1308, 1436 - the last of them the
    /// eighth slot and the clock the fourth word is written at. Note that the
    /// first is at 272 rather than the 280 of H32: the line's slow HSYNC head is
    /// 332 clocks over 34 pixels, so the first 34 pixels cost 12 clocks more than
    /// the 8 a pixel of the active display costs.
    #[test]
    fn four_words_at_the_start_of_an_active_h40_line_arrive_at_their_slots() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 1, 0x40);
        set_reg(&mut vdp, 12, 0x81);
        set_reg(&mut vdp, 15, 2);
        vdp.in_hblank = false;
        vdp.in_vblank = false;
        assert!(vdp.h40(), "the beam is in H40");
        assert_eq!(vdp.line_mclk, 0, "and at the head of the line");

        vram_write_at(&mut vdp, 0x0000);
        for word in [0x1111u16, 0x2222, 0x3333, 0x4444] {
            vdp.write_data(word);
        }

        let eighth = vdp.mclk_to_slots(8);
        assert_eq!(
            eighth, 1436,
            "the eighth free slot of the line, from the table"
        );
        assert_eq!(eighth, slot_mclk(86, true), "slot 86 of the H40 line");

        vdp.advance(eighth - 1);
        assert_eq!(
            &vdp.vram[0..8],
            [0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0, 0],
            "three of the four words, one clock before the last slot"
        );
        vdp.advance(1);
        assert_eq!(
            &vdp.vram[0..8],
            [0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44],
            "and all four on the clock of the eighth slot"
        );
    }

    /// Empty it by advancing, and the words arrive in the order they were
    /// written: the queue is a queue.
    #[test]
    fn advancing_a_little_writes_all_four_in_order() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);

        for word in [0x1111u16, 0x2222, 0x3333, 0x4444] {
            vdp.write_data(word);
        }
        // Four VRAM words, two of the active line's 18 slots each.
        let to = vdp.mclk_to_slots(8);
        vdp.advance(to);

        assert_eq!(
            &vdp.vram[0..8],
            [0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44],
            "written in the order they were queued"
        );
        let status = vdp.read_status();
        assert_ne!(status & (1 << 9), 0, "d9 FIFO empty");
        assert_eq!(status & (1 << 8), 0, "d8 FIFO full");
    }

    /// The FIFO reports neither bit while it holds one to four entries, which is
    /// the intermediate state genvdp.txt section 6 quotes: "EMPTY only goes 1
    /// when there is nothing on the stack. The intermediate state is signaled by
    /// both of them showing 0".
    #[test]
    fn a_partly_full_fifo_reports_neither_empty_nor_full() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        vdp.write_data(0xABCD);

        let status = vdp.read_status();
        assert_eq!(status & (1 << 8), 0, "not full");
        assert_eq!(status & (1 << 9), 0, "not empty either");
    }

    /// The fifth write while the FIFO is full reports a stall, and the sixth
    /// one is what a bus that ignored the stall would have to be told about.
    #[test]
    fn a_fifth_write_while_full_reports_a_stall() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        assert_eq!(
            vdp.data_port_stall_mclk(),
            0,
            "an empty FIFO never holds the bus"
        );

        for _ in 0..4 {
            vdp.write_data(0xABCD);
        }
        let stall = vdp.data_port_stall_mclk();
        assert_ne!(
            stall, 0,
            "the port is full: the 68k waits for an access slot"
        );
        assert_eq!(
            stall,
            vdp.mclk_to_slots(2),
            "to the clock of the second free slot: the VRAM word at the head needs both"
        );

        // And the wait ends when the slots arrive: after eight of them the FIFO
        // is empty and the bus is on its way again.
        let to = vdp.mclk_to_slots(8);
        vdp.advance(to);
        assert_eq!(vdp.data_port_stall_mclk(), 0);
    }

    /// A held write is not a lost write: it joins the queue as the slots free a
    /// place, so a burst longer than the FIFO still arrives in order.
    #[test]
    fn a_write_held_at_the_port_arrives_last_and_unchanged() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        for word in [0x1111u16, 0x2222, 0x3333, 0x4444, 0x5555] {
            vdp.write_data(word);
        }

        // Five VRAM words, so ten of the line's free slots.
        let to = vdp.mclk_to_slots(10);
        vdp.advance(to);
        assert_eq!(
            &vdp.vram[0..10],
            [0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55]
        );
    }

    /// A long to the data port is two writes and the FIFO is not allowed to take
    /// the second in place of the first.
    ///
    /// The port is mirrored at `$C00002` as well as `$C00000` (genvdp.txt section
    /// 3), so `move.l #$11223333,($C00000).w` reaches it as the two words `$1122`
    /// and `$3333` with no access slot between them. Once the queue is full the
    /// second of those used to overwrite the first where it was held, so the long
    /// wrote its second word and lost its first: two bytes of VRAM missing, every
    /// time, which is #540.
    #[test]
    fn a_long_to_the_data_port_arrives_as_its_two_words() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        // Three longs' worth of words: the first two take the four entries, and
        // the third is written across a FIFO that has no room left for it.
        for word in [0x1111u16, 0x2222, 0x3333, 0x4444, 0x5555, 0x6666] {
            vdp.write_data(word);
        }

        // Six VRAM words, so twelve of the line's free slots.
        let to = vdp.mclk_to_slots(12);
        vdp.advance(to);
        assert_eq!(
            &vdp.vram[0..12],
            [
                0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55, 0x66, 0x66
            ],
            "the fifth word, written across a full FIFO, is the one that went missing"
        );
    }

    /// In VBlank the schedule is the whole grid, one slot every two pixels, which
    /// is the "almost at once" of the ticket: one VRAM word, two slots of
    /// blanking and a twentieth of a line.
    #[test]
    fn a_write_in_vblank_is_visible_after_a_tiny_advance() {
        let mut vdp = in_vblank();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0100);
        vdp.write_data(0xABCD);

        let slot = vdp.mclk_to_slots(1);
        assert_eq!(slot, 20, "two pixels of H32, the grid of a blank line");
        vdp.advance(slot);
        assert_eq!(
            &vdp.vram[0x0100..0x0102],
            [0, 0],
            "one of the two slots, so not yet"
        );
        vdp.advance(slot);
        assert_eq!(&vdp.vram[0x0100..0x0102], [0xAB, 0xCD]);
    }

    /// A read reaches the memory the write is on its way to, which is the
    /// ordering the 68k expects: no read of a location before the write queued
    /// for it has landed.
    #[test]
    fn a_read_after_a_write_returns_the_written_word() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0100);
        vdp.write_data(0xBEEF);
        assert_eq!(
            &vdp.vram[0x0100..0x0102],
            [0, 0],
            "still queued: this is the state the read has to see past"
        );

        let (first, second) = vram_read_cmd(0x0100);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0xBEEF);
    }

    /// The address register moves with the write, not with the memory: the
    /// increment follows "every read or write to the data port" (genvdp.txt
    /// section 17 "$0F"), which the port saw even though the memory did not.
    #[test]
    fn the_address_register_moves_at_enqueue_time() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);

        for _ in 0..4 {
            vdp.write_data(0xABCD);
        }
        assert_eq!(vdp.addr, 8, "four writes of R15 = 2, and nothing drained");
        assert!(
            vdp.vram[0..8].iter().all(|&byte| byte == 0),
            "the writes are still on the queue"
        );
    }

    /// With the display off the VDP fetches nothing, so a slot is ready for a
    /// write the moment it is made and the FIFO is never seen holding anything.
    /// This is what keeps every M4-02 (#80) and M4-03 (#81) test of the data
    /// port true without an `advance` in it.
    #[test]
    fn a_write_with_the_display_off_drains_at_once() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);

        for word in [0x1111u16, 0x2222, 0x3333, 0x4444, 0x5555] {
            vdp.write_data(word);
        }

        assert_eq!(
            &vdp.vram[0..10],
            [0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55]
        );
        let status = vdp.read_status();
        assert_ne!(status & (1 << 9), 0, "d9 FIFO empty");
        assert_eq!(status & (1 << 8), 0, "d8 FIFO full");
        assert_eq!(vdp.data_port_stall_mclk(), 0);
    }

    /// An entry's slot count is the width of the memory it is written to.
    #[test]
    fn an_entry_costs_its_memory_one_or_two_slots() {
        assert_eq!(Entry::new(0x01, 0, 0).slots, 2, "VRAM word");
        assert_eq!(Entry::new(0x03, 0, 0).slots, 1, "CRAM word");
        assert_eq!(Entry::new(0x05, 0, 0).slots, 1, "VSRAM word");
        assert_eq!(Entry::new(0x00, 0, 0).slots, 0, "VRAM read: no write");
    }

    /// A write with no write in it - a code register naming a read, whose write
    /// genvdp.txt section 7 says is ignored - takes no entry, so it cannot fill
    /// the FIFO and hold the bus for no reason.
    #[test]
    fn a_write_that_reaches_no_memory_takes_no_entry() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 0);
        let (first, _second) = vram_read_cmd(0x0000);
        vdp.write_control(first);

        for _ in 0..4 {
            vdp.write_data(0xABCD);
        }
        assert_eq!(
            vdp.data_port_stall_mclk(),
            0,
            "four writes with nothing to write, and no reason to hold the bus"
        );
        let status = vdp.read_status();
        assert_ne!(status & (1 << 9), 0, "d9 FIFO empty");
    }

    /// The queue itself, without a VDP around it: four entries deep, and the
    /// fifth waits its turn behind them rather than overwriting one.
    #[test]
    fn the_queue_holds_four_entries_and_holds_the_fifth_back() {
        let mut fifo = Fifo::new();
        for word in 1..=5u16 {
            fifo.push(Entry::new(0x03, 0, word));
        }
        assert!(fifo.is_full());
        assert!(!fifo.is_empty());

        let mut drained = [0u16; 5];
        for slot in drained.iter_mut() {
            *slot = fifo.pop().expect("five writes, five pops").value;
        }
        assert_eq!(drained, [1, 2, 3, 4, 5], "first in, first out");
        assert!(fifo.is_empty());
    }

    /// A slot is only taken when one has come round, which the queue is now the
    /// only judge of on its own terms: `take_slot` moves one slot, a VRAM word
    /// needs two of them, and nothing at all happens to a queue that has been
    /// handed no slot. The clock's half of that - the slots a run of master
    /// clocks covers - is `slots`', and is tested there.
    #[test]
    fn an_idle_fifo_owes_itself_no_slots() {
        let mut fifo = Fifo::new();
        assert!(
            fifo.take_slot().is_none(),
            "an empty queue has nothing to write"
        );

        fifo.push(Entry::new(0x01, 0x0100, 0xBEEF));
        assert!(
            fifo.take_slot().is_none(),
            "a VRAM word is two slots, so this one writes nothing yet"
        );
        let entry = fifo.take_slot().expect("second slot");
        assert_eq!((entry.addr, entry.value), (0x0100, 0xBEEF));
        assert!(fifo.is_empty());
    }

    /// The first write past the four entries waits for the word at the head, and
    /// nothing else: genvdp.txt section 6 freezes the 68k "until the FIFO unit
    /// manages to deliver the first stacked word to its rightful owner", which at
    /// four VRAM words is the two slots of the entry at the head.
    #[test]
    fn the_fifth_write_waits_for_the_word_at_the_head() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        for _ in 0..4 {
            vdp.write_data(0xABCD);
        }
        assert!(vdp.fifo_full());
        assert_eq!(
            vdp.data_port_stall_mclk(),
            vdp.mclk_to_slots(2),
            "one VRAM word in front of the arriving write"
        );
    }

    /// A write behind a queue that is past the chip's four is behind all of it.
    ///
    /// The hold is the wait until there is a place for the write, so behind six
    /// entries three of them have to reach memory before the seventh is taken: six
    /// slots, not the two of the entry at the head alone. That depth is the one a
    /// cart reaches when the hold is short - see [`Fifo::queue`] and #545.
    #[test]
    fn a_write_behind_a_deeper_queue_waits_for_everything_in_front_of_it() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        // Six VRAM words with no clock between them: nothing has drained, so the
        // queue holds six and each still owes its two slots.
        for _ in 0..6 {
            vdp.write_data(0xABCD);
        }
        assert_eq!(vdp.fifo_len(), 6);
        assert_eq!(
            vdp.data_port_stall_mclk(),
            vdp.mclk_to_slots(6),
            "three VRAM words have to reach memory before the arriving word fits"
        );

        // And the queue the carts of #545 reach: one write behind 28 entries is
        // held for the wait of the 25 in front of the four the chip holds.
        for _ in 0..22 {
            vdp.write_data(0xABCD);
        }
        assert_eq!(vdp.fifo_len(), 28);
        assert_eq!(
            vdp.data_port_stall_mclk(),
            vdp.mclk_to_slots(50),
            "25 VRAM words, 50 slots, before the arriving word has a place"
        );
    }

    /// The wait is the slots the entries in front actually owe, so a CRAM word -
    /// one slot, not two - shortens it at the head and in the middle alike.
    #[test]
    fn the_wait_counts_the_width_of_every_entry_in_front() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 3);
        cram_write_at(&mut vdp, 0x0000);
        for _ in 0..4 {
            vdp.write_data(0x0EEE);
        }
        assert_eq!(
            vdp.data_port_stall_mclk(),
            vdp.mclk_to_slots(1),
            "four CRAM words, and the first owes one slot"
        );

        // Two more of them, so three of the six in front have to clear: three slots.
        for _ in 0..2 {
            vdp.write_data(0x0EEE);
        }
        assert_eq!(vdp.fifo_len(), 6);
        assert_eq!(vdp.data_port_stall_mclk(), vdp.mclk_to_slots(3));
    }

    /// The other half of the regression: paying the hold once takes a queue deeper
    /// than the chip back to the chip's own depth.
    ///
    /// This is the cart-level check #545 asks for, without a cart. `Vdp::write_data`
    /// performs a write that a real port would have refused, so the only thing
    /// keeping the queue to the four entries the chip has is the charge, and the
    /// charge is right exactly when paying it buys back `FIFO_DEPTH`: the hold for
    /// the write that would be the fifth is the wait for the head to go, the wait
    /// for the queue to stand at four again. The hold used to be the head entry's
    /// remaining slots, a slot short of that every second write, and the carts of
    /// `docs/compat-vdp.md` ran the queue to 28 on the shortfall.
    #[test]
    fn paying_the_hold_once_takes_a_deep_queue_back_to_the_chip() {
        // Queue depths past the four the chip has, and the entries behind them:
        // four is where the first write is refused, twelve is a burst in flight, 28
        // is the deepest queue a cart has been measured with.
        for depth in [FIFO_DEPTH + 1, 12, 28] {
            // Display on, and inside the active display rather than a blanking line:
            // a word queued for VRAM takes a slot pair there, which is the case the
            // shortfall is measured on.
            let mut vdp = active_display();
            set_reg(&mut vdp, 15, 2);
            vram_write_at(&mut vdp, 0x0000);
            for _ in 0..depth {
                vdp.write_data(0xABCD);
            }
            assert_eq!(vdp.fifo_len(), depth);

            let hold = vdp.data_port_stall_mclk();
            assert!(hold > 0, "a queue {depth} deep must hold the port at all");
            vdp.advance(hold);
            assert!(
                vdp.fifo_len() <= FIFO_DEPTH,
                "paying the {hold} mclk a write at depth {depth} is charged for left {} \
                 entries queued: the 68k is let go before it could have been, and every \
                 write like that is one more entry the chip never had room for",
                vdp.fifo_len(),
            );
        }
    }

    /// The stall is the head entry's slots along the schedule, so a CRAM word at
    /// the head - one slot, not two - holds the port for one slot's wait.
    #[test]
    fn the_stall_is_the_head_entry_s_own_wait() {
        let mut vdp = active_display();
        set_reg(&mut vdp, 15, 3);
        cram_write_at(&mut vdp, 0x0000);
        for _ in 0..4 {
            vdp.write_data(0x0EEE);
        }
        assert!(vdp.fifo_full());
        let stall = vdp.data_port_stall_mclk();
        assert!(stall > 0, "the port is full");

        let mut vram = active_display();
        set_reg(&mut vram, 15, 2);
        vram_write_at(&mut vram, 0x0000);
        for _ in 0..4 {
            vram.write_data(0xBEEF);
        }
        assert!(vram.fifo_full());
        assert!(
            stall < vram.data_port_stall_mclk(),
            "a CRAM word at the head waits one slot, a VRAM word two"
        );
    }

    /// A 68000 streaming VRAM words through the port of an active H40 line gets
    /// nine of them a line, whatever the part of the line it is in (#678).
    ///
    /// An active line has 18 external slots for the whole of itself, its blanking
    /// included - the tail sprite fetches are out there - and a VRAM word is two of
    /// them, so 9; the Sega manual's DMA capacity table says the same for the same
    /// slots, 18 bytes a line for a 68k -> VRAM transfer with the display on
    /// (`slots::Vdp::dma_slot_table`). The writer is `MOVE.W (A1)+,(A6)` and `DBF`,
    /// 22 clocks or 154 master clocks a word, which on its own would make 22 a
    /// line, and it pays [`Vdp::data_port_stall_mclk`] before each write the way
    /// the bus does. Handing it the blank line's slots in the blanking of each line
    /// let 16 a line through.
    #[test]
    fn a_68000_streaming_vram_through_active_h40_lines_gets_nine_words_a_line() {
        const LINES: u32 = 10;
        const WORD_MCLK: u32 = 154;

        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 1, 0x40);
        set_reg(&mut vdp, 12, 0x81);
        set_reg(&mut vdp, 15, 2);
        vdp.advance(100 * MCLKS_PER_LINE);
        vram_write_at(&mut vdp, 0xC000);

        // Words the port has taken while the beam is still inside the ten lines:
        // a write whose stall runs past the last of them is not one of theirs.
        let end = vdp.line + LINES as u16;
        let mut words = 0;
        loop {
            vdp.advance(vdp.data_port_stall_mclk());
            if vdp.line >= end {
                break;
            }
            vdp.write_data(0x1234);
            vdp.advance(WORD_MCLK);
            words += 1;
        }
        assert!(!vdp.in_vblank, "every line of the run was an active one");
        assert!(
            (9 * LINES..=9 * LINES + 4).contains(&words),
            "{words} VRAM words in {LINES} active H40 lines: 9 a line, and the four the \
             queue holds at the start"
        );
    }
}
