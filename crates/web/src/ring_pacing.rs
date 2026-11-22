//! Pacing for the AudioWorklet ring output (#629): the pure half, so it is
//! testable on the host.
//!
//! With a ring, the console is paced on the display, not on the audio clock:
//! [`crate::frame_pacer::FramePacer`] says how many frames this animation frame
//! is owed (one, on a 60 Hz display), and [`nudge_frames`] adds or takes away a
//! frame only when the ring's lead has drifted out of a band around the
//! target. The audio clock never decides a single tick. That is the point: on
//! Safari, `AudioContext.currentTime` and the render callbacks move in coarse
//! steps on real hardware, and a loop that asked them every tick ran 0 or 2
//! frames on the ticks a step landed on, which is a dropped frame on screen.
//!
//! [`RingLead`] is how the main thread knows the lead at all: the worklet
//! reports how full the ring is and how many frames it has been sent, a few
//! times a second of render quanta, and the main thread adds what it has sent
//! since and takes away the time that has passed since the report.

/// How far ahead of the speakers the ring aims to hold the console's sound,
/// seconds. The same 60 ms the scheduled output aims for
/// ([`crate::audio_pacing::TARGET_AHEAD_SECS`]): the latency a player hears.
pub const RING_TARGET_SECS: f64 = 0.06;

/// How many console frames the lead may sit either side of the target before
/// [`nudge_frames`] steps in.
///
/// Two frames each side covers a report that is a render callback old (a
/// 1024-frame CoreAudio callback is 21 ms, a frame and a third) plus a tick
/// that ran late, so a ring that is merely being read coarsely is never
/// corrected; only real drift between the console rate and the display is.
pub const RING_BAND_FRAMES: f64 = 2.0;

/// The frames to run this tick: the display pacer's `display_due`, corrected
/// by the ring's `lead_secs` against `target_secs`.
///
/// * Lead under the band: run one frame more, or, when the ring is far
///   behind (a first fill, or after a stall), as many as it takes to reach
///   the target, capped at `max_frames`.
/// * Lead over the band: run one frame fewer, which is how a console a tenth
///   of a percent faster than the display stays put.
/// * Inside the band: exactly what the display asked for.
pub fn nudge_frames(
    display_due: u32,
    lead_secs: f64,
    target_secs: f64,
    frame_secs: f64,
    max_frames: u32,
) -> u32 {
    if frame_secs.is_nan() || frame_secs <= 0.0 || lead_secs.is_nan() {
        return display_due.min(max_frames);
    }
    let band = RING_BAND_FRAMES * frame_secs;
    if lead_secs < target_secs - band {
        let catch_up = ((target_secs - lead_secs) / frame_secs).floor() as u32;
        return (display_due + 1).max(catch_up).min(max_frames);
    }
    if lead_secs > target_secs + band {
        return display_due.saturating_sub(1);
    }
    display_due.min(max_frames)
}

/// The main thread's estimate of how much sound the ring holds.
///
/// Every push says how many frames it carries, and every frame sent is
/// counted in `sent`. The worklet's reports carry how many frames it holds and
/// how many it has *received* in all, so the frames still in flight between the
/// two threads (`sent - received`) are counted exactly once, whichever side of
/// the report they landed on.
#[derive(Clone, Debug, Default)]
pub struct RingLead {
    /// Frames sent to the worklet since the last reset.
    sent: f64,
    /// The worklet's fill at its last report, frames.
    fill: f64,
    /// The worklet's received count at its last report, frames.
    received: f64,
    /// When the last report arrived, `performance.now()` ms; `None` before
    /// the first.
    report_ms: Option<f64>,
}

impl RingLead {
    /// Counts `frames` more sent to the worklet.
    pub fn sent(&mut self, frames: usize) {
        self.sent += frames as f64;
    }

    /// Takes a report from the worklet: `fill` frames held, `received` frames
    /// received in all, arriving at `now_ms`.
    pub fn report(&mut self, fill: f64, received: f64, now_ms: f64) {
        self.fill = fill;
        self.received = received;
        self.report_ms = Some(now_ms);
    }

    /// The lead at `now_ms`, seconds at `rate` frames a second: the ring's fill,
    /// plus what is in flight, less what the speakers have eaten since the
    /// report. Before any report, everything sent counts as held.
    pub fn lead_secs(&self, now_ms: f64, rate: f64) -> f64 {
        if rate <= 0.0 {
            return 0.0;
        }
        let in_flight = (self.sent - self.received).max(0.0);
        let eaten = self
            .report_ms
            .map_or(0.0, |at| ((now_ms - at) / 1000.0).max(0.0));
        ((self.fill + in_flight) / rate - eaten).max(0.0)
    }

    /// Forgets everything: the ring has been told to empty itself.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Whether an output gets the ring: `?audio=ring` or `?audio=scheduled` in the
/// page's `search` string decides on any browser; otherwise a WebKit
/// `user_agent` does ([`crate::audio_pacing::frames_per_block_for`] is the same
/// cut).
pub fn wants_ring(user_agent: &str, search: &str) -> bool {
    let choice = search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("audio="));
    match choice {
        Some("ring") => true,
        Some("scheduled") => false,
        _ => crate::audio_pacing::frames_per_block_for(user_agent) > 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_pacer::FramePacer;

    const NTSC: f64 = 1.0 / 60.0;
    const RATE: f64 = 48_000.0;

    const SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
    const CHROME: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0 Safari/537.36";

    #[test]
    fn webkit_gets_the_ring_and_chrome_does_not() {
        assert!(wants_ring(SAFARI, ""));
        assert!(!wants_ring(CHROME, ""));
    }

    #[test]
    fn the_url_overrides_the_user_agent() {
        assert!(wants_ring(CHROME, "?audio=ring"));
        assert!(!wants_ring(SAFARI, "?audio=scheduled"));
        assert!(wants_ring(CHROME, "?rom=x&audio=ring"));
        assert!(wants_ring(SAFARI, "?audio=other"));
    }

    #[test]
    fn inside_the_band_the_display_decides() {
        assert_eq!(
            nudge_frames(1, RING_TARGET_SECS, RING_TARGET_SECS, NTSC, 4),
            1
        );
        assert_eq!(
            nudge_frames(0, RING_TARGET_SECS + NTSC, RING_TARGET_SECS, NTSC, 4),
            0
        );
        assert_eq!(
            nudge_frames(2, RING_TARGET_SECS - NTSC, RING_TARGET_SECS, NTSC, 4),
            2
        );
    }

    #[test]
    fn a_short_ring_gets_one_more_frame() {
        let lead = RING_TARGET_SECS - 2.5 * NTSC;
        assert_eq!(nudge_frames(1, lead, RING_TARGET_SECS, NTSC, 4), 2);
    }

    #[test]
    fn an_empty_ring_catches_up_to_the_cap() {
        assert_eq!(nudge_frames(1, 0.0, RING_TARGET_SECS, NTSC, 4), 3);
        assert_eq!(nudge_frames(1, 0.0, 0.2, NTSC, 4), 4);
    }

    #[test]
    fn a_full_ring_gets_one_frame_fewer() {
        let lead = RING_TARGET_SECS + 2.5 * NTSC;
        assert_eq!(nudge_frames(1, lead, RING_TARGET_SECS, NTSC, 4), 0);
        assert_eq!(nudge_frames(0, lead, RING_TARGET_SECS, NTSC, 4), 0);
    }

    #[test]
    fn a_frame_that_is_not_a_length_leaves_the_display_alone() {
        assert_eq!(nudge_frames(1, 0.0, RING_TARGET_SECS, 0.0, 4), 1);
        assert_eq!(nudge_frames(1, f64::NAN, RING_TARGET_SECS, NTSC, 4), 1);
    }

    #[test]
    fn before_a_report_everything_sent_is_held() {
        let mut lead = RingLead::default();
        lead.sent(4800);
        assert!((lead.lead_secs(1000.0, RATE) - 0.1).abs() < 1e-9);
    }

    #[test]
    fn frames_in_flight_count_once() {
        let mut lead = RingLead::default();
        lead.sent(1600);
        lead.sent(1600);
        // The worklet had received the first push when it reported.
        lead.report(1600.0, 1600.0, 0.0);
        let expected = 3200.0 / RATE;
        assert!((lead.lead_secs(0.0, RATE) - expected).abs() < 1e-9);
    }

    #[test]
    fn time_since_the_report_is_taken_away() {
        let mut lead = RingLead::default();
        lead.sent(4800);
        lead.report(4800.0, 4800.0, 1000.0);
        assert!((lead.lead_secs(1050.0, RATE) - 0.05).abs() < 1e-9);
        assert_eq!(lead.lead_secs(2000.0, RATE), 0.0);
    }

    #[test]
    fn a_reset_forgets_everything() {
        let mut lead = RingLead::default();
        lead.sent(4800);
        lead.report(100.0, 4800.0, 0.0);
        lead.reset();
        assert_eq!(lead.lead_secs(0.0, RATE), 0.0);
    }

    /// A worklet as the processor in `audio.rs` runs it: a ring the main
    /// thread appends to, drained by the speakers in `callback` frames at a
    /// time (coarse, the way CoreAudio hands Safari its render quanta), silent
    /// until it first holds `prime` frames and again after it runs dry, and
    /// reporting its fill every `report_every` callbacks.
    struct Model {
        fill: f64,
        received: f64,
        primed: bool,
        underruns: u32,
    }

    struct Run {
        frames_per_tick: Vec<u32>,
        underruns: u32,
        max_lead: f64,
    }

    /// Runs `secs` of the ring loop: a `display_hz` display, a console of
    /// `console_hz` (whose frames carry `RATE / console_hz` frames of sound),
    /// a ring drained `callback` frames at a time, reported every
    /// `report_every` callbacks with `latency_ms` of message delay. Returns
    /// what happened after the first second.
    fn run(
        display_hz: f64,
        console_hz: f64,
        callback: f64,
        report_every: u32,
        latency_ms: f64,
        secs: f64,
    ) -> Run {
        let frame_secs = 1.0 / console_hz;
        let samples_per_frame = RATE / console_hz;
        let mut pacer = FramePacer::new(60.0);
        let mut lead = RingLead::default();
        let mut ring = Model {
            fill: 0.0,
            received: 0.0,
            primed: false,
            underruns: 0,
        };
        let prime = RING_TARGET_SECS * RATE;
        let tick_ms = 1000.0 / display_hz;
        let callback_ms = callback / RATE * 1000.0;
        let mut next_callback_ms = callback_ms;
        let mut callbacks = 0u32;
        let mut pending_report: Option<(f64, f64, f64)> = None;
        let mut out = Run {
            frames_per_tick: Vec::new(),
            underruns: 0,
            max_lead: 0.0,
        };
        let ticks = (secs * display_hz) as u32;
        for tick in 0..ticks {
            let now = f64::from(tick) * tick_ms;
            // The speakers, up to now.
            while next_callback_ms <= now {
                if ring.primed {
                    if ring.fill >= callback {
                        ring.fill -= callback;
                    } else {
                        ring.fill = 0.0;
                        ring.primed = false;
                        if tick as f64 > display_hz {
                            ring.underruns += 1;
                        }
                    }
                } else if ring.fill >= prime {
                    ring.primed = true;
                }
                callbacks += 1;
                if callbacks.is_multiple_of(report_every) {
                    pending_report =
                        Some((ring.fill, ring.received, next_callback_ms + latency_ms));
                }
                next_callback_ms += callback_ms;
            }
            if let Some((fill, received, at)) = pending_report
                && at <= now
            {
                lead.report(fill, received, at);
                pending_report = None;
            }
            let display_due = pacer.frames_due(now);
            let due = nudge_frames(
                display_due,
                lead.lead_secs(now, RATE),
                RING_TARGET_SECS,
                frame_secs,
                4,
            );
            for _ in 0..due {
                lead.sent(samples_per_frame as usize);
                ring.fill += samples_per_frame;
                ring.received += samples_per_frame;
            }
            if f64::from(tick) >= display_hz {
                out.frames_per_tick.push(due);
                out.max_lead = out.max_lead.max(ring.fill / RATE);
            }
        }
        out.underruns = ring.underruns;
        out
    }

    fn off_beat(run: &Run) -> usize {
        run.frames_per_tick.iter().filter(|&&due| due != 1).count()
    }

    /// Safari's coarse render callbacks (1024 frames, 21 ms) do not reach the
    /// frame count: every tick runs one frame and the ring never runs dry.
    #[test]
    fn coarse_render_callbacks_still_run_one_frame_per_tick() {
        let run = run(60.0, 60.0, 1024.0, 2, 5.0, 61.0);
        assert_eq!(
            off_beat(&run),
            0,
            "frames per tick: {:?}",
            &run.frames_per_tick[..120]
        );
        assert_eq!(run.underruns, 0);
    }

    /// The same at the fine 128-frame quantum Chrome and headless WebKit use.
    #[test]
    fn fine_render_callbacks_run_one_frame_per_tick() {
        let run = run(60.0, 60.0, 128.0, 16, 2.0, 61.0);
        assert_eq!(off_beat(&run), 0);
        assert_eq!(run.underruns, 0);
    }

    /// The real NTSC console is 59.92 Hz against a 60 Hz display: a tenth of a
    /// percent too much sound. The nudge drops a frame now and then (about
    /// one every twelve seconds) and the ring stays bounded.
    #[test]
    fn console_drift_is_absorbed_by_rare_skips() {
        let run = run(60.0, 59.92, 1024.0, 2, 5.0, 61.0);
        assert!(
            off_beat(&run) <= 8,
            "{} off-beat ticks in 60 s",
            off_beat(&run)
        );
        assert_eq!(run.underruns, 0);
        assert!(
            run.max_lead < RING_TARGET_SECS + 4.0 * NTSC,
            "lead grew to {}",
            run.max_lead
        );
    }

    /// A 120 Hz display (ProMotion) runs a frame every other tick, steadily.
    #[test]
    fn a_120_hz_display_alternates_without_running_dry() {
        let run = run(120.0, 60.0, 1024.0, 2, 5.0, 61.0);
        assert!(run.frames_per_tick.iter().all(|&due| due <= 1));
        assert_eq!(run.underruns, 0);
    }
}
