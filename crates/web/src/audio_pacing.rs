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

/// How many console frames of sound one scheduled block carries on a WebKit
/// browser (#629). Safari on macOS and iOS is the one engine reporting choppy
/// audio and rendering, and the cost the measurement could put numbers on is
/// per-second scheduling churn: a fresh `AudioBufferSourceNode` plus two
/// channel copies every console frame. Four frames per block is a fifteenth of
/// a second of sound per node - 15 nodes a second instead of 60 - while
/// staying inside [`TARGET_AHEAD_SECS`], so the cursor never carries more than
/// the target's worth of undelivered sound. Chrome and Firefox keep one frame
/// per block: the 60-second per-engine measurement shows the per-frame scheme
/// working there with nothing to fix.
pub const WEBKIT_FRAMES_PER_BLOCK: u32 = 4;

/// How many console frames of sound one scheduled block carries for a browser,
/// read off its user agent.
///
/// The WebKit family is "says `Safari` and does not say `Chrome`": Chrome's UA
/// still carries the historical `Safari` token, and the whole Chromium family
/// (Chrome, Edge, Opera, Brave) carries `Chrome`. Firefox names itself. A UA a
/// browser did not hand over is not a reason to change anything.
///
/// This is a scheduling-shape cut, not a capability test: both answers produce
/// the same sound, the same cursor arithmetic and the same underrun counting;
/// see [`BlockBatcher`] for what batching changes.
pub fn frames_per_block_for(user_agent: &str) -> u32 {
    let webkit = user_agent.contains("Safari") && !user_agent.contains("Chrome");
    if webkit { WEBKIT_FRAMES_PER_BLOCK } else { 1 }
}

/// The scheduled-ahead amount a batched browser aims for: one whole batch, plus
/// three frames of room for the pacer to see the deficit and run before the play
/// head reaches the cursor.
///
/// The why is measured (#629): a batched cursor advances only once per batch, so
/// between two flushes the play head eats up to a whole batch length out of the
/// lead while the batcher is still gathering. At the plain [`TARGET_AHEAD_SECS`]
/// the lead's floor lands right on the too-late line and the schedule restarts
/// about twice a second; at one-batch-plus-two the floor sits three frames
/// clear.
///
/// The third frame came with [`lead_secs`] counting the batcher's pending frames
/// (#629): a batch now flushes the tick the lead needs it instead of a tick
/// late, so the cursor sits up to a batch lower than before and one batch plus
/// two placed blocks only 33 ms ahead. Plus three puts that back at 50 ms. The
/// total lead is still lower than the burst-paced loop ran at, which held the
/// cursor 110-145 ms ahead. Unbatched browsers keep [`TARGET_AHEAD_SECS`].
pub fn target_ahead_secs(frames_per_block: u32, frame_secs: f64) -> f64 {
    if frames_per_block <= 1 {
        TARGET_AHEAD_SECS
    } else {
        (f64::from(frames_per_block) + 3.0) * frame_secs
    }
}

/// Holds console frames of sound until one scheduled block's worth has
/// gathered, for the browsers that get batched blocks (#629, and
/// [`frames_per_block_for`] for where the count comes from).
///
/// The caller is [`crate::audio::AudioOut::push`], which is handed one console
/// frame at a time and schedules one block at a time; the batcher sits between
/// the two. Nothing else about the schedule changes: the block still starts at
/// the cursor and the cursor still moves by the block's own length, so the
/// pacing arithmetic ([`frames_to_run`]) is untouched. A cursor re-seat -
/// [`crate::audio::AudioOut::discard`], `resume`, `suspend` - drops whatever
/// is pending, because those samples belong to a moment the schedule no longer
/// covers.
///
/// A batcher built with one frame per block hands every push straight back, so
/// the non-WebKit browsers take the same path they always took.
pub struct BlockBatcher {
    /// The frames gathered so far, interleaved stereo, exactly as they arrived.
    pending: Vec<i16>,
    /// Kept empty but reserved, so handing a batch out (`std::mem::swap`)
    /// never gives the pending buffer's allocation back to the allocator.
    spare: Vec<i16>,
    /// The sample count one scheduled block carries: frames per block times
    /// samples per frame.
    capacity: usize,
}

impl BlockBatcher {
    /// A batcher for `frames_per_block` console frames of `samples_per_frame`
    /// interleaved samples each. `frames_per_block` of one means every push is
    /// a full block: the batcher never holds anything.
    pub fn new(frames_per_block: u32, samples_per_frame: usize) -> Self {
        let capacity = frames_per_block.max(1) as usize * samples_per_frame;
        Self {
            pending: Vec::with_capacity(capacity),
            spare: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds one console frame of interleaved samples. Returns the whole block
    /// once at least one block's worth has gathered; `None` while it is still
    /// short.
    pub fn push(&mut self, samples: &[i16]) -> Option<Vec<i16>> {
        self.pending.extend_from_slice(samples);
        if self.capacity == 0 || self.pending.len() < self.capacity {
            return None;
        }
        let block = std::mem::take(&mut self.pending);
        std::mem::swap(&mut self.pending, &mut self.spare);
        self.spare.clear();
        Some(block)
    }

    /// Drops whatever is pending: the sound of a moment the console has left.
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    /// Whether anything is pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// How many interleaved samples are gathered but not yet handed out as a
    /// block: the sound [`lead_secs`] has to count, because the console has
    /// already made it even though no block carries it yet.
    pub fn pending_samples(&self) -> usize {
        self.pending.len()
    }
}

/// How far ahead of the play head the console's sound reaches, seconds: the
/// scheduled part (`next_time - now`, the cursor minus `currentTime`) plus the
/// `pending_samples` a [`BlockBatcher`] is still holding, at `samples_per_sec`
/// interleaved samples a second (sample rate times channels).
///
/// The pending part is what keeps a batched schedule paced one frame per tick
/// (#629). Leave it out and the lead only moves when a whole batch flushes:
/// the pacer sees no progress for the frames it ran into the batcher, runs
/// them again on the next tick, and then sits idle while the flushed batch
/// drains - the `0 0 1 2 0 0 1 2 3` frames-per-tick pattern that showed on
/// Safari as dropped frames, because a tick paints only its last frame.
pub fn lead_secs(next_time: f64, now: f64, pending_samples: usize, samples_per_sec: f64) -> f64 {
    let pending_secs = if samples_per_sec > 0.0 {
        pending_samples as f64 / samples_per_sec
    } else {
        0.0
    };
    next_time - now + pending_secs
}

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

    // ---- #629: batching console frames into fewer AudioBuffers on WebKit.
    // Safari (macOS + iOS) is the one engine reporting choppy audio and
    // rendering, and the difference this rig could measure is per-second
    // scheduling churn: one `AudioBufferSourceNode` plus two channel copies per
    // console frame. The batch arithmetic is pure so the engine cut and the
    // fill/flush behaviour are testable on the host.

    /// Safari desktop, what a macOS report shows.
    const SAFARI_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";
    /// Safari iOS, where the ticket says the same choppiness shows.
    const SAFARI_IOS: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";
    /// Chrome's UA still says Safari (historical), so "contains Safari" alone
    /// is not the cut.
    const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
    /// Edge, Opera and the rest of the Chromium family all carry the Chrome
    /// token.
    const EDGE: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0";
    /// Firefox has no Safari token at all.
    const FIREFOX: &str =
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0";

    /// The WebKit engines the ticket is about batch; everything else -
    /// Chromium family, Firefox, and a UA a browser did not hand over - keeps
    /// the per-frame scheme that the measurement shows working (60 s, 0
    /// underruns).
    #[test]
    fn only_webkit_engines_batch() {
        assert_eq!(frames_per_block_for(SAFARI_MAC), WEBKIT_FRAMES_PER_BLOCK);
        assert_eq!(frames_per_block_for(SAFARI_IOS), WEBKIT_FRAMES_PER_BLOCK);
        // A WPE/GTK WebKit UA (the engine the measurement ran on) batches too.
        assert_eq!(
            frames_per_block_for(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15"
            ),
            WEBKIT_FRAMES_PER_BLOCK
        );
        assert_eq!(frames_per_block_for(CHROME), 1);
        assert_eq!(frames_per_block_for(EDGE), 1);
        assert_eq!(frames_per_block_for(FIREFOX), 1);
        assert_eq!(frames_per_block_for(""), 1);
    }

    /// A batcher built for one frame per block never forms, which is what the
    /// non-WebKit path passes so its `push` stays a straight schedule.
    #[test]
    fn a_one_frame_batcher_schedules_every_push_at_once() {
        let mut batcher = BlockBatcher::new(1, 800);
        assert_eq!(batcher.push(&[0; 800]).map(|block| block.len()), Some(800));
    }

    /// The WebKit path: four console frames of 800 samples are held until the
    /// fourth arrives, and leave as one 3200-sample batch.
    #[test]
    fn four_frames_leave_as_one_batch() {
        let mut batcher = BlockBatcher::new(WEBKIT_FRAMES_PER_BLOCK, 800);
        assert_eq!(batcher.push(&[1; 800]), None);
        assert_eq!(batcher.push(&[2; 800]), None);
        assert_eq!(batcher.push(&[3; 800]), None);
        assert!(!batcher.is_empty());
        let batch = batcher
            .push(&[4; 800])
            .expect("the fourth push fills the batch");
        assert_eq!(batch.len(), 4 * 800);
        assert!(batch.starts_with(&[1; 800]));
        assert!(batch.ends_with(&[4; 800]));
        assert!(batcher.is_empty());
    }

    /// A partial batch left pending is dropped by `clear`, which is what a
    /// cursor re-seat (`discard`, `resume`, `suspend`) asks for: those samples
    /// belong to a moment the schedule no longer covers.
    #[test]
    fn clear_drops_a_pending_batch() {
        let mut batcher = BlockBatcher::new(WEBKIT_FRAMES_PER_BLOCK, 800);
        assert_eq!(batcher.push(&[1; 800]), None);
        batcher.clear();
        assert!(batcher.is_empty());
        // And the batcher keeps working after the drop.
        assert_eq!(batcher.push(&[2; 800]), None);
        assert_eq!(batcher.push(&[3; 800]), None);
        assert_eq!(batcher.push(&[4; 800]), None);
        assert_eq!(
            batcher.push(&[5; 800]).map(|block| block.len()),
            Some(4 * 800)
        );
    }

    /// A batch that arrives over capacity in one push still comes out, whole.
    #[test]
    fn an_oversized_push_comes_out_whole() {
        let mut batcher = BlockBatcher::new(WEBKIT_FRAMES_PER_BLOCK, 800);
        assert_eq!(batcher.push(&[7; 3 * 800]), None);
        assert_eq!(
            batcher.push(&[8; 2 * 800]).map(|block| block.len()),
            Some(5 * 800)
        );
    }

    /// The pending buffer is reused across batches: `push` after a flush must
    /// not pay the capacity back allocation by allocation (same contract as
    /// `deinterleave`'s reuse note above).
    #[test]
    fn the_pending_buffer_keeps_its_capacity_across_flushes() {
        let mut batcher = BlockBatcher::new(WEBKIT_FRAMES_PER_BLOCK, 800);
        assert_eq!(batcher.push(&[1; 800]), None);
        assert_eq!(batcher.push(&[2; 800]), None);
        assert_eq!(batcher.push(&[3; 800]), None);
        assert_eq!(
            batcher.push(&[4; 800]).map(|block| block.len()),
            Some(4 * 800)
        );
        // Second batch: three pushes pending again, and the same capacity is
        // already there.
        assert_eq!(batcher.push(&[5; 800]), None);
        assert_eq!(batcher.push(&[6; 800]), None);
        assert_eq!(batcher.push(&[7; 800]), None);
        assert_eq!(
            batcher.push(&[8; 800]).map(|block| block.len()),
            Some(4 * 800)
        );
    }

    // ---- #629, second half: the cursor target must cover a batch. A batched
    // cursor advances only once per batch, so between two flushes the play head
    // eats up to a whole batch length out of the lead while the batcher is still
    // gathering. Measured on WebKit: at the unbatched target the lead's floor
    // lands right on the 10 ms underrun line and the schedule restarts about
    // twice a second (28 underruns in the 60 s window); at a target of one
    // batch plus two frames of pacer room the floor sits three frames clear and
    // the window is clean.

    /// The unbatched target is the unbatched target.
    #[test]
    fn a_one_frame_browser_keeps_the_plain_target() {
        assert_eq!(target_ahead_secs(1, NTSC), TARGET_AHEAD_SECS);
        assert_eq!(target_ahead_secs(0, PAL), TARGET_AHEAD_SECS);
    }

    /// Batched blocks get one batch plus three frames: 4 NTSC frames of batch +
    /// 3 of room = 7 frames of lead, PAL 7 fiftieths.
    #[test]
    fn a_batched_browser_gets_a_batch_plus_three_frames() {
        assert_eq!(target_ahead_secs(WEBKIT_FRAMES_PER_BLOCK, NTSC), 7.0 * NTSC);
        assert_eq!(target_ahead_secs(WEBKIT_FRAMES_PER_BLOCK, PAL), 7.0 * PAL);
    }

    /// Where the lead bottoms out over a batch cycle: a batched cursor only
    /// advances once per batch, so between two flushes the play head eats up to
    /// a whole batch length out of the lead while the batcher is still
    /// gathering. The floor is therefore `target - batch + one frame`.
    fn batched_floor(target: f64, frames_per_block: u32, frame_secs: f64) -> f64 {
        target - f64::from(frames_per_block) * frame_secs + frame_secs
    }

    /// The 10 ms too-late margin of `AudioOut::UNDERRUN_SECS`, restated here
    /// because that constant is private to the browser module.
    const UNDERRUN_SECS: f64 = 0.01;

    /// At the plain target a batched cursor's floor lands exactly on the
    /// too-late line: 60 ms - 3 frames = 10 ms, which is what the first WebKit
    /// measurement with batching alone hit - the schedule restarting whenever
    /// a tick ran long (28 restarts in the measured 60 s window). This is the
    /// arithmetic reason the target has to deepen with the batch.
    #[test]
    fn a_batched_cursor_at_the_plain_target_floors_on_the_too_late_line() {
        let floor = batched_floor(TARGET_AHEAD_SECS, WEBKIT_FRAMES_PER_BLOCK, NTSC);
        assert!(
            floor <= UNDERRUN_SECS,
            "plain target with 4-frame batches floors at {floor}, under the {UNDERRUN_SECS} line"
        );
        assert!(
            (floor - UNDERRUN_SECS).abs() < 1e-9,
            "the floor is the line itself"
        );
    }

    /// At the batched target the floor sits clear of the line. In this
    /// burst-paced model it was 100 ms - 3 frames = 50 ms at the original
    /// one-batch-plus-two, which is what the re-measured WebKit window ran on
    /// (0 restarts in 60 s). Since the lead counts pending frames, the loop
    /// model's `a_batched_loop_places_every_block_two_frames_clear_of_the_line`
    /// is the floor that applies.
    #[test]
    fn the_batched_target_holds_the_floor_clear_of_the_line() {
        let target = target_ahead_secs(WEBKIT_FRAMES_PER_BLOCK, NTSC);
        let floor = batched_floor(target, WEBKIT_FRAMES_PER_BLOCK, NTSC);
        assert!(
            floor > UNDERRUN_SECS + 2.0 * NTSC,
            "the deep target floors at {floor}, inside jitter reach of the line"
        );
        // And the PAL batch, whose cycle is longer: 6 fiftieths - 4 fiftieths
        // + 1 = 3 fiftieths = 60 ms.
        let pal_target = target_ahead_secs(WEBKIT_FRAMES_PER_BLOCK, PAL);
        assert!(
            batched_floor(pal_target, WEBKIT_FRAMES_PER_BLOCK, PAL) > UNDERRUN_SECS + 2.0 * PAL
        );
    }

    /// One console frame of 48 kHz stereo at 60 Hz, interleaved samples.
    const FRAME_SAMPLES: usize = 1600;
    /// 48 kHz stereo, interleaved samples a second.
    const SAMPLES_PER_SEC: f64 = 96_000.0;

    #[test]
    fn pending_frames_count_toward_the_lead() {
        let lead = lead_secs(1.0, 0.9, 2 * FRAME_SAMPLES, SAMPLES_PER_SEC);
        assert!((lead - (0.1 + 2.0 / 60.0)).abs() < 1e-9, "lead {lead}");
    }

    #[test]
    fn an_empty_batcher_adds_nothing_to_the_lead() {
        let batch = BlockBatcher::new(4, FRAME_SAMPLES);
        assert_eq!(batch.pending_samples(), 0);
        assert_eq!(lead_secs(1.0, 0.9, 0, SAMPLES_PER_SEC), 1.0 - 0.9);
    }

    #[test]
    fn a_batcher_reports_what_it_holds_until_it_flushes() {
        let mut batch = BlockBatcher::new(4, FRAME_SAMPLES);
        let frame = [0i16; FRAME_SAMPLES];
        for held in 1..4 {
            assert!(batch.push(&frame).is_none());
            assert_eq!(batch.pending_samples(), held * FRAME_SAMPLES);
        }
        assert!(batch.push(&frame).is_some());
        assert_eq!(batch.pending_samples(), 0);
    }

    /// The emulator's audio-paced loop, modelled the way `AudioOut` and
    /// `Emulator::frames_due` run it: a play head moving in real time, one
    /// display tick every 60th of a second, [`frames_to_run`] deciding each
    /// tick from [`lead_secs`], every frame pushed through a [`BlockBatcher`],
    /// and a flushed block placed at the cursor with `AudioOut::schedule`'s
    /// too-late re-seat (10 ms line, 50 ms restart). Returns the frames each
    /// tick ran and the lowest scheduled lead (cursor minus play head) any
    /// block was placed against, both after a second of settling.
    fn run_loop(frames_per_block: u32) -> (Vec<u32>, f64) {
        let frame_secs = 1.0 / 60.0;
        let target = target_ahead_secs(frames_per_block, frame_secs);
        let mut batch = BlockBatcher::new(frames_per_block, FRAME_SAMPLES);
        let frame = [0i16; FRAME_SAMPLES];
        let mut next_time = 0.05;
        let mut ran = Vec::new();
        let mut floor = f64::INFINITY;
        for tick in 0..660 {
            let now = f64::from(tick) * frame_secs;
            let lead = lead_secs(next_time, now, batch.pending_samples(), SAMPLES_PER_SEC);
            let due = frames_to_run(lead, target, frame_secs, MAX_FRAMES_PER_TICK);
            for _ in 0..due {
                if let Some(block) = batch.push(&frame) {
                    if tick >= 60 {
                        floor = floor.min(next_time - now);
                    }
                    if next_time < now + 0.01 {
                        next_time = now + 0.05;
                    }
                    next_time += block.len() as f64 / SAMPLES_PER_SEC;
                }
            }
            if tick >= 60 {
                ran.push(due);
            }
        }
        (ran, floor)
    }

    fn frames_per_tick(frames_per_block: u32) -> Vec<u32> {
        run_loop(frames_per_block).0
    }

    /// Counting the pending frames (#629) means a batch flushes the moment the
    /// lead needs it rather than a tick late, so the scheduled part of the lead
    /// runs lower than the burst-paced loop let it. The target has to keep the
    /// lowest point a block is placed at two frames clear of the 10 ms
    /// too-late line, the same margin the measured WebKit window ran on.
    #[test]
    fn a_batched_loop_places_every_block_two_frames_clear_of_the_line() {
        let (_, floor) = run_loop(WEBKIT_FRAMES_PER_BLOCK);
        assert!(
            floor > UNDERRUN_SECS + 2.0 * NTSC,
            "blocks placed as close as {floor} s to the play head"
        );
    }

    #[test]
    fn a_batched_loop_runs_one_frame_every_tick() {
        let ran = frames_per_tick(WEBKIT_FRAMES_PER_BLOCK);
        assert!(ran.iter().all(|&due| due == 1), "frames per tick: {ran:?}");
    }

    #[test]
    fn an_unbatched_loop_runs_one_frame_every_tick() {
        let ran = frames_per_tick(1);
        assert!(ran.iter().all(|&due| due == 1), "frames per tick: {ran:?}");
    }
}
