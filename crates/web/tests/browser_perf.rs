//! The in-browser frame budget, measured (M7-21, #154).
//!
//! [`genesis::Genesis::run_frame`] is the call the whole frontend is built
//! around, and this asks the question a player asks of it: does a frame of real
//! software fit in the time a frame is given? The budget is an average of 8.0 ms
//! over 600 frames and a 99th percentile of 14.0 ms - a frame and a half at
//! 60 Hz for the average, two frames for the tail, so a machine that is
//! comfortably over budget on the whole fails, and so does one that hits a
//! regularly-repeated spike.
//!
//! # Running it
//!
//! ```text
//! nix develop -c just perf-web
//! ```
//!
//! That is the only supported way in, and the test is deliberately outside
//! `just check` and outside CI. Two reasons, both structural:
//!
//! * **A browser cannot read `$RUSTSEGA_TEST_ROM_DIR`.** The cartridge has to
//!   travel inside the test binary, so it is `include_bytes!`ed from the path
//!   that `just perf-web` puts in `RUSTSEGA_PERF_ROM` - and `include_bytes!` is
//!   compile-time, so an unset variable is a build failure, not a skipped test.
//!   That is why all of this file sits behind the `perf-rom` cargo feature, off
//!   by default: a runtime `SKIPPED` print would still have had to compile this
//!   line first, and neither `just check` nor the `just test-web` that CI runs
//!   sets it.
//! * **A debug wasm build is not a thing worth measuring.** The recipe builds
//!   `--release`; see the `just web-serve` comment for how far apart the two are.
//!
//! # What it runs
//!
//! Overdrive 2 (TiTAN, 2017), the one entry of the pinned public set that is a
//! *load* rather than a *judge*: a 68000 demo that keeps the VDP busy all frame -
//! mid-line register writes, the write FIFO, DMA during the raster, H32/H40
//! switching, sprite limits - and drives both sound chips at once. The other
//! candidates in `docs/test-roms.md` are the `md-test-pack` ROMs, 1 KB to 128 KB
//! of pattern-and-register tests that draw a picture and then sit there, silent.
//! They would measure the frame loop of an idle machine. The cost of the choice
//! is 8 MB baked into the test binary, measured and recorded in
//! `docs/TESTING.md` alongside the numbers it earns.
//!
//! Audio is deliberately not drained from the console: [`genesis`] mixes into
//! its own per-frame buffers during `run_frame` - which is the cost being
//! measured - and holds nothing else until the caller drains it, so a harness
//! that never drains is not growing memory behind the timer.

#![cfg(all(target_arch = "wasm32", feature = "perf-rom"))]

use wasm_bindgen_test::wasm_bindgen_test;
use wasm_bindgen_test::wasm_bindgen_test_configure;
use wasm_bindgen_test::{console_error, console_log};

wasm_bindgen_test_configure!(run_in_browser);

/// The cartridge under measurement: Overdrive 2, as `just perf-web` found it.
///
/// `include_bytes!` of an [`env!`] path is a compile-time read - the reason this
/// file is gated on the `perf-rom` feature rather than skipping at run time.
static ROM: &[u8] = include_bytes!(env!("RUSTSEGA_PERF_ROM"));

/// Frames run before any of them is timed: the point is to measure the emulator
/// warm - palette, sprite cache, name tables and the YM2612 note state all
/// written to - not to measure the first frame of a cold cartridge.
const WARM_UP_FRAMES: usize = 120;

/// Frames timed, and so the population the percentile is taken over. Two
/// hundredths of it, the tail the budget judges, is twelve frames.
const TIMED_FRAMES: usize = 600;

/// The average a frame may cost, in milliseconds.
const BUDGET_AVG_MS: f64 = 8.0;

/// The 99th percentile a frame may cost, in milliseconds.
const BUDGET_P99_MS: f64 = 14.0;

/// `performance.now()`, in milliseconds: the browser's own sub-microsecond
/// clock, and the same ruler `requestAnimationFrame` timestamps the real loop
/// against (see [`web::storage::SramWatcher`], which clocks itself the same way).
fn now_ms() -> f64 {
    web_sys::window()
        .expect("a browser window")
        .performance()
        .expect("the Page Visibility performance timeline")
        .now()
}

/// 120 warm-up frames, then 600 timed ones, and the numbers printed either way.
#[wasm_bindgen_test]
fn run_frame_fits_the_budget() {
    let mut console = genesis::Genesis::new(ROM, genesis::Config::default())
        .expect("the perf cartridge boots in a browser");

    for _ in 0..WARM_UP_FRAMES {
        console.run_frame();
    }

    let mut samples: Vec<f64> = Vec::with_capacity(TIMED_FRAMES);
    for _ in 0..TIMED_FRAMES {
        let start = now_ms();
        console.run_frame();
        samples.push(now_ms() - start);
    }

    let avg = samples.iter().sum::<f64>() / samples.len() as f64;
    // `percentile` sorts in place, so the back of the slice is the slowest frame
    // of the run by the time it comes back.
    let p99 = web::perf::percentile(&mut samples, 0.99);
    let max = samples[samples.len() - 1];

    let line = format!(
        "run_frame: {} frames of {} B ROM - avg {avg:.3} ms, p99 {p99:.3} ms, max {max:.3} ms (budget avg {BUDGET_AVG_MS} ms, p99 {BUDGET_P99_MS} ms)",
        samples.len(),
        ROM.len(),
    );
    if avg > BUDGET_AVG_MS || p99 > BUDGET_P99_MS {
        console_error!("{line}");
    } else {
        console_log!("{line}");
    }

    assert!(
        avg <= BUDGET_AVG_MS,
        "{avg:.3} ms average over {} frames is over the {BUDGET_AVG_MS} ms budget",
        samples.len(),
    );
    assert!(
        p99 <= BUDGET_P99_MS,
        "{p99:.3} ms is the 99th percentile of {} frames, over the {BUDGET_P99_MS} ms budget",
        samples.len(),
    );
}
