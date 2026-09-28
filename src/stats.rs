//! The numbers a report is allowed to print.
//!
//! Every latency in a hivebox report is a distribution, and it is summarized the same way
//! everywhere: the count, the median, p90, p99, the maximum and the interquartile range. Never a
//! mean on its own, because a create path with a slow tail and a fast one with the same mean are
//! different systems, and never a minimum, because a minimum is what the machine can do once and
//! not what it does.
//!
//! Percentiles use the nearest rank method. It always returns a value that was actually observed,
//! which matters when somebody goes looking for the request behind the p99.

use std::fmt;
use std::time::Duration;

/// A summary of one set of latency samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// How many samples there were.
    pub count: usize,
    /// The 50th percentile.
    pub p50: Duration,
    /// The 90th percentile.
    pub p90: Duration,
    /// The 99th percentile.
    pub p99: Duration,
    /// The largest sample.
    pub max: Duration,
    /// The 75th percentile minus the 25th.
    pub iqr: Duration,
}

impl Summary {
    /// Summarizes the samples, or returns `None` when there are none. The input does not need to
    /// be sorted.
    #[must_use]
    pub fn of(samples: &[Duration]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let at = |p| percentile(&sorted, p);
        Some(Self {
            count: sorted.len(),
            p50: at(50.0),
            p90: at(90.0),
            p99: at(99.0),
            max: sorted[sorted.len() - 1],
            iqr: at(75.0) - at(25.0),
        })
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} p50={:?} p90={:?} p99={:?} max={:?} iqr={:?}",
            self.count, self.p50, self.p90, self.p99, self.max, self.iqr
        )
    }
}

/// The nearest rank percentile of an already sorted, non empty slice.
///
/// # Panics
///
/// If `sorted` is empty or `p` is outside 0 to 100.
#[must_use]
pub fn percentile(sorted: &[Duration], p: f64) -> Duration {
    assert!(!sorted.is_empty(), "a percentile of nothing");
    assert!((0.0..=100.0).contains(&p), "a percentile outside 0 to 100: {p}");
    // The rank is ceil(p/100 * n), counted from one, and p = 0 means the smallest sample.
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    sorted[rank.clamp(1, n) - 1]
}

/// The share of attempts that failed for a reason that belongs to the platform rather than to the
/// workload. The spec's target is 0.1% or less, and a trainer masks exactly these out of the
/// reward, so the classification is what the number means.
#[must_use]
pub fn infra_error_ratio(infra_errors: u64, attempts: u64) -> Option<f64> {
    (attempts > 0).then(|| infra_errors as f64 / attempts as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|&v| Duration::from_millis(v)).collect()
    }

    #[test]
    fn nearest_rank_on_one_to_a_hundred() {
        let samples = ms(&(1..=100).collect::<Vec<_>>());
        let s = Summary::of(&samples).unwrap();
        assert_eq!(s.count, 100);
        assert_eq!(s.p50, Duration::from_millis(50));
        assert_eq!(s.p90, Duration::from_millis(90));
        assert_eq!(s.p99, Duration::from_millis(99));
        assert_eq!(s.max, Duration::from_millis(100));
        assert_eq!(s.iqr, Duration::from_millis(50));
    }

    #[test]
    fn order_of_input_does_not_matter() {
        let a = Summary::of(&ms(&[5, 1, 4, 2, 3])).unwrap();
        let b = Summary::of(&ms(&[1, 2, 3, 4, 5])).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.p50, Duration::from_millis(3));
    }

    #[test]
    fn every_percentile_is_an_observed_sample() {
        let samples = ms(&[7, 300, 12, 12, 45, 9000]);
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        for p in [0.0, 1.0, 33.3, 50.0, 90.0, 99.0, 99.9, 100.0] {
            assert!(sorted.contains(&percentile(&sorted, p)), "p{p}");
        }
    }

    #[test]
    fn one_sample_is_every_percentile() {
        let s = Summary::of(&ms(&[42])).unwrap();
        assert_eq!((s.p50, s.p99, s.max, s.iqr), (s.max, s.max, s.max, Duration::ZERO));
    }

    #[test]
    fn nothing_summarizes_to_nothing() {
        assert_eq!(Summary::of(&[]), None);
        assert_eq!(infra_error_ratio(0, 0), None);
        assert_eq!(infra_error_ratio(1, 1000), Some(0.001));
    }
}
