//! Compaction (spec §6.4). Port the approach of jcode `jcode-compaction-core`.

use std::sync::Arc;

use pacode_types::Message;

use crate::CoreError;
use crate::agent::Agent;
use crate::session::Session;

/// True when the last reported input tokens (or the estimate) exceed
/// `threshold × context_window`.
pub fn needs_compaction(context_tokens: u32, context_window: u32, threshold: f32) -> bool {
    context_window > 0 && (context_tokens as f32) >= (context_window as f32) * threshold
}

/// Split history: everything except the last `keep_recent` messages (never splitting a
/// tool_use/tool_result pair) goes into the summary request.
pub fn split_for_compaction(
    messages: &[Arc<Message>],
    keep_recent: usize,
) -> (Vec<Arc<Message>>, Vec<Arc<Message>>) {
    if messages.len() <= keep_recent {
        return (Vec::new(), messages.to_vec());
    }
    let mut cut = messages.len().saturating_sub(keep_recent);
    while cut > 0 {
        if messages[cut].role == pacode_types::Role::Tool
            || (messages[cut - 1].has_tool_uses()
                && cut < messages.len()
                && messages[cut].role == pacode_types::Role::Tool)
        {
            cut -= 1;
        } else {
            break;
        }
    }
    (messages[..cut].to_vec(), messages[cut..].to_vec())
}

/// The summarisation prompt: previous summary (if any) + the messages to fold in.
pub fn build_summary_request(
    previous_summary: Option<&str>,
    messages: &[Arc<Message>],
) -> Vec<Message> {
    use pacode_types::{ContentBlock, Role};
    let mut out = Vec::new();
    out.push(Message::new(
        Role::System,
        vec![ContentBlock::Text {
            text: "Summarize the conversation so far for continuing the work: goals, decisions, files touched, open items. Be concise and structured.".to_string(),
        }],
    ));
    if let Some(prev) = previous_summary {
        out.push(Message::user(format!(
            "[Previous conversation summary to incorporate and update]\n{prev}"
        )));
    }
    for msg in messages {
        out.push((**msg).clone());
    }
    out.push(Message::user(
        "Summarize the conversation so far for continuing the work: goals, decisions, files touched, open items.",
    ));
    out
}

/// Run the summary turn with `context.compaction_model` (or the agent's model), replace
/// the history, persist, emit a `Notice` item. Returns false when nothing was done.
pub async fn compact(session: &Arc<Session>, agent: &Arc<Agent>) -> Result<bool, CoreError> {
    use futures::StreamExt;

    let (to_summarize, to_keep, prev_summary, upto_seq) = {
        let hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        let keep_recent = session.config.context.keep_recent_messages;
        let (to_summarize, to_keep) = split_for_compaction(&hist.messages, keep_recent);
        if to_summarize.is_empty() {
            return Ok(false);
        }
        let start_seq = hist.next_seq.saturating_sub(hist.messages.len() as u64);
        let upto_seq = start_seq + (to_summarize.len() as u64) - 1;
        (to_summarize, to_keep, hist.summary.clone(), upto_seq)
    };

    let summary_messages = build_summary_request(prev_summary.as_deref(), &to_summarize);
    let agent_info = agent.info();
    let route = session
        .config
        .context
        .compaction_model
        .as_deref()
        .and_then(|m| session.providers.parse_route(m))
        .unwrap_or_else(|| agent_info.model.clone());

    let provider = session.providers.resolve(&route)?;
    let req = pacode_provider::CompletionRequest {
        model: route.model.clone(),
        system_static: "You are a concise conversation summarizer.".to_string(),
        system_dynamic: String::new(),
        messages: summary_messages,
        tools: Vec::new(),
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = provider.complete(req).await?;
    let mut summary_text = String::new();
    while let Some(ev_res) = stream.next().await {
        match ev_res {
            Ok(pacode_types::StreamEvent::TextDelta { text }) => {
                summary_text.push_str(&text);
            }
            Ok(_) => {}
            Err(e) => return Err(CoreError::Provider(e)),
        }
    }

    let summary = summary_text.trim().to_string();
    if summary.is_empty() {
        return Ok(false);
    }

    {
        let mut hist = agent.history.lock().unwrap_or_else(|p| p.into_inner());
        hist.summary = Some(summary.clone());
        hist.summary_upto = upto_seq;
        hist.messages = to_keep;
        let summary_tokens = pacode_types::estimate_tokens(&summary);
        let kept_tokens: u32 = hist.messages.iter().map(|m| m.estimate_tokens()).sum();
        hist.estimated_tokens = summary_tokens.saturating_add(kept_tokens);
    }

    session
        .store
        .save_compaction(&session.id, &agent.id, &summary, upto_seq)
        .await?;

    let notice_item = pacode_types::TranscriptItem {
        seq: agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq(),
        agent: agent.id.clone(),
        ts_ms: pacode_types::now_ms(),
        kind: pacode_types::TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Info,
            text: format!("Conversation history compacted up to seq {upto_seq}"),
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(notice_item.clone());
    session
        .events
        .emit(pacode_types::Event::ItemAdded(notice_item));

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacode_types::{CallId, ContentBlock, Message, Role};

    #[test]
    fn test_needs_compaction() {
        assert!(!needs_compaction(500, 1000, 0.8));
        assert!(!needs_compaction(799, 1000, 0.8));
        assert!(needs_compaction(800, 1000, 0.8));
        assert!(needs_compaction(1200, 1000, 0.8));
        assert!(!needs_compaction(1000, 0, 0.8));
    }

    #[test]
    fn test_split_for_compaction_basic() {
        let msgs: Vec<Arc<Message>> = (0..10)
            .map(|i| Arc::new(Message::user(format!("msg {i}"))))
            .collect();

        let (to_summarize, to_keep) = split_for_compaction(&msgs, 4);
        assert_eq!(to_summarize.len(), 6);
        assert_eq!(to_keep.len(), 4);
        assert_eq!(to_summarize[0].text(), "msg 0");
        assert_eq!(to_keep[0].text(), "msg 6");
    }

    #[test]
    fn test_split_for_compaction_keeps_tool_pairs() {
        let call_id = CallId::generate();
        let msgs = vec![
            Arc::new(Message::user("do something")),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls"}),
                }],
            )),
            Arc::new(Message::tool_result(call_id, "file1.txt", false)),
            Arc::new(Message::user("next prompt")),
        ];

        // If keep_recent = 2, naive cut would be at index 2 (ToolResult),
        // which would separate ToolUse (index 1) and ToolResult (index 2).
        // The algorithm must shift cut to 1 so that both ToolUse and ToolResult stay in keep!
        let (to_summarize, to_keep) = split_for_compaction(&msgs, 2);
        assert_eq!(to_summarize.len(), 1);
        assert_eq!(to_summarize[0].text(), "do something");
        assert_eq!(to_keep.len(), 3);
        assert_eq!(to_keep[0].role, Role::Assistant);
        assert_eq!(to_keep[1].role, Role::Tool);
    }
}
