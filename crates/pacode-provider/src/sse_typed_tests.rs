use pacode_types::StreamEvent;

use super::*;
use crate::sse::SseParser;

/// A recorded-style stream: CRLF and LF delimiters, escapes, unicode, tool calls with and
/// without `index`, reasoning under all three field names, usage and a finish reason.
fn recorded() -> String {
    let events = [
        r#"{"model":"m-1","choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        r#"{"model":"m-1","choices":[{"delta":{"content":"Hel\"lo\né "}}]}"#,
        r#"{"choices":[{"delta":{"content":"wörld 🌍"}}]}"#,
        r#"{"choices":[{"delta":{"reasoning_content":"think "}}]}"#,
        r#"{"choices":[{"delta":{"reasoning":"more "}}]}"#,
        r#"{"choices":[{"delta":{"thinking":"most"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_2","function":{"name":"grep","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"unknown":{"x":[1,2]}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":2},"prompt_tokens_details":{"cached_tokens":4}}}"#,
        r#"{"model":"m-2","choices":[{"delta":{"content":"tail"}}]}"#,
    ];
    let mut out = String::new();
    for (i, e) in events.iter().enumerate() {
        out.push_str("data: ");
        out.push_str(e);
        out.push_str(if i % 2 == 0 { "\n\n" } else { "\r\n\r\n" });
    }
    out.push_str("data: [DONE]\n\n");
    out
}

/// Tiny deterministic generator so the split points are reproducible without a dependency.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound.max(1)
    }
}

fn payloads_with_splits(raw: &[u8], seed: u64) -> Vec<String> {
    let mut rng = Lcg(seed);
    let mut parser = SseParser::new();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < raw.len() {
        let take = 1 + rng.next(24);
        let end = (pos + take).min(raw.len());
        out.extend(parser.feed(&raw[pos..end]));
        pos = end;
    }
    out
}

fn run(
    payloads: &[String],
    typed: bool,
) -> (Vec<Result<Vec<StreamEvent>, String>>, Option<String>) {
    let mut state = ChunkState::new();
    let mut model = Some("req".to_string());
    let mut out = Vec::new();
    for p in payloads {
        if p.trim() == "[DONE]" {
            break;
        }
        let r = if typed {
            payload_to_events(p, &mut state, &mut model)
        } else {
            value_path(p, &mut state, &mut model)
        };
        out.push(r.map_err(|e| e.to_string()));
    }
    (out, model)
}

#[test]
fn parser_payloads_are_independent_of_chunk_splits() {
    let raw = recorded();
    let whole = SseParser::new().feed(raw.as_bytes());
    assert_eq!(whole.len(), 14);
    for seed in 0..200 {
        assert_eq!(
            payloads_with_splits(raw.as_bytes(), seed),
            whole,
            "seed {seed}"
        );
    }
}

#[test]
fn typed_and_value_paths_agree_on_every_split() {
    let raw = recorded();
    let oracle = run(&SseParser::new().feed(raw.as_bytes()), false);
    for seed in 0..200 {
        let payloads = payloads_with_splits(raw.as_bytes(), seed);
        assert_eq!(run(&payloads, true), oracle, "seed {seed}");
    }
    assert_eq!(oracle.1.as_deref(), Some("m-2"));
}

#[test]
fn error_object_inside_a_chunk_is_still_classified() {
    let payload = r#"{"error":{"message":"slow down","code":429}}"#;
    let mut state = ChunkState::new();
    let mut model = None;
    match payload_to_events(payload, &mut state, &mut model) {
        Err(ProviderError::RateLimited(msg)) => assert!(msg.contains("slow down")),
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[test]
fn shapes_the_typed_parse_rejects_fall_back_to_the_value_path() {
    // Float token counts and non-string content do not fit the typed shape.
    let payloads = vec![
        r#"{"choices":[],"usage":{"prompt_tokens":1.5,"completion_tokens":2}}"#.to_string(),
        r#"{"choices":[{"delta":{"content":["x"],"tool_calls":[{"index":0,"id":"c","function":{"name":"n","arguments":{"k":1}}}]}}]}"#
            .to_string(),
    ];
    assert_eq!(run(&payloads, true), run(&payloads, false));
}

#[test]
fn invalid_json_is_malformed() {
    let mut state = ChunkState::new();
    let mut model = None;
    match payload_to_events("{nope", &mut state, &mut model) {
        Err(ProviderError::Malformed(msg)) => assert!(msg.starts_with("invalid SSE JSON")),
        other => panic!("expected Malformed, got {other:?}"),
    }
}

#[test]
fn model_is_replaced_only_when_it_differs() {
    let mut state = ChunkState::new();
    let mut model = Some("m-1".to_string());
    payload_to_events(r#"{"model":"m-1","choices":[]}"#, &mut state, &mut model).expect("ok");
    assert_eq!(model.as_deref(), Some("m-1"));
    payload_to_events(r#"{"model":"m-9","choices":[]}"#, &mut state, &mut model).expect("ok");
    assert_eq!(model.as_deref(), Some("m-9"));
}

#[test]
fn large_event_in_tiny_chunks_matches_one_shot_feed() {
    let big = "x".repeat(200_000);
    let raw = format!("data: {big}\n\ndata: tail\n\n");
    let mut parser = SseParser::new();
    let mut out = Vec::new();
    for piece in raw.as_bytes().chunks(7) {
        out.extend(parser.feed(piece));
    }
    assert_eq!(out, vec![big, "tail".to_string()]);
}
