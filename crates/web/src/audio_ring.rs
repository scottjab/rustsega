//! The AudioWorklet ring output (#629): the console's sound posted into a ring
//! that a processor on the audio thread drains, so no block boundary is ever
//! scheduled against `AudioContext.currentTime`.
//!
//! Why it exists: on real Safari hardware (macOS and iOS) the scheduled
//! `AudioBufferSourceNode` output stuttered in both sound and picture, while
//! headless WebKit measured clean. The scheduled output asks the audio clock
//! every animation frame how many console frames to run, and Safari's clock and
//! render callbacks move in coarse steps on a device; the ring takes the clock
//! out of the frame decision entirely ([`crate::ring_pacing`]) and plays the
//! sound as one continuous stream, the AudioWorklet + ring-buffer pattern the
//! Web Audio spec authors document for wasm audio ("Audio Worklet design
//! pattern", developer.chrome.com, 2020).
//!
//! The processor is the one piece of hand-written JavaScript in the app. It is
//! `inline_js`, so `wasm-bindgen` emits it as one of its own snippets next to
//! the glue it already generates, served from the app's origin (the CSP's
//! `script-src 'self'` covers it); `import.meta.url` is how the worklet is told
//! where to load the same module from. On the main thread the module only
//! defines that function: `AudioWorkletProcessor` and `registerProcessor`
//! exist in the worklet's scope alone.

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Array;
use js_sys::Float32Array;
use js_sys::Object;
use js_sys::Reflect;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen_futures::JsFuture;
use web_sys::AudioContext;
use web_sys::AudioWorkletNode;
use web_sys::AudioWorkletNodeOptions;
use web_sys::GainNode;
use web_sys::MessageEvent;
use web_sys::MessagePort;

use crate::ring_pacing::RING_TARGET_SECS;
use crate::ring_pacing::RingLead;

/// The name the processor registers under.
const PROCESSOR: &str = "rustsega-ring";

/// How much sound the ring can hold, seconds: well past any lead the pacer
/// aims for, so a full ring means a stall the oldest frames are dropped for.
const CAPACITY_SECS: f64 = 0.5;

/// How many 128-frame render quanta pass between two fill reports: 1024
/// frames, 21 ms at 48 kHz, about fifty messages a second.
const REPORT_EVERY_QUANTA: u32 = 8;

#[wasm_bindgen(inline_js = r#"
if (typeof AudioWorkletProcessor !== 'undefined' && typeof registerProcessor !== 'undefined') {
  class RustsegaRing extends AudioWorkletProcessor {
    constructor(options) {
      super();
      const o = (options && options.processorOptions) || {};
      this.cap = Math.max(1, o.capacity | 0);
      this.prime = o.prime | 0;
      this.every = Math.max(1, o.reportEvery | 0);
      this.l = new Float32Array(this.cap);
      this.r = new Float32Array(this.cap);
      this.clear();
      this.underruns = 0;
      this.quanta = 0;
      this.port.onmessage = (e) => {
        const d = e.data;
        if (d.reset) { this.clear(); this.report(); return; }
        this.write(d.l, d.r);
      };
    }
    clear() { this.read = 0; this.fill = 0; this.received = 0; this.primed = false; }
    write(l, r) {
      const n = l.length;
      this.received += n;
      for (let i = 0; i < n; i++) {
        if (this.fill === this.cap) { this.read = (this.read + 1) % this.cap; this.fill--; }
        const w = (this.read + this.fill) % this.cap;
        this.l[w] = l[i];
        this.r[w] = r[i];
        this.fill++;
      }
    }
    report() {
      this.port.postMessage({ fill: this.fill, received: this.received, underruns: this.underruns });
    }
    process(inputs, outputs) {
      const out = outputs[0];
      const ol = out[0];
      const or = out[1] || out[0];
      const n = ol.length;
      if (!this.primed && this.fill >= this.prime) this.primed = true;
      if (this.primed) {
        const take = Math.min(n, this.fill);
        for (let i = 0; i < take; i++) {
          ol[i] = this.l[this.read];
          or[i] = this.r[this.read];
          this.read = (this.read + 1) % this.cap;
        }
        this.fill -= take;
        if (take < n) {
          for (let i = take; i < n; i++) { ol[i] = 0; or[i] = 0; }
          this.primed = false;
          this.underruns++;
        }
      }
      if (++this.quanta % this.every === 0) this.report();
      return true;
    }
  }
  registerProcessor('rustsega-ring', RustsegaRing);
}
export function rustsega_ring_url() { return import.meta.url; }
"#)]
extern "C" {
    /// The URL of the module above, which is what `audioWorklet.addModule`
    /// loads into the worklet's scope.
    fn rustsega_ring_url() -> String;
}

/// A running ring: the node on the graph, its port, and what the main thread
/// knows about how full it is.
pub struct RingSink {
    node: AudioWorkletNode,
    port: MessagePort,
    lead: Rc<RefCell<RingLead>>,
    underruns: Rc<Cell<u32>>,
    /// Kept alive for as long as the port can call it.
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl RingSink {
    /// Loads the processor into `ctx` and puts a ring in front of `gain`.
    /// `None` when this browser has no AudioWorklet or refused any step, which
    /// leaves the caller on the scheduled output.
    pub async fn start(ctx: AudioContext, gain: GainNode) -> Option<Self> {
        let worklet = ctx.audio_worklet().ok()?;
        JsFuture::from(worklet.add_module(&rustsega_ring_url()).ok()?)
            .await
            .ok()?;

        let rate = f64::from(ctx.sample_rate());
        let processor = Object::new();
        let set = |key: &str, value: f64| {
            let _ = Reflect::set(
                &processor,
                &JsValue::from_str(key),
                &JsValue::from_f64(value),
            );
        };
        set("capacity", (CAPACITY_SECS * rate).round());
        set("prime", (RING_TARGET_SECS * rate).round());
        set("reportEvery", f64::from(REPORT_EVERY_QUANTA));
        let options = AudioWorkletNodeOptions::new();
        options.set_number_of_inputs(0);
        options.set_number_of_outputs(1);
        options.set_output_channel_count(&Array::of1(&JsValue::from_f64(2.0)));
        options.set_processor_options(Some(&processor));
        let node = AudioWorkletNode::new_with_options(&ctx, PROCESSOR, &options).ok()?;
        node.connect_with_audio_node(&gain).ok()?;
        let port = node.port().ok()?;

        let lead = Rc::new(RefCell::new(RingLead::default()));
        let underruns = Rc::new(Cell::new(0));
        let on_message = {
            let lead = Rc::clone(&lead);
            let underruns = Rc::clone(&underruns);
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let data = event.data();
                let read = |key: &str| {
                    Reflect::get(&data, &JsValue::from_str(key))
                        .ok()
                        .and_then(|value| value.as_f64())
                };
                if let (Some(fill), Some(received)) = (read("fill"), read("received")) {
                    lead.borrow_mut()
                        .report(fill, received, crate::emulator::now_ms());
                }
                if let Some(count) = read("underruns") {
                    underruns.set(count as u32);
                }
            })
        };
        port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        Some(Self {
            node,
            port,
            lead,
            underruns,
            _on_message: on_message,
        })
    }

    /// Posts one console frame of sound, already split into channels. The
    /// arrays' buffers are transferred, not copied. `false` when the port
    /// refused it.
    pub fn push(&self, left: &[f32], right: &[f32]) -> bool {
        let l = Float32Array::from(left);
        let r = Float32Array::from(right);
        let message = Object::new();
        let _ = Reflect::set(&message, &JsValue::from_str("l"), &l);
        let _ = Reflect::set(&message, &JsValue::from_str("r"), &r);
        let transfer = Array::of2(&l.buffer(), &r.buffer());
        if self
            .port
            .post_message_with_transferable(&message, &transfer)
            .is_err()
        {
            return false;
        }
        self.lead.borrow_mut().sent(left.len());
        true
    }

    /// Empties the ring: the sound in it belongs to a moment the console has
    /// left (a load, a reset, a resume after a pause).
    pub fn reset(&self) {
        let message = Object::new();
        let _ = Reflect::set(&message, &JsValue::from_str("reset"), &JsValue::TRUE);
        let _ = self.port.post_message(&message);
        self.lead.borrow_mut().reset();
    }

    /// The main thread's estimate of the ring's lead at `now_ms`, seconds.
    pub fn lead_secs(&self, now_ms: f64, rate: f64) -> f64 {
        self.lead.borrow().lead_secs(now_ms, rate)
    }

    /// How many times the processor has run dry, as of its last report.
    pub fn underruns(&self) -> u32 {
        self.underruns.get()
    }
}

impl Drop for RingSink {
    fn drop(&mut self) {
        self.port.set_onmessage(None);
        let _ = self.node.disconnect();
    }
}
