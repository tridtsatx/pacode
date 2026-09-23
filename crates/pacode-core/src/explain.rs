//! Permission "explain" (spec: the `e` key on a pending permission cell):
//! a detached one-shot model call describes what the gated tool call does and
//! rates its danger. The result is broadcast as `Event::PermissionExplained`
//! and is display-only — it never feeds the permission gate.

#[cfg(test)]
#[path = "explain_tests.rs"]
mod explain_tests;

use std::sync::Arc;

use futures::StreamExt;
use pacode_types::{
    Event, ExplainLevel, Message, PermissionRequest, StreamEvent, truncate_head_tail,
};

use crate::session::Session;

/// `req.detail` can hold a whole file diff; the explainer only needs enough
/// of it to judge danger.
const DETAIL_CAP_CHARS: usize = 12_000;
/// The explanation rides a broadcast event to every client — a paragraph.
const EXPLANATION_CAP_CHARS: usize = 2_000;
/// Two-to-four sentences plus the level marker; reasoning stays unfunded.
const MAX_OUTPUT_TOKENS: u32 = 400;

const SYSTEM_PROMPT: &str = "You are a permission explainer in a coding agent. Given a tool call, explain in 2-4 plain sentences what it does and what could go wrong, then rate its danger. Reply in EXACTLY this format:\nLEVEL: <none|low|medium|high|catastrophic>\nEXPLANATION: <text>";

/// The request is already accepted when this runs: the model call must not
/// hold the connection handler, so it runs detached. Whatever the outcome —
/// provider error, stream failure, unparseable reply — the event is still
/// emitted so no client waits forever.
pub fn spawn(session: Arc<Session>, req: PermissionRequest) {
    tokio::spawn(async move {
        let (level, text) = run(&session, &req).await;
        session.events.emit(Event::PermissionExplained {
            permission: req.id.clone(),
            text,
            level,
        });
    });
}

/// Route → ZDR guard → one-shot completion → parse. Every failure maps to
/// `(None, honest text)`; the event goes out regardless.
async fn run(session: &Arc<Session>, req: &PermissionRequest) -> (Option<ExplainLevel>, String) {
    // `permissions.explain_model` wins; default is the session's own model.
    let route = match &session.config.permissions.explain_model {
        Some(configured) => match session.providers.parse_route(configured) {
            Some(route) => route,
            None => {
                return (
                    None,
                    format!("explain failed: unknown model route '{configured}'"),
                );
            }
        },
        None => session.meta().model,
    };

    // A pending command must not leak to a non-ZDR route while retention is on.
    if session.zdr_active() && !session.model_flags(&route.to_string()).zdr_capable {
        return (
            None,
            "explain unavailable under zero-data-retention".to_string(),
        );
    }

    let provider = match session.providers.resolve(&route).await {
        Ok(provider) => provider,
        Err(e) => {
            log::warn!("permission explain: provider resolve failed: {e}");
            return (None, format!("explain failed: {e}"));
        }
    };

    let detail = truncate_head_tail(&req.detail, DETAIL_CAP_CHARS);
    let user = format!("Tool: {}\nTitle: {}\n\n{}", req.tool, req.title, detail);
    let completion_req = pacode_provider::CompletionRequest {
        model: route.model.clone(),
        system_static: SYSTEM_PROMPT.to_string(),
        system_dynamic: String::new(),
        messages: vec![Message::user(user)],
        tools: Vec::new(),
        effort: None,
        max_output_tokens: Some(MAX_OUTPUT_TOKENS),
    };

    let mut stream = match provider.complete(completion_req).await {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!("permission explain: completion failed: {e}");
            return (None, format!("explain failed: {e}"));
        }
    };
    let mut raw = String::new();
    while let Some(event_res) = stream.next().await {
        match event_res {
            Ok(StreamEvent::TextDelta { text }) => raw.push_str(&text),
            Ok(
                StreamEvent::MessageStart { .. }
                | StreamEvent::ReasoningDelta { .. }
                | StreamEvent::ReasoningSignature { .. }
                | StreamEvent::ToolCallStart { .. }
                | StreamEvent::ToolCallArgsDelta { .. }
                | StreamEvent::Usage(_)
                | StreamEvent::MessageEnd { .. },
            ) => {}
            Err(e) => {
                log::warn!("permission explain: stream failed: {e}");
                return (None, format!("explain failed: {e}"));
            }
        }
    }

    parse_output(&raw)
}

/// `LEVEL:` / `EXPLANATION:` tag prefix, matched case-insensitively after
/// leading whitespace — models drift on casing more often than on the tag.
fn strip_tag<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let trimmed = line.trim_start();
    if trimmed.len() >= tag.len() && trimmed[..tag.len()].eq_ignore_ascii_case(tag) {
        Some(&trimmed[tag.len()..])
    } else {
        None
    }
}

/// Split the fixed-format reply into the display level and the explanation
/// text. A missing/unparseable `LEVEL:` degrades to `None` — the text still
/// ships. With no `EXPLANATION:` marker the raw reply (minus the LEVEL line)
/// is shown as-is, so a chatty model cannot produce an empty cell.
fn parse_output(raw: &str) -> (Option<ExplainLevel>, String) {
    let mut level = None;
    let mut explanation = String::new();
    let mut in_explanation = false;
    for line in raw.lines() {
        if let Some(rest) = strip_tag(line, "LEVEL:") {
            level = ExplainLevel::parse(rest);
            continue;
        }
        if let Some(rest) = strip_tag(line, "EXPLANATION:") {
            in_explanation = true;
            explanation.push_str(rest);
            continue;
        }
        if in_explanation {
            if !explanation.is_empty() {
                explanation.push('\n');
            }
            explanation.push_str(line);
        }
    }
    let text = if in_explanation {
        explanation.trim().to_string()
    } else {
        raw.lines()
            .filter(|line| strip_tag(line, "LEVEL:").is_none())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    };
    if text.is_empty() {
        return (level, "explain failed: empty response".to_string());
    }
    (level, truncate_head_tail(&text, EXPLANATION_CAP_CHARS))
}
