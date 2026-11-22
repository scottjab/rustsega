//! How many emulator frames one `requestAnimationFrame` tick owes (M7-04, #137).
//!
//! The browser offers animation frames at the display's rate; the console runs at
//! its own - 60 Hz for NTSC, 50 Hz for PAL. A 120 Hz display therefore has to run
//! the console on every other tick, and a 30 Hz one twice per tick. The audio
//! clock is the master ([`crate::emulator`], M7-05, #138); this module is the
//! fallback used when the audio context has not started yet, so the loop has a
//! pacing policy before the first sample.
//!
//! Pure, and the reason the loop is testable at all: no browser, no clock, a
//! `now_ms` handed in by the caller.

/// The most frames a single tick will ever be told to run.
///
/// A tab in the background stops getting animation frames entirely, and a tab
/// that comes back after five seconds would otherwise be asked for three hundred
/// frames at once - the spiral of death, where catching up is what makes the
/// catch-up worse. Four is long enough to look smooth after a brief stall and
/// short enough that the emulation clock never runs away from the wall clock by
/// more than a twentieth of a second.
const MAX_CATCHUP: u32 = 4;

/// How much of a frame a tick may fall short by and still be owed that frame.
///
/// The debt pays out a frame once it reaches `1 - SLACK` rather than 1, so it
/// settles a quarter of a frame clear of both edges: a tick on a 60 Hz display
/// leaves it near 0, half-ticks on a 120 Hz one walk it between 0 and a half.
/// That margin is what absorbs jitter in the timestamps. Safari gives
/// `requestAnimationFrame` whole milliseconds (17, 17, 16 for a 60 Hz display),
/// which is 6% of a frame either way; with the pay-out at exactly 1 the debt
/// sat on the edge, and the ticks ran 1, 0, 2 frames, a new picture on two
/// ticks out of three and sound that stuttered with it (#629).
///
/// It is a shift, not a gain: the debt is still every millisecond that passed
/// minus every frame paid, so the console keeps its rate over any length of
/// time. It also covers the 1e-16 residue a 1000 / 60 ms interval leaves.
const SLACK: f64 = 0.25;

/// The slowest fast forward the shell offers, and the floor under a speed that has
/// rotted in storage (M7-17, #150). One is not a fast forward, and zero would be a
/// console that stops rather than speeds up.
const MIN_FAST_FORWARD_SPEED: u32 = 2;

/// The fastest fast forward the shell offers.
const MAX_FAST_FORWARD_SPEED: u32 = 8;

/// The most frames fast forward will ever run in one animation frame.
///
/// [`MAX_CATCHUP`] is the ceiling for a loop trying to keep up; fast forward is a
/// loop deliberately not trying to keep up, and it gets twice that. Eight frames of
/// emulation is a tick that still fits inside one animation frame on a machine that
/// is doing nothing else, which is the bargain a player strikes by holding the key -
/// and past it the tick starts missing the animation frames it is paced by, which
/// makes the whole thing slower rather than faster.
const MAX_FAST_FORWARD_FRAMES_PER_TICK: u32 = 8;

/// How many frames one tick runs while fast forward is held.
///
/// `base` is what the same tick would have run at normal speed, and it comes from
/// [`FramePacer::frames_due`] rather than from [`crate::audio_pacing::frames_to_run`]
/// for one reason: fast forward schedules no sound, so the audio lead it would read
/// is a number that has already noticed the console is not keeping the speakers fed
/// and is asking for the cap on top of the multiplier.
///
/// A `speed` outside the range the settings offer is the nearest end of it, so a
/// stored setting cannot be a stall or a runaway.
pub fn ff_frames(speed: u32, base: u32) -> u32 {
    base.saturating_mul(speed.clamp(MIN_FAST_FORWARD_SPEED, MAX_FAST_FORWARD_SPEED))
        .min(MAX_FAST_FORWARD_FRAMES_PER_TICK)
}

/// Turns `requestAnimationFrame` timestamps into a number of `Genesis::run_frame`
/// calls. See the [module docs](self) for what it is for and what replaces it.
#[derive(Clone, Debug)]
pub struct FramePacer {
    /// The length of one console frame in ms: `1000 / target_hz`.
    interval_ms: f64,
    /// Console frames that have passed but not yet been handed out, which is how
    /// a 120 Hz display is made to skip every other tick rather than drift.
    debt: f64,
    /// The timestamp of the previous tick; `None` until the first one arrives.
    last_ms: Option<f64>,
}

impl FramePacer {
    /// A pacer for a console running at `target_hz`: 60.0 for NTSC, 50.0 for PAL.
    pub fn new(target_hz: f64) -> Self {
        Self {
            interval_ms: 1000.0 / target_hz,
            debt: 0.0,
            last_ms: None,
        }
    }

    /// How many frames to run for an animation frame that arrived at `now_ms`.
    ///
    /// `now_ms` is the timestamp the browser gives a `requestAnimationFrame`
    /// callback: milliseconds, monotonic, shared by every callback of the frame.
    pub fn frames_due(&mut self, now_ms: f64) -> u32 {
        let Some(last_ms) = self.last_ms.replace(now_ms) else {
            // Nothing has been drawn yet, so the first tick always draws: waiting
            // one frame for the debt to build would be a black canvas for the
            // sake of arithmetic.
            return 1;
        };
        // A clock that steps backwards owes nothing, and must not be allowed to
        // save up the difference as credit.
        self.debt += (now_ms - last_ms).max(0.0) / self.interval_ms;

        if self.debt > MAX_CATCHUP as f64 {
            // The frames are gone: drop them, rather than paying them out four at
            // a time for the next five seconds.
            self.debt = 0.0;
            return MAX_CATCHUP;
        }

        let due = (self.debt + SLACK).floor().max(0.0) as u32;
        self.debt -= f64::from(due);
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One tick of a 60 Hz display, ms.
    const TICK_60: f64 = 1000.0 / 60.0;
    /// One tick of a 120 Hz display, ms.
    const TICK_120: f64 = 1000.0 / 120.0;
    /// One tick of a 30 Hz display, ms.
    const TICK_30: f64 = 1000.0 / 30.0;

    /// Nothing is on screen until the first tick says so, and a first tick that
    /// returned 0 would hold up the picture by a frame for no reason.
    #[test]
    fn the_first_tick_runs_one_frame() {
        let mut pacer = FramePacer::new(60.0);
        assert_eq!(pacer.frames_due(0.0), 1);
    }

    /// The base case: a 60 Hz console on a 60 Hz display, one frame per tick,
    /// forever - no drift, and no frame double-run because of rounding.
    #[test]
    fn a_60_hz_console_on_a_60_hz_display_runs_one_frame_per_tick() {
        let mut pacer = FramePacer::new(60.0);
        for tick in 0..600 {
            assert_eq!(pacer.frames_due(tick as f64 * TICK_60), 1, "tick {tick}");
        }
    }

    /// A 120 Hz display gets the console at half that rate, so half its ticks
    /// must run nothing at all. Alternating rather than 1,1,0: 1,0 is what keeps
    /// the two clocks together.
    #[test]
    fn a_60_hz_console_on_a_120_hz_display_alternates_one_and_zero() {
        let mut pacer = FramePacer::new(60.0);
        // The first tick is the one frame the pacer always gives away; the
        // alternation is what follows it.
        assert_eq!(pacer.frames_due(0.0), 1);
        for tick in 1..20 {
            let want = if tick % 2 == 1 { 0 } else { 1 };
            assert_eq!(
                pacer.frames_due(tick as f64 * TICK_120),
                want,
                "tick {tick} of a 120 Hz display"
            );
        }
    }

    /// The other direction: a display slower than the console owes two frames
    /// per tick, or the game runs at half speed.
    #[test]
    fn a_60_hz_console_on_a_30_hz_display_runs_two_frames_per_tick() {
        let mut pacer = FramePacer::new(60.0);
        // The first tick is still the one free frame: no console time has passed
        // yet, so nothing is overdue.
        assert_eq!(pacer.frames_due(0.0), 1);
        for tick in 1..20 {
            assert_eq!(pacer.frames_due(tick as f64 * TICK_30), 2, "tick {tick}");
        }
    }

    /// Safari hands `requestAnimationFrame` whole milliseconds: a 60 Hz display
    /// reads 17, 17, 16 rather than 16.667 three times (#629). Ticks that close
    /// to one frame must each still be one frame, not a 1, 0, 2 judder that
    /// shows a new picture on two ticks out of three.
    fn safari_ms(tick: u32, tick_ms: f64, start_ms: f64) -> f64 {
        (start_ms + f64::from(tick) * tick_ms).round()
    }

    #[test]
    fn whole_millisecond_ticks_on_a_60_hz_display_run_one_frame_each() {
        for start in [0.0, 62.0, 1234.4, 7.9] {
            let mut pacer = FramePacer::new(60.0);
            for tick in 0..600 {
                assert_eq!(
                    pacer.frames_due(safari_ms(tick, TICK_60, start)),
                    1,
                    "tick {tick} from {start} ms"
                );
            }
        }
    }

    #[test]
    fn whole_millisecond_ticks_on_a_120_hz_display_still_alternate() {
        for start in [0.0, 62.0, 1234.4, 7.9] {
            let mut pacer = FramePacer::new(60.0);
            assert_eq!(pacer.frames_due(safari_ms(0, TICK_120, start)), 1);
            for tick in 1..240 {
                let want = if tick % 2 == 1 { 0 } else { 1 };
                assert_eq!(
                    pacer.frames_due(safari_ms(tick, TICK_120, start)),
                    want,
                    "tick {tick} from {start} ms"
                );
            }
        }
    }

    #[test]
    fn whole_millisecond_ticks_on_a_30_hz_display_run_two_frames_each() {
        for start in [0.0, 62.0, 1234.4, 7.9] {
            let mut pacer = FramePacer::new(60.0);
            assert_eq!(pacer.frames_due(safari_ms(0, TICK_30, start)), 1);
            for tick in 1..120 {
                assert_eq!(
                    pacer.frames_due(safari_ms(tick, TICK_30, start)),
                    2,
                    "tick {tick} from {start} ms"
                );
            }
        }
    }

    /// The slack that absorbs the rounding must not become drift: an hour of
    /// whole-millisecond ticks is still an hour of console frames, give or take
    /// the one the debt is holding.
    #[test]
    fn whole_millisecond_ticks_keep_the_console_rate_over_an_hour() {
        let mut pacer = FramePacer::new(60.0);
        let ticks = 60 * 60 * 60;
        let total: u32 = (0..=ticks)
            .map(|tick| pacer.frames_due(safari_ms(tick, TICK_60, 3.0)))
            .sum();
        assert!(
            total.abs_diff(ticks + 1) <= 1,
            "{total} frames for {ticks} ticks"
        );
    }

    /// PAL is the other console rate: 250 frames of a 50 Hz machine in the five
    /// seconds a 60 Hz display spends on 300 ticks.
    #[test]
    fn a_50_hz_console_on_a_60_hz_display_holds_its_rate() {
        let mut pacer = FramePacer::new(50.0);
        let mut total = 0;
        for tick in 1..=300 {
            total += pacer.frames_due(tick as f64 * TICK_60);
        }
        assert_eq!(total, 250);
    }

    /// Five seconds without a tick - a hidden tab, a breakpoint, a dragged
    /// window. The frames are gone; running three hundred of them now would
    /// freeze the tab doing it.
    #[test]
    fn a_five_second_gap_catches_up_with_at_most_four_frames() {
        let mut pacer = FramePacer::new(60.0);
        assert_eq!(pacer.frames_due(0.0), 1);
        assert_eq!(pacer.frames_due(5000.0), MAX_CATCHUP);
    }

    /// The other half of not spiralling: the frames the cap refused to run are
    /// dropped, not banked, so the tick after the gap is an ordinary one.
    #[test]
    fn after_a_long_gap_the_very_next_tick_is_ordinary_again() {
        let mut pacer = FramePacer::new(60.0);
        pacer.frames_due(0.0);
        pacer.frames_due(5000.0);
        assert_eq!(pacer.frames_due(5000.0 + TICK_60), 1);
    }

    /// Fast forward is a multiplier on what a tick already owed, so a 120 Hz
    /// display still runs nothing on its skipped ticks and a 30 Hz one still runs
    /// its two - the console goes faster without the loop changing what a frame is.
    #[test]
    fn fast_forward_runs_the_frames_due_multiplied_by_the_speed() {
        assert_eq!(ff_frames(2, 1), 2);
        assert_eq!(ff_frames(4, 1), 4);
        assert_eq!(ff_frames(4, 2), 8);
    }

    /// The cap is absolute, and it is what keeps a 30 Hz display holding a 60 Hz
    /// console at 8 frames a tick rather than the 8.08 it would want at 4x: a tick
    /// that ran thirty frames would be a tick long enough to miss the next one.
    #[test]
    fn fast_forward_never_runs_more_than_eight_frames_in_one_tick() {
        assert_eq!(ff_frames(4, 4), 8);
        assert_eq!(ff_frames(MAX_FAST_FORWARD_SPEED, MAX_CATCHUP), 8);
        assert_eq!(ff_frames(MAX_FAST_FORWARD_SPEED, 1000), 8);
    }

    /// A tick that owes nothing runs nothing fast forward either: the multiplier is
    /// of the frames due, not a floor under them.
    #[test]
    fn a_tick_that_owes_nothing_runs_nothing_fast_forward_or_otherwise() {
        assert_eq!(ff_frames(4, 0), 0);
    }

    /// A speed outside the range the settings offer is the nearest end of it, so
    /// a stored setting that has rotted is a slow fast forward rather than a way
    /// to divide by zero or run the console backwards.
    #[test]
    fn a_speed_outside_the_offered_range_is_the_nearest_end_of_it() {
        assert_eq!(ff_frames(0, 1), MIN_FAST_FORWARD_SPEED);
        assert_eq!(ff_frames(1, 1), MIN_FAST_FORWARD_SPEED);
        assert_eq!(ff_frames(MAX_FAST_FORWARD_SPEED + 1, 1), 8);
    }
}
