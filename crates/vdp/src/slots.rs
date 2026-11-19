//! The external access slots of a line: when the 68k may have the memories.
//!
//! The VDP owns the VRAM, CRAM and VSRAM buses and gives them up only in the
//! gaps between its own fetches, the *access slots*. Each is 32 bits wide and
//! comes round every two pixels, so a line is divided into [`SLOTS_H40`] of them
//! in H40 and [`SLOTS_H32`] in H32: "210 accesses per line in H40, 171 in H32,
//! one every 2 cycles, each 32 bits wide" (plutiedev, "SEGA Mega Drive / Genesis
//! hardware notes", kabuto mirror). Which of those slots the 68k can use depends
//! on what the fetch machine is doing at that point of the line, and that is what
//! this module is: a table of positions, per mode and per part of the line.
//!
//! Positions are pixels, and the clock half of the conversion is [`crate::hv`]'s.
//! That matters in H40, where a slot is 16 master clocks wide across the active
//! display and 20 across the slow head of the line: the schedule cannot be the
//! one number per line the placeholder of M4-21a (#99) was.
//!
//! # Confirmed and inferred
//!
//! The *counts* are documented. A blank line has 205 external slots in H40 and
//! 167 in H32 - "H32: 167+4=171, H40: 205+5=210" (Nemesis, "VDP Internals",
//! SpritesMind gendev thread 1291), which are also the Sega DMA bandwidth
//! figures. With the display off an H40 line carries 198 transfers ([`OFF_TRANSFERS_H40`]).
//!
//! The *positions* of the free slots of an active line are measured, not
//! invented: both active tables are counted out of plutiedev's per-line fetch
//! pattern, reproduced on each table, one character per slot, and that pattern
//! is kabuto's transcription of Nemesis's logic-analyzer captures - the
//! hand-drawn "Mega Drive VDP VRAM access timing - H40/H32 mode" diagrams of
//! SpritesMind gendev thread 851 (host dead; Wayback snapshot 2024-05-04).
//! Decoded against their grids, the diagrams draw the free slots at exactly the
//! indices below, one refresh per fetch group at group offset 25
//! ([`REFRESH_ACTIVE_H40`] / [`REFRESH_ACTIVE_H32`]), and the tail runs the
//! pattern notates. jgenesis's independently published schedule
//! (`jsgroth/jgenesis`, `genesis-components/src/vdp.rs`, "Adapted from ... t=851")
//! is this same table at every index, shifted 8 slots by its own choice of
//! origin, so the spacings have two readings that agree. What no source pins is
//! the anchor: which HV-counter reading slot 0 - the H scroll fetch, master
//! clock 0 here - falls on. That is #301's residue and only hardware answers
//! it; the blank-line refresh spread below is still INFERRED.
//!
//! # The display-off line: measured, and deliberately not waited for (#302)
//!
//! A line with the display off is not "always ready". It carries
//! [`OFF_TRANSFERS_H40`] transfers in H40 - 198, because the line runs
//! [`REFRESH_OFF_H40`] DRAM refreshes and each costs a 68k read two slots - which
//! is a grid about 17.3 master clocks wide, not an open door. #302 offered two
//! ways to handle that and the decision is to keep the shortcut in
//! [`Vdp::fifo_clock`] and [`Vdp::dma_slot_table`]. The reason is a rate
//! comparison, not convenience:
//!
//! * **supply.** `MCLKS_PER_LINE` master clocks carry [`OFF_TRANSFERS_H40`]
//!   transfers, so the drain can take one every 17.3 clocks.
//! * **demand.** The cheapest instruction that reaches the port is a `MOVE` into a
//!   memory operand at 8 clocks. That figure is not asserted here: it is what
//!   `m68k::ea::EaUse` documents from M68000UM Appendix C "Instruction Timing"
//!   and what the Golden suite checks for `MOVE.B D0,-(A0)`, and `m68k` charges
//!   `BUS_CYCLE_CLOCKS` per access so the emulator cannot issue a port write in
//!   fewer than two of them (fetch and write) either. At master / 7
//!   (docs/ARCHITECTURE.md, "Clocks and timing") that is 56 master clocks a word,
//!   and a VRAM word needs two transfers (`crate::fifo::Entry`, genvdp.txt
//!   section 8).
//!
//! 3420 / 56 words a line, two transfers each, is 122 against 198 available: the
//! display-off line drains a port write faster than a 68000 can make one, with
//! over a third of the line spare. That is what `fifo_clock`'s shortcut rests on,
//! and `a_display_off_line_drains_faster_than_the_68k_can_write` pins the
//! inequality so the shortcut fails loudly if the grid is ever refined or the bus
//! model gets quicker.
//!
//! What the claim does *not* cover, said rather than hidden: it is a rate bound on
//! word-at-a-time writes. A single instruction that puts several words on the port
//! (a `MOVEM` of a register list) has a better transfers-per-clock figure and
//! narrows the margin, and nothing here measured it because neither the crate's
//! bus accounting (which is per access, not per instruction) nor any document on
//! disk supports the arithmetic. It is also a *rate* argument: it says the queue
//! never becomes the limiter, not that a display-off write is free.
//!
//! Two more reasons the shortcut stayed, both measured rather than assumed:
//!
//! * the H32 display-off transfer count is in nothing, and #302 did not invent
//!   one. It is not needed: H32 lines have [`SLOTS_H32`] = 171 slots to spend, so
//!   the demand above fits whatever the refresh count turns out to be (up to 24
//!   of them), which is why the missing figure cannot change this decision.
//! * modelling it is expensive in a way the ticket did not price. Running the
//!   shortcut removed and the suite run over it costs 15 tests in two crates -
//!   11 in `vdp` (#99's `a_write_with_the_display_off_drains_at_once` and ten of
//!   the M4-02 `ports` data-port tests) and 4 in `genesis`
//!   (`bus::tests::{mirror_address_hits_vdp, reset_devices_resets_the_vdp,
//!   word_write_data_port_reaches_vdp}`, `z80_bus::tests::z80_7f00_window_reaches_vdp`),
//!   all of which write the port with the display off and no `advance`, because
//!   they are not about timing. Each would need the beam walked, and goldens would
//!   move with it.
//!
//! The DMA line is the one to look at next, and it is not this decision: with the
//! display off [`Vdp::dma_slot_table`] hands a transfer [`BLANK_SLOTS_H40`] = 205
//! slots, which is the Sega bandwidth table's own display-off figure, while
//! SpritesMind measures 198 transfers on such a line. A DMA that takes its whole
//! allowance is therefore modelling something 3.5% wider than hardware carries -
//! a demand shape this port argument does not speak to. #305 is open on the DMA
//! table (against the Sega manual) and is where that belongs; #101 shipped the DMA
//! timing itself and did not have the display-off measurement to compare to.

use crate::Vdp;
use crate::hv::{MCLKS_PER_LINE, pixel_to_mclk};

/// Access slots in an H40 line: 210, two pixels each (plutiedev, kabuto notes).
pub(crate) const SLOTS_H40: usize = 210;
/// Access slots in an H32 line: 171, two pixels each (plutiedev, kabuto notes:
/// "167+4=171").
pub(crate) const SLOTS_H32: usize = 171;

/// DRAM refresh slots on a blank line, H40: 5 ("4 fixed refresh slots in H32, 5
/// in H40" - Nemesis, "VDP Internals", SpritesMind thread 1291).
pub(crate) const REFRESH_BLANK_H40: usize = 5;
/// DRAM refresh slots on a blank line, H32: 4 (see [`REFRESH_BLANK_H40`]).
pub(crate) const REFRESH_BLANK_H32: usize = 4;
/// External access slots on a blank line - VBlank, or the blanking of an active
/// line - in H40: the 210 of [`SLOTS_H40`] less the 5 refresh slots of
/// [`REFRESH_BLANK_H40`] is the documented 205.
pub(crate) const BLANK_SLOTS_H40: usize = SLOTS_H40 - REFRESH_BLANK_H40;
/// The H32 half of [`BLANK_SLOTS_H40`]: 171 less 4 is the documented 167.
pub(crate) const BLANK_SLOTS_H32: usize = SLOTS_H32 - REFRESH_BLANK_H32;

/// DRAM refresh slots on a line with the display off, H40: 6, not the 5 of a
/// blank line, and each of them costs a 68k read *two* slots rather than the one
/// a write pays. "There are 6 refresh slots when the display is off ... a refresh
/// costs one write slot but two 68k read slots" (SpritesMind thread 1291 page 3).
pub(crate) const REFRESH_OFF_H40: usize = 6;
/// Transfers an H40 line with the display off carries: 198, which is
/// [`SLOTS_H40`] less two slots for each of the 6 refresh slots.
///
/// This is **not** what the write FIFO waits on, and #302 decided to leave it
/// that way rather than model it - see `a_display_off_line_drains_faster_than_the_68k_can_write`
/// for the rate comparison the shortcut rests on, and the display-off note in this
/// module's header for what the alternative costs.
#[allow(dead_code)] // read by tests only: the display-off decision of #302
pub(crate) const OFF_TRANSFERS_H40: usize = SLOTS_H40 - (2 * REFRESH_OFF_H40);

/// The DRAM refresh slots of an *active* line, H40, as slot indices: one per
/// fetch group, at group offset 25 - 38, 70, 102, 134, 166. Nemesis's measured
/// timing diagram draws a refresh mark exactly there (SpritesMind thread 851,
/// "Mega Drive VDP VRAM access timing - H40 mode"), and the position is what
/// makes the documented FIFO-full stall arithmetic work: "Because of the
/// refresh cycle, there's a gap of 16 access slots between external access
/// slots. So, that's a maximum wait time of 4/MCLK * 4 * 16 = 4.77us"
/// (TascoDLX, same thread). Nothing charges for these yet - the active-line
/// schedule only names the free slots - so they are read by tests only, and a
/// refresh-aware model of DMA or the FIFO would spend them (#305).
#[allow(dead_code)] // read by tests only: nothing charges for active-line refresh yet
const REFRESH_ACTIVE_H40: [u16; REFRESH_BLANK_H40] = [38, 70, 102, 134, 166];

/// [`REFRESH_ACTIVE_H40`]'s H32 twin: the H32 diagram draws four of them, at 38,
/// 70, 102, 134 (SpritesMind thread 851), which is Nemesis's "4 fixed refresh
/// slots in H32 mode" (thread 1291). One group less, so one refresh less.
#[allow(dead_code)] // read by tests only, as [`REFRESH_ACTIVE_H40`] is
const REFRESH_ACTIVE_H32: [u16; REFRESH_BLANK_H32] = [38, 70, 102, 134];

/// Every slot of an H40 line but the [`REFRESH_BLANK_H40`] the DRAM refresh
/// takes: the 205 of [`BLANK_SLOTS_H40`], which is the schedule of a blank line.
const BLANK_H40: [u16; BLANK_SLOTS_H40] = blank_line(SLOTS_H40, REFRESH_BLANK_H40);
/// [`BLANK_H40`]'s H32 twin, the 167 of [`BLANK_SLOTS_H32`].
const BLANK_H32: [u16; BLANK_SLOTS_H32] = blank_line(SLOTS_H32, REFRESH_BLANK_H32);

/// The slots of a line that are not DRAM refresh: `0 .. total` without `count` of
/// them, those being spread evenly over it.
///
/// How many refresh slots a line has is [`REFRESH_BLANK_H40`] and
/// [`REFRESH_BLANK_H32`]; where they are, no source names a blank line outright
/// and the nearest measurements disagree, so they go every `total / count`
/// slots - and refresh is a property of the whole frame rather than of the
/// beam, so any regular spread is as good an answer as another. The anchors:
/// Nemesis's active-line diagrams draw the refresh one per fetch group at
/// offset 25 ([`REFRESH_ACTIVE_H40`], [`REFRESH_ACTIVE_H32`]); kabuto says that
/// with the display disabled "the VDP still does those refresh cycles and uses
/// all other cycles as access slots"; Mask of Destiny measured six refreshes on
/// a display-off H40 line, "one refresh slot every 32 slots starting at slot
/// 37", which lands near those five places plus one at the tail slot the
/// active line keeps free, and read 204 write slots off that where this table
/// keeps the documented 205 (SpritesMind thread 1291 page 3). The count
/// conflict is #305's to settle, and landing either correction moves every
/// blank-line DMA deadline and FIFO wait. What that buys and costs is measured
/// against the counts, which do not depend on it: [`BLANK_SLOTS_H40`] and
/// [`BLANK_SLOTS_H32`] come out of the length of this. INFERRED, TODO(#301).
const fn blank_line<const N: usize>(total: usize, count: usize) -> [u16; N] {
    let mut slots = [0u16; N];
    let mut out = 0;
    let mut slot = 0;
    while slot < total {
        if !is_refresh(slot, total, count) {
            slots[out] = slot as u16;
            out += 1;
        }
        slot += 1;
    }
    slots
}

/// Whether slot `slot` of a line of `total` is one of the `count` DRAM refresh
/// slots, spread from slot 0 at `total / count` apart.
const fn is_refresh(slot: usize, total: usize, count: usize) -> bool {
    let mut n = 0;
    while n < count {
        if n * total / count == slot {
            return true;
        }
        n += 1;
    }
    false
}

/// The free access slots of an active H40 line, as slot indices: the 18 that the
/// fetch pattern leaves unassigned.
///
/// The pattern, one character per access slot, from plutiedev's hardware notes
/// (kabuto mirror), whose legend is `H`=H scroll fetch, `A`/`B`=plane A/B
/// name-table reads, `a`/`b`=plane A/B pixel rows, `S`=sprite tile/x, `s`=sprite
/// pixel data, `r`=DRAM refresh, `~`=68k access slot:
///
/// ```text
/// Hssss AsaaBsbb ((A~aaBSbb)*3 AraaBSbb)*5 ~~ s*23 ~ s*11
/// ```
///
/// Counting out the `~`s along those 210 characters gives the 18 slots below:
/// three between the fetch groups of each of five groups, the `~~` pair at 173
/// and 174 where the groups end, and one at 198 out in the tail. The gaps run
/// in eights and sixteens of a slot because a fetch group is eight slots wide
/// with one gap in it.
///
/// CONFIRMED, no longer only a reading of the notation: Nemesis's measured
/// timing diagram ("Mega Drive VDP VRAM access timing - H40 mode", SpritesMind
/// thread 851, the capture kabuto's pattern transcribes) draws exactly these
/// positions, and `the_diagram_pins_the_active_line_positions` checks the
/// match. The tail `s` runs are sprite pixel data for the next line - "all the
/// mapping data for every sprite, and most of the pattern data, is read during
/// the previous line" (Nemesis, same thread) - so they keep the blanking busy
/// and make two gaps wider than the documented FIFO-full stall without
/// contradicting it, which `the_tail_and_wrap_gaps_of_an_active_h40_line_exceed_the_documented_stall`
/// measures. What is confirmed of this line besides the positions is the count,
/// 18, and the 32-pixel largest gap across its fetch groups, which
/// [`gap_is_the_documented_fifo_full_stall`] measures. The anchor - which
/// HV-counter reading slot 0 falls on - is still hardware's, TODO(#301).
const ACTIVE_H40: [u16; 18] = [
    14, 22, 30, 46, 54, 62, 78, 86, 94, 110, 118, 126, 142, 150, 158, 173, 174, 198,
];

/// The free access slots of an active H32 line, as slot indices: the 16 the same
/// pattern leaves over,
///
/// ```text
/// Hssss AsaaBsbb ((A~aaBSbb)*3 AraaBSbb)*4 ~~ s*13 ~ s*13 ~
/// ```
///
/// which is 171 characters - [`SLOTS_H32`] - of which 16 are `~`. It is the H40
/// line with a fetch group less, and the tail spread over two sprite-pixel runs
/// instead of one.
///
/// CONFIRMED as [`ACTIVE_H40`] is: the H32 diagram of SpritesMind thread 851
/// draws the tail pair at 141 and 142, the lone one at 156 and the last at 170,
/// which is this table. There the documented 32-pixel gap covers the whole
/// line, the tail of sprite fetches included.
const ACTIVE_H32: [u16; 16] = [
    14, 22, 30, 46, 54, 62, 78, 86, 94, 110, 118, 126, 141, 142, 156, 170,
];

/// Master clock the access slot `slot` of the line is taken at: slot `n` starts at
/// pixel `2n`, both modes having two pixels to a slot.
pub(crate) fn slot_mclk(slot: usize, h40: bool) -> u32 {
    pixel_to_mclk(
        2 * u32::try_from(slot).expect("a line has at most 210 slots"),
        h40,
    )
}

impl Vdp {
    /// The access slots of the current line that the 68k can use, as slot
    /// indices.
    ///
    /// Two states, and the beam picks between them by the two blanking flags:
    ///
    /// * the active display: [`ACTIVE_H40`] or [`ACTIVE_H32`], the 18 and 16 slots
    ///   the plane and sprite fetches leave over;
    /// * blanking, horizontal or vertical: [`BLANK_H40`] / [`BLANK_H32`], every
    ///   slot of the line but the [`REFRESH_BLANK_H40`] and
    ///   [`REFRESH_BLANK_H32`] that DRAM refresh takes - the 205 and 167 of
    ///   [`BLANK_SLOTS_H40`] and [`BLANK_SLOTS_H32`]. Where the refresh slots
    ///   fall is INFERRED (see [`blank_line`]); that they are 5 and 4 is not.
    ///
    /// With the display off this is not consulted at all: nothing is being
    /// fetched, so the memories are free for the port the moment a write is made,
    /// which is what [`Vdp::fifo_clock`] does with them and what #99 pins in
    /// `a_write_with_the_display_off_drains_at_once`. Hardware charges for it
    /// anyway - [`OFF_TRANSFERS_H40`] transfers a line in H40, 17.3 clocks apiece -
    /// and #302 decided to keep the shortcut; the display-off note at the top of
    /// this module is the decision and
    /// `a_display_off_line_drains_faster_than_the_68k_can_write` is the guard.
    fn free_slot_indices(&self) -> &'static [u16] {
        if self.in_hblank || self.in_vblank {
            return if self.h40() { &BLANK_H40 } else { &BLANK_H32 };
        }
        if self.h40() { &ACTIVE_H40 } else { &ACTIVE_H32 }
    }

    /// The access slots a DMA is given for the line the beam is on.
    ///
    /// Two differences from the port's schedule of [`Vdp::free_slot_indices`],
    /// and both are the Sega DMA bandwidth table's:
    ///
    /// * the table is a property of the *line*, not of the point the beam has
    ///   reached. An active line has [`ACTIVE_H40`] free slots for the whole of
    ///   itself - the tail sprite fetches out in its blanking are fetches like
    ///   any other - which is why the table quotes 18 bytes a line for a transfer
    ///   with the display on and 205 with it off. Choosing on `in_hblank` the way
    ///   the port does would hand an active line's blanking slots to a DMA as
    ///   well and turn 18 into 46;
    /// * a fill pays one slot of every line and moves nothing: 17 and 15 active,
    ///   204 and 166 blank, one short of the transfer in all four columns.
    ///   Nothing says where in the line that slot goes, so it is the first one
    ///   and the four counts come out right. INFERRED, TODO(#101): the fill and
    ///   copy figures are what #101 was given rather than something a page was
    ///   found for, and #305 is checking them against the Sega manual.
    ///
    /// A line with the display off is a blank one here: nothing is being fetched
    /// so [`BLANK_H40`] is the schedule, which is the same reading
    /// [`Vdp::fifo_clock`] makes. Hardware charges a little more for it -
    /// [`OFF_TRANSFERS_H40`]'s 198 rather than 205 - and #302 is where that is
    /// paid.
    pub(crate) fn dma_slot_table(&self, fill: bool) -> &'static [u16] {
        let line = self.dma_line_slots(self.in_vblank || !self.display_enabled());
        if fill { &line[1..] } else { line }
    }

    /// The DMA's slots of a line the walk of [`Vdp::dma_mclk_to_slots`] has
    /// reached, which [`Vdp::dma_slot_table`] cannot answer because it keys on
    /// `in_vblank` and that is the chip's present tense.
    ///
    /// VBlank is the span [`Vdp::start_line`] keeps the flag up over: `vblank_line`
    /// through to but not including the last line of the frame. The display is read
    /// as it stands, which is all a prediction of a line the beam has not got to
    /// can do, and it is the same reading [`Vdp::dma_slot_table`] makes.
    fn dma_line_slots_at(&self, line: u16, fill: bool) -> &'static [u16] {
        let blank = !self.display_enabled()
            || (line >= self.vblank_line() && line + 1 < self.frame_lines());
        let table = self.dma_line_slots(blank);
        if fill { &table[1..] } else { table }
    }

    /// The two DMA lines of the mode, blank or drawn.
    fn dma_line_slots(&self, blank: bool) -> &'static [u16] {
        match (blank, self.h40()) {
            (true, true) => &BLANK_H40 as &[u16],
            (true, false) => &BLANK_H32 as &[u16],
            (false, true) => &ACTIVE_H40 as &[u16],
            (false, false) => &ACTIVE_H32 as &[u16],
        }
    }

    /// How many of the DMA's access slots the master clocks in `(from, to]` of
    /// the current line take: `to` is the beam's next clock and never past the
    /// end of the line, because [`Vdp::advance`] runs the DMA one segment of a
    /// line at a time.
    pub(crate) fn dma_slots_in(&self, fill: bool, from: u32, to: u32) -> u32 {
        let h40 = self.h40();
        u32::try_from(
            self.dma_slot_table(fill)
                .iter()
                .filter(|&&slot| {
                    let at = slot_mclk(usize::from(slot), h40);
                    at > from && at <= to
                })
                .count(),
        )
        .expect("a line has at most 210 slots")
    }

    /// Master clocks from the beam to the `slots`th DMA access slot in front of
    /// it, over as many lines as it takes: how long a transfer of `slots` still
    /// has to run, and so how long the 68k stays frozen
    /// ([`Vdp::dma_68k_stall_mclk`]).
    ///
    /// The loop is one turn per line the transfer spans - at most 640 of them for
    /// the whole of VRAM, and every table has slots in it, so it always makes the
    /// `left` it is counting down smaller.
    ///
    /// Each turn is counted against **that line's** table, which is what makes the
    /// figure a real deadline rather than an estimate. Held to the table of the line
    /// the walk started on, a transfer begun in blanking was timed at 205 slots a
    /// line for all of them while [`Vdp::dma_clock`] was spending them at the 18 an
    /// active line leaves free, so the 68000 was let off the bus long before the
    /// transfer was over - far enough ahead of it that the cart's next control write,
    /// a register write, cleared the code register (genvdp.txt section 7) and the
    /// rest of the transfer wrote nothing. #536 is `Sonic & Knuckles` losing the last
    /// 1536 bytes of a 7232-slot DMA to exactly that.
    pub(crate) fn dma_mclk_to_slots(&self, fill: bool, slots: u32) -> u32 {
        let h40 = self.h40();
        let mut left = slots;
        let mut line = self.line;
        let mut from = self.line_mclk;
        let mut clocks = 0;
        loop {
            // The line the beam is on is answered by `dma_slot_table` because the
            // chip is already saying whether it is blank; the ones after it are what
            // the walk is for.
            let table = if line == self.line {
                self.dma_slot_table(fill)
            } else {
                self.dma_line_slots_at(line, fill)
            };
            let mut ahead = 0;
            for &slot in table {
                let at = slot_mclk(usize::from(slot), h40);
                if at <= from {
                    continue;
                }
                ahead += 1;
                if ahead == left {
                    return clocks + (at - from);
                }
            }
            // Every slot this line had is spent on it: carry the rest of the
            // transfer, and the rest of the line's clocks, into the next one.
            left -= ahead;
            clocks += MCLKS_PER_LINE - from;
            from = 0;
            line = if line + 1 >= self.frame_lines() {
                0
            } else {
                line + 1
            };
        }
    }

    /// Master clocks from the beam to the `slots`th free access slot in front of
    /// it, counting over as many lines as it takes.
    ///
    /// The slot the beam is standing on is not one it takes: a write made at the
    /// clock of a free slot is written by the next one, which is what the "the
    /// first entry of an idle FIFO starts its own wait" rule of `fifo` comes to
    /// once the slots have positions in the line.
    pub(crate) fn mclk_to_slots(&self, slots: u32) -> u32 {
        let mut reached = 0;
        for _ in 0..slots {
            let mut next = u32::MAX;
            for ahead in self.slot_clocks_ahead() {
                let mut candidate = ahead;
                while candidate <= reached {
                    candidate += MCLKS_PER_LINE;
                }
                next = next.min(candidate);
            }
            reached = next;
        }
        reached
    }

    /// How many free access slots the `mclk` master clocks in front of the beam
    /// cover, over as many lines as they run across.
    ///
    /// `mclk` of 0 covers none, which is [`Vdp::write_data`]: a write has no clocks
    /// in front of it, so it never writes itself out.
    pub(crate) fn slots_ahead(&self, mclk: u32) -> u32 {
        if mclk == 0 {
            return 0;
        }
        self.slot_clocks_ahead()
            .map(|ahead| {
                if ahead <= mclk {
                    1 + (mclk - ahead) / MCLKS_PER_LINE
                } else {
                    0
                }
            })
            .sum()
    }

    /// Master clocks from the beam to each free access slot in front of it,
    /// measured into the first line the beam is still in and so in `1..=3420`: a
    /// slot at the beam's own clock is a whole line away, not none away.
    fn slot_clocks_ahead(&self) -> impl Iterator<Item = u32> + use<'_> {
        let from = self.line_mclk;
        self.free_slot_indices()
            .iter()
            .map(move |&slot| slot_mclk(usize::from(slot), self.h40()))
            .map(move |at| (at + MCLKS_PER_LINE - from) % MCLKS_PER_LINE)
            .map(|ahead| if ahead == 0 { MCLKS_PER_LINE } else { ahead })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hv::{PIXELS_H32, PIXELS_H40};

    /// A beam to ask questions of: the display on, in `h40` mode, mid-active-line
    /// so that the schedule is the active table rather than the grid, at the clock
    /// the test wants.
    fn beam(h40: bool, line_mclk: u32) -> Vdp {
        let mut vdp = Vdp::new(false);
        vdp.write_register(1, 0x40);
        if h40 {
            vdp.write_register(12, 0x81);
        }
        vdp.in_hblank = false;
        vdp.in_vblank = false;
        vdp.set_position(0, line_mclk);
        vdp
    }

    /// A beam in VBlank, which is where the grid rather than the active table is
    /// what the schedule offers.
    fn blank(h40: bool) -> Vdp {
        let mut vdp = Vdp::new(false);
        vdp.write_register(1, 0x40);
        if h40 {
            vdp.write_register(12, 0x81);
        }
        vdp.in_vblank = true;
        vdp
    }

    /// The widest gap between neighbouring free slots of an active line, over the
    /// fetch groups: the table without its last entry, which sits out in the tail
    /// of sprite fetches that both patterns end with.
    fn widest_active_gap(h40: bool) -> u32 {
        let active: &[u16] = if h40 {
            &ACTIVE_H40[..ACTIVE_H40.len() - 1]
        } else {
            &ACTIVE_H32[..ACTIVE_H32.len() - 1]
        };
        let clocks: Vec<u32> = active
            .iter()
            .map(|&slot| slot_mclk(usize::from(slot), h40))
            .collect();
        clocks
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .max()
            .expect("an active line has free slots")
    }

    /// The counts the ticket gives, which are the one thing about an active line
    /// that is documented: 18 external slots in H40 and 16 in H32, on lines of
    /// 210 and 171 slots.
    #[test]
    fn an_active_line_has_eighteen_free_slots_in_h40_and_sixteen_in_h32() {
        assert_eq!(ACTIVE_H40.len(), 18);
        assert_eq!(ACTIVE_H32.len(), 16);
        assert_eq!(SLOTS_H40, 210, "one every two pixels of the 420");
        assert_eq!(SLOTS_H32, 171, "one every two pixels of the 342");
        assert_eq!(2 * SLOTS_H40, usize::try_from(PIXELS_H40).unwrap());
        assert_eq!(2 * SLOTS_H32, usize::try_from(PIXELS_H32).unwrap());
    }

    /// Every table is a set of slots of its own line, ascending and inside it,
    /// which is what lets the schedule look one up by clock.
    #[test]
    fn the_slot_tables_are_ascending_and_inside_their_lines() {
        for (slots, slots_per_line, name) in [
            (&ACTIVE_H40 as &[u16], SLOTS_H40, "active H40"),
            (&ACTIVE_H32 as &[u16], SLOTS_H32, "active H32"),
            (&BLANK_H40 as &[u16], SLOTS_H40, "blank H40"),
            (&BLANK_H32 as &[u16], SLOTS_H32, "blank H32"),
        ] {
            let mut previous: Option<u16> = None;
            for &slot in slots {
                assert!(usize::from(slot) < slots_per_line, "{name}: slot {slot}");
                assert!(
                    previous.is_none_or(|last| slot > last),
                    "{name}: {slots:?} is not ascending at {slot}"
                );
                previous = Some(slot);
            }
        }
    }

    /// A blank line is 205 external slots in H40 and 167 in H32: the line less
    /// the 5 and 4 refresh slots of "VDP Internals" (SpritesMind thread 1291),
    /// which are also what the Sega DMA bandwidth table quotes. The tables are
    /// that long because the counts are those, and no refresh slot is missing
    /// from or repeated in them.
    #[test]
    fn a_blank_line_is_205_slots_in_h40_and_167_in_h32() {
        assert_eq!(BLANK_SLOTS_H40, 205);
        assert_eq!(BLANK_SLOTS_H32, 167);
        assert_eq!(BLANK_H40.len(), BLANK_SLOTS_H40);
        assert_eq!(BLANK_H32.len(), BLANK_SLOTS_H32);
        assert_eq!(
            BLANK_H40
                .iter()
                .map(|&slot| usize::from(slot))
                .collect::<std::collections::HashSet<usize>>()
                .len(),
            BLANK_SLOTS_H40,
            "every slot of a blank line once and no more"
        );
    }

    /// With the display off an H40 line carries 198 transfers: the 6 refresh slots
    /// of SpritesMind thread 1291 page 3, two slots each for a 68k read.
    #[test]
    fn a_display_off_h40_line_carries_198_transfers() {
        assert_eq!(OFF_TRANSFERS_H40, 198);
    }

    /// The inequality [`Vdp::fifo_clock`]'s display-off shortcut rests on (#302):
    /// a line with the display off drains port writes faster than a 68000 can
    /// make them, so the grid of [`OFF_TRANSFERS_H40`] transfers never becomes the
    /// thing a write waits for.
    ///
    /// Both sides are the crate's own numbers. Demand is 8 clocks a write - the
    /// `MOVE`-into-memory figure `m68k::ea::EaUse` documents from M68000UM
    /// Appendix C "Instruction Timing" and the Golden suite checks, and the floor
    /// this emulator cannot beat either, since `m68k` charges `BUS_CYCLE_CLOCKS`
    /// per access and a write costs a fetch as well as the write. At master / 7
    /// that is 56 master clocks; a VRAM word then costs the drain two transfers
    /// (`crate::fifo::Entry`, genvdp.txt section 8), the widest case there is.
    ///
    /// It is a guard, not decoration: it fails if the display-off grid is refined
    /// downward or the bus model gets quicker, and either of those is exactly the
    /// moment the shortcut stops being equivalent to waiting.
    #[test]
    fn a_display_off_line_drains_faster_than_the_68k_can_write() {
        /// Master clocks in one 68000 clock: "68000: master / 7"
        /// (docs/ARCHITECTURE.md, "Clocks and timing").
        const MCLK_PER_68K: usize = 7;
        /// Clocks of the cheapest instruction that reaches the port; see above.
        const MOVE_TO_MEMORY: usize = 8;
        /// Transfers one VRAM word costs the drain.
        const VRAM_WORD: usize = 2;

        let writes_a_line = MCLKS_PER_LINE as usize / (MOVE_TO_MEMORY * MCLK_PER_68K);
        let demand = VRAM_WORD * writes_a_line;
        assert!(
            demand <= OFF_TRANSFERS_H40,
            "a 68000 writing VRAM words as fast as it can wants {demand} transfers a \
             line; a display-off H40 line carries {OFF_TRANSFERS_H40}"
        );

        // H32's display-off count is in nothing (the module header says so), so
        // what gets checked is that the decision cannot depend on it: the demand
        // fits however many refresh slots the mode turns out to spend, up to 24 of
        // them - far past the 6 an H40 line is measured at.
        assert!(
            demand <= SLOTS_H32 - 2 * 24,
            "{demand} against an H32 line of {SLOTS_H32} slots, even charging 24 refreshes"
        );
    }

    /// The widest gap between two free slots across the fetch groups of an active
    /// line is 32 pixels: 256 master clocks of 8 in H40 and 320 of 10 in H32,
    /// which is the 4.77 us and the 5.96 us of "VDP Internals" (SpritesMind thread
    /// 1291) to the clock. It is what bounds how long a full FIFO holds the port.
    #[test]
    fn gap_is_the_documented_fifo_full_stall() {
        assert_eq!(widest_active_gap(true), 256, "32 pixels of 8 clocks, H40");
        assert_eq!(widest_active_gap(false), 320, "32 pixels of 10 clocks, H32");
    }

    /// A slot is two pixels across in both modes, so the grid is the pixel clock
    /// read every other pixel: 16 master clocks apart in H40 across the active
    /// display, 20 in H32, and 210 of them to the last clock of the line.
    #[test]
    fn slots_are_two_pixels_across_in_both_modes() {
        assert_eq!(slot_mclk(40, false) - slot_mclk(39, false), 20);
        assert_eq!(
            slot_mclk(200, true) - slot_mclk(199, true),
            16,
            "both of them inside the fast part of the H40 line"
        );
        assert_eq!(slot_mclk(0, true), 0, "slot 0 is the head of the line");
        assert_eq!(slot_mclk(SLOTS_H40 - 1, true) + 16, MCLKS_PER_LINE);
        assert_eq!(slot_mclk(SLOTS_H32 - 1, false) + 20, MCLKS_PER_LINE);
    }

    /// The beam's own slot is not one it takes: `mclk_to_slots` and `slots_ahead`
    /// look strictly in front, which is the rule that keeps a write from being
    /// written by the slot it arrived on top of.
    #[test]
    fn a_slot_at_the_beam_s_own_clock_is_not_one_it_takes() {
        let first = usize::from(ACTIVE_H32[0]);
        let at_slot = beam(false, slot_mclk(first, false));
        assert_eq!(at_slot.slots_ahead(0), 0, "no clocks, no slots");
        assert_eq!(at_slot.slots_ahead(1), 0, "and one clock is not a line");
        assert_eq!(
            at_slot.mclk_to_slots(1),
            slot_mclk(usize::from(ACTIVE_H32[1]), false) - slot_mclk(first, false),
            "the next free slot of the line along"
        );

        let at_the_end = beam(false, MCLKS_PER_LINE - 1);
        assert_eq!(
            at_the_end.mclk_to_slots(1),
            slot_mclk(first, false) + 1,
            "and past the last slot of a line comes the first of the next"
        );
    }

    /// `slots_ahead` counts the table over the clocks it is given, and keeps
    /// counting across as many lines as they run across.
    #[test]
    fn slots_ahead_counts_the_table_over_the_clocks_it_is_given() {
        let start = beam(true, 0);
        assert_eq!(start.slots_ahead(3420), 18, "one whole H40 line of them");
        assert_eq!(start.slots_ahead(3 * 3420), 54, "and three lines of them");
        assert_eq!(
            start.slots_ahead(start.mclk_to_slots(4)),
            4,
            "the fourth slot of the line is four ahead of the head of it"
        );
        assert_eq!(
            start.slots_ahead(start.mclk_to_slots(4) - 1),
            3,
            "and one clock short of that is three"
        );

        assert_eq!(
            blank(false).slots_ahead(MCLKS_PER_LINE),
            u32::try_from(BLANK_SLOTS_H32).unwrap(),
            "a blank H32 line of the grid less its 4 refresh slots"
        );
        assert_eq!(
            blank(true).slots_ahead(MCLKS_PER_LINE),
            u32::try_from(BLANK_SLOTS_H40).unwrap(),
            "and 205 for a blank H40 one"
        );
    }

    /// An active H40 line's first free slot is the 14th of the line, and on the
    /// exact pixel clock that is master clock 272 rather than the 280 a line of
    /// 8-clock pixels would put it at: the slow head of the line is not finished
    /// by pixel 28.
    #[test]
    fn the_first_free_slot_of_an_active_h40_line_is_at_clock_272() {
        assert_eq!(slot_mclk(usize::from(ACTIVE_H40[0]), true), 272);
        assert_eq!(slot_mclk(usize::from(ACTIVE_H32[0]), false), 280);
        assert_eq!(beam(true, 0).mclk_to_slots(1), 272);
        assert_eq!(beam(false, 0).mclk_to_slots(1), 280);
    }

    /// The active tables' positions are not just self-consistent arithmetic: they
    /// are the positions Nemesis's measured timing diagram draws. The diagram is
    /// the logic-analyzer capture behind both the fetch pattern and this module
    /// (SpritesMind thread 851, "Mega Drive VDP VRAM access timing - H40/H32
    /// mode"), and decoded against its grid it draws the three free slots of each
    /// fetch group at group offsets 1, 9 and 17, the tail pair at 173 and 174
    /// with the lone one at 198 in H40, and 141, 142, 156 and 170 in H32. Pinned
    /// literally, so that regenerating the tables has to confront the diagram.
    #[test]
    fn the_diagram_pins_the_active_line_positions() {
        assert_eq!(
            ACTIVE_H40,
            [
                14, 22, 30, 46, 54, 62, 78, 86, 94, 110, 118, 126, 142, 150, 158, 173, 174, 198
            ]
        );
        assert_eq!(
            ACTIVE_H32,
            [
                14, 22, 30, 46, 54, 62, 78, 86, 94, 110, 118, 126, 141, 142, 156, 170
            ]
        );
    }

    /// The refresh cycles of an active line fall one per fetch group, at group
    /// offset 25: 38 and every 32 slots after it, one less in H32 for want of a
    /// fifth group. That is where Nemesis's diagrams draw the refresh mark
    /// (SpritesMind thread 851), and it is what makes the documented FIFO-full
    /// stall arithmetic work: "Because of the refresh cycle, there's a gap of 16
    /// access slots between external access slots. So, that's a maximum wait time
    /// of 4/MCLK * 4 * 16 = 4.77us" (TascoDLX, same thread). The 16-slot gap is
    /// the one from the third `~` of a group to the first of the next, and the
    /// refresh sits inside it - except at the group-to-tail transition, where the
    /// gap to the tail pair is a slot shorter.
    #[test]
    fn active_line_refresh_slots_fall_one_per_fetch_group() {
        assert_eq!(REFRESH_ACTIVE_H40, [38, 70, 102, 134, 166]);
        assert_eq!(REFRESH_ACTIVE_H32, [38, 70, 102, 134]);

        let containing_gap = |refresh: u16, active: &[u16], h40: bool| {
            let before = active
                .iter()
                .rev()
                .find(|&&s| s < refresh)
                .expect("a free slot before every refresh slot");
            let after = active
                .iter()
                .find(|&&s| s > refresh)
                .expect("a free slot after every refresh slot");
            slot_mclk(usize::from(*after), h40) - slot_mclk(usize::from(*before), h40)
        };

        for &refresh in &REFRESH_ACTIVE_H40[..4] {
            assert_eq!(
                containing_gap(refresh, &ACTIVE_H40, true),
                256,
                "refresh slot {refresh} sits inside a 32-pixel gap"
            );
        }
        assert_eq!(
            containing_gap(REFRESH_ACTIVE_H40[4], &ACTIVE_H40, true),
            240,
            "the last H40 refresh sits in the 15-slot gap to the tail pair"
        );
        for &refresh in &REFRESH_ACTIVE_H32[..3] {
            assert_eq!(
                containing_gap(refresh, &ACTIVE_H32, false),
                320,
                "refresh slot {refresh} sits inside a 32-pixel gap"
            );
        }
        assert_eq!(
            containing_gap(REFRESH_ACTIVE_H32[3], &ACTIVE_H32, false),
            300,
            "the last H32 refresh sits in the 15-slot gap to the tail pair"
        );
    }

    /// The tail of an active H40 line really is two gaps wider than the
    /// documented FIFO-full stall, and that contradicts nothing because the
    /// stall figure is scoped to the active display: "the maximal wait time when
    /// VDP Write FIFO is full is the maximal delay between 2 external access
    /// slots, which, during active display, is exactly 32 pixels" (Eke,
    /// SpritesMind thread 1291 page 1), and the freeze itself "only occurs
    /// during the active display period" (genvdp.txt section 6). The wide gaps
    /// are measured too, not just notated: "according to the new data, the
    /// largest gap is actually 26 slots, and half of that is during H-sync with
    /// the overall slower clock" (TascoDLX, thread 851) - the 26 slot periods
    /// from the lone tail slot 198 around to the next line's 14, whose second
    /// half crosses this model's slow line head. H32's tail gaps stay under its
    /// active 32-pixel gap, so its widest gap is the documented one whole-line.
    #[test]
    fn the_tail_and_wrap_gaps_of_an_active_h40_line_exceed_the_documented_stall() {
        assert_eq!(
            slot_mclk(198, true) - slot_mclk(174, true),
            384,
            "24 fast-pixel slot periods over the blanking, 48 px"
        );
        assert_eq!(
            MCLKS_PER_LINE - slot_mclk(198, true) + slot_mclk(14, true),
            464,
            "26 slot periods, the wrap TascoDLX names, across the slow head"
        );
        assert_eq!(slot_mclk(156, false) - slot_mclk(142, false), 280);
        assert_eq!(
            MCLKS_PER_LINE - slot_mclk(170, false) + slot_mclk(14, false),
            300,
            "H32's wrap stays under its 320-clock active gap"
        );
    }
}
