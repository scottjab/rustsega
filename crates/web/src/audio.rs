//! Audio output through the Web Audio API (M7-05, #138).
//!
//! [`AudioOut`] owns one [`web_sys::AudioContext`], the [`web_sys::GainNode`] in
//! front of its destination, and the time the next block of sound is due at.
//! Every console frame appends whatever [`genesis::Genesis::drain_audio`] made to
//! a fresh [`web_sys::AudioBuffer`] and schedules it with
//! [`web_sys::AudioBufferSourceNode::start_with_when`], so the speakers are told
//! what is coming before they are asked to play it and the emulator never has to
//! guess how long a frame took. No AudioWorklet: that would need a JavaScript
//! file, and docs/ARCHITECTURE.md says the only JavaScript here is what
//! wasm-bindgen generates.
//!
//! The scheduling scheme, and why it is click-free:
//!
//! * One cursor, [`AudioOut::next_time`], is the end of everything already
//!   promised to the context. Each block starts exactly there and the cursor
//!   moves on by the block's own length, so blocks meet end to end with neither a
//!   gap - which is a click - nor an overlap - which is a doubling.
//! * A block that arrives too late to fit is not squeezed in after the play head,
//!   which would play samples from the wrong moment: the cursor jumps forward
//!   instead, which drops audio and keeps the clock honest. That is an
//!   *underrun*, counted in [`AudioOut::underruns`] and published to
//!   `window.rustsegaAudio` for anyone measuring the loop from devtools.
//! * The emulator runs enough frames to keep the cursor [`crate::audio_pacing`]
//!   frames ahead of the play head, so the audio clock - not
//!   `requestAnimationFrame`, not the wall clock - decides how fast the console
//!   goes.
//!
//! Everything here is browser code, so it has no native tests: the arithmetic it
//! depends on lives in [`crate::audio_pacing`], which does.

use js_sys::Reflect;
use leptos::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::AudioContext;
use web_sys::AudioContextOptions;
use web_sys::AudioContextState;
use web_sys::GainNode;

use crate::audio_pacing::deinterleave;

/// The sample rate asked of the browser, which is `Config::default`'s too.
///
/// It is a request, not a demand - the rest of this module reads the rate back
/// off the context, and [`crate::emulator`] hands it to `Genesis::new`, so the
/// mixer and the output cannot disagree.
const REQUESTED_SAMPLE_RATE: f32 = 48_000.0;

/// The console makes two channels; one `AudioBuffer` channel is one of them.
const CHANNELS: u32 = 2;

/// How close to the play head a block has to be to count as too late, seconds.
///
/// Ten ms: less than a block of console audio, and less than the shortest gap a
/// render loop can be expected to hit, so a block that arrives inside it has
/// either already been played or will be played badly.
const UNDERRUN_SECS: f64 = 0.01;

/// Where the cursor is put when the schedule has collapsed, and where a fresh
/// context starts: far enough that the next block is a scheduling deadline
/// rather than a race, short enough not to be a delay you can hear.
const RESTART_LEAD_SECS: f64 = 0.05;

/// The default volume of a new console, full.
pub const DEFAULT_VOLUME: f64 = 1.0;

/// The name of the debug mirror on `window`, published by [`AudioOut`].
const MIRROR: &str = "rustsegaAudio";

/// The volume and the mute, as one Leptos context (M7-05, #138).
///
/// Provided by `App`, written by the settings drawer and the top bar's mute
/// button, read by [`crate::emulator`] when it starts a console and again
/// whenever either changes. M7-10 (#143) is what persists them; until then this
/// is the one place they live, so the drawer and the top bar cannot disagree.
#[derive(Clone, Copy, Debug)]
pub struct AudioSettings {
    /// 0.0 to 1.0, as the drawer's slider has it.
    pub volume: RwSignal<f64>,
    /// Whether the output is muted, which keeps `volume` as it was.
    pub muted: RwSignal<bool>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            volume: RwSignal::new(DEFAULT_VOLUME),
            muted: RwSignal::new(false),
        }
    }
}

impl AudioSettings {
    /// The gain a console should put on its output for these settings.
    pub fn gain(&self) -> f32 {
        if self.muted.get() {
            0.0
        } else {
            self.volume.get() as f32
        }
    }
}

/// One browser audio output: the context, the volume in front of it, and the
/// cursor that says when the next block of console sound is due.
pub struct AudioOut {
    ctx: AudioContext,
    /// The one node every block is connected to, so volume and mute are a single
    /// write rather than a walk over the source nodes.
    gain: GainNode,
    /// The context time the next block starts at: the end of everything already
    /// scheduled, in the context's own clock.
    next_time: f64,
    /// The two channel buffers of the block being scheduled, reused every frame
    /// (see [`crate::audio_pacing::deinterleave`]).
    left: Vec<f32>,
    right: Vec<f32>,
    /// The volume [`Self::set_volume`] last asked for, which is what mute returns
    /// to when it is switched back off.
    volume: f32,
    muted: bool,
    /// Blocks that arrived too late to fit the schedule: one of these is a
    /// click-in-waiting, and it is the only honest number for "is this loop
    /// keeping up" from outside the tab.
    underruns: u32,
    /// Blocks handed to the context, for the same measurement.
    blocks: u32,
    /// The `window.rustsegaAudio` object this output keeps current. Web Audio
    /// gives a tab no other way to be observed from devtools, and "no drift" is
    /// an acceptance criterion that has to be measured to be believed.
    mirror: js_sys::Object,
}

impl AudioOut {
    /// A new output, or `None` if this browser would not give up an
    /// `AudioContext` or the `GainNode` in front of it.
    ///
    /// A context created without a user gesture starts `suspended` and stays
    /// there: [`Self::resume`] is what starts it, and it has to be called from
    /// inside a gesture for the browser to allow it. Creating it is still the
    /// right thing to do here because the *rate* has to be known before
    /// `Genesis::new`, and because a suspended context costs nothing.
    pub fn new() -> Option<Self> {
        let options = AudioContextOptions::new();
        options.set_sample_rate(REQUESTED_SAMPLE_RATE);
        let ctx = AudioContext::new_with_context_options(&options).ok()?;
        let gain = ctx.create_gain().ok()?;
        gain.connect_with_audio_node(&ctx.destination()).ok()?;

        let mut out = Self {
            ctx,
            gain,
            next_time: 0.0,
            left: Vec::new(),
            right: Vec::new(),
            volume: DEFAULT_VOLUME as f32,
            muted: false,
            underruns: 0,
            blocks: 0,
            mirror: js_sys::Object::new(),
        };
        out.next_time = out.ctx.current_time() + RESTART_LEAD_SECS;
        out.publish();
        Some(out)
    }

    /// The rate this context really plays at, for `Config::sample_rate`.
    ///
    /// The browser may have ignored [`REQUESTED_SAMPLE_RATE`], and the mixer has
    /// to be told the truth rather than the request: `drain_audio` produces
    /// `config.sample_rate` seconds-worth of audio per second, and every duration
    /// in this module is divided by `ctx.sample_rate()`.
    pub fn sample_rate(&self) -> u32 {
        self.ctx.sample_rate() as u32
    }

    /// Whether the play head is moving.
    ///
    /// `false` for a context that autoplay is holding back and for a paused one,
    /// which are exactly the two cases where the emulator has to pace itself on
    /// [`crate::frame_pacer`] instead and schedule nothing.
    pub fn is_running(&self) -> bool {
        self.ctx.state() == AudioContextState::Running
    }

    /// How far the schedule is ahead of the play head, seconds: what
    /// [`crate::audio_pacing::frames_to_run`] wants to know, and the number that
    /// says whether the two clocks have drifted apart.
    pub fn buffered_secs(&self) -> f64 {
        self.next_time - self.ctx.current_time()
    }

    /// Schedules one block of interleaved stereo `i16` - what
    /// [`genesis::Genesis::drain_audio`] appends - after everything already
    /// promised, and moves the cursor to the end of it.
    ///
    /// Returns `false` when the browser refused any part of it, which means this
    /// output cannot take more sound; the caller is expected to stop asking.
    pub fn push(&mut self, samples: &[i16]) -> bool {
        self.schedule(samples).is_some()
    }

    /// The `?`-shaped half of [`Self::push`]: stops at the first thing the
    /// browser refuses, which is what `false` means to the loop.
    fn schedule(&mut self, samples: &[i16]) -> Option<()> {
        deinterleave(samples, &mut self.left, &mut self.right);
        let frames = self.left.len() as u32;
        if frames == 0 {
            return Some(());
        }

        let rate = f64::from(self.ctx.sample_rate());
        let now = self.ctx.current_time();
        if self.next_time < now + UNDERRUN_SECS {
            self.underruns += 1;
            self.next_time = now + RESTART_LEAD_SECS;
        }
        let start = self.next_time;

        let buffer = self.ctx.create_buffer(CHANNELS, frames, rate as f32).ok()?;
        buffer.copy_to_channel(&self.left, 0).ok()?;
        buffer.copy_to_channel(&self.right, 1).ok()?;

        let source = self.ctx.create_buffer_source().ok()?;
        source.set_buffer(Some(&buffer));
        source.connect_with_audio_node(&self.gain).ok()?;
        source.start_with_when(start).ok()?;

        self.next_time = start + f64::from(frames) / rate;
        self.blocks += 1;
        self.publish();
        Some(())
    }

    /// Sets the volume of the whole output, `0.0` to `1.0`; values outside that
    /// range are the nearest end of it.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        self.apply_gain();
    }

    /// Mutes, or stops muting. The volume is left where it was, so unmuting
    /// returns to the level the slider shows rather than to full.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_gain();
    }

    fn apply_gain(&mut self) {
        self.gain
            .gain()
            .set_value(if self.muted { 0.0 } else { self.volume });
        self.publish();
    }

    /// Asks the browser to start the play head, and re-seats the cursor behind
    /// nothing.
    ///
    /// The cursor has to move because it was written against a clock that was
    /// standing still: `current_time()` did not advance while the context was
    /// suspended, so `next_time` says whatever it said when the pause happened,
    /// and every block scheduled against it would read as an underrun.
    ///
    /// Only a call made from inside a user gesture is allowed to start anything;
    /// `resume()` on a context the gesture does not own resolves and changes
    /// nothing.
    pub fn resume(&mut self) {
        let _ = self.ctx.resume();
        self.next_time = self.ctx.current_time() + RESTART_LEAD_SECS;
        self.publish();
    }

    /// Stops the play head, which is what a paused console means for sound: the
    /// blocks already scheduled stop where they are rather than playing out.
    pub fn suspend(&mut self) {
        let _ = self.ctx.suspend();
        self.publish();
    }

    /// Refreshes `window.rustsegaAudio`, which is the only way anything outside
    /// this tab can see the schedule.
    ///
    /// Cheap enough to do on every change - a dozen property writes, against a
    /// frame that is hundreds of samples and a canvas draw - and it is what makes
    /// "no drift" and "no underruns" measurable rather than plausible.
    fn publish(&self) {
        let set = |key: &str, value: JsValue| {
            let _ = Reflect::set(&self.mirror, &js_sys::JsString::from(key), &value);
        };
        set("state", JsValue::from_str(self.state_name()));
        set("currentTime", JsValue::from_f64(self.ctx.current_time()));
        set("nextTime", JsValue::from_f64(self.next_time));
        set("ahead", JsValue::from_f64(self.buffered_secs()));
        set("underruns", JsValue::from_f64(f64::from(self.underruns)));
        set("blocks", JsValue::from_f64(f64::from(self.blocks)));
        set(
            "sampleRate",
            JsValue::from_f64(f64::from(self.ctx.sample_rate())),
        );
        set(
            "gain",
            JsValue::from_f64(f64::from(self.gain.gain().value())),
        );
        set("volume", JsValue::from_f64(f64::from(self.volume)));
        set("muted", JsValue::from(self.muted));
        if let Some(window) = web_sys::window() {
            let _ = Reflect::set(&window, &js_sys::JsString::from(MIRROR), &self.mirror);
        }
    }

    fn state_name(&self) -> &'static str {
        match self.ctx.state() {
            AudioContextState::Running => "running",
            AudioContextState::Suspended => "suspended",
            // `AudioContextState` is a JS string enum, so it is non-exhaustive:
            // the rest is `Closed` and whatever a later browser adds, neither of
            // which is a context that is playing.
            _ => "closed",
        }
    }
}

impl Drop for AudioOut {
    /// Gives the context back to the browser.
    ///
    /// A browser allows only a handful of `AudioContext`s per tab, and dropping a
    /// Rust handle does not close the JS object: six cartridges in one tab would
    /// be six live contexts and then silence. Dropping this output *is* the end of
    /// it - a second cartridge, a start that failed after the context was made -
    /// so the context goes here rather than at one call site that could be
    /// forgotten.
    fn drop(&mut self) {
        let _ = self.ctx.close();
        self.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mute is a held value, not a zeroed one: unmuting returns to wherever the
    /// slider was rather than to full, which is what a person who muted mid-game
    /// expects the slider to still say.
    #[test]
    fn mute_holds_the_volume_rather_than_replacing_it() {
        let owner = Owner::new();
        owner.set();
        let settings = AudioSettings::default();
        assert_eq!(settings.gain(), DEFAULT_VOLUME as f32);

        settings.volume.set(0.4);
        assert_eq!(settings.gain(), 0.4);

        settings.muted.set(true);
        assert_eq!(settings.gain(), 0.0);
        assert_eq!(settings.volume.get(), 0.4, "the slider still says 0.4");

        settings.muted.set(false);
        assert_eq!(settings.gain(), 0.4);
    }

    /// A new tab is not muted and is not quiet: the first thing a person hears
    /// after the enabling click should be the game at the volume the slider shows.
    #[test]
    fn a_new_console_is_unmuted_and_at_full_volume() {
        let owner = Owner::new();
        owner.set();
        let settings = AudioSettings::default();
        assert!(!settings.muted.get());
        assert_eq!(settings.volume.get(), DEFAULT_VOLUME);
    }
}
