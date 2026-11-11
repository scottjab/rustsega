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
//! The *positions* of the free slots of an active line are in nothing this ticket
//! could reach, so they are not invented here either: both active tables are
//! counted out of plutiedev's per-line fetch pattern, reproduced on each table,
//! one character per slot. They are marked INFERRED because that pattern is the
//! only source for where in the line the gaps fall - even though counting its
//! `~`s gives 18 slots for H40 and 16 for H32 out of lines of exactly 210 and 171
//! slots, which is the evidence that the pattern is being read right. TODO(#301).

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
#[allow(dead_code)] // first caller is the display-off wait of #302
pub(crate) const REFRESH_OFF_H40: usize = 6;
/// Transfers an H40 line with the display off carries: 198, which is
/// [`SLOTS_H40`] less two slots for each of the 6 refresh slots.
///
/// This is not what the write FIFO waits on: with the display off nothing is
/// being fetched and [`Vdp::fifo_clock`] gives a write the memories as it is
/// made. The note under [`Vdp::free_slot_indices`] says what that costs.
#[allow(dead_code)] // first caller is the display-off wait of #302
pub(crate) const OFF_TRANSFERS_H40: usize = SLOTS_H40 - (2 * REFRESH_OFF_H40);

/// Every slot of an H40 line but the [`REFRESH_BLANK_H40`] the DRAM refresh
/// takes: the 205 of [`BLANK_SLOTS_H40`], which is the schedule of a blank line.
const BLANK_H40: [u16; BLANK_SLOTS_H40] = blank_line(SLOTS_H40, REFRESH_BLANK_H40);
/// [`BLANK_H40`]'s H32 twin, the 167 of [`BLANK_SLOTS_H32`].
const BLANK_H32: [u16; BLANK_SLOTS_H32] = blank_line(SLOTS_H32, REFRESH_BLANK_H32);

/// The slots of a line that are not DRAM refresh: `0 .. total` without `count` of
/// them, those being spread evenly over it.
///
/// How many refresh slots a line has is [`REFRESH_BLANK_H40`] and
/// [`REFRESH_BLANK_H32`]; where they are is in nothing this ticket could reach,
/// so they go every `total / count` slots - and refresh is a property of the
/// whole frame rather than of the beam, so any regular spread is as good an
/// answer as another. What that buys and costs is measured against the counts,
/// which do not depend on it: [`BLANK_SLOTS_H40`] and [`BLANK_SLOTS_H32`] come
/// out of the length of this. INFERRED, TODO(#301).
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
/// (kabuto mirror; `H`=HSYNC, `s`=scroll, `A`/`B`=plane A/B, `a`/`b`=sprite,
/// `S`/`r`=the rest of the fetch machine, `~`=nothing):
///
/// ```text
/// Hssss AsaaBsbb ((A~aaBSbb)*3 AraaBSbb)*5 ~~ s*23 ~ s*11
/// ```
///
/// Counting out the `~`s along those 210 characters gives the 18 slots below:
/// three between the fetch groups of each of five groups, the `~~` pair at 173
/// and 174 where the groups end, and one at 198 out in the scroll fetches. The
/// gaps run in eights and sixteens of a slot because a fetch group is eight slots
/// wide with one gap in it.
///
/// INFERRED - see the module note and TODO(#301). What is confirmed of this line
/// is the count, 18, and the 32-pixel largest gap across its fetch groups, which
/// [`gap_is_the_documented_fifo_full_stall`] measures.
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
/// line with a fetch group less, and the tail of the line spread over two scroll
/// runs instead of one.
///
/// INFERRED as [`ACTIVE_H40`] is; there the documented 32-pixel gap covers the
/// whole line, the tail of scroll fetches included.
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
    /// anyway - [`OFF_TRANSFERS_H40`] transfers a line in H40, 17.3 clocks apiece
    /// - and #302 is whether to start modelling that wait.
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
    ///   itself - the scroll fetches out in its blanking are fetches like any
    ///   other - which is why the table quotes 18 bytes a line for a transfer
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
        let line = if self.in_vblank || !self.display_enabled() {
            if self.h40() {
                &BLANK_H40 as &[u16]
            } else {
                &BLANK_H32
            }
        } else if self.h40() {
            &ACTIVE_H40 as &[u16]
        } else {
            &ACTIVE_H32
        };
        if fill { &line[1..] } else { line }
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
    pub(crate) fn dma_mclk_to_slots(&self, fill: bool, slots: u32) -> u32 {
        let table = self.dma_slot_table(fill);
        let h40 = self.h40();
        let mut left = slots;
        let mut from = self.line_mclk;
        let mut clocks = 0;
        loop {
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
    /// of scroll fetches that both patterns end with.
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
}
