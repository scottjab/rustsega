//! Beam timing: walking the raster, and the interrupts that fall out of it.
//!
//! [`Vdp::advance`] is what moves the beam that [`Vdp::set_position`] used to
//! be the only way of moving (M4-05, #83), and it is what drives the status
//! flags `status` only packs (M4-04, #82). The scheduler of M3-14 (#75) is the
//! intended caller: it runs a line of the machine and hands the clocks here,
//! and takes the interrupts from [`Vdp::pending_irq`] / [`Vdp::ack_irq`] and
//! the end-of-frame signal from [`Vdp::frame_done`].
//!
//! Work is done at line boundaries and at the two points of the line where the
//! hardware changes state, the back porch and the H-blank release, so the cost
//! of `advance` is a few events a line rather than one iteration a clock.
//!
//! The exact clock of the HINT and VINT pulses is out of scope here (M4-23).
//! The write FIFO is not: every clock the beam moves takes the FIFO's external
//! access slots with it, so `advance` hands its clocks to `fifo::Vdp::fifo_clock`
//! before it looks at line events (M4-21a, #99, timed by `slots` for M4-21b #100).
//! A DMA in flight is paid out of the same slots, one segment of the line at a
//! time (`dma::Vdp::dma_clock`, M4-22 #101).

use crate::hv::MCLKS_PER_LINE;
use crate::{InterlaceMode, Vdp};

/// Interrupt level 6: the vertical interrupt (genvdp.txt section 4
/// "Interrupts": "The only hardware interrupts available are 2, 4, and 6", and
/// section 4 "Vertical interrupts" makes the vertical one a level 6).
pub(crate) const IRQ_VINT: u8 = 6;
/// Interrupt level 4: the line interrupt (genvdp.txt section 4, as above).
pub(crate) const IRQ_HINT: u8 = 4;

/// Lines per frame, NTSC: 262, `0..=261` (genvdp.txt section 5: the V counter
/// covers "the entire 262 line display"; `docs/ARCHITECTURE.md` says the same).
const FRAME_LINES_NTSC: u16 = 262;
/// Lines per frame, PAL: 313 (`docs/ARCHITECTURE.md`: "262 lines per frame
/// (313 PAL)"; `hv` uses the same number for the V counter table).
const FRAME_LINES_PAL: u16 = 313;

/// Master clocks into the line at which status bit 2 (HB) goes up, H40.
///
/// genvdp.txt section 6: d2 "is set at H counter cycle E4h and cleared at H
/// counter cycle 08h", with the caveat "the H counter values are very likely
/// different for 32-cell and interlaced displays; they were taken from a test
/// with a 40-cell screen". The value used here is the H counter 0xB3 the ticket
/// quotes rather than genvdp's 0xE4; they are six pixels apart, both in the back
/// porch (0xE4 is the point the H40 counter jumps to, see [`crate::hv`]), and
/// the exact position is M4-23's to settle.
///
/// Converting the counter to a clock: the counter holds for two pixels
/// (`hv::h_count`), and a pixel of the 420-pixel H40 line is `3420 / 420`
/// master clocks, so 0xB3 is pixel 358 and 358 * 3420 / 420 is 2915.
const HBLANK_SET_MCLK_H40: u32 = 2915;
/// Master clocks into the line at which status bit 2 goes up, H32.
///
/// genvdp.txt gives no H32 number (see [`HBLANK_SET_MCLK_H40`] for why it could
/// not); this is the ticket's 0x93, the last counter value before the H32 jump,
/// which is pixel 294 and 2940 master clocks at ten to the pixel.
const HBLANK_SET_MCLK_H32: u32 = 2940;
/// Master clocks into the line at which status bit 2 goes down, H40: genvdp.txt
/// section 6's "cleared at H counter cycle 08h" is 0x05 in the ticket, and both
/// are near the front porch - 0x05 is pixel 10 and 10 * 3420 / 420 is 81.
const HBLANK_CLEAR_MCLK_H40: u32 = 81;
/// Master clocks into the line at which status bit 2 goes down, H32: the ticket's
/// 0x05 again, ten master clocks to the pixel, so 100.
pub(crate) const HBLANK_CLEAR_MCLK_H32: u32 = 100;

/// How long the VDP holds the Z80's interrupt line at VBlank, in master clocks.
///
/// Inferred. genvdp.txt is about the 68000 and says so at the top of its
/// interrupt section - "The VDP generates all interrupts for the 68000" - and
/// never mentions the Z80's VBlank interrupt at all; 2560 clocks (about 1.6 ms,
/// roughly three quarters of a line) is the figure the ticket brings, from the
/// plutiedev hardware notes that are not part of this checkout.
const Z80_INT_MCLK: u32 = 2560;

impl Vdp {
    /// Advance the beam by `mclk` master clocks, running the per-line work and
    /// the line events that fall inside the jump.
    ///
    /// The work is done at the moment the beam reaches it, so a call that stops
    /// halfway across a line leaves VBlank not yet started, and the clocks that
    /// finish the line are what start it. Nothing is done per clock: a line has
    /// three points of interest, the H-blank release, the back porch (which is
    /// also where the HINT is raised, see [`Vdp::hint_step`]) and the line end.
    ///
    /// Crossing into a line runs [`Vdp::start_line`] straight away, so the beam
    /// never sits on a line whose VBlank and HINT-counter state is not applied.
    /// [`Vdp::set_position`] is the exception: it jumps the beam without running
    /// that work, and is for the scheduler and tests that start a frame rather
    /// than move within one.
    pub fn advance(&mut self, mclk: u32) {
        // The FIFO's slots come round on the beam too, and a slot is the only
        // thing that empties it: `slots` is what says how many of them fit in
        // these clocks and `fifo` is what writes them out.
        self.fifo_clock(mclk);

        let mut left = mclk;
        while left > 0 {
            if !self.line_started {
                self.start_line();
            }

            let stop = self.next_event_mclk();
            let step = (stop - self.line_mclk).min(left);
            // A DMA in flight is spent from the same slots the FIFO drains on,
            // segment by segment so that the line they are counted against is the
            // line the beam is on (`dma::Vdp::dma_clock`).
            self.dma_clock(self.line_mclk, self.line_mclk + step);
            self.line_mclk += step;
            left -= step;

            if self.line_mclk != stop {
                // Out of clocks before the next event: the rest of the wait is
                // the caller's next `advance`.
                break;
            }
            if stop == MCLKS_PER_LINE {
                self.finish_line();
            } else {
                self.line_event();
            }
        }
    }

    /// Move the beam to the first line of VBlank: where a chip that is being
    /// started, or restarted, is rather than where one that has been advanced got
    /// to (#310).
    ///
    /// [`Vdp::new`] leaves a chip at line 0 clock 0, and a host that resets a
    /// console into that state then runs the 68000's first instruction 40 master
    /// clocks inside the active display of line 0 - past the point a line is drawn
    /// from (the latch of [`crate::latch`], at [`HBLANK_CLEAR_MCLK_H32`] and
    /// [`HBLANK_CLEAR_MCLK_H40`]) - so a ROM's start-up register writes miss line 0
    /// and the first frame of the run is drawn in the mode the chip reset into.
    /// Hardware resets into blanking, where a write is in time for the line that
    /// follows; this is that position.
    ///
    /// The line's own blanking state is applied as [`Vdp::start_line`] would have
    /// left it, and [`Vdp::line_started`] with it, so the first [`Vdp::advance`]
    /// does not do it a second time. The two event flags of that line are not, and
    /// that is the point: no vertical interrupt has occurred and no frame has been
    /// drawn since this chip was reset, so `vint_pending` stays down and so does
    /// [`Vdp::frame_ready`]. A scheduler waiting on the latter cannot tell a beam
    /// parked with the flag already up from one that has drawn a frame, and hands
    /// the first frame of the run to whoever asks first - after which the call that
    /// should have drawn it finds the flag spent and returns having done nothing.
    pub fn start_in_vblank(&mut self) {
        self.line = self.vblank_line();
        self.line_mclk = 0;
        // The same three writes `start_line` makes on arriving here: a line begins
        // in the horizontal blanking it lets go of at [`Vdp::hblank_clear_mclk`],
        // and this line is the first of VBlank.
        self.line_started = true;
        self.in_hblank = true;
        self.in_vblank = true;
    }

    /// The first point in [`Vdp::line`] after the beam's current clock.
    ///
    /// The release always precedes the back porch in a line (100 clocks, 2940
    /// in H32), so the two are tested in that order and the first one ahead of
    /// the beam wins.
    fn next_event_mclk(&self) -> u32 {
        for event in [self.hblank_clear_mclk(), self.hblank_set_mclk()] {
            if event > self.line_mclk {
                return event;
            }
        }
        MCLKS_PER_LINE
    }

    /// The back-porch point of the line, where status bit 2 goes up.
    #[inline]
    fn hblank_set_mclk(&self) -> u32 {
        if self.h40() {
            HBLANK_SET_MCLK_H40
        } else {
            HBLANK_SET_MCLK_H32
        }
    }

    /// The front-porch point of the line, where status bit 2 comes down.
    #[inline]
    fn hblank_clear_mclk(&self) -> u32 {
        if self.h40() {
            HBLANK_CLEAR_MCLK_H40
        } else {
            HBLANK_CLEAR_MCLK_H32
        }
    }

    /// The line the beam is on after [`Vdp::line`], wrapping at the end of the
    /// frame: the line the sprite search of this one leaves its list for (#311).
    #[inline]
    pub(crate) fn next_line(&self) -> u16 {
        if self.line + 1 >= self.frame_lines() {
            0
        } else {
            self.line + 1
        }
    }

    /// The line the beam has just finished: draw it, then move along.
    fn finish_line(&mut self) {
        if usize::from(self.line) < self.active_height() {
            self.render_line(self.line);
        }

        self.line_mclk = 0;
        self.line = self.next_line();
        self.start_line();
    }

    /// The beam has entered [`Vdp::line`]: the state that is decided once a line
    /// rather than once a clock.
    ///
    /// genvdp.txt section 4 "Line interrupts" is the HINT counter's load rule
    /// and is read literally here: the counter is loaded on "Line zero of the
    /// frame" and on "Lines 225 through 261. (note that line 224 is not
    /// included)", which is every line of VBlank except the one VBlank starts
    /// on. The blanking flags and the vertical interrupt follow genvdp.txt
    /// section 6 and section 4, as the field comments in `lib.rs` quote them.
    fn start_line(&mut self) {
        self.line_started = true;
        // A line always begins inside the blanking window, which only lets go at
        // [`Vdp::hblank_clear_mclk`] of this line.
        self.in_hblank = true;

        if self.line == 0 {
            self.frame_start();
        }

        if self.line == 0 || self.line > self.vblank_line() {
            self.hint_counter = self.hint_reload();
        }

        if self.line == self.vblank_line() {
            // "Set on line E0h ... and cleared on line FFh" for the flag, and
            // "a level 6 interrupt will occur when the V counter is at line
            // E0h" for the interrupt, both genvdp.txt sections 6 and 4. `frame_ready`
            // is the scheduler's copy of the same moment.
            self.in_vblank = true;
            self.vint_pending = true;
            self.frame_ready = true;
        } else if self.line == self.frame_lines() - 1 {
            // genvdp.txt clears the flag partway through the last line, at H
            // counter cycle AAh; the line is where the ticket puts it, and
            // M4-23 can move it to the clock.
            self.in_vblank = false;
        }

        // A beam that `set_position` jumped into the middle of this line walked
        // past no event of it, so if where it landed is past the latch point the
        // line still has to be latched - the `render_line` at the end of it reads
        // that latch (#102).
        if self.line_mclk >= self.hblank_clear_mclk() {
            self.latch_line_state();
        }
    }

    /// The first line of a frame.
    fn frame_start(&mut self) {
        // "Set when the display is interlaced, and in the odd frame, otherwise
        // it is cleared in the even frame" (genvdp.txt section 6), so the flag
        // only moves at all when the display is interlaced.
        if self.interlace_mode() != InterlaceMode::Off {
            self.odd_frame = !self.odd_frame;
        }
    }

    /// One of the two points of the line that are not the line end.
    fn line_event(&mut self) {
        if self.line_mclk == self.hblank_clear_mclk() {
            self.in_hblank = false;
            // The active display begins here, so this is where the line's scroll
            // and mode state stops being writable: `latch` is what the line is
            // drawn from from now on (M4-23, #102).
            self.latch_line_state();
        } else {
            self.in_hblank = true;
            self.hint_step();
        }
    }

    /// The line interrupt, at the back porch of the line.
    ///
    /// genvdp.txt section 4 never says where in the line the interrupt is
    /// raised, only that "The VDP has a counter that is decremented on every
    /// line. When the counter has expired, and if bit 4 (IE1) of register #0 is
    /// set, then a level 4 interrupt will occur", and that the counter is loaded
    /// from R10 when it expires. The line's counter is therefore read at the end
    /// of the line, at the start of the blanking period; the real position is
    /// nearer H counter 0xA6 in H40 and 0x85 in H32, which M4-23 moves this to.
    ///
    /// The counter runs whether or not IE1 is set - genvdp.txt gates the
    /// interrupt on the bit, not the counter - so `pending_irq` is where the
    /// enable is read.
    fn hint_step(&mut self) {
        if self.line > self.vblank_line() {
            // A VBlank line reloads rather than decrements, so it cannot expire.
            return;
        }
        if self.hint_counter == 0 {
            self.hint_pending = true;
            self.hint_counter = self.hint_reload();
        } else {
            self.hint_counter -= 1;
        }
    }

    /// The last line of the frame, so the beam wraps after it.
    #[inline]
    fn frame_lines(&self) -> u16 {
        if self.pal {
            FRAME_LINES_PAL
        } else {
            FRAME_LINES_NTSC
        }
    }

    /// The first line of VBlank: the first line past the active display, 224 in
    /// V28 and 240 in V30.
    ///
    /// genvdp.txt names line E0h, 224, and never discusses V30 at all; the
    /// `active_height` rule is the ticket's, and the same shift `hv`'s V counter
    /// tables assume for PAL. Inferred for V30 (M4-20, #98).
    #[inline]
    fn vblank_line(&self) -> u16 {
        // `active_height` is 224 or 240, both of which fit a `u16`.
        self.active_height() as u16
    }

    /// The highest interrupt the VDP is offering the 68000: level 6 for a
    /// vertical interrupt, level 4 for a line interrupt, `None` for neither.
    ///
    /// The enable bits are read here rather than when the interrupt happened,
    /// which is what lets a VINT that was masked when it fired be taken after
    /// IE0 goes up: genvdp.txt section 6 makes the pending flag independent of
    /// IE0 ("Bit 7 is set when a vertical interrupt occurs") and section 17
    /// makes IE0 the gate on delivery ("Bit 5 will enable vertical blanking
    /// interrupts when set"), and only the pending flag is remembered.
    ///
    /// Level 6 outranks level 4 because it is the higher level, and genvdp.txt
    /// section 4's note that "line interrupts are processed *before* vertical
    /// interrupts within a scanline" is about a line interrupt that shares a
    /// line with the vertical one: the 68000 takes whichever is up, and this
    /// answers one at a time.
    pub fn pending_irq(&self) -> Option<u8> {
        if self.vint_pending && self.vint_enabled() {
            Some(IRQ_VINT)
        } else if self.hint_pending && self.hint_enabled() {
            Some(IRQ_HINT)
        } else {
            None
        }
    }

    /// The 68000 has taken the interrupt, so the VDP stops offering it.
    ///
    /// "Reading the control port does not clear this bit" (genvdp.txt section 6,
    /// of status bit 7), so the acknowledge is the only thing that clears it.
    /// Levels 2 - the TH pin's, which belongs to the console I/O registers - and
    /// anything else are not the VDP's to clear, and are ignored.
    pub fn ack_irq(&mut self, level: u8) {
        match level {
            IRQ_VINT => self.vint_pending = false,
            IRQ_HINT => self.hint_pending = false,
            _ => {}
        }
    }

    /// A frame has been drawn: true once per frame, at VBlank start, which is
    /// what the scheduler of M3-14 (#75) ends `run_frame` with.
    ///
    /// Reading it is what consumes it, so the answer is true for one call per
    /// frame however the clocks are chopped up between calls.
    pub fn frame_done(&mut self) -> bool {
        let done = self.frame_ready;
        self.frame_ready = false;
        done
    }

    /// Whether the VDP is holding the Z80's interrupt line up.
    ///
    /// The Z80 gets one interrupt per frame, at VBlank, for a stretch of the
    /// first line of it: see [`Z80_INT_MCLK`] for where that width comes from.
    /// Derived from the beam position, so it needs no state of its own and a
    /// Z80 that samples it late in the line sees the same thing hardware gives.
    pub fn z80_int_line(&self) -> bool {
        self.line == self.vblank_line() && self.line_mclk < Z80_INT_MCLK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::MAX_HEIGHT;

    /// A VDP in the plain H32 / V28 mode, display on and VINT enabled: the state
    /// of a chip that has been started up but has never been advanced.
    fn running(pal: bool) -> Vdp {
        let mut vdp = Vdp::new(pal);
        vdp.write_register(1, 0x60);
        vdp
    }

    /// The lines whose HINT event found the counter expired, `lines` whole lines
    /// of walking with every HINT acknowledged as it shows up.
    ///
    /// A step of one whole line runs the events of the line the beam was on, so
    /// the loop index is the line a HINT inside it belongs to.
    fn hint_lines(vdp: &mut Vdp, lines: u16) -> Vec<u16> {
        let mut fired = Vec::new();
        for line in 0..lines {
            vdp.advance(MCLKS_PER_LINE);
            if vdp.hint_pending {
                fired.push(line);
                vdp.ack_irq(4);
            }
        }
        fired
    }

    /// genvdp.txt section 4 "Line interrupts": the counter is loaded on line 0 and
    /// decremented on every line after it, and with R10 = 3 that is four lines from
    /// the load to the expiry, so the HINTs land on 3 and 7.
    #[test]
    fn hint_counter_with_r10_three_fires_on_lines_three_and_seven() {
        let mut vdp = running(false);
        vdp.write_register(0, 0x10);
        vdp.write_register(10, 3);

        assert_eq!(
            hint_lines(&mut vdp, 11),
            vec![3, 7],
            "the counter runs 3, 2, 1, 0 and expires on the line after the last one it was loaded on"
        );
    }

    /// R10 = 0 expires on every line that decrements, and genvdp.txt is explicit
    /// that lines 225 to 261 reload instead: "Lines 225 through 261. (note that line
    /// 224 is not included)".
    #[test]
    fn hint_counter_with_r10_zero_fires_every_active_line_and_never_in_vblank() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(1, 0x40);
        vdp.write_register(10, 0);

        let fired = hint_lines(&mut vdp, 262);
        assert_eq!(
            fired,
            (0..=224).collect::<Vec<u16>>(),
            "every line of the active display, and nothing from 225 on"
        );
    }

    /// genvdp.txt section 4: the level 6 interrupt happens "when the V counter is
    /// at line E0h", which is 224, once per frame.
    #[test]
    fn vint_fires_once_per_frame_at_line_224() {
        let mut vdp = running(false);
        let mut fired = Vec::new();

        for _ in 0..262 {
            vdp.advance(MCLKS_PER_LINE);
            if vdp.vint_pending {
                fired.push(vdp.line);
                vdp.ack_irq(6);
            }
        }

        assert_eq!(fired, vec![224], "once, and on the first line of VBlank");
    }

    /// PAL in V30 has a 240-line display, so its VBlank starts a sixteen lines
    /// later. genvdp.txt only ever names 224; see the `vblank_line` comment.
    #[test]
    fn pal_v30_vint_is_at_line_240() {
        let mut vdp = Vdp::new(true);
        vdp.write_register(1, 0x68);
        let mut fired = Vec::new();

        for _ in 0..313 {
            vdp.advance(MCLKS_PER_LINE);
            if vdp.vint_pending {
                fired.push(vdp.line);
                vdp.ack_irq(6);
            }
        }

        assert_eq!(fired, vec![240], "V30 moves VBlank, and VINT with it");
    }

    /// IE0 is read when the interrupt is offered rather than when it happened:
    /// "Bit 5 will enable vertical blanking interrupts when set" (genvdp.txt section
    /// 17 "$01") says nothing about the pending flag that is already up.
    #[test]
    fn pending_irq_reads_ie0_live() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(1, 0x40);

        for _ in 0..225 {
            vdp.advance(MCLKS_PER_LINE);
        }

        assert!(vdp.vint_pending, "the vertical interrupt happened");
        assert_eq!(
            vdp.pending_irq(),
            None,
            "but IE0 is clear, so nobody is told"
        );

        vdp.write_register(1, 0x60);
        assert_eq!(
            vdp.pending_irq(),
            Some(6),
            "enabling IE0 delivers the VINT that is already pending"
        );

        vdp.ack_irq(6);
        assert_eq!(vdp.pending_irq(), None);
        assert!(!vdp.vint_pending, "and the acknowledge cleared the flag");
    }

    /// genvdp.txt section 4: "line interrupts are processed *before* vertical
    /// interrupts within a scanline", so of two pending sources the 68k sees the
    /// level 6 one first; each is acknowledged on its own.
    #[test]
    fn vint_outranks_hint_and_each_has_its_own_acknowledge() {
        let mut vdp = running(false);
        vdp.write_register(0, 0x10);
        vdp.write_register(10, 0);

        for _ in 0..225 {
            vdp.advance(MCLKS_PER_LINE);
        }

        assert!(vdp.vint_pending && vdp.hint_pending, "both are up");
        assert_eq!(vdp.pending_irq(), Some(6), "level 6 first");
        vdp.ack_irq(6);
        assert_eq!(vdp.pending_irq(), Some(4), "the HINT is still waiting");
        vdp.ack_irq(4);
        assert_eq!(vdp.pending_irq(), None, "and both are gone");
    }

    /// [`Vdp::start_in_vblank`] parks the beam on the first line of VBlank with that
    /// line's blanking applied and its two event flags down: the chip is in
    /// blanking, and it has drawn nothing yet (#310).
    #[test]
    fn start_in_vblank_is_blank_and_has_drawn_nothing() {
        let mut vdp = Vdp::new(false);
        vdp.start_in_vblank();

        assert_eq!(
            (vdp.line, vdp.line_mclk),
            (224, 0),
            "the E0h of genvdp.txt section 4, at the start of it"
        );
        assert!(
            vdp.in_vblank && vdp.in_hblank,
            "and the blanking of that line is what a status read sees"
        );
        assert!(
            vdp.line_started,
            "the once-a-line work of the line is applied, not still owed to it"
        );
        assert!(
            !vdp.frame_done() && !vdp.vint_pending,
            "and neither of the line's two events comes up with the parking"
        );

        // Which is what a host waiting on the flag needs: from a parked beam to the
        // flag is a whole raster, not nothing at all.
        let mut lines = 0;
        while !vdp.frame_done() {
            vdp.advance(MCLKS_PER_LINE);
            lines += 1;
            assert!(lines <= 262, "one frame, however the wait is written");
        }
        assert_eq!(lines, 262, "one whole raster from the beam to the flag");
    }

    #[test]
    fn frame_done_is_true_once_per_frame() {
        let mut vdp = running(false);
        let mut seen = 0;

        for _ in 0..262 {
            vdp.advance(MCLKS_PER_LINE);
            if vdp.frame_done() {
                seen += 1;
            }
        }
        assert_eq!(seen, 1, "once over a frame of line-at-a-time stepping");
        assert!(!vdp.frame_done(), "and the second ask is false");

        vdp.advance(262 * MCLKS_PER_LINE);
        assert!(vdp.frame_done(), "one whole-frame call, one frame");
        assert!(!vdp.frame_done(), "the flag does not stay up");
    }

    /// genvdp.txt section 6: d3 "is set on line E0h ... and cleared on line FFh",
    /// and d2 "is set at H counter cycle E4h and cleared at H counter cycle 08h".
    #[test]
    fn blanking_flags_follow_the_beam() {
        let mut vdp = running(false);
        assert!(
            !vdp.in_hblank,
            "reset is not a beam position, see status_read tests"
        );

        vdp.advance(1710);
        assert!(!vdp.in_hblank, "mid-line is active");
        assert!(!vdp.in_vblank);

        // 3010 mclk into the line: past the set point of both modes.
        vdp.advance(1300);
        assert!(vdp.in_hblank, "the back porch is blank");

        // The clear point is 81 (H40) / 100 (H32) mclk into the line, so a line
        // ends blank, the next one starts blank, and it is only 81 clocks in
        // that the flag comes down.
        vdp.advance(410);
        assert_eq!((vdp.line, vdp.line_mclk), (1, 0));
        assert!(vdp.in_hblank, "and the next line starts blank too");

        vdp.advance(200);
        assert!(!vdp.in_hblank, "past the clear point of line 1");

        while vdp.line < 224 {
            vdp.advance(MCLKS_PER_LINE);
        }
        assert_eq!(vdp.line, 224);
        assert!(vdp.in_vblank, "line E0h is VBlank");
    }

    /// genvdp.txt section 4: "The VDP generates all interrupts for the 68000", and
    /// the Z80's are not in that file at all. The Z80 gets a VBlank interrupt of
    /// about one line, and this is the line it is on.
    #[test]
    fn z80_int_line_is_the_first_line_of_vblank() {
        let mut vdp = running(false);
        vdp.advance(MCLKS_PER_LINE * 223);
        assert_eq!((vdp.line, vdp.line_mclk), (223, 0));
        assert!(!vdp.z80_int_line(), "line 223 is not the one");

        vdp.advance(MCLKS_PER_LINE);
        assert_eq!((vdp.line, vdp.line_mclk), (224, 0));
        assert!(vdp.z80_int_line(), "line 224, at its start, is");
        vdp.advance(Z80_INT_MCLK - 1);
        assert!(vdp.z80_int_line(), "and it is held for about 2560 clocks");
        vdp.advance(1);
        assert!(!vdp.z80_int_line(), "then it drops");
    }

    /// The renderer is M4-10 onwards, but `advance` has to call it for `frame_done`
    /// and the scheduler to mean anything, and the ticket keeps it a backdrop stub.
    #[test]
    fn render_line_paints_each_active_line_once() {
        let mut vdp = running(false);
        vdp.cram[0] = 0x0240;
        vdp.advance(262 * MCLKS_PER_LINE);

        let frame = vdp.frame();
        let backdrop = vdp.backdrop_pixel();
        let painted: Vec<usize> = (0..MAX_HEIGHT)
            .filter(|&y| {
                (0..frame.width).all(|x| frame.pixels[frame.pixel_index(x, y)] == backdrop)
            })
            .collect();

        assert_eq!(
            painted,
            (0..224).collect::<Vec<usize>>(),
            "one pass of `render_line` over each line of the active display, and no others"
        );
    }

    /// `render_line` stays callable on its own, as the architecture doc says the
    /// tests use it ("Render one line directly. Public for tests").
    #[test]
    fn render_line_can_be_called_directly() {
        let mut vdp = running(false);
        vdp.cram[0] = 0x001C;
        vdp.render_line(5);

        let frame = vdp.frame();
        let backdrop = vdp.backdrop_pixel();
        let row: Vec<u32> = (0..frame.width)
            .map(|x| frame.pixels[frame.pixel_index(x, 5)])
            .collect();
        assert!(row.iter().all(|&p| p == backdrop), "line 5 is the backdrop");
        assert_ne!(
            frame.pixels[frame.pixel_index(0, 6)],
            backdrop,
            "and nothing else was painted"
        );
    }

    /// genvdp.txt section 6: d4 "is set when the display is interlaced, and in the
    /// odd frame, otherwise it is cleared in the even frame".
    #[test]
    fn odd_frame_flag_alternates_while_interlaced() {
        let mut vdp = running(false);
        vdp.write_register(12, 0x02);

        vdp.advance(262 * MCLKS_PER_LINE);
        let first = vdp.odd_frame;
        vdp.advance(262 * MCLKS_PER_LINE);
        assert_eq!(vdp.odd_frame, !first, "a frame later the field has flipped");

        let mut plain = running(false);
        plain.advance(262 * MCLKS_PER_LINE);
        assert!(!plain.odd_frame, "and with no interlace d4 stays clear");
    }

    /// The mode 2 half of the same rule, which #98 adds a use for: the flag is the
    /// field the renderer draws, so it alternates in mode 2 exactly as it does in
    /// mode 1 - "This applies to both interlace modes" (genvdp.txt section 6).
    #[test]
    fn odd_frame_flag_alternates_in_interlace_mode_2() {
        let mut vdp = running(false);
        vdp.write_register(12, 0x06);

        vdp.advance(262 * MCLKS_PER_LINE);
        let first = vdp.odd_frame;
        vdp.advance(262 * MCLKS_PER_LINE);
        assert_eq!(vdp.odd_frame, !first, "a frame later the field has flipped");

        vdp.advance(262 * MCLKS_PER_LINE);
        assert_eq!(
            vdp.odd_frame, first,
            "and back again, so it is a toggle and not a clock"
        );
    }
}
