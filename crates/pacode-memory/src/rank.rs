//! Ranking: `score = relevance (-bm25) * recency * pinned * recall-count boost`.
//! Pure functions; boosts are bounded so relevance stays the dominant term.

use crate::types::Entry;

const DAY_MS: f64 = 86_400_000.0;
/// Recency boost halves its excess over the floor every this many days.
const RECENCY_HALF_LIFE_DAYS: f64 = 60.0;
const RECENCY_FLOOR: f64 = 0.7;
pub const PINNED_BOOST: f64 = 1.25;
const COUNT_BOOST_PER_LN: f64 = 0.1;

/// Multiplier in `[0.7, 1.0]`: fresh entries 1.0, old entries approach 0.7.
pub fn recency_boost(age_ms: i64) -> f64 {
    let days = (age_ms.max(0) as f64) / DAY_MS;
    RECENCY_FLOOR + (1.0 - RECENCY_FLOOR) * 0.5f64.powf(days / RECENCY_HALF_LIFE_DAYS)
}

/// `1 + 0.1 * ln(1 + recall_count)`.
pub fn count_boost(recall_count: i64) -> f64 {
    1.0 + COUNT_BOOST_PER_LN * (1.0 + recall_count.max(0) as f64).ln()
}

/// Combined multiplier of recency, pin and recall count.
pub fn boost(e: &Entry, now_ms: i64) -> f64 {
    let pinned = if e.pinned { PINNED_BOOST } else { 1.0 };
    recency_boost(now_ms - e.updated_ms) * pinned * count_boost(e.recall_count)
}
