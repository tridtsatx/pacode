use std::sync::Arc;

use pacode_types::{CallId, ContentBlock, Message, MessageKind, Role};
use serde_json::json;

use super::*;

fn user(text: &str) -> Arc<Message> {
    Arc::new(Message::user(text))
}

/// assistant tool call + its result message
fn pair(name: &str, input: serde_json::Value, result: &str) -> Vec<Arc<Message>> {
    let id = CallId::generate();
    vec![
        Arc::new(Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: name.into(),
                input,
            }],
        )),
        Arc::new(Message::tool_result(id, result, false)),
    ]
}

fn big(tag: &str) -> String {
    format!("HEAD-{tag}{}TAIL-{tag}", "m".repeat(5_000))
}

fn params(k: usize, target: u32) -> ShakeParams {
    ShakeParams {
        keep_recent_turns: k,
        min_chars: 1_000,
        target_tokens: target,
        overhead_tokens: 0,
    }
}

fn result_text(msg: &Message) -> String {
    match &msg.content[0] {
        ContentBlock::ToolResult { content, .. } => content.clone(),
        other => panic!("not a tool result: {other:?}"),
    }
}

/// `n` turns, each: user, assistant tool call, big bash result.
fn turns(n: usize) -> Vec<Arc<Message>> {
    let mut msgs = Vec::new();
    for i in 0..n {
        msgs.push(user(&format!("turn {i}")));
        msgs.extend(pair(
            "bash",
            json!({"command": format!("cmd {i}")}),
            &big(&i.to_string()),
        ));
    }
    msgs
}

#[test]
fn old_large_result_becomes_stub_with_metadata_and_excerpts() {
    let msgs = turns(4);
    let out = shake(&msgs, &params(2, 0));
    assert_eq!(out.stubbed, 2);
    assert_eq!(out.messages.len(), msgs.len());
    let stub = result_text(&out.messages[2]);
    assert!(stub.starts_with(STUB_PREFIX), "{stub}");
    assert!(stub.contains("bash"));
    assert!(stub.contains("`cmd 0`"));
    assert!(stub.contains("HEAD-0"));
    assert!(stub.contains("TAIL-0"));
    assert!(stub.contains("chars omitted"));
    assert!(stub.chars().count() < 1_000);
    assert!(out.tokens < msgs.iter().map(|m| m.estimate_tokens()).sum::<u32>());
}

#[test]
fn last_k_turns_are_never_touched() {
    let msgs = turns(4);
    let out = shake(&msgs, &params(2, 0));
    for idx in [8, 11] {
        assert_eq!(result_text(&out.messages[idx]), result_text(&msgs[idx]));
    }
    assert!(result_text(&out.messages[2]).starts_with(STUB_PREFIX));
    assert!(result_text(&out.messages[5]).starts_with(STUB_PREFIX));
    assert!(result_text(&out.messages[8]).len() > 5_000);
}

#[test]
fn fewer_turns_than_k_changes_nothing() {
    let msgs = turns(3);
    let out = shake(&msgs, &params(6, 0));
    assert_eq!(out.stubbed, 0);
    assert_eq!(out.messages, msgs);
}

#[test]
fn pairs_stay_intact() {
    let msgs = turns(5);
    let out = shake(&msgs, &params(1, 0));
    assert_eq!(out.messages.len(), msgs.len());
    for (before, after) in msgs.iter().zip(&out.messages) {
        assert_eq!(before.role, after.role);
        if let (
            ContentBlock::ToolResult { call_id: a, .. },
            ContentBlock::ToolResult { call_id: b, .. },
        ) = (&before.content[0], &after.content[0])
        {
            assert_eq!(a, b);
        }
        assert_eq!(before.has_tool_uses(), after.has_tool_uses());
    }
}

#[test]
fn latest_result_per_path_is_kept() {
    let mut msgs = vec![user("t0")];
    msgs.extend(pair("read", json!({"path": "a.rs"}), &big("a1")));
    msgs.push(user("t1"));
    msgs.extend(pair("read", json!({"path": "a.rs"}), &big("a2")));
    msgs.push(user("t2"));
    msgs.extend(pair("read", json!({"path": "b.rs"}), &big("b1")));
    for i in 3..6 {
        msgs.push(user(&format!("t{i}")));
    }
    let out = shake(&msgs, &params(3, 0));
    assert!(
        result_text(&out.messages[2]).starts_with(STUB_PREFIX),
        "older a.rs read"
    );
    assert!(
        !result_text(&out.messages[5]).starts_with(STUB_PREFIX),
        "latest a.rs read"
    );
    assert!(
        !result_text(&out.messages[8]).starts_with(STUB_PREFIX),
        "only b.rs read"
    );
    assert_eq!(out.stubbed, 1);
}

#[test]
fn pinned_items_are_never_touched() {
    let mut msgs = vec![user("t0")];
    msgs.extend(pair("plan", json!({"action": "show"}), &big("plan")));
    let reminder_id = CallId::generate();
    msgs.push(Arc::new(
        Message::tool_result(reminder_id, big("rem"), false).with_kind(MessageKind::SystemReminder),
    ));
    msgs.extend(turns(3));
    let out = shake(&msgs, &params(2, 0));
    assert!(
        !result_text(&out.messages[2]).starts_with(STUB_PREFIX),
        "plan tool"
    );
    assert!(
        !result_text(&out.messages[3]).starts_with(STUB_PREFIX),
        "non-normal kind"
    );
    assert!(result_text(&out.messages[6]).starts_with(STUB_PREFIX));
}

#[test]
fn stops_once_target_is_reached() {
    let msgs = turns(6);
    let total: u32 = msgs.iter().map(|m| m.estimate_tokens()).sum();
    // One stub saves ~1.2k tokens; ask for slightly less than that.
    let out = shake(&msgs, &params(1, total - 500));
    assert_eq!(out.stubbed, 1);
    assert!(out.tokens <= total - 500);
}

#[test]
fn small_results_and_second_pass_are_noops() {
    let mut msgs = vec![user("t0")];
    msgs.extend(pair("bash", json!({"command": "ls"}), "short output"));
    msgs.extend(turns(3));
    let first = shake(&msgs, &params(1, 0));
    assert_eq!(result_text(&first.messages[2]), "short output");
    let second = shake(&first.messages, &params(1, 0));
    assert_eq!(second.stubbed, 0);
}
