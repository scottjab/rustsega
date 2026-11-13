//! What the console is really running at, and what it costs (M7-17, #150).
//!
//! The [`crate::emulator`] loop knows how many frames it *wanted*; this knows how
//! many it *made*, which is the number a player is asking about when the picture
//! looks like a stutter. It is a window over the last second of finished frames
//! rather than a running average, so a one-second hitch shows for a second and
//! then goes, and so `60 fps` here means the same as `60 fps` on the box art:
//! console frames per wall-clock second, PAL included.
//!
//! Pure - no browser, no `performance.now()`, a `now_ms` handed in by the caller -
//! which is what makes the overlay testable at all. [`crate::frame_pacer`] is the
//! same arrangement for the other half of the loop.

/// How long a reading is taken over, milliseconds: one second, which is what the
/// player is comparing the number against, and short enough that a hitch shows for
/// a second and then leaves rather than sit in the average for a minute.
const WINDOW_MS: f64 = 1_000.0;

/// One finished console frame: the moment it ended, on the loop's own clock, and
/// what [`genesis::Genesis::run_frame`] took to make it.
type Record = (f64, f64);

/// A window over the frames of the last second: how many there were, and what they
/// cost.
///
/// Owned by [`crate::emulator`] and recorded once per frame, which is why it keeps a
/// deque rather than a `Vec`: at 60 fps the window is sixty entries and at 8x fast
/// forward nearly five hundred, and both are bounded by [`WINDOW_MS`] rather than by
/// how long the game has been running.
#[derive(Debug, Default)]
pub struct FpsMeter {
    /// The frames inside the window, oldest first - so the front is what an
    /// out-of-window frame leaves behind, and the back is always the newest.
    records: std::collections::VecDeque<Record>,
}

impl FpsMeter {
    /// An empty meter: no frames yet, so no rate yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes one console frame that ended at `now_ms` and took `emu_ms` to make.
    ///
    /// `now_ms` is the animation frame's clock, the same one [`crate::frame_pacer`]
    /// is driven by, so a frame recorded here and a frame the pacer handed out are
    /// on one ruler.
    pub fn record(&mut self, now_ms: f64, emu_ms: f64) {
        // A clock that steps backwards is a clock this cannot be measured against,
        // and a negative span is worse than an empty table: start over.
        if self
            .records
            .back()
            .is_some_and(|(finished, _)| now_ms < *finished)
        {
            self.records.clear();
        }
        self.records.push_back((now_ms, emu_ms));
        let oldest = now_ms - WINDOW_MS;
        while self
            .records
            .front()
            .is_some_and(|(finished, _)| *finished < oldest)
        {
            self.records.pop_front();
        }
    }

    /// The reading as it stands, which is to say as of the last
    /// [`record`](Self::record).
    ///
    /// The rate is of the *span* of the window rather than of [`WINDOW_MS`] so that
    /// a meter that has seen sixty frames a sixtieth of a second apart says 60 and
    /// not 61: nought to ninety-eight-three milliseconds is fifty-nine intervals,
    /// not sixty.
    pub fn stats(&self) -> Stats {
        let count = self.records.len();
        let Some(first) = self.records.front() else {
            return Stats::default();
        };
        let last = self.records.back().copied().unwrap_or_default();
        let span = last.0 - first.0;
        let fps = if count < 2 || span <= 0.0 {
            0.0
        } else {
            (count as f64 - 1.0) * 1000.0 / span
        };
        let mut stats = Stats {
            fps,
            avg_emu_ms: 0.0,
            max_emu_ms: 0.0,
        };
        for (_, emu_ms) in self.records.iter() {
            stats.avg_emu_ms += emu_ms;
            stats.max_emu_ms = stats.max_emu_ms.max(*emu_ms);
        }
        stats.avg_emu_ms /= count as f64;
        stats
    }
}

/// One reading of an [`FpsMeter`]: the console's frame rate, and the milliseconds
/// a frame of it costs the emulator.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    /// Console frames per wall-clock second: 60 for NTSC, 50 for PAL, and four
    /// times either while fast forward is held.
    pub fps: f64,
    /// The mean of [`genesis::Genesis::run_frame`] over the window, which is the
    /// number that says whether the emulator is keeping up or scraping at it.
    pub avg_emu_ms: f64,
    /// The worst single frame of the window: a stutter the average has swallowed
    /// shows here instead of nowhere.
    pub max_emu_ms: f64,
}

impl Stats {
    /// The overlay's one line: `60 fps  2.1 ms`.
    ///
    /// The average rather than the peak: an overlay is a glance, and the glance wants
    /// to know what a frame costs on the whole. `max_emu_ms` is still on [`Stats`]
    /// for whoever is measuring rather than glancing.
    pub fn label(&self) -> String {
        format!("{:.0} fps  {:.1} ms", self.fps, self.avg_emu_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of a 60 Hz console, ms.
    const TICK_60: f64 = 1000.0 / 60.0;
    /// One frame of a 50 Hz console, ms.
    const TICK_50: f64 = 1000.0 / 50.0;

    /// Records `count` frames of `emu_ms` each, `tick` ms apart, starting at zero.
    fn record_all(meter: &mut FpsMeter, count: u32, tick: f64, emu_ms: f64) {
        for index in 0..count {
            meter.record(f64::from(index) * tick, emu_ms);
        }
    }

    /// Nothing at all is not yet a reading: an empty meter says zero rather than
    /// inventing a rate the console has not earned.
    #[test]
    fn an_empty_meter_reads_zero() {
        assert_eq!(FpsMeter::new().stats(), Stats::default());
    }

    /// One frame is a duration, not a rate. This is the state of the meter for the
    /// whole of the first frame of a cartridge, and the overlay has to have some
    /// answer for it.
    #[test]
    fn one_record_is_a_cost_but_not_yet_a_rate() {
        let mut meter = FpsMeter::new();
        meter.record(0.0, 1.5);
        let stats = meter.stats();
        assert_eq!(stats.fps, 0.0);
        assert_eq!(stats.avg_emu_ms, 1.5);
        assert_eq!(stats.max_emu_ms, 1.5);
    }

    /// The ticket's own case: sixty frames in one second is 60 fps.
    #[test]
    fn sixty_records_over_one_second_read_sixty_frames_a_second() {
        let mut meter = FpsMeter::new();
        record_all(&mut meter, 60, TICK_60, 1.0);
        let stats = meter.stats();
        assert!(
            (stats.fps - 60.0).abs() < 0.01,
            "expected 60 fps, got {}",
            stats.fps
        );
    }

    /// A PAL console is 50 frames of that same second, and the overlay is the one
    /// place a player can tell the two machines apart.
    #[test]
    fn fifty_records_over_one_second_read_fifty_frames_a_second() {
        let mut meter = FpsMeter::new();
        record_all(&mut meter, 50, TICK_50, 2.0);
        let stats = meter.stats();
        assert!(
            (stats.fps - 50.0).abs() < 0.01,
            "expected 50 fps, got {}",
            stats.fps
        );
    }

    /// Two frames 10 ms apart is 100 fps: the window is a rate, not a count.
    #[test]
    fn two_frames_a_hundredth_of_a_second_apart_read_a_hundred_frames_a_second() {
        let mut meter = FpsMeter::new();
        meter.record(0.0, 1.0);
        meter.record(10.0, 1.0);
        assert!((meter.stats().fps - 100.0).abs() < 0.01);
    }

    /// The average and the maximum are over the frames of the window, which is
    /// what makes the overlay's `2.1 ms` the cost of a frame rather than of
    /// something that happened since the page loaded.
    #[test]
    fn the_cost_is_the_average_and_the_peak_of_the_frames_in_the_window() {
        let mut meter = FpsMeter::new();
        meter.record(0.0, 1.0);
        meter.record(100.0, 2.0);
        meter.record(200.0, 6.0);
        let stats = meter.stats();
        assert_eq!(stats.avg_emu_ms, 3.0);
        assert_eq!(stats.max_emu_ms, 6.0);
    }

    /// A one second window means the old frames go: a hitch from the first second
    /// of a game must not sit in the maximum for the rest of the session.
    #[test]
    fn a_frame_older_than_the_window_is_gone_from_the_average_and_the_peak() {
        let mut meter = FpsMeter::new();
        meter.record(0.0, 500.0);
        for index in 1..=60 {
            meter.record(f64::from(index) * TICK_60 + 1000.0, 1.0);
        }
        let stats = meter.stats();
        assert_eq!(
            stats.max_emu_ms, 1.0,
            "the 500 ms frame is out of the window"
        );
        assert!(
            (stats.avg_emu_ms - 1.0).abs() < 0.01,
            "average {} is the window's own",
            stats.avg_emu_ms
        );
        assert!((stats.fps - 60.0).abs() < 0.01);
    }

    /// The window slides rather than tumbling: ten seconds of ordinary frames are
    /// still 60 fps, and still sixty-odd records rather than ten seconds of them.
    #[test]
    fn ten_seconds_of_frames_are_still_read_as_one_second_of_them() {
        let mut meter = FpsMeter::new();
        record_all(&mut meter, 600, TICK_60, 1.0);
        let stats = meter.stats();
        assert!((stats.fps - 60.0).abs() < 0.01);
        assert!((stats.avg_emu_ms - 1.0).abs() < 0.01);
        assert_eq!(stats.max_emu_ms, 1.0);
    }

    /// A tab that was hidden gets one enormous gap. The frame before the gap is
    /// outside the window by the time the next one arrives, so the reading comes
    /// back on its own without a reset.
    #[test]
    fn a_five_second_gap_leaves_the_window_empty_and_the_meter_recovers() {
        let mut meter = FpsMeter::new();
        record_all(&mut meter, 60, TICK_60, 1.0);
        for index in 1..=60 {
            meter.record(5000.0 + f64::from(index) * TICK_60, 1.0);
        }
        let stats = meter.stats();
        assert!((stats.fps - 60.0).abs() < 0.01);
        assert_eq!(stats.max_emu_ms, 1.0);
    }

    /// A clock that steps backwards is a clock that cannot be measured against, so
    /// it starts the table again rather than producing a negative span.
    #[test]
    fn a_clock_that_steps_backwards_starts_the_window_again() {
        let mut meter = FpsMeter::new();
        record_all(&mut meter, 60, TICK_60, 1.0);
        meter.record(0.0, 2.0);
        assert_eq!(meter.stats().max_emu_ms, 2.0);
        meter.record(TICK_60, 2.0);
        assert!((meter.stats().fps - 60.0).abs() < 0.01);
    }

    /// The one line of the overlay, formatted as the ticket writes it.
    #[test]
    fn the_overlay_line_is_the_reading_in_one_string() {
        let stats = Stats {
            fps: 59.88,
            avg_emu_ms: 2.14,
            max_emu_ms: 18.75,
        };
        assert_eq!(stats.label(), "60 fps  2.1 ms");
        assert_eq!(Stats::default().label(), "0 fps  0.0 ms");
    }

    /// Nothing measured yet is `0 fps`, and the overlay shows that rather than an
    /// empty box: an em dash would be a second way to say zero.
    #[test]
    fn a_reading_of_nothing_still_says_something() {
        assert_eq!(FpsMeter::new().stats().label(), "0 fps  0.0 ms");
    }
}
