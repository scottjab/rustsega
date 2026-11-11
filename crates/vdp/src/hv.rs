//! The beam position and the HV counter the 68k reads it as.
//!
//! "The HV counter returns the vertical and horizontal position of the
//! television's raster beam. Reading the HV counter will return the following
//! data: `VC7 VC6 VC5 VC4 VC3 VC2 VC1 VC0` (D15-D08), `HC8 HC7 HC6 HC5 HC4 HC3
//! HC2 HC1` (D07-D00)" (genvdp.txt section 5 "HV Counter"). The port is
//! `$C00008` and its mirrors (genvdp.txt section 3 "VDP port map"), it is
//! read-only, and for a byte access "the even byte (e.g. C00008h) returns the V
//! counter, and the odd byte (e.g. C00009h) returns the H counter" - which is
//! the word split above, so the bus needs no port-side fix-up either.
//!
//! Neither counter is the frame position itself. Both are 8-bit values covering
//! a whole line and a whole frame, and each gets there by jumping: the line is
//! 342 pixels in H32 and 420 in H40, and a frame is 262 lines on NTSC and 313
//! on PAL, so more positions exist than the eight bits of one counter can
//! count. [`h_count`] and [`v_count`] are those two jumps as arithmetic, and
//! the tests of this module are the tables they have to reproduce.
//!
//! The position is state ([`Vdp::set_position`]) rather than a clock: the
//! scheduler of M4-06 (#84) is what walks it, and this module answers the
//! question "where in the frame is the beam, and what does the counter read
//! there". R0 bit 1 (M3) can freeze the answer, which is [`Vdp::hv_counter`]'s
//! other job.

use crate::InterlaceMode;
use crate::Vdp;

/// Master clocks in one scanline: 3420, for both regions (genvdp.txt says
/// nothing about the line's clock; this is the number ARCHITECTURE.md's clocks
/// section fixes, and it is what makes 342 H32 pixels of 10 clocks each).
pub(crate) const MCLKS_PER_LINE: u32 = 3420;

/// Master clocks per pixel in H32: one seventh of the 68k's clock, ten of the
/// master (`docs/ARCHITECTURE.md`, "Clocks and timing").
///
/// H32 is even and stays so: 342 of them make [`MCLKS_PER_LINE`], and the slow
/// pixel clock of [`H40_CLOCK_RUNS`] is an H40 thing.
const MCLKS_PER_PIXEL_H32: u32 = 10;

/// Pixel positions per line in H32, blanking and retrace included.
pub(crate) const PIXELS_H32: u32 = 342;
/// Pixel positions per line in H40, likewise included: 420, of which 320 are the
/// active picture.
///
/// A pixel is not [`MCLKS_PER_LINE`] / 420 of a clock. The line is 388 pixels of
/// 8 master clocks, 4 of 9 and 28 of 10, laid out as [`H40_CLOCK_RUNS`].
pub(crate) const PIXELS_H40: u32 = 420;

/// The H40 line as runs of `(pixels, master clocks each)`.
///
/// The 420 pixels of an H40 line are not all the same width in time. "Two
/// internal pixel clocks: MCLK/10 (slow) and MCLK/8 (fast)", and across the
/// HSYNC region "HSYNC uses the pattern 1 fast, 7 slow, 2 medium, 7 slow", the
/// medium being MCLK/9 ("Megadrive VDP H40 timing", nesdev forum thread 20410).
/// One pass of that pattern is 17 pixels of 8 + 70 + 18 + 70 = 166 master
/// clocks, so two of them are the 332-clock HSYNC block of
/// "((30+13+320+14+9)*8)+332=3420" ("VDP FIFO and DMA questions", SpritesMind
/// gendev thread 3221), and the remaining 386 pixels of the line run on 8.
///
/// Read as a per-line total that is 2 + 386 = 388 pixels at 8 clocks, 4 at 9 and
/// 28 at 10, for 3104 + 36 + 280 = 3420 clocks over 420 pixels: nesdev 20410's
/// own table ("H320: slow 28, medium 4, fast 388"). The ticket this clock was
/// written from says "most pixels 8 mclk, the HSYNC region uses 10 mclk pixels"
/// and so misses the MCLK/9 step; without those four pixels of nine the line is
/// 3416 clocks and does not close on [`MCLKS_PER_LINE`] at all.
///
/// # Phase
///
/// That the slow block *starts* the line is INFERRED. t=3221 gives the line
/// composition as "HSYNC 34, left blanking 30, left border 13, active 320, right
/// border 14, right blanking 9", listing HSYNC first, and the scope measurement
/// agrees: "in H40 mode the VDP slows down its clock for a number of cycles
/// during HBlank (32 pixel clocks according to my oscilloscope tests) - starting
/// 24 pixels after the right edge" (plutiedev, "SEGA Mega Drive / Genesis
/// hardware notes", kabuto mirror). The right edge of the 320-pixel window is
/// pixel 397 of this line, and 397 + 24 is 421 - one past the end of the line,
/// which is pixel 1: the first of the slow runs, straight after the single fast
/// pixel that opens the pattern.
///
/// TODO(#300): confirm on hardware where in the line the slow block sits, and
/// where its four MCLK/9 pixels fall. The counts are firm, being what makes the
/// line 3420 clocks; the phase is what is inferred.
const H40_CLOCK_RUNS: [(u32, u32); 9] = [
    (1, 8),
    (7, 10),
    (2, 9),
    (7, 10),
    (1, 8),
    (7, 10),
    (2, 9),
    (7, 10),
    (PIXELS_H40 - 34, 8),
];

/// Master clock each H40 pixel starts at, `H40_PIXEL_START[420]` being the end of
/// the line: [`H40_CLOCK_RUNS`] accumulated, so that a clock finds its pixel by
/// lookup and neither direction of the conversion has to round anything.
const H40_PIXEL_START: [u32; (PIXELS_H40 + 1) as usize] = h40_pixel_start();

/// [`H40_PIXEL_START`] itself, walked out of [`H40_CLOCK_RUNS`].
const fn h40_pixel_start() -> [u32; (PIXELS_H40 + 1) as usize] {
    let mut start = [0u32; (PIXELS_H40 + 1) as usize];
    let mut pixel = 0usize;
    let mut clock = 0u32;
    let mut run = 0;
    while run < H40_CLOCK_RUNS.len() {
        let (pixels, mclk) = H40_CLOCK_RUNS[run];
        let mut n = 0;
        while n < pixels {
            start[pixel] = clock;
            clock += mclk;
            pixel += 1;
            n += 1;
        }
        run += 1;
    }
    start[PIXELS_H40 as usize] = clock;
    start
}

/// Master clock the pixel `pixel` of the line starts at: ten of them in H32, and
/// [`H40_PIXEL_START`] in H40.
///
/// The inverse of [`mclk_to_pixel`], and exact in both directions - which is what
/// lets `slots` name an access slot by pixel and still have the FIFO wait on
/// clocks.
pub(crate) fn pixel_to_mclk(pixel: u32, h40: bool) -> u32 {
    if h40 {
        H40_PIXEL_START[pixel as usize]
    } else {
        pixel * MCLKS_PER_PIXEL_H32
    }
}

/// Pixel of the line the master clock `mclk` falls in.
///
/// Ten clocks a pixel in H32; in H40 the last pixel whose start clock in
/// [`H40_PIXEL_START`] is not past `mclk`. Every clock of a line is inside
/// exactly one pixel, and clock 0 is pixel 0.
pub(crate) fn mclk_to_pixel(mclk: u32, h40: bool) -> u32 {
    if !h40 {
        return mclk / MCLKS_PER_PIXEL_H32;
    }
    // `partition_point` is the first index whose pixel starts past `mclk`, and
    // pixel 0 starts at clock 0, so this is never empty and `- 1` is safe.
    H40_PIXEL_START.partition_point(|&start| start <= mclk) as u32 - 1
}

/// Where the H counter lands after its jump, in H32.
///
/// genvdp.txt section 5 states the two ends of the jump as "The H counter
/// counts up from 00h to E9h, then it jumps back to 93h and continues counting
/// up to FFh. This allows it to cover an entire 342 pixel line", adding that
/// the description "is based upon known information about the SMS VDP's H
/// counter" and "could be accurate" for the Genesis 32-cell display. The two
/// magic numbers of that sentence are 0x93 and 0xE9; which way round they go
/// follows from the same section's bus layout, `HC8..HC1`, which puts one
/// counter value across two pixels and so leaves 171 reads for the line:
/// 0x00..=0x93 is 148 of them (296 pixels) and 0xE9..=0xFF the last 23 (46
/// pixels), which is 342 pixels and 171 reads exactly. The other order needs
/// 343 reads of the line, which the bus cannot show.
const H_JUMP_H32: u8 = 0xE9;
/// Where the H counter lands after its jump, in H40.
///
/// Not in genvdp.txt: section 5 says only "This is not the case for the 40-cell
/// display, where I would assume the H counter jumps back farther at a later
/// point in the count-up", so this is the H32 rule applied to a 420-pixel line,
/// the counter still ending the line on 0xFF: 0xFF less 0xE4 plus 1 is 28 values
/// over 56 pixels, so the jump is at pixel 364 and the count before it runs to
/// 0xB5.
const H_JUMP_H40: u8 = 0xE4;

/// First line whose V counter has jumped, NTSC.
///
/// "The V counter counts up from 00h to EAh, then it jumps back to E5h and
/// continues counting up to FFh. This allows it to cover the entire 262 line
/// display" (genvdp.txt section 5). 0x00..=0xEA is 235 lines and 0xE5..=0xFF
/// the last 27, so line `0xEB` - one past the last line counted normally - is
/// where the jump lands on 0xE5.
const V_JUMP_NTSC_LINE: u16 = 0xEB;
/// Value the V counter jumps to, NTSC (genvdp.txt section 5, as above).
const V_JUMP_NTSC: u8 = 0xE5;

/// First line whose V counter has jumped, PAL in V28.
///
/// genvdp.txt section 5 gives the NTSC wrap only; it never mentions the PAL
/// frame. These two PAL tables are the ones the ticket specifies, and each is
/// arithmetically closed: the 0x00..=0xFF run and the 0x00..=0x02 repeat take
/// 259 lines and 0xCA..=0xFF is the last 54, which is 313 lines (genvdp.txt
/// section 5 for the shape, nothing for the numbers). TODO(M6-06 #125): confirm
/// against hardware; treat as inferred until then.
const V_JUMP_PAL_V28_LINE: u16 = 259;
/// Value the V counter jumps to, PAL in V28 (see [`V_JUMP_PAL_V28_LINE`]).
const V_JUMP_PAL_V28: u8 = 0xCA;
/// First line whose V counter has jumped, PAL in V30: 256 + 11 lines of the
/// 0x00..=0x0A repeat, then 0xD2..=0xFF over the last 46 (see
/// [`V_JUMP_PAL_V28_LINE`] for where these tables come from).
const V_JUMP_PAL_V30_LINE: u16 = 267;
/// Value the V counter jumps to, PAL in V30 (see [`V_JUMP_PAL_V30_LINE`]).
const V_JUMP_PAL_V30: u8 = 0xD2;

/// R0 bit 1 (M3): the HV counter latch enable (genvdp.txt section 17 "$00").
const R0_M3: u8 = 1 << 1;

/// The H counter byte at pixel `pixel` of the line.
///
/// The counter holds for two pixels, which is what `HC8..HC1` of a pixel
/// counter means (see [`H_JUMP_H32`]), and it ends the line on 0xFF: the jump
/// is exactly `0x100 - jump` values, each of two pixels, before the end.
fn h_count(pixel: u32, h40: bool) -> u8 {
    let (pixels, jump) = if h40 {
        (PIXELS_H40, H_JUMP_H40)
    } else {
        (PIXELS_H32, H_JUMP_H32)
    };
    let after_jump = pixels - (0x100 - u32::from(jump)) * 2;
    if pixel < after_jump {
        (pixel >> 1) as u8
    } else {
        (u32::from(jump) + ((pixel - after_jump) >> 1)) as u8
    }
}

/// The V counter byte on frame line `line`, before the interlace transform.
///
/// One count per line of the frame, and the last line of the frame counts
/// 0xFF: see [`V_JUMP_NTSC_LINE`] for the NTSC table and
/// [`V_JUMP_PAL_V28_LINE`] for the two PAL ones.
///
/// NTSC has no V30. R1 bit 3 "will select between a PAL (240) and NTSC (224
/// lines) display" (genvdp.txt section 17 "$01"), and an NTSC frame is still
/// 262 lines however that bit is set, so the counter cannot jump anywhere else
/// and NTSC reads the NTSC table whatever `v30` says. The display is what
/// rolls when a game asks for it (plutiedev "$81xx": "V30 mode only works
/// properly on PAL systems"); [`Vdp::active_height`] is where the picture side
/// of that is decided.
fn v_count(line: u16, pal: bool, v30: bool) -> u8 {
    let (jump_line, jump) = if !pal {
        (V_JUMP_NTSC_LINE, V_JUMP_NTSC)
    } else if v30 {
        (V_JUMP_PAL_V30_LINE, V_JUMP_PAL_V30)
    } else {
        (V_JUMP_PAL_V28_LINE, V_JUMP_PAL_V28)
    };
    if line < jump_line {
        // Lines 0..=255 count as themselves and the few after that repeat from
        // 0, which is the same `line & 0xFF`.
        (line & 0xFF) as u8
    } else {
        jump + (line - jump_line) as u8
    }
}

/// The interlace transform of the V counter: what the port answers for the
/// frame line, rather than the count of [`v_count`] alone.
///
/// genvdp.txt says one thing about this, and one thing only: "According to the
/// manual, VC0 is replaced with VC8 when in interlace mode 2" (section 5). The
/// mode 2 rule below is that sentence: the doubled value has bit 0 spare, and
/// bit 8 of the internal counter goes into it. Mode 1 is the same replacement
/// without the doubling, which is what the ticket says and is not something
/// genvdp.txt says at all - it names mode 2 and nothing else. Both are therefore
/// inferred, TODO(M6-06 #125) to confirm them on hardware.
///
/// `field` is bit 8 of the internal counter, `VC8`: the second half of the
/// frame, which is the half that the 8-bit V counter cannot name and which
/// interlacing exists to tell apart. M4-06 (#84) is what will make it a field
/// parity rather than a bit of the line number.
fn interlace_v(count: u8, field: bool, mode: InterlaceMode) -> u8 {
    let field_bit = u8::from(field);
    match mode {
        InterlaceMode::Off => count,
        InterlaceMode::Mode1 => (count & 0xFE) | field_bit,
        InterlaceMode::Mode2 => (count << 1) | field_bit,
    }
}

impl Vdp {
    /// The HV counter: the word the `$C00008` block answers with.
    ///
    /// `(V << 8) | H`, the `VC7-VC0` / `HC8-HC1` layout of genvdp.txt section
    /// 5, and the latch comes first: while R0 bit 1 (M3) is set the port keeps
    /// answering the value the counter had when M3 went up, and "The HV counter
    /// will resume normal operation when this bit is cleared" (genvdp.txt
    /// section 17 "$00").
    #[inline]
    pub fn hv_counter(&self) -> u16 {
        self.hv_latch.unwrap_or_else(|| self.hv_counter_now())
    }

    /// The counter as the beam position has it, ignoring the M3 latch.
    fn hv_counter_now(&self) -> u16 {
        let h = h_count(self.pixel_in_line(), self.h40());
        let v = interlace_v(
            v_count(self.line, self.pal, self.v30_mode()),
            self.line & 0x100 != 0,
            self.interlace_mode(),
        );
        (u16::from(v) << 8) | u16::from(h)
    }

    /// Pixel the beam is at within [`Vdp::line`], in the mode the display is in
    /// now: R12 can be rewritten mid-line, and the counter answers for the mode
    /// of the moment rather than for the mode the line started in.
    ///
    /// The clock is the exact one in both modes: [`mclk_to_pixel`], with the
    /// 8/9/10-clock run of [`H40_CLOCK_RUNS`] behind it (M4-21b, #100, which is
    /// where the 3420 / 420 approximation this replaces had its TODO).
    fn pixel_in_line(&self) -> u32 {
        mclk_to_pixel(self.line_mclk, self.h40())
    }

    /// Move the beam: `line` counts from the first line of the frame, and
    /// `line_mclk` is the master clock within it, `0..MCLKS_PER_LINE`.
    ///
    /// Public because the scheduler that walks it lives in `genesis`; nothing
    /// calls it until M4-06 (#84) runs the frame, so the position of a VDP that
    /// has not been told otherwise is line 0, clock 0.
    #[inline]
    pub fn set_position(&mut self, line: u16, line_mclk: u32) {
        self.line = line;
        self.line_mclk = line_mclk;
    }

    /// R0 has been written: move the M3 latch with the bit.
    ///
    /// Hardware latches on the level 2 interrupt that a TH transition causes -
    /// "If bit 1 of VDP register #0 is set, the contents of the HV counter will
    /// be latched" (genvdp.txt section 4) and section 17 "$00" repeats it as
    /// "Bit 1 will cause the HV counter to be latched when a level 2 interrupt
    /// is generated" - and there is no lightgun here to make that interrupt. So
    /// the latch is taken at the M3 0-to-1 transition instead, which is the
    /// closest thing to it that a register write offers: a game that sets M3 and
    /// then waits for the interrupt reads the counter some cycles later, and
    /// genvdp.txt itself gives up on the detail ("I don't know if the HV counter
    /// resumes normal operation after the first read, or if the HV counter latch
    /// bit has to be cleared and set again").
    pub(crate) fn m3_written(&mut self, previous: u8, value: u8) {
        let was_set = previous & R0_M3 != 0;
        let is_set = value & R0_M3 != 0;
        if is_set && !was_set {
            self.hv_latch = Some(self.hv_counter_now());
        } else if was_set && !is_set {
            self.hv_latch = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The H counter reads of a line, one per counter value: the counter holds
    /// for two pixels, so every second pixel of the line is a distinct read,
    /// from pixel 0 to the last one the counter still covers.
    fn h_sequence(h40: bool) -> Vec<u8> {
        let pixels = if h40 { PIXELS_H40 } else { PIXELS_H32 };
        (0..pixels / 2).map(|step| h_count(step * 2, h40)).collect()
    }

    /// The V counter reads of a whole frame, line 0 to the last one.
    fn v_frame(pal: bool, v30: bool) -> Vec<u8> {
        let lines = if pal { 313 } else { 262 };
        (0..lines).map(|line| v_count(line, pal, v30)).collect()
    }

    /// A VDP in the given mode, ready for a position to be set.
    fn vdp_modes(pal: bool, h40: bool) -> Vdp {
        let mut vdp = Vdp::new(pal);
        if h40 {
            vdp.write_register(12, 0x81);
        }
        vdp
    }

    #[test]
    fn h32_h_counter_is_171_reads_over_the_line() {
        let reads = h_sequence(false);
        let distinct: std::collections::HashSet<u8> = reads.iter().copied().collect();

        assert_eq!(reads.len(), 171, "342 pixels, one read across two of them");
        assert_eq!(distinct.len(), 171, "no read repeats");
        assert_eq!(reads[0], 0x00, "the line starts the counter at 0");
        assert_eq!(reads[0x93], 0x93, "the last read before the jump");
        assert_eq!(reads[0x94], 0xE9, "the jump, straight to 0xE9");
        assert_eq!(*reads.last().unwrap(), 0xFF, "and 0xFF at the end of it");
    }

    #[test]
    fn h40_h_counter_is_210_reads_over_the_line() {
        let reads = h_sequence(true);
        let distinct: std::collections::HashSet<u8> = reads.iter().copied().collect();

        assert_eq!(reads.len(), 210, "420 pixels, one read across two");
        assert_eq!(distinct.len(), 210, "no read repeats");
        assert_eq!(reads[0], 0x00);
        assert_eq!(reads[0xB5], 0xB5, "the last read before the jump");
        assert_eq!(reads[0xB6], 0xE4, "the jump, further and later than H32");
        assert_eq!(*reads.last().unwrap(), 0xFF);
    }

    #[test]
    fn h_counter_holds_for_two_pixels_and_never_goes_backwards() {
        for h40 in [false, true] {
            let pixels = if h40 { PIXELS_H40 } else { PIXELS_H32 };
            for pixel in (0..pixels).step_by(2) {
                assert_eq!(
                    h_count(pixel, h40),
                    h_count(pixel + 1, h40),
                    "pixel {pixel} and the one after it, h40={h40}"
                );
                if pixel > 0 {
                    assert!(
                        h_count(pixel, h40) > h_count(pixel - 2, h40),
                        "pixel {pixel} does not move the counter forwards, h40={h40}"
                    );
                }
            }
        }
    }

    #[test]
    fn h32_counter_follows_the_master_clock_of_the_line() {
        let mut vdp = vdp_modes(false, false);
        // 10 master clocks to a pixel in H32, so the counter of a clock is the
        // counter of the pixel it lands on: two pixels, one counter value.
        for pixel in (0..PIXELS_H32).step_by(2) {
            let mclk = pixel * MCLKS_PER_PIXEL_H32;
            vdp.set_position(0, mclk);
            assert_eq!(
                vdp.hv_counter() & 0xFF,
                u16::from(h_count(pixel, false)),
                "mclk {mclk} is pixel {pixel}"
            );
        }

        vdp.set_position(0, 296 * MCLKS_PER_PIXEL_H32);
        assert_eq!(vdp.hv_counter() & 0xFF, 0xE9, "pixel 296 is the H32 jump");
        vdp.set_position(0, 341 * MCLKS_PER_PIXEL_H32);
        assert_eq!(vdp.hv_counter() & 0xFF, 0xFF, "pixel 341 is the end of it");
    }

    /// The same measurement in H40, on the exact clock of [`H40_CLOCK_RUNS`].
    ///
    /// This test asked for mclk 2964 before #100, which is 364 of the 3420 / 420
    /// clocks the line was spread over then. The slow HSYNC block is 12 clocks
    /// longer than a run of 8-clock pixels that wide, so pixel 364 really starts
    /// at 2972 and 2964 is still pixel 363, one counter value short of the jump.
    #[test]
    fn h40_counter_ends_the_line_on_ff_from_the_clock_too() {
        let mut vdp = vdp_modes(false, true);
        vdp.set_position(0, 0);
        assert_eq!(vdp.hv_counter() & 0xFF, 0x00);
        vdp.set_position(0, 2972);
        assert_eq!(
            vdp.hv_counter() & 0xFF,
            0xE4,
            "mclk 2972 is pixel 364, where H40 jumps"
        );
        vdp.set_position(0, 2971);
        assert_eq!(
            vdp.hv_counter() & 0xFF,
            0xB5,
            "and one clock earlier is still the last value before it"
        );
        vdp.set_position(0, MCLKS_PER_LINE - 1);
        assert_eq!(vdp.hv_counter() & 0xFF, 0xFF, "the last clock of the line");
    }

    /// The two ends of the H40 line the ticket names: clock 0 is pixel 0, and the
    /// last clock of the line is still inside the last pixel of it.
    #[test]
    fn mclk_to_pixel_covers_the_h40_line_from_end_to_end() {
        assert_eq!(mclk_to_pixel(0, true), 0);
        assert_eq!(mclk_to_pixel(7, true), 0, "the first pixel is a fast one");
        assert_eq!(mclk_to_pixel(8, true), 1, "and the slow run starts after it");
        assert_eq!(mclk_to_pixel(3419, true), 419);
    }

    /// The clock and its inverse agree on every clock and every pixel of both
    /// lines, which is the property the FIFO's slot tables depend on: a slot
    /// named by pixel waits by exactly that many clocks.
    #[test]
    fn the_pixel_clock_round_trips_every_pixel_of_both_lines() {
        for h40 in [false, true] {
            let pixels = if h40 { PIXELS_H40 } else { PIXELS_H32 };
            for pixel in 0..pixels {
                assert_eq!(
                    mclk_to_pixel(pixel_to_mclk(pixel, h40), h40),
                    pixel,
                    "the first clock of pixel {pixel} is in pixel {pixel}, h40={h40}"
                );
            }
            for mclk in 0..MCLKS_PER_LINE {
                let pixel = mclk_to_pixel(mclk, h40);
                assert!(
                    pixel_to_mclk(pixel, h40) <= mclk && mclk < pixel_to_mclk(pixel + 1, h40),
                    "mclk {mclk} is inside pixel {pixel} and no other, h40={h40}"
                );
            }
        }
    }

    /// The mix of pixel widths #100 asks for: "most pixels 8 mclk, the HSYNC
    /// region uses 10 mclk pixels". The 10 is right and the ticket is short a
    /// bucket - nesdev 20410 measures 388 of 8, 4 of 9 and 28 of 10, and the line
    /// does not reach [`MCLKS_PER_LINE`] without the four of nine.
    #[test]
    fn the_h40_line_is_388_pixels_of_eight_four_of_nine_and_twenty_eight_of_ten() {
        let mut widths = [0u32; 11];
        for pixel in 0..PIXELS_H40 {
            let width = pixel_to_mclk(pixel + 1, true) - pixel_to_mclk(pixel, true);
            widths[usize::try_from(width).expect("a pixel is at most ten clocks")] += 1;
        }
        assert_eq!(widths[8], 388, "fast pixels");
        assert_eq!(widths[9], 4, "medium pixels");
        assert_eq!(widths[10], 28, "slow pixels");
        assert_eq!(
            widths[8] + widths[9] + widths[10],
            PIXELS_H40,
            "and nothing else in the line"
        );
    }

    /// Both lines close on 3420 clocks, and both spend 2560 of them on the active
    /// picture: 320 H40 pixels of 8, 256 H32 pixels of 10 (Eke, "VDP registers
    /// timings", SpritesMind gendev thread 291: "2560 mclk active display and 860
    /// mclk blanking, in both H32 and H40").
    #[test]
    fn both_modes_spend_2560_clocks_of_3420_on_the_active_picture() {
        assert_eq!(pixel_to_mclk(PIXELS_H32, false), MCLKS_PER_LINE);
        assert_eq!(pixel_to_mclk(PIXELS_H40, true), MCLKS_PER_LINE);

        // The H40 window of the t=3221 composition: HSYNC 34, left blanking 30,
        // left border 13 in front of the 320, right border 14 and right blanking
        // 9 behind it.
        let left = 34 + 30 + 13;
        assert_eq!(pixel_to_mclk(left + 320, true) - pixel_to_mclk(left, true), 2560);
        assert_eq!(
            MCLKS_PER_LINE - (pixel_to_mclk(left + 320, true) - pixel_to_mclk(left, true)),
            860,
            "the blanking half of the same figure"
        );
    }

    #[test]
    fn ntsc_v28_v_counter_wraps_at_line_eb() {
        assert_eq!(v_count(0x00, false, false), 0x00);
        assert_eq!(v_count(0xE9, false, false), 0xE9);
        assert_eq!(v_count(0xEA, false, false), 0xEA, "the last line before it");
        assert_eq!(v_count(0xEB, false, false), 0xE5, "the jump");
        assert_eq!(v_count(261, false, false), 0xFF, "the last line of 262");
    }

    #[test]
    fn pal_v28_v_counter_wraps_at_line_259() {
        assert_eq!(v_count(0, true, false), 0x00);
        assert_eq!(v_count(255, true, false), 0xFF);
        assert_eq!(v_count(256, true, false), 0x00, "the repeat starts");
        assert_eq!(v_count(258, true, false), 0x02, "the last line of it");
        assert_eq!(v_count(259, true, false), 0xCA, "the jump");
        assert_eq!(v_count(312, true, false), 0xFF, "the last line of 313");
    }

    #[test]
    fn pal_v30_v_counter_wraps_at_line_267() {
        assert_eq!(v_count(0, true, true), 0x00);
        assert_eq!(v_count(256, true, true), 0x00);
        assert_eq!(
            v_count(266, true, true),
            0x0A,
            "the last line of the repeat"
        );
        assert_eq!(v_count(267, true, true), 0xD2, "the jump");
        assert_eq!(v_count(312, true, true), 0xFF);
    }

    #[test]
    fn ntsc_v30_is_ntsc_v28_as_far_as_the_counter_goes() {
        for line in 0..262 {
            assert_eq!(
                v_count(line, false, true),
                v_count(line, false, false),
                "line {line}: an NTSC frame is 262 lines whatever R1 bit 3 says"
            );
        }
    }

    /// Where each table stops counting up: the line whose V counter is not one
    /// more than the line before it, counting 0xFF to 0x00 as one on since the
    /// counter is a byte. NTSC has the one jump of genvdp.txt section 5 at 0xEB,
    /// and a PAL table the one of its own at 259 and 267 - the 0xFF to 0x00
    /// repeat of line 256 is not a third, and the jump then runs to 0xFF at the
    /// last line of the frame.
    #[test]
    fn v_counter_counts_the_frame_to_ff_and_jumps_where_the_table_says() {
        for (pal, v30, lines, jumps_at) in [
            (false, false, 262, vec![0xEBusize]),
            (true, false, 313, vec![259]),
            (true, true, 313, vec![267]),
        ] {
            let reads = v_frame(pal, v30);
            assert_eq!(reads.len(), lines, "pal={pal} v30={v30}");
            assert_eq!(reads[0], 0x00, "pal={pal} v30={v30}");
            assert_eq!(*reads.last().unwrap(), 0xFF, "pal={pal} v30={v30}");

            let jumps: Vec<usize> = (1..reads.len())
                .filter(|&n| reads[n] != reads[n - 1].wrapping_add(1))
                .collect();
            assert_eq!(jumps, jumps_at, "pal={pal} v30={v30}");

            let covered: std::collections::HashSet<u8> = reads.iter().copied().collect();
            assert_eq!(
                covered.len(),
                0x100,
                "pal={pal} v30={v30}: every value of the byte is read on some line"
            );
        }
    }

    #[test]
    fn hv_counter_is_v_over_h() {
        let mut vdp = Vdp::new(false);
        vdp.set_position(0xEA, 640);
        assert_eq!(
            vdp.hv_counter(),
            0xEA20,
            "line 0xEA reads 0xEA, and 640 clocks is pixel 64 for H20"
        );

        let mut pal = Vdp::new(true);
        pal.set_position(259, 0);
        assert_eq!(pal.hv_counter(), 0xCA00, "the PAL V28 jump, on the bus");

        let mut h40 = vdp_modes(false, true);
        h40.set_position(1, 2972);
        assert_eq!(h40.hv_counter(), 0x01E4, "the H40 jump, on the bus");
    }

    #[test]
    fn interlace_mode_1_replaces_bit_0_with_the_eighth_bit_of_the_line() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x02);
        assert_eq!(vdp.interlace_mode(), InterlaceMode::Mode1);

        vdp.set_position(1, 0);
        assert_eq!(vdp.hv_counter() >> 8, 0x00, "line 1 loses its parity");
        vdp.set_position(2, 0);
        assert_eq!(vdp.hv_counter() >> 8, 0x02, "line 2 keeps its own bit 0");

        // Line 256 is where the 9th bit of the line number is: 0xFA once the
        // NTSC wrap has had it, and 0xFB with bit 0 replaced.
        vdp.set_position(256, 0);
        assert_eq!(v_count(256, false, false), 0xFA);
        assert_eq!(vdp.hv_counter() >> 8, 0xFB, "VC0 replaced with VC8");
    }

    #[test]
    fn interlace_mode_2_doubles_the_v_counter_and_puts_vc8_in_bit_0() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x06);
        assert_eq!(vdp.interlace_mode(), InterlaceMode::Mode2);

        vdp.set_position(0, 0);
        assert_eq!(vdp.hv_counter() >> 8, 0x00);
        vdp.set_position(1, 0);
        assert_eq!(vdp.hv_counter() >> 8, 0x02, "doubled");
        vdp.set_position(0xEA, 0);
        assert_eq!(
            vdp.hv_counter() >> 8,
            0xD4,
            "0xEA doubled is 0x1D4 and the port has 8 bits for it"
        );
        vdp.set_position(256, 0);
        assert_eq!(
            vdp.hv_counter() >> 8,
            0xF5,
            "0xFA doubled, and VC8 in bit 0"
        );
    }

    #[test]
    fn m3_freezes_the_counter_and_clearing_it_frees_it() {
        let mut vdp = Vdp::new(false);
        vdp.set_position(0x10, 0);
        assert_eq!(vdp.hv_counter(), 0x1000);

        vdp.write_register(0, 0x02);
        vdp.set_position(0x40, 0);
        assert_eq!(
            vdp.hv_counter(),
            0x1000,
            "M3 holds what the counter was when it was set"
        );

        vdp.write_register(0, 0x00);
        assert_eq!(
            vdp.hv_counter(),
            0x4000,
            "cleared, and it follows the beam again (genvdp.txt section 17 \"$00\")"
        );
    }

    #[test]
    fn a_write_to_r0_that_leaves_m3_alone_leaves_the_latch_alone() {
        let mut vdp = Vdp::new(false);
        vdp.set_position(0x10, 0);
        vdp.write_register(0, 0x02);
        vdp.set_position(0x40, 0);

        vdp.write_register(0, 0x02 | 0x20);
        assert_eq!(vdp.hv_counter(), 0x1000, "M3 was already set, no new latch");
        vdp.write_register(1, 0x40);
        assert_eq!(
            vdp.hv_counter(),
            0x1000,
            "and a different register is not M3"
        );
    }

    #[test]
    fn setting_m3_again_latches_afresh() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(0, 0x02);
        vdp.set_position(0x10, 0);
        vdp.write_register(0, 0x00);
        assert_eq!(vdp.hv_counter(), 0x1000);

        vdp.set_position(0x20, 0);
        vdp.write_register(0, R0_M3);
        vdp.set_position(0x30, 0);
        assert_eq!(
            vdp.hv_counter(),
            0x2000,
            "the 0-to-1 transition is the latch point"
        );
    }
}
