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

use crate::Vdp;
use crate::ports::{DataTarget, data_target};

/// Entries the write FIFO holds.
///
/// Four is the depth of the real thing (Nemesis on SpritesMind, and the number
/// the `VDPFIFOTesting` ROMs push against), and it is what lets a game lay down
/// a burst of palette writes and carry on without waiting for any of them.
pub(crate) const FIFO_DEPTH: usize = 4;

/// One data port write, waiting its turn at the memories.
///
/// It carries the code and the address of the write rather than reading them
/// when it is written out: the 68k sets up its next command word as soon as the
/// last word is away, so both registers have moved on long before the first
/// word lands.
#[derive(Debug, Clone, Copy)]
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
pub(crate) struct Fifo {
    /// The queue, `queue[head]` being the entry next in line. `len` is kept
    /// alongside rather than read from the slots, so nothing looks at the
    /// entries past `head + len` and this is a ring rather than a puzzle.
    queue: [Option<Entry>; FIFO_DEPTH],
    head: usize,
    len: usize,
    /// The write the port is holding while the queue is full.
    ///
    /// A write that finds no room cannot happen: the 68k is left waiting with
    /// the word on the bus until a slot frees an entry, which is
    /// [`Vdp::data_port_stall_mclk`]. Until the bus waits (M3-16, #77) the word
    /// waits here rather than being dropped, so the only write that ever goes
    /// missing is a second one made while the first is still held - which on
    /// real hardware is one of the bus locks Nemesis's FIFO tests circle, and
    /// which no ticket has taken on yet.
    waiting: Option<Entry>,
}

impl Fifo {
    /// An empty FIFO, which is what a VDP that has never been written to has.
    pub(crate) fn new() -> Self {
        Self {
            queue: [None; FIFO_DEPTH],
            head: 0,
            len: 0,
            waiting: None,
        }
    }

    /// Queue one data port write, which is what [`Vdp::write_data`] does.
    ///
    /// A write with nothing to write - a code register naming a read, whose
    /// write "is ignored" (genvdp.txt section 7) - takes no entry, so it cannot
    /// fill the FIFO or hold up the write behind it. A write that arrives when
    /// all four entries are taken goes to [`Fifo::waiting`], the port's own
    /// latch, and joins the queue behind them as each slot empties one.
    pub(crate) fn push(&mut self, entry: Entry) {
        if entry.slots == 0 {
            return;
        }
        if self.len < FIFO_DEPTH {
            self.queue[(self.head + self.len) % FIFO_DEPTH] = Some(entry);
            self.len += 1;
        } else {
            self.waiting = Some(entry);
        }
    }

    /// One access slot has come round: `None` when the entry at the head still
    /// has slots to go, and that entry when this one was its last.
    ///
    /// A slot is a position in the line, not a width of time, so whether one has
    /// come round is `slots`' question rather than this module's - see
    /// [`Vdp::fifo_clock`], which is what calls this once for each of them.
    pub(crate) fn take_slot(&mut self) -> Option<Entry> {
        if self.len == 0 {
            return None;
        }

        // `len` says there is an entry at the head, and the slots it has left
        // are what stands between it and memory: this one is spent, and only a
        // VRAM word still has another to wait for.
        if let Some(head) = self.queue[self.head].as_mut() {
            head.slots -= 1;
            if head.slots > 0 {
                return None;
            }
        }
        self.pop()
    }

    /// The next write off the queue, whatever slots it still had left.
    ///
    /// The write that [`Fifo::waiting`] holds goes into the space this frees,
    /// so a drain of the queue reaches it too and the order the 68k wrote in is
    /// the order the memories see.
    pub(crate) fn pop(&mut self) -> Option<Entry> {
        if self.len == 0 {
            return None;
        }
        let entry = self.queue[self.head].take();
        self.head = (self.head + 1) % FIFO_DEPTH;
        self.len -= 1;
        if let Some(waiting) = self.waiting.take() {
            self.queue[(self.head + self.len) % FIFO_DEPTH] = Some(waiting);
            self.len += 1;
        }
        entry
    }

    /// Nothing on the queue: status bit 9.
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// All four entries taken: status bit 8. A held write implies this, since
    /// a write is only held when there was no room for it.
    pub(crate) fn is_full(&self) -> bool {
        self.len == FIFO_DEPTH
    }

    /// The access slots the entry at the head is still waiting for, and `0`
    /// whenever the port is not being held.
    ///
    /// Only a full queue holds the port, and only the entry at the head has to
    /// clear first: see [`Vdp::data_port_stall_mclk`], which turns this many
    /// slots into the clocks the bus waits.
    pub(crate) fn head_slots(&self) -> u32 {
        if !self.is_full() {
            return 0;
        }
        u32::from(self.queue[self.head].map_or(1, |entry| entry.slots))
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
    /// data port test of #80 and #81 true without an `advance` in it; real
    /// hardware still only lets the 68k have the memories
    /// [`slots::OFF_TRANSFERS_H40`](crate::slots::OFF_TRANSFERS_H40) times an H40
    /// line, 17.3 clocks apiece, and #302 is whether to start waiting for that.
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
    /// display-off case in [`Vdp::fifo_clock`] does, for the reason its comment
    /// gives.
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
    /// "How long" is the head entry's own slots counted along the slot schedule:
    /// the entry cannot move until every slot it needs has come round, and a slot
    /// at the beam's own clock does not count. Over the active display that is two
    /// of the 18 free slots a line offers, so two of the gaps `slots` tabulates -
    /// the largest of which is the documented 32-pixel FIFO-full stall; in
    /// blanking it is under two pixels.
    ///
    /// The write is not lost meanwhile: [`Fifo::waiting`] holds it, and joins the
    /// queue as each slot frees a place. A second write made while the first is
    /// still held is the bus lock every FIFO document describes and which
    /// nothing here models yet.
    pub fn data_port_stall_mclk(&self) -> u32 {
        let slots = self.fifo.head_slots();
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
        assert!(fifo.take_slot().is_none(), "an empty queue has nothing to write");

        fifo.push(Entry::new(0x01, 0x0100, 0xBEEF));
        assert!(
            fifo.take_slot().is_none(),
            "a VRAM word is two slots, so this one writes nothing yet"
        );
        let entry = fifo.take_slot().expect("second slot");
        assert_eq!((entry.addr, entry.value), (0x0100, 0xBEEF));
        assert!(fifo.is_empty());
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
}
