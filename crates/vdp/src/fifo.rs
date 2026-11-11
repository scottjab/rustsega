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
//! clocks depends on where the beam is, and the schedule here is the
//! placeholder the ticket asks for: one slot every [`SLOT_MCLK_ACTIVE`] clocks
//! over the active display, one every [`SLOT_MCLK_BLANK`] while blanking, and
//! no wait at all while the display is off. M4-21b (#100) replaces it with the
//! real per-mode tables.
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

/// Master clocks between access slots over the active display.
///
/// A placeholder rather than a measurement: an active line is a line of pixel
/// fetches, so slots are the exception on it. M4-21b (#100) puts the real
/// per-mode figures here.
pub(crate) const SLOT_MCLK_ACTIVE: u32 = 32;

/// Master clocks between access slots while blanking.
///
/// "About one every 2 pixels" (ticket #99), a pixel of the 420-pixel H40 line
/// being 3420 / 420 master clocks, so two of them is 16.
pub(crate) const SLOT_MCLK_BLANK: u32 = 16;

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
    /// Master clocks counted since the last access slot was taken.
    slot_clock: u32,
}

impl Fifo {
    /// An empty FIFO, which is what a VDP that has never been written to has.
    pub(crate) fn new() -> Self {
        Self {
            queue: [None; FIFO_DEPTH],
            head: 0,
            len: 0,
            waiting: None,
            slot_clock: 0,
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
        // The first entry of an idle FIFO starts its own wait: the FIFO banks
        // no slots for a write that had not arrived yet, so a write made just
        // before a slot boundary is not written at once for free.
        if self.len == 1 {
            self.slot_clock = 0;
        }
    }

    /// `mclk` master clocks of beam time pass. An idle FIFO banks nothing, so
    /// the clock does not run while there is nothing to write.
    pub(crate) fn count(&mut self, mclk: u32) {
        if self.len == 0 {
            self.slot_clock = 0;
            return;
        }
        self.slot_clock += mclk;
    }

    /// Whether an access slot of `slot_mclk` master clocks has come round. Zero
    /// is always ready, which is how the display-off VDP of
    /// [`Vdp::fifo_slot_mclk`] writes a word the moment it is written.
    pub(crate) fn slot_ready(&self, slot_mclk: u32) -> bool {
        self.slot_clock >= slot_mclk
    }

    /// One access slot: `None` when none has come round or when the entry at
    /// the head still has slots to go, and the head entry when this slot is the
    /// one that writes it.
    pub(crate) fn take_slot(&mut self, slot_mclk: u32) -> Option<Entry> {
        if self.len == 0 || !self.slot_ready(slot_mclk) {
            return None;
        }
        self.slot_clock -= slot_mclk;
        if self.queue[self.head]?.slots > 1 {
            self.queue[self.head].as_mut()?.slots -= 1;
            return None;
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
        if self.len == 0 {
            self.slot_clock = 0;
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

    /// The master clocks the port has to wait for room to put a write in,
    /// counting the slots the entry at the head still needs. See
    /// [`Vdp::data_port_stall_mclk`], which is the bus-facing version.
    pub(crate) fn stall_mclk(&self, slot_mclk: u32) -> u32 {
        if !self.is_full() {
            return 0;
        }
        // Counted from the slot that has just gone rather than the next one to
        // arrive, so this can name a slot that is already spent: it is a floor
        // on how long the 68k waits, never an overestimate of it. The exact
        // clock is M4-21b (#100)'s.
        let slots = self.queue[self.head].map_or(1, |entry| entry.slots);
        u32::from(slots) * slot_mclk
    }
}

impl Vdp {
    /// Master clocks between the external access slots, for where the beam is.
    ///
    /// The VDP lets the 68k at the memories only when it is not fetching
    /// pixels, so how long a write waits depends on what the chip is doing:
    ///
    /// * display off: nothing is being fetched, so a slot is available for
    ///   every write the moment it is made - `0`, which every drain in this
    ///   file reads as "no clock has to pass";
    /// * blanking, horizontal or vertical: [`SLOT_MCLK_BLANK`], "about one
    ///   every 2 pixels", which is why a palette load in VBlank runs at the
    ///   speed of the 68k;
    /// * the active display: [`SLOT_MCLK_ACTIVE`], the same idea with the busy
    ///   line's number.
    ///
    /// The split is a placeholder. M4-21b (#100) replaces it with the per-mode
    /// slot tables, which are positions in the line rather than one number
    /// chosen by which flag happens to be up.
    fn fifo_slot_mclk(&self) -> u32 {
        if !self.display_enabled() {
            0
        } else if self.in_hblank || self.in_vblank {
            SLOT_MCLK_BLANK
        } else {
            SLOT_MCLK_ACTIVE
        }
    }

    /// The FIFO's share of the beam moving on: `mclk` master clocks pass, and
    /// every access slot inside them writes its entry to memory.
    ///
    /// Called from [`Vdp::advance`] and from [`Vdp::write_data`], the latter
    /// with no clocks at all so that a VDP with the display off - where a slot
    /// is always ready - writes the word it has just been given now.
    pub(crate) fn fifo_clock(&mut self, mclk: u32) {
        self.fifo.count(mclk);
        let slot = self.fifo_slot_mclk();
        while !self.fifo.is_empty() && self.fifo.slot_ready(slot) {
            self.service_slot();
        }
    }

    /// One external access slot: the entry at the head of the FIFO moves a slot
    /// closer to memory, and is written out when this one was its last.
    fn service_slot(&mut self) {
        let slot = self.fifo_slot_mclk();
        if let Some(entry) = self.fifo.take_slot(slot) {
            self.write_code_word(entry.code, entry.addr, entry.value);
        }
    }

    /// Everything the FIFO is holding, written out without waiting for slots.
    ///
    /// This is what a read of the data port waits for: the 68k that wrote three
    /// words and then reads the port is reading the memory those words are on
    /// their way to, and it is the bus's own access rather than a clock, so the
    /// slots are spent in one go rather than counted out.
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
    /// The write is not lost meanwhile: [`Fifo::waiting`] holds it, and joins
    /// the queue as each slot frees a place. A second write made while the
    /// first is still held is the bus lock every FIFO document describes and
    /// which nothing here models yet.
    pub fn data_port_stall_mclk(&self) -> u32 {
        self.fifo.stall_mclk(self.fifo_slot_mclk())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::testutil::{cram_write_cmd, vram_read_cmd, vram_write_cmd};

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
    /// placeholder schedule where a write has to wait for its slots.
    ///
    /// 200 master clocks is past the front porch of the H32 line
    /// (`timing::HBLANK_CLEAR_MCLK_H32`, 100) and nowhere near the back porch or
    /// the end of the line, so no event and no render falls in the setup.
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
        assert_eq!(vdp.cram[0], 0, "no slot has come round yet");
        vdp.advance(SLOT_MCLK_ACTIVE);
        assert_eq!(vdp.cram[0], 0x0EEE, "CRAM is word wide: one slot");

        vram_write_at(&mut vdp, 0x0100);
        vdp.write_data(0xBEEF);
        vdp.advance(SLOT_MCLK_ACTIVE);
        assert_eq!(
            &vdp.vram[0x0100..0x0102],
            [0, 0],
            "one slot of a VRAM word is not a written word"
        );
        vdp.advance(SLOT_MCLK_ACTIVE);
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
        // Four VRAM words, two slots each, of the active-display schedule.
        vdp.advance(8 * SLOT_MCLK_ACTIVE);

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

        // And the wait ends when the slots arrive: after eight of them the FIFO
        // is empty and the bus is on its way again.
        vdp.advance(8 * SLOT_MCLK_ACTIVE);
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

        vdp.advance(10 * SLOT_MCLK_ACTIVE);
        assert_eq!(
            &vdp.vram[0..10],
            [0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55]
        );
    }

    /// In VBlank the slots come round in a sixteenth of a line, which is the
    /// "almost at once" of the ticket: one VRAM word, two slots of blanking.
    #[test]
    fn a_write_in_vblank_is_visible_after_a_tiny_advance() {
        let mut vdp = in_vblank();
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0100);
        vdp.write_data(0xABCD);

        vdp.advance(SLOT_MCLK_BLANK);
        assert_eq!(
            &vdp.vram[0x0100..0x0102],
            [0, 0],
            "one of the two slots, so not yet"
        );
        vdp.advance(SLOT_MCLK_BLANK);
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

    /// A slot is only taken when one has come round: a FIFO that has been
    /// counting nothing is not owed a write.
    #[test]
    fn an_idle_fifo_owes_itself_no_slots() {
        let mut fifo = Fifo::new();
        fifo.push(Entry::new(0x01, 0x0100, 0xBEEF));
        assert!(
            fifo.take_slot(SLOT_MCLK_ACTIVE).is_none(),
            "the wait starts with the write, not with the slot before it"
        );

        fifo.count(SLOT_MCLK_ACTIVE);
        assert!(
            fifo.take_slot(SLOT_MCLK_ACTIVE).is_none(),
            "a VRAM word is two slots, so this one writes nothing yet"
        );
        fifo.count(SLOT_MCLK_ACTIVE);
        let entry = fifo.take_slot(SLOT_MCLK_ACTIVE).expect("second slot");
        assert_eq!((entry.addr, entry.value), (0x0100, 0xBEEF));
        assert!(fifo.is_empty());
    }
}
