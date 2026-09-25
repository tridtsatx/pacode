//! Fixed-bucket latency histograms, cheap enough to update on every request.
//!
//! The daemon fans every store and provider call through a bounded queue and a
//! single writer, so the questions worth answering in the field are "how long
//! did a caller wait", "how long did the work take" and "how deep did the queue
//! get". Those are per-operation costs, so they are measured where the operation
//! happens rather than sampled by a background timer: a poller would either miss
//! the spikes or keep waking an idle daemon, and idle wakeups are exactly what
//! this codebase refuses to pay for.
//!
//! Buckets are log2 on microseconds, so bucket `i` counts durations in
//! `[2^i, 2^(i+1))`. That keeps the resolution coarse enough to be honest about
//! what an atomics counter can measure and fine enough to tell an fsync-sized
//! commit (~10³µs) from a queue wait behind 128 streams (~10⁵µs). The last
//! bucket is the overflow, so nothing is silently dropped.

use std::sync::atomic::{AtomicU64, Ordering};

/// 24 buckets: 1µs up to ~8.4s, then one overflow bucket.
pub const HISTOGRAM_BUCKETS: usize = 24;

/// Microsecond-resolution log2 histogram. One atomic add per observation, no
/// allocation on the hot path.
#[derive(Debug)]
pub struct LatencyHist {
    buckets: [AtomicU64; HISTOGRAM_BUCKETS],
    count: AtomicU64,
    total_micros: AtomicU64,
    max_micros: AtomicU64,
}

impl Default for LatencyHist {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyHist {
    pub const fn new() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; HISTOGRAM_BUCKETS],
            count: AtomicU64::new(0),
            total_micros: AtomicU64::new(0),
            max_micros: AtomicU64::new(0),
        }
    }

    /// Bucket index for a duration in microseconds.
    pub fn bucket_of(micros: u64) -> usize {
        match 63 - micros.max(1).leading_zeros() as usize {
            index if index < HISTOGRAM_BUCKETS => index,
            _ => HISTOGRAM_BUCKETS - 1,
        }
    }

    /// Record one observation. `micros` is clamped into the overflow bucket
    /// rather than wrapping.
    pub fn record(&self, micros: u64) {
        self.buckets[Self::bucket_of(micros)].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_micros.fetch_add(micros, Ordering::Relaxed);
        // Relaxed is enough for a high-water mark: a lost race costs at most one
        // slightly smaller maximum, and it must never contend with the request
        // path.
        self.max_micros.fetch_max(micros, Ordering::Relaxed);
    }

    /// Record a `Duration`. The conversion is the only lossy part, at the
    /// microsecond scale, which is far below anything the buckets resolve.
    pub fn record_duration(&self, elapsed: std::time::Duration) {
        self.record(elapsed.as_micros().min(u64::MAX as u128) as u64);
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            buckets: std::array::from_fn(|i| self.buckets[i].load(Ordering::Relaxed)),
            count: self.count.load(Ordering::Relaxed),
            total_micros: self.total_micros.load(Ordering::Relaxed),
            max_micros: self.max_micros.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time reading of a [`LatencyHist`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistogramSnapshot {
    /// Log2 buckets: bucket `i` counts `[2^i, 2^(i+1))` microseconds. The last
    /// entry is the overflow bucket.
    pub buckets: [u64; HISTOGRAM_BUCKETS],
    pub count: u64,
    pub total_micros: u64,
    pub max_micros: u64,
}

impl HistogramSnapshot {
    /// Mean in microseconds, or 0 with no observations.
    pub fn mean_micros(&self) -> u64 {
        self.total_micros.checked_div(self.count).unwrap_or(0)
    }

    /// Upper bound of the bucket holding the `quantile` observation, in
    /// microseconds. Quantile is a fraction in `0.0..=1.0`. Buckets are log2, so
    /// this reports the top of a bucket, not an interpolated value — reading it
    /// as "somewhere below this" is the honest interpretation.
    pub fn quantile_micros(&self, quantile: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((self.count as f64) * quantile.clamp(0.0, 1.0)).ceil() as u64;
        let target = target.max(1);
        let mut seen = 0u64;
        for (index, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= target {
                return bucket_upper_micros(index);
            }
        }
        self.max_micros
    }

    /// A one-line summary for a log or a CLI: count, mean, p50, p99, max.
    pub fn summary(&self) -> String {
        format!(
            "n={} mean={}us p50={}us p99={}us max={}us",
            self.count,
            self.mean_micros(),
            self.quantile_micros(0.50),
            self.quantile_micros(0.99),
            self.max_micros,
        )
    }
}

/// Exclusive upper bound of bucket `index`, in microseconds. The overflow
/// bucket reports its own observed maximum rather than a synthetic bound.
pub fn bucket_upper_micros(index: usize) -> u64 {
    if index + 1 >= HISTOGRAM_BUCKETS {
        return u64::MAX;
    }
    1u64 << (index as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_log2_microseconds() {
        assert_eq!(LatencyHist::bucket_of(1), 0);
        assert_eq!(LatencyHist::bucket_of(2), 1);
        assert_eq!(LatencyHist::bucket_of(3), 1);
        assert_eq!(LatencyHist::bucket_of(4), 2);
        assert_eq!(LatencyHist::bucket_of(1_000), 9);
        assert_eq!(
            LatencyHist::bucket_of(0),
            0,
            "zero clamps into the first bucket"
        );
    }

    #[test]
    fn a_huge_observation_lands_in_the_overflow_bucket() {
        assert_eq!(
            LatencyHist::bucket_of(u64::MAX),
            HISTOGRAM_BUCKETS - 1,
            "an absurd duration must not index out of bounds"
        );
        let hist = LatencyHist::new();
        hist.record(u64::MAX);
        assert_eq!(hist.snapshot().buckets[HISTOGRAM_BUCKETS - 1], 1);
    }

    #[test]
    fn count_mean_and_max_track_the_observations() {
        let hist = LatencyHist::new();
        hist.record(10);
        hist.record(20);
        hist.record(30);
        let snap = hist.snapshot();
        assert_eq!(snap.count, 3);
        assert_eq!(snap.total_micros, 60);
        assert_eq!(snap.mean_micros(), 20);
        assert_eq!(snap.max_micros, 30);
    }

    #[test]
    fn quantiles_walk_the_buckets_in_order() {
        let hist = LatencyHist::new();
        for _ in 0..90 {
            hist.record(1); // bucket 0
        }
        for _ in 0..10 {
            hist.record(1_000_000); // bucket 19
        }
        let snap = hist.snapshot();
        assert_eq!(snap.quantile_micros(0.5), bucket_upper_micros(0));
        assert_eq!(snap.quantile_micros(0.95), bucket_upper_micros(19));
        assert_eq!(snap.quantile_micros(1.0), bucket_upper_micros(19));
    }

    #[test]
    fn an_empty_histogram_reports_zero_rather_than_garbage() {
        let snap = LatencyHist::new().snapshot();
        assert_eq!(snap.count, 0);
        assert_eq!(snap.mean_micros(), 0);
        assert_eq!(snap.quantile_micros(0.99), 0);
    }

    #[test]
    fn durations_convert_to_microseconds() {
        let hist = LatencyHist::new();
        hist.record_duration(std::time::Duration::from_micros(1_500));
        assert_eq!(hist.snapshot().count, 1);
        assert_eq!(hist.snapshot().total_micros, 1_500);
    }
}

/// The single store writer behind every session: how much work it has done, how
/// deep its queue got, and how long callers waited versus how long the work
/// took. `exec` is the number that settles "is the database the bottleneck" —
/// for a write it is dominated by the fsync that `synchronous=FULL` asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StoreStats {
    pub enqueued: u64,
    pub completed: u64,
    pub refused: u64,
    pub faults: u64,
    /// Calls waiting for the writer right now.
    pub queue_depth: u64,
    pub queue_high_water: u64,
    /// Time from enqueue to the writer picking the call up.
    pub wait: HistogramSnapshot,
    /// Time the writer spent on the call, fsync included.
    pub exec: HistogramSnapshot,
}

/// One provider governor: a (base url, credential) pair with its own ceiling
/// and queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GovernorStats {
    pub max_concurrency: usize,
    pub in_flight: usize,
    pub high_water_in_flight: usize,
    pub queued: usize,
    pub high_water_queued: usize,
    pub admitted: u64,
    pub queued_admissions: u64,
    pub refused: u64,
    pub cooldown_entries: u64,
    pub cooldown_remaining_ms: u64,
    /// How long callers waited for a permit. Non-zero means the ceiling, not the
    /// model, is what is throttling the session.
    pub wait: HistogramSnapshot,
}

/// Every live provider governor, folded into one reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderStats {
    pub governors: usize,
    pub in_flight: usize,
    pub high_water_in_flight: usize,
    pub queued: usize,
    pub high_water_queued: usize,
    pub admitted: u64,
    pub queued_admissions: u64,
    pub refused: u64,
    pub cooldown_entries: u64,
    /// Permit waits summed across governors. Each governor keeps its own
    /// histogram, so this is a total plus a max rather than a merged histogram —
    /// merging would need the registry lock on the read path.
    pub wait_total_micros: u64,
    pub wait_max_micros: u64,
    pub wait_count: u64,
}

/// What the core is holding, and what it costs to get there.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeStats {
    pub sessions_resident: usize,
    pub agents_resident: usize,
    pub agents_running: usize,
    /// Bytes held by the in-memory transcript tails across every resident agent.
    pub transcript_bytes: u64,
    pub transcript_items: u64,
    /// Session open, resume included. This is where a long conversation is
    /// turned back into resident state, so it is the number that decides whether
    /// a big session is still cheap to reopen.
    pub resume: HistogramSnapshot,
    /// History pages served out of the store.
    pub history_pages: u64,
    pub store: StoreStats,
}
