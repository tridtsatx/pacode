use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pacode_types::{CallId, ContentBlock, Message, Role};
use serde_json::json;

use super::*;
use crate::compaction::pinned::PINNED_MARKER;

struct MockSummarizer {
    reply: Result<String, String>,
    seen: Mutex<Vec<String>>,
}

impl MockSummarizer {
    fn ok(text: &str) -> Self {
        Self {
            reply: Ok(text.to_string()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn failing() -> Self {
        Self {
            reply: Err("boom".to_string()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn last_prompt(&self) -> String {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Summarizer for MockSummarizer {
    async fn summarize(&self, request: Vec<Message>) -> Result<String, CoreError> {
        self.seen.lock().unwrap().push(request[0].text());
        self.reply.clone().map_err(CoreError::Invalid)
    }
}

/// `n` turns of user + assistant tool call + 8000-char (~2000 token) bash result.
fn history(n: usize) -> Vec<Arc<Message>> {
    let mut msgs = Vec::new();
    for i in 0..n {
        let id = CallId::generate();
        msgs.push(Arc::new(Message::user(format!("do step {i}"))));
        msgs.push(Arc::new(Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: "bash".into(),
                input: json!({"command": format!("cmd {i}")}),
            }],
        )));
        msgs.push(Arc::new(Message::tool_result(id, "o".repeat(8_000), false)));
    }
    msgs
}

fn cfg(window: u32) -> PipelineConfig {
    PipelineConfig {
        context_window: window,
        threshold: 0.8,
        target_ratio: 0.4,
        keep_recent_turns: 6,
        keep_recent_messages: 4,
        overhead_tokens: 0,
        force: false,
    }
}

fn input(messages: Vec<Arc<Message>>, previous: Option<&str>) -> PipelineInput {
    PipelineInput {
        messages,
        previous_summary: previous.map(str::to_string),
        pinned: vec!["## Current plan (verbatim)\n[~] 1 (active): ship it".to_string()],
    }
}

#[tokio::test]
async fn below_threshold_is_a_noop() {
    let s = MockSummarizer::ok("unused");
    let out = run(input(history(3), None), &cfg(1_000_000), &s).await;
    assert_eq!(out.method, None);
    assert_eq!(out.messages.len(), 9);
    assert_eq!(out.tokens_before, out.tokens_after);
    assert_eq!(s.calls(), 0);
}

#[tokio::test]
async fn shake_alone_is_enough_and_second_round_shrinks_k() {
    let s = MockSummarizer::ok("unused");
    let cfg = cfg(28_000);
    let out = run(input(history(12), None), &cfg, &s).await;
    assert_eq!(out.method, Some(CompactionMethod::Shake));
    assert_eq!(
        s.calls(),
        0,
        "no model call when shake gets under threshold"
    );
    assert_eq!(out.folded, 0);
    assert_eq!(
        out.rounds, 2,
        "over target after round 1 -> K halved, shake retried"
    );
    assert!(out.tokens_after <= (28_000f32 * 0.4) as u32);
    assert_eq!(out.messages.len(), 36);
    assert!(out.tokens_after < out.tokens_before);
}

#[tokio::test]
async fn summary_stage_folds_older_part_and_attaches_pinned() {
    let s = MockSummarizer::ok("## Goal & user constraints\n- \"never touch main\"");
    let out = run(input(history(12), None), &cfg(10_000), &s).await;
    assert_eq!(out.method, Some(CompactionMethod::Summary));
    assert_eq!(s.calls(), 1);
    assert!(!out.summary_failed);
    // The cut stops at the oldest recent mutating result inside the lookback
    // (every `bash` call counts) and moves back to its tool call.
    assert_eq!(out.folded, 19);
    assert_eq!(out.messages.len(), 17);
    assert_eq!(out.messages[0].role, Role::Assistant);
    assert!(out.messages[0].has_tool_uses());
    assert_eq!(out.messages[1].role, Role::Tool);

    let summary = out.summary.expect("summary");
    assert!(summary.contains("never touch main"));
    assert!(summary.contains(PINNED_MARKER));
    assert!(summary.contains("ship it"));

    let prompt = s.last_prompt();
    for section in [
        "Goal & user constraints",
        "Decisions made & why",
        "Files touched",
        "Commands run & outcomes",
        "Open problems / next steps",
        "Facts learned about the codebase",
    ] {
        assert!(prompt.contains(section), "missing section {section}");
    }
    assert!(!prompt.contains("<previous-summary>"));
    assert!(prompt.contains("do step 0"));
}

#[tokio::test]
async fn existing_summary_is_updated_without_stale_pins() {
    let s = MockSummarizer::ok("updated body");
    let prev = format!("OLD BODY{PINNED_MARKER}STALE PIN");
    let out = run(input(history(12), Some(&prev)), &cfg(10_000), &s).await;
    assert_eq!(out.method, Some(CompactionMethod::Summary));
    let prompt = s.last_prompt();
    assert!(prompt.contains("<previous-summary>\nOLD BODY\n</previous-summary>"));
    assert!(prompt.contains("UPDATE the previous summary"));
    assert!(!prompt.contains("STALE PIN"));
    let summary = out.summary.expect("summary");
    assert!(summary.starts_with("updated body"));
    assert!(!summary.contains("STALE PIN"));
    assert!(summary.contains("ship it"));
}

#[tokio::test]
async fn summary_failure_falls_back_to_digest_without_error() {
    let s = MockSummarizer::failing();
    let out = run(input(history(12), None), &cfg(10_000), &s).await;
    assert!(out.summary_failed);
    assert_eq!(out.method, Some(CompactionMethod::Shake));
    assert!(out.folded > 0);
    let summary = out.summary.expect("digest summary");
    assert!(summary.contains("Automatic digest"));
    assert!(summary.contains("`cmd 0` -> ok"));
    assert!(
        summary.contains("ship it"),
        "pinned data survives the fallback"
    );
    assert!(out.tokens_after < out.tokens_before);
}

#[tokio::test]
async fn empty_summary_counts_as_failure() {
    let s = MockSummarizer::ok("   ");
    let out = run(input(history(12), None), &cfg(10_000), &s).await;
    assert!(out.summary_failed);
}

#[tokio::test]
async fn forced_run_summarises_even_below_threshold() {
    let s = MockSummarizer::ok("forced summary");
    let mut cfg = cfg(1_000_000);
    cfg.force = true;
    let out = run(input(history(6), None), &cfg, &s).await;
    assert_eq!(out.method, Some(CompactionMethod::Summary));
    assert_eq!(s.calls(), 1);
}

#[tokio::test]
async fn never_more_than_two_rounds() {
    let s = MockSummarizer::ok("s");
    let mut cfg = cfg(2_000);
    cfg.target_ratio = 0.01;
    let out = run(input(history(12), None), &cfg, &s).await;
    assert!(out.rounds <= MAX_ROUNDS);
}

#[test]
fn render_transcript_caps_tool_results() {
    let rendered = render_transcript(&history(1));
    assert!(rendered.contains("[User]\ndo step 0"));
    assert!(rendered.contains("[Tool call] bash"));
    assert!(rendered.contains("[Tool result]"));
    assert!(rendered.len() < 4_000);
}
