//! The session ledger: what every request and every tool call cost.
//!
//! Owner: the session, for the session's lifetime. There is no background task
//! and no timer behind it — an idle session pays one uncontended mutex and
//! nothing else, and a turn pays a few integer adds.
//!
//! **What the cap drops.** Per-request *detail* (the numbers the per-request
//! table draws) is capped at [`REQUEST_RECORD_CAP`]; the oldest record is
//! dropped and counted in `dropped_requests`. The session-wide category totals
//! are **not** capped and never shrink when a record is dropped, so the totals
//! always describe the whole session while the detail table honestly says it
//! is a tail. Tool *names* are capped too, but a name past the cap is folded
//! into one row rather than dropped — the cost of a tool result is the number
//! the user is after, and dropping it would under-report silently.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use pacode_types::metrics::LatencyHist;
use pacode_types::{
    AnalyzeReport, CategoryTotals, ContextQualityReport, REQUEST_RECORD_CAP, RequestRecord,
    TOOL_NAME_CAP, TOOL_OVERFLOW_NAME, ToolCost,
};

use crate::analyze::attribution::RequestAttribution;

/// Wall-clock cost of one request. `ttft` is `None` when the request produced
/// no tokens at all — a cancelled turn, or a reply that is nothing but tool
/// calls. That is an absence, not a zero, and it must not land in the
/// histogram as a suspiciously instant request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestLatency {
    pub ttft: Option<Duration>,
    pub total: Duration,
}

/// Per-session token and latency accounting. Cheap to call on every request
/// and every tool result; a [`AnalyzeReport`] is a point-in-time copy, so the
/// page never holds a borrow into the session.
#[derive(Debug, Default)]
pub struct SessionAccounting {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    categories: CategoryTotals,
    reported_input: u64,
    reported_output: u64,
    cache_read: u64,
    requests: VecDeque<RequestRecord>,
    dropped_requests: u64,
    tools: BTreeMap<String, ToolCost>,
    folded_tools: u64,
    tool_result_tokens: u64,
    ttft: LatencyHist,
    total_latency: LatencyHist,
    quality: ContextQualityReport,
}

impl SessionAccounting {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charge one completed request. Call it once per provider call, after the
    /// stream ends, with the same `history` slice the request carried.
    pub fn record_request(&self, attribution: RequestAttribution, latency: Option<RequestLatency>) {
        let mut inner = self.lock();
        for (category, tokens) in attribution.categories.rows() {
            inner.categories.add(category, tokens);
        }
        inner.reported_input = inner
            .reported_input
            .saturating_add(attribution.reported_input);
        inner.reported_output = inner
            .reported_output
            .saturating_add(attribution.reported_output);
        inner.cache_read = inner.cache_read.saturating_add(attribution.cache_read);

        let (ttft_micros, total_micros) = match latency {
            Some(latency) => {
                if let Some(ttft) = latency.ttft {
                    inner.ttft.record_duration(ttft);
                }
                inner.total_latency.record_duration(latency.total);
                (latency.ttft.map_or(0, micros), micros(latency.total))
            }
            None => (0, 0),
        };

        inner.requests.push_back(RequestRecord {
            categories: attribution.categories,
            reported_input: attribution.reported_input,
            cache_read: attribution.cache_read,
            ttft_micros,
            total_micros,
        });
        while inner.requests.len() > REQUEST_RECORD_CAP {
            inner.requests.pop_front();
            inner.dropped_requests = inner.dropped_requests.saturating_add(1);
        }
    }

    /// Charge one tool *result*. `result_tokens` is what the model was sent,
    /// after the tool's own output cap — not the raw tool output, which the
    /// model never saw and was never charged for.
    pub fn record_tool_call(&self, tool: &str, result_tokens: u64) {
        let mut inner = self.lock();
        inner.tool_result_tokens = inner.tool_result_tokens.saturating_add(result_tokens);
        let name = if inner.tools.contains_key(tool) || inner.tools.len() < TOOL_NAME_CAP {
            tool
        } else {
            inner.folded_tools = inner.folded_tools.saturating_add(1);
            TOOL_OVERFLOW_NAME
        };
        // `or_default` would leave `tool` empty, and the report reads the values
        // rather than the map keys — so every row on the page would name
        // nothing. The name goes in with the row.
        let entry = inner
            .tools
            .entry(name.to_string())
            .or_insert_with(|| ToolCost {
                tool: name.to_string(),
                ..ToolCost::default()
            });
        entry.calls = entry.calls.saturating_add(1);
        entry.result_tokens = entry.result_tokens.saturating_add(result_tokens);
    }

    /// Replace the context-quality scorecard wholesale. The checkpoint game
    /// owns the counters; this only publishes them.
    pub fn record_context_quality(&self, report: ContextQualityReport) {
        self.lock().quality = report;
    }

    /// Fold one checkpoint into the live scorecard, for a caller that would
    /// otherwise read, edit and write back a snapshot it may already have lost
    /// a race on.
    pub fn update_context_quality(&self, edit: impl FnOnce(&mut ContextQualityReport)) {
        edit(&mut self.lock().quality);
    }

    /// A point-in-time copy for the page.
    pub fn report(&self) -> AnalyzeReport {
        let inner = self.lock();
        let mut tools: Vec<ToolCost> = inner.tools.values().cloned().collect();
        // Heaviest first; the name breaks ties so the page does not reorder
        // itself between two frames when two tools cost the same.
        tools.sort_by(|left, right| {
            right
                .result_tokens
                .cmp(&left.result_tokens)
                .then_with(|| left.tool.cmp(&right.tool))
        });
        AnalyzeReport {
            categories: inner.categories,
            reported_input: inner.reported_input,
            reported_output: inner.reported_output,
            cache_read: inner.cache_read,
            requests: inner.requests.iter().copied().collect(),
            dropped_requests: inner.dropped_requests,
            tools,
            folded_tools: inner.folded_tools,
            tool_result_tokens: inner.tool_result_tokens,
            ttft: inner.ttft.snapshot(),
            total_latency: inner.total_latency.snapshot(),
            quality: inner.quality,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panicking turn must not take the whole session's accounting with
        // it: the numbers are diagnostic, and losing them over a poisoned lock
        // would be the one failure the page exists to explain.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}
