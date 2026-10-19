//! Planner-half unit tests: the refusal the planner gets instead of a tool,
//! and the stream collector that has to leave no trace.

use pacode_provider::{EventStream, ProviderError};
use pacode_types::{CallId, StopReason, StreamEvent, Usage};
use tokio_util::sync::CancellationToken;

use super::{ALLOWED_TOOLS, collect, refusal_text};

type Events = Vec<Result<StreamEvent, ProviderError>>;

fn stream(events: Events) -> EventStream {
    Box::pin(futures::stream::iter(events))
}

fn call_events(id: &str, args: &str) -> Events {
    vec![
        Ok(StreamEvent::ToolCallStart {
            index: 0,
            id: CallId::new(id.to_string()),
            name: "read".into(),
        }),
        Ok(StreamEvent::ToolCallArgsDelta {
            index: 0,
            delta: args.to_string(),
        }),
        Ok(StreamEvent::MessageEnd {
            stop: StopReason::ToolUse,
        }),
    ]
}

#[test]
fn the_refusal_names_the_tool_and_the_allowlist() {
    let refusal = refusal_text("write");
    assert!(refusal.contains("write"), "{refusal}");
    // A planner told only that it was refused retries the call; told what it
    // may use instead, it plans. Every allowed name has to be in the text, so
    // this fails when the allowlist grows and the message does not.
    for name in ALLOWED_TOOLS {
        assert!(refusal.contains(name), "{name} missing from: {refusal}");
    }
}

#[test]
fn the_allowlist_cannot_reach_a_write_or_an_exec() {
    // The whole point of a name allowlist over `filter_kinds`: these are the
    // names that would give the planner hands or a shell.
    for forbidden in ["edit", "write", "multi_edit", "bash", "batch", "agent"] {
        assert!(
            !ALLOWED_TOOLS.contains(&forbidden),
            "{forbidden} is not read-only"
        );
    }
}

#[tokio::test]
async fn collect_keeps_text_calls_and_usage_without_touching_anything() {
    let mut events = vec![Ok(StreamEvent::TextDelta {
        text: "reading the parser".into(),
    })];
    events.extend(call_events("c1", r#"{"path":"src/lib.rs"}"#));
    events.push(Ok(StreamEvent::Usage(Usage {
        input_tokens: 11,
        output_tokens: 5,
        ..Usage::default()
    })));
    let step = collect(stream(events), &CancellationToken::new())
        .await
        .expect("a healthy stream is not a failure");
    assert_eq!(step.text, "reading the parser");
    assert_eq!(step.calls.len(), 1);
    assert_eq!(step.calls[0].name, "read");
    assert_eq!(step.calls[0].input["path"], "src/lib.rs");
    assert!(step.calls[0].parse_err.is_none());
    assert_eq!(step.usage.input_tokens, 11);
    assert!(!step.interrupted);
    // No transcript cell is allocated for a planner call: it is not the user's
    // conversation, and a resumed session must not grow one.
    assert!(step.calls[0].item_seq.is_none());
}

#[tokio::test]
async fn arguments_past_the_cap_are_refused_rather_than_parsed() {
    let oversized = "x".repeat(crate::turn::stream::RAW_ARGS_CAP + 1);
    let step = collect(
        stream(call_events("c1", &oversized)),
        &CancellationToken::new(),
    )
    .await
    .expect("collected");
    let call = &step.calls[0];
    let err = call
        .parse_err
        .as_deref()
        .expect("a JSON prefix is not input");
    assert!(err.contains("streaming cap"), "{err}");
    assert_eq!(
        call.input,
        serde_json::json!({}),
        "nothing is dispatched from a truncated argument buffer"
    );
}

#[tokio::test]
async fn a_reissued_call_id_leaves_one_call_to_dispatch() {
    let mut events = call_events("c1", r#"{"path":"a"}"#);
    // A provider retry re-issues the same id under a new index; both must not
    // reach the dispatcher, or the plan pays for two reads where it asked for
    // one and the second answer answers a call that no longer exists.
    events.push(Ok(StreamEvent::ToolCallStart {
        index: 1,
        id: CallId::new("c1".to_string()),
        name: "read".into(),
    }));
    events.push(Ok(StreamEvent::ToolCallArgsDelta {
        index: 1,
        delta: r#"{"path":"b"}"#.to_string(),
    }));
    events.push(Ok(StreamEvent::MessageEnd {
        stop: StopReason::ToolUse,
    }));
    let step = collect(stream(events), &CancellationToken::new())
        .await
        .expect("collected");
    assert_eq!(step.calls.len(), 1);
    assert_eq!(step.calls[0].input["path"], "b");
}

#[tokio::test]
async fn a_cancelled_turn_stops_the_collector_where_it_stands() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let hanging: EventStream = Box::pin(futures::stream::pending());
    let step = collect(hanging, &cancel).await.expect("not a stream error");
    assert!(step.interrupted);
    assert!(step.calls.is_empty());
    assert!(step.text.is_empty());
}
