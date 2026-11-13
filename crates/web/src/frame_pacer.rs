//! How many emulator frames one `requestAnimationFrame` tick owes (M7-04, #137).
//!
//! The browser offers animation frames at the display's rate; the console runs at
//! its own - 60 Hz for NTSC, 50 Hz for PAL. A 120 Hz display therefore has to run
//! the console on every other tick, and a 30 Hz one twice per tick. This is the
//! whole policy of the loop until [`crate::emulator`] has audio to pace itself
//! against, which is M7-05 (#138): the audio clock is the master and this module
//! goes away with the guess.
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

/// How far a tick may be behind the console rate before it stops mattering.
///
/// A 60 Hz console paced at 1000 / 60 ms is a comparison of nearly equal floats,
/// so each tick leaves a residue of about 1e-16 frames. Ignored, those add up over
/// an afternoon into a frame that is never run; taken at face value, they make a
/// tick that should be a 1 read as a 0. A nanoframe is neither.
const DRIFT_TOLERANCE: f64 = 1e-9;

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

        let due = (self.debt + DRIFT_TOLERANCE).floor().max(0.0) as u32;
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
