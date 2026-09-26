//! Token and latency accounting contracts for the `/analyze` page.
//!
//! Two facts shape every number here.
//!
//! **There is no tokenizer in pacode.** The only token count a provider
//! reports is the per-request `input_tokens`/`output_tokens` pair — one total
//! for the whole prompt, with no breakdown. Anything finer than that (what the
//! instructions cost, what the tool definitions weighed, how much of the
//! history was tool output) can only come from the chars/4 estimator in
//! [`crate::message`]. Those numbers are estimates and the page labels them as
//! such: a per-category figure printed to the token with no decimal point reads
//! as measured when it is a character count in a trench coat. `reply` is the
//! one exception — it is the provider's own `output_tokens`.
//!
//! **The accounting is bounded.** A session accumulates one record per request
//! and one entry per tool name; both are capped, and a drop is counted rather
//! than hidden. [`AnalyzeReport::dropped_requests`] and `folded_tools` exist so
//! the page can say what the numbers do not cover.

use serde::{Deserialize, Serialize};

use crate::metrics::HistogramSnapshot;

/// Per-request detail records kept. Each one is a handful of integers (no
/// message text), so 512 of them is a few tens of KiB; past that the oldest
/// are dropped and [`AnalyzeReport::dropped_requests`] counts them.
pub const REQUEST_RECORD_CAP: usize = 512;

/// Distinct tool names ranked on their own. Names come from the tool registry
/// and are effectively bounded already, but MCP servers and plugins bring
/// their own, so the map needs a ceiling. Names past it are folded into a
/// single [`TOOL_OVERFLOW_NAME`] row instead of being dropped: the number the
/// user is after is the total a tool result cost, and dropping it would make
/// the page quietly under-report.
pub const TOOL_NAME_CAP: usize = 64;

/// The row every tool name past [`TOOL_NAME_CAP`] is folded into.
pub const TOOL_OVERFLOW_NAME: &str = "(other tools)";

/// Where a request's tokens went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenCategory {
    /// System instructions: the static prompt plus the per-turn dynamic block.
    Instructions,
    /// Tool definitions advertised with the request.
    ToolDefs,
    /// Conversation history minus tool output: user turns, assistant prose,
    /// the tool *calls* themselves, images, compaction summaries.
    History,
    /// `ToolResult` blocks already in history. Split out from `History`
    /// because a single grep that returns 200k tokens is the one finding this
    /// page exists to surface, and burying it in "history" hides exactly that.
    ToolResults,
    /// The assistant's own reply. The provider's `output_tokens`, not an
    /// estimate.
    Reply,
}

/// Display order of the category table.
pub const TOKEN_CATEGORIES: [TokenCategory; 5] = [
    TokenCategory::Instructions,
    TokenCategory::ToolDefs,
    TokenCategory::History,
    TokenCategory::ToolResults,
    TokenCategory::Reply,
];

impl TokenCategory {
    pub fn label(self) -> &'static str {
        match self {
            TokenCategory::Instructions => "instructions",
            TokenCategory::ToolDefs => "tool defs",
            TokenCategory::History => "history",
            TokenCategory::ToolResults => "tool results",
            TokenCategory::Reply => "reply",
        }
    }
}

/// Tokens per category, summed over every request in the session.
///
/// Note what this is *not*: a one-shot breakdown of the current prompt. Each
/// request re-sends the whole history, so a session's totals are the sum of
/// what every request was charged, which is why the `history` and
/// `tool_results` columns grow with the conversation even though the prompt
/// only got longer once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryTotals {
    pub instructions: u64,
    pub tool_defs: u64,
    pub history: u64,
    pub tool_results: u64,
    pub reply: u64,
}

impl CategoryTotals {
    pub fn get(&self, category: TokenCategory) -> u64 {
        match category {
            TokenCategory::Instructions => self.instructions,
            TokenCategory::ToolDefs => self.tool_defs,
            TokenCategory::History => self.history,
            TokenCategory::ToolResults => self.tool_results,
            TokenCategory::Reply => self.reply,
        }
    }

    pub fn add(&mut self, category: TokenCategory, tokens: u64) {
        let slot = match category {
            TokenCategory::Instructions => &mut self.instructions,
            TokenCategory::ToolDefs => &mut self.tool_defs,
            TokenCategory::History => &mut self.history,
            TokenCategory::ToolResults => &mut self.tool_results,
            TokenCategory::Reply => &mut self.reply,
        };
        *slot = slot.saturating_add(tokens);
    }

    /// Every category with its number, in display order.
    pub fn rows(&self) -> [(TokenCategory, u64); 5] {
        [
            (TokenCategory::Instructions, self.instructions),
            (TokenCategory::ToolDefs, self.tool_defs),
            (TokenCategory::History, self.history),
            (TokenCategory::ToolResults, self.tool_results),
            (TokenCategory::Reply, self.reply),
        ]
    }

    /// The total this split is a split of: the sum of the five categories.
    ///
    /// Because each category is estimated independently at chars/4, this can
    /// exceed the estimator's answer for the request as a whole by up to four
    /// tokens of rounding. That direction is the safe one — the split never
    /// claims less than the whole.
    pub fn total(&self) -> u64 {
        self.rows()
            .into_iter()
            .fold(0u64, |sum, (_, tokens)| sum.saturating_add(tokens))
    }
}

/// One provider request. `ttft_micros` is 0 when the request produced no
/// tokens at all (a cancelled turn, a provider that answers with tool calls
/// only and no preamble).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRecord {
    pub categories: CategoryTotals,
    /// Provider-reported input for this request; 0 when it reported none.
    pub reported_input: u64,
    pub cache_read: u64,
    pub ttft_micros: u64,
    pub total_micros: u64,
}

/// Tokens one tool's *results* cost, over the whole session, ranked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCost {
    pub tool: String,
    pub calls: u64,
    pub result_tokens: u64,
}

/// What happened to one context checkpoint.
///
/// `LostToCompaction` is not a failure mode. A checkpoint word that compaction
/// removed before the model could answer it is a fact about the session, not
/// about the model's memory, and scoring it as a miss would punish the model
/// for the user enabling `/compact`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointOutcome {
    Passed,
    /// The model saw the checkpoint and said something else.
    Ignored,
    /// The model saw the checkpoint and echoed the wrong word.
    Wrong,
    LostToCompaction,
}

impl CheckpointOutcome {
    fn bump(self, report: &mut ContextQualityReport) {
        match self {
            CheckpointOutcome::Passed => report.passed = report.passed.saturating_add(1),
            CheckpointOutcome::Ignored => report.ignored = report.ignored.saturating_add(1),
            CheckpointOutcome::Wrong => report.wrong = report.wrong.saturating_add(1),
            CheckpointOutcome::LostToCompaction => {
                report.lost_to_compaction = report.lost_to_compaction.saturating_add(1)
            }
        }
    }
}

/// The context-quality scorecard the `/analyze` page reports into.
///
/// Built by the checkpoint game ([`record`](Self::record)) and read by the
/// page. `lost_to_compaction` is here from the first commit because retrofitting
/// it later would mean re-deciding what past sessions were allowed to count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextQualityReport {
    pub checkpoints: u32,
    pub passed: u32,
    pub ignored: u32,
    pub wrong: u32,
    pub lost_to_compaction: u32,
    /// `passed / scored_checkpoints`, 0.0 when nothing was scoreable. Compute
    /// it through [`ContextQualityReport::scored`] or [`Self::record`] — the
    /// field is public so the page and the wire can read it, not so that
    /// callers can invent it.
    pub score: f64,
}

impl ContextQualityReport {
    /// A report with every counter set at once. The only place `score` is
    /// computed outside `record`, so the two paths cannot disagree.
    pub fn scored(
        checkpoints: u32,
        passed: u32,
        ignored: u32,
        wrong: u32,
        lost_to_compaction: u32,
    ) -> Self {
        let mut report = Self {
            checkpoints,
            passed,
            ignored,
            wrong,
            lost_to_compaction,
            score: 0.0,
        };
        report.refresh_score();
        report
    }

    /// Fold one checkpoint outcome in and re-derive the score.
    pub fn record(&mut self, outcome: CheckpointOutcome) {
        self.checkpoints = self.checkpoints.saturating_add(1);
        outcome.bump(self);
        self.refresh_score();
    }

    /// Checkpoints the model could actually have got right: everything that
    /// survived in context until it was asked.
    pub fn scored_checkpoints(&self) -> u32 {
        self.checkpoints.saturating_sub(self.lost_to_compaction)
    }

    /// True when every checkpoint left the context before it was asked, so the
    /// score carries no evidence at all. The page must say "no scored
    /// checkpoints" here rather than print a `0.00` that reads as total failure.
    pub fn is_unscored(&self) -> bool {
        self.checkpoints > 0 && self.scored_checkpoints() == 0
    }

    fn refresh_score(&mut self) {
        self.score = self.passed as f64 / self.scored_checkpoints().max(1) as f64;
    }
}

/// Everything the `/analyze` page draws, as a point-in-time reading.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyzeReport {
    /// Session-wide category totals. These cover **every** request, including
    /// the ones whose detail record was dropped at [`REQUEST_RECORD_CAP`].
    pub categories: CategoryTotals,
    pub reported_input: u64,
    pub reported_output: u64,
    pub cache_read: u64,
    /// The most recent requests, oldest first, at most [`REQUEST_RECORD_CAP`].
    pub requests: Vec<RequestRecord>,
    /// Detail records dropped to stay under the cap. Non-zero means the
    /// per-request table below is a tail, not the session.
    pub dropped_requests: u64,
    /// Ranked by `result_tokens`, descending.
    pub tools: Vec<ToolCost>,
    /// Calls folded into the [`TOOL_OVERFLOW_NAME`] row.
    pub folded_tools: u64,
    /// Every tool result's tokens, including the folded ones: the sum of
    /// `tools[].result_tokens` is always this.
    pub tool_result_tokens: u64,
    pub ttft: HistogramSnapshot,
    pub total_latency: HistogramSnapshot,
    pub quality: ContextQualityReport,
}

impl AnalyzeReport {
    /// Requests this report still holds detail for.
    pub fn requests_detailed(&self) -> usize {
        self.requests.len()
    }

    /// Requests the session made in total — detailed plus dropped.
    pub fn requests_seen(&self) -> u64 {
        self.requests.len() as u64 + self.dropped_requests
    }
}

#[cfg(test)]
#[path = "analyze_tests.rs"]
mod analyze_tests;
