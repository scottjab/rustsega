//! Arithmetic over frame-time samples (M7-21, #154).
//!
//! `crates/web/tests/browser_perf.rs` runs the console in a real browser, times
//! every frame, and has to turn those samples into the two numbers the budget
//! names: an average and a 99th percentile. The average is one line of `iter`,
//! the percentile is not, and a browser is the worst place in the world to debug
//! arithmetic. So the percentile lives here, pure, and is tested on the host -
//! the same arrangement as [`crate::fps`], which is the other clock of the loop.

/// The `p` quantile of `samples`, by the nearest-rank method.
///
/// `p` is a fraction of the distribution, not a percentage: `0.99` is the 99th
/// percentile, `0.5` the median. The samples are in whatever unit the caller
/// measured in - milliseconds for the frame budget - and the answer is in that
/// unit too.
///
/// Nearest rank means the returned value is always one of the samples, never an
/// interpolated one: the sample is ranked by value, and the answer is the value
/// at rank `ceil(p * n)` counting from 1. So `p = 1.0` is the maximum, `p = 0.0`
/// and everything below the first rank is the minimum, and the 99th percentile of
/// 600 frame times is the 594th fastest of them - which is what "one frame in a
/// hundred is allowed to be this slow, and no more" has to mean for a budget.
///
/// `samples` is sorted in place, because six hundred `f64`s is not worth a second
/// allocation in a harness whose whole job is to measure the frame loop.
///
/// An empty sample set has no percentile and answers [`f64::NAN`]. No comparison
/// with NaN holds, so a caller that collected nothing fails its budget loudly
/// instead of passing one it never measured.
pub fn percentile(samples: &mut [f64], p: f64) -> f64 {
    assert!(
        (0.0..=1.0).contains(&p),
        "percentile p must be within 0.0..=1.0, got {p}"
    );
    if samples.is_empty() {
        return f64::NAN;
    }
    samples.sort_by(f64::total_cmp);
    // Nearest rank, 1-based: `ceil` so a hundredth of six hundred samples is the
    // 594th and not the 593rd, `saturating_sub` so `p = 0.0` - rank 0, no sample
    // at all - falls to the minimum rather than off the front of the slice.
    let rank = (p * samples.len() as f64).ceil() as usize;
    samples[rank.saturating_sub(1)]
}

#[cfg(test)]
mod tests {
    use super::percentile;

    /// `count` samples: 1.0, 2.0, ... through `count`.
    fn samples_1_to(count: usize) -> Vec<f64> {
        (1..=count).map(|i| i as f64).collect()
    }

    /// The middle of an odd set is the median, and that is the whole of what a
    /// median means at this size.
    #[test]
    fn the_median_of_an_odd_set_is_its_middle_value() {
        let mut samples = vec![3.0, 1.0, 2.0];
        assert_eq!(percentile(&mut samples, 0.5), 2.0);
    }

    /// The two ends of the distribution are the minimum and the maximum: `p = 0.0`
    /// must not index off the front, and `p = 1.0` must not index off the back.
    #[test]
    fn the_endpoints_of_a_distribution_are_its_min_and_max() {
        let mut samples = vec![5.0, 1.0, 4.0, 2.0, 3.0];
        assert_eq!(percentile(&mut samples, 0.0), 1.0);

        let mut samples = vec![5.0, 1.0, 4.0, 2.0, 3.0];
        assert_eq!(percentile(&mut samples, 1.0), 5.0);
    }

    /// The gate this whole module exists for, at the size the browser harness
    /// uses: of 600 samples the 99th percentile is the 594th ranked, i.e. the
    /// seventh slowest frame of 600. `0.99 * 600` is not exactly 594 in binary
    /// floating point, so this test is also the reason the rank is `ceil`ed rather
    /// than rounded or truncated.
    #[test]
    fn the_99th_percentile_of_six_hundred_samples_is_the_594th_ranked() {
        let mut samples = samples_1_to(600);
        assert_eq!(percentile(&mut samples, 0.99), 594.0);
    }

    /// One sample is a percentile: a harness that timed a single frame should get
    /// that frame back, not a panic and not NaN.
    #[test]
    fn one_sample_is_its_own_percentile_at_every_p() {
        for p in [0.0, 0.5, 0.99, 1.0] {
            let mut samples = vec![7.5];
            assert_eq!(percentile(&mut samples, p), 7.5, "p = {p}");
        }
    }

    /// A percentile of a set of one value is that value, however many times it is
    /// repeated - the case a steady 60 fps console produces when the clock behind
    /// it only ticks in 0.1 ms steps.
    #[test]
    fn a_repeated_value_is_its_own_percentile() {
        let mut samples = vec![16.7; 100];
        assert_eq!(percentile(&mut samples, 0.99), 16.7);
    }

    /// The caller's order is not part of the answer: rank is by value, and this
    /// sorts the caller's slice in place to get there.
    #[test]
    fn an_unsorted_caller_still_gets_the_ranked_answer() {
        let mut samples = vec![9.0, 2.0, 7.0, 4.0, 6.0, 1.0, 8.0, 3.0, 5.0];
        assert_eq!(percentile(&mut samples, 1.0), 9.0);
        assert_eq!(samples, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    /// Nothing was measured, so nothing can be reported - and the reported value
    /// has to be one that a budget comparison rejects, so an empty harness cannot
    /// pass by having done nothing.
    #[test]
    fn an_empty_sample_set_has_no_percentile() {
        let mut samples: Vec<f64> = Vec::new();
        assert!(percentile(&mut samples, 0.99).is_nan());
    }

    /// A `p` outside a distribution is a caller bug, not a clamp: `1.5` is not the
    /// maximum and `-0.1` is not the minimum, and either one silently clamped would
    /// be a budget measured against nothing.
    #[test]
    #[should_panic(expected = "within 0.0..=1.0")]
    fn a_percentile_above_one_panics() {
        let mut samples = vec![1.0, 2.0];
        percentile(&mut samples, 1.5);
    }

    #[test]
    #[should_panic(expected = "within 0.0..=1.0")]
    fn a_percentile_below_zero_panics() {
        let mut samples = vec![1.0, 2.0];
        percentile(&mut samples, -0.1);
    }
}
