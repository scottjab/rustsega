//! How many emulator frames one tick owes, measured against the audio clock
//! (M7-05, #138).
//!
//! Once the Web Audio context is running it is the master clock: the loop is
//! ahead of the speakers exactly as far as it has scheduled, so the size of that
//! lead says whether the console is keeping up. [`frames_to_run`] is that
//! decision - pure, no browser, no `AudioContext`, a `buffered_secs` handed in by
//! the caller - and it replaces [`crate::frame_pacer::FramePacer`] for as long as
//! the context is running. The pacer stays as the fallback for a suspended
//! context: an autoplay-blocked browser still has to play the game, silently.
//!
//! The other half here, [`deinterleave`], is the sample-format conversion of
//! [`genesis::Genesis::drain_audio`] - interleaved stereo `i16` in, two `f32`
//! channels out for [`web_sys::AudioBuffer::copy_to_channel`]. It is kept here for
//! the same reason: it is arithmetic, and arithmetic is testable on the host.

/// The scheduled-ahead amount the loop aims for, in seconds: three frames of a
/// 60 Hz console, and a fifth of the 300 ms a browser typically lets a tab get
/// away with before it starts sounding like a stuttering record.
///
/// Bigger is more robust against a hiccup in the rAF loop - a frame that arrives
/// 20 ms late still has somewhere to be scheduled - and smaller is less delay
/// between pressing a button and hearing the game answer, which is why this is not
/// a second.
pub const TARGET_AHEAD_SECS: f64 = 0.06;

/// The most frames a single tick will ever run.
///
/// The same spiral-of-death cap as [`crate::frame_pacer`]: an audio lead that has
/// been dropped on the floor - a tab restored after a minute hidden - is not
/// recoverable by running a hundred frames now, and trying is what makes the tab
/// unresponsive.
pub const MAX_FRAMES_PER_TICK: u32 = 4;

/// A whole frame of slack that does not count as a deficit.
///
/// `buffered_secs` arrives as the difference of two `AudioContext` times, so a
/// schedule that is exactly full reads as `0.06 - 0.016666666666666666 * 4` and a
/// `ceil` on the residue of that asks for a frame nobody needed. A nanosecond of
/// lead is neither a frame nor nothing, and rounding it down is what keeps a
/// healthy loop from creeping ahead.
const DEFICIT_TOLERANCE: f64 = 1e-9;

/// How many console frames this tick should run to keep the schedule at
/// `target_secs` ahead of the play head.
///
/// `buffered_secs` is how far the schedule is ahead right now - for Web Audio,
/// `next_start_time - ctx.currentTime` - which may be negative when the loop has
/// fallen behind the speakers. `frame_secs` is one console frame, so 1/60 for
/// NTSC and 1/50 for PAL: the console rate, not the display rate, which is the
/// whole point of pacing on audio rather than on `requestAnimationFrame`.
///
/// Returns at most `max_frames`; frames the cap refused are dropped rather than
/// banked, exactly as [`crate::frame_pacer::FramePacer`] drops them.
pub fn frames_to_run(
    buffered_secs: f64,
    target_secs: f64,
    frame_secs: f64,
    max_frames: u32,
) -> u32 {
    // A caller with no frame length - a rate of 0, or a NaN from a clock that has
    // not been read yet - gets "wait", which is the answer that does not spin.
    if max_frames == 0 || frame_secs.is_nan() || frame_secs <= 0.0 {
        return 0;
    }
    let deficit = target_secs - buffered_secs;
    // NaN here is a clock that has not been read, and reads as "already full".
    if deficit.is_nan() || deficit <= 0.0 {
        return 0;
    }
    let frames = (deficit / frame_secs - DEFICIT_TOLERANCE).ceil();
    frames.clamp(0.0, f64::from(max_frames)) as u32
}

/// One `i16` as a Web Audio `-1.0..=1.0` sample: a power of two, so the
/// multiplication is exact and `i16::MIN` lands on `-1.0` rather than near it.
const FULL_SCALE: f32 = 1.0 / 32768.0;

/// Splits interleaved stereo `i16` into two `f32` channels.
///
/// `src` is what [`genesis::Genesis::drain_audio`] appends - `left right left
/// right ...` - and `left`/`right` are what [`web_sys::AudioBuffer::copy_to_channel`]
/// takes. Both are cleared first and keep their capacity, so a caller holds two
/// vectors for the life of the console rather than allocating per frame
/// (docs/ARCHITECTURE.md: "Core crates must not allocate per frame in hot paths").
///
/// The scale is `1 / 32768`, so `-32768` is exactly `-1.0` and `32767` is one
/// step short of `1.0`: dividing by `32767` would make the most negative sample
/// overflow the `-1.0..=1.0` range an `AudioBuffer` expects.
///
/// A trailing half-sample - an odd `src`, which the console never produces but a
/// caller could hand this - is dropped rather than written to one channel alone.
pub fn deinterleave(src: &[i16], left: &mut Vec<f32>, right: &mut Vec<f32>) {
    left.clear();
    right.clear();
    let frames = src.len() / 2;
    left.reserve(frames);
    right.reserve(frames);
    // `as_chunks` is what drops the trailing half-sample.
    for [low, high] in src.as_chunks::<2>().0 {
        left.push(f32::from(*low) * FULL_SCALE);
        right.push(f32::from(*high) * FULL_SCALE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of a 60 Hz console, seconds.
    const NTSC: f64 = 1.0 / 60.0;
    /// One frame of a 50 Hz console, seconds.
    const PAL: f64 = 1.0 / 50.0;

    /// Nothing scheduled at all, the state at the first tick and after every
    /// stall: run as many frames as the cap allows.
    #[test]
    fn an_empty_schedule_runs_to_the_cap() {
        assert_eq!(frames_to_run(0.0, TARGET_AHEAD_SECS, NTSC, 4), 4);
    }

    /// Just under the target: one frame takes it over, two overshoots.
    #[test]
    fn a_schedule_a_fifth_short_runs_one_frame() {
        assert_eq!(frames_to_run(0.05, TARGET_AHEAD_SECS, NTSC, 4), 1);
    }

    /// At and beyond the target the console waits for the speakers.
    #[test]
    fn a_schedule_at_or_past_the_target_runs_nothing() {
        assert_eq!(
            frames_to_run(TARGET_AHEAD_SECS, TARGET_AHEAD_SECS, NTSC, 4),
            0
        );
        assert_eq!(frames_to_run(0.07, TARGET_AHEAD_SECS, NTSC, 4), 0);
        assert_eq!(frames_to_run(12.0, TARGET_AHEAD_SECS, NTSC, 4), 0);
    }

    /// The cap is absolute, whatever the deficit: catching up is not worth a
    /// frozen tab.
    #[test]
    fn it_never_returns_more_than_max_frames() {
        assert_eq!(frames_to_run(0.0, 10.0, NTSC, 3), 3);
        assert_eq!(frames_to_run(-5.0, TARGET_AHEAD_SECS, NTSC, 4), 4);
        assert_eq!(frames_to_run(0.0, TARGET_AHEAD_SECS, NTSC, 0), 0);
    }

    /// PAL frames are a fiftieth of a second, not a sixtieth: two of them cover
    /// the deficit that would take three NTSC frames.
    #[test]
    fn a_pal_frame_is_counted_as_a_fiftieth_of_a_second() {
        assert_eq!(frames_to_run(0.03, TARGET_AHEAD_SECS, PAL, 4), 2);
        // Three 20 ms frames fill the 60 ms target exactly, and a fourth is not
        // asked for: an NTSC loop from empty would want four.
        assert_eq!(frames_to_run(0.0, TARGET_AHEAD_SECS, PAL, 4), 3);
    }

    /// Rounding: a schedule exactly `k` frames short of the target must ask for
    /// `k`, not `k + 1`, or every tick of a healthy loop runs a spare frame.
    #[test]
    fn a_schedule_exactly_whole_frames_short_runs_those_frames() {
        assert_eq!(
            frames_to_run(TARGET_AHEAD_SECS - 2.0 * NTSC, TARGET_AHEAD_SECS, NTSC, 4),
            2
        );
        assert_eq!(
            frames_to_run(TARGET_AHEAD_SECS - NTSC, TARGET_AHEAD_SECS, NTSC, 4),
            1
        );
    }

    /// Degenerate inputs are answered with "run nothing", not a panic and not an
    /// infinite loop of one frame per tick: a caller that has lost its frame rate
    /// should stop the console rather than spin it.
    #[test]
    fn a_frame_that_is_not_a_length_runs_nothing() {
        assert_eq!(frames_to_run(0.0, TARGET_AHEAD_SECS, 0.0, 4), 0);
        assert_eq!(frames_to_run(0.0, TARGET_AHEAD_SECS, -NTSC, 4), 0);
    }

    #[test]
    fn a_full_stereo_pair_splits_into_two_equal_channels() {
        let mut left = Vec::new();
        let mut right = Vec::new();
        deinterleave(&[1, -1, 2, -2, 3, -3], &mut left, &mut right);
        assert_eq!(left, [1.0, 2.0, 3.0].map(|sample| sample / 32768.0));
        assert_eq!(right, [-1.0, -2.0, -3.0].map(|sample| sample / 32768.0));
    }

    /// `i16::MAX` is one step below full scale and `i16::MIN` is exactly full
    /// scale the other way: see the scaling note on [`deinterleave`].
    #[test]
    fn full_scale_is_minus_one_and_just_short_of_plus_one() {
        let mut left = Vec::new();
        let mut right = Vec::new();
        deinterleave(
            &[i16::MAX, i16::MIN, i16::MIN, i16::MAX],
            &mut left,
            &mut right,
        );
        assert!((left[0] - 1.0).abs() < 1e-4, "{} is near 1.0", left[0]);
        assert!(left[0] < 1.0, "the positive end never reaches 1.0");
        assert_eq!(left[1], -1.0);
        assert_eq!(right[0], -1.0);
        assert!((right[1] - 1.0).abs() < 1e-4);
    }

    /// Silence is exactly zero either way, and the sign of a small sample
    /// survives: a channel that lost its sign would sound the same only on
    /// paper.
    #[test]
    fn silence_is_zero_and_signs_survive() {
        let mut left = Vec::new();
        let mut right = Vec::new();
        deinterleave(&[0, 0, -1, 1], &mut left, &mut right);
        assert_eq!(left, vec![0.0, -(1.0 / 32768.0)]);
        assert_eq!(right, vec![0.0, 1.0 / 32768.0]);
    }

    /// An empty drain leaves both channels empty: the loop calls this every
    /// frame, and a frame that produced no audio must schedule nothing.
    #[test]
    fn an_empty_drain_leaves_both_channels_empty() {
        let mut left = vec![9.0];
        let mut right = vec![9.0];
        deinterleave(&[], &mut left, &mut right);
        assert!(left.is_empty());
        assert!(right.is_empty());
    }

    /// The reuse contract, same as `emulator::frame_to_rgba_bytes`: a second call
    /// replaces the channel data instead of appending to it, and does not hand
    /// back a new allocation.
    #[test]
    fn a_second_call_replaces_the_channels_rather_than_appending() {
        let mut left = Vec::new();
        let mut right = Vec::new();
        deinterleave(&[1, 2, 3, 4, 5, 6], &mut left, &mut right);
        let capacity = left.capacity();

        deinterleave(&[7, 8], &mut left, &mut right);
        assert_eq!(left, vec![7.0 / 32768.0]);
        assert_eq!(right, vec![8.0 / 32768.0]);
        assert_eq!(
            left.capacity(),
            capacity,
            "the channel buffers are reused, not rebuilt"
        );
    }

    /// A half pair cannot be played: one sample on one channel only is a click,
    /// so it goes the way of the sample it was not paired with.
    #[test]
    fn a_trailing_half_sample_is_dropped() {
        let mut left = Vec::new();
        let mut right = Vec::new();
        deinterleave(&[1, 2, 3], &mut left, &mut right);
        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        assert_eq!(left[0], 1.0 / 32768.0);
        assert_eq!(right[0], 2.0 / 32768.0);
    }
}
