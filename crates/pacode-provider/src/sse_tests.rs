use pacode_types::{StopReason, StreamEvent, Usage};
use serde_json::json;

use crate::sse::{ChunkState, SseParser, chunk_to_events};

#[test]
fn test_sse_parser_crlf_and_single_line() {
    let mut parser = SseParser::new();
    let data = b"data: hello world\r\n\r\n";
    let events = parser.feed(data);
    assert_eq!(events, vec!["hello world".to_string()]);
}

#[test]
fn test_sse_parser_split_across_chunk_boundaries() {
    let mut parser = SseParser::new();

    // Chunk 1: partial line
    let ev1 = parser.feed(b"dat");
    assert!(ev1.is_empty());

    // Chunk 2: rest of field and partial payload
    let ev2 = parser.feed(b"a: hel");
    assert!(ev2.is_empty());

    // Chunk 3: rest of payload and partial delimiter
    let ev3 = parser.feed(b"lo\n");
    assert!(ev3.is_empty());

    // Chunk 4: final newline completing the delimiter
    let ev4 = parser.feed(b"\n");
    assert_eq!(ev4, vec!["hello".to_string()]);

    // Chunk 5: CRLF split across chunks
    let ev5 = parser.feed(b"data: world\r\n\r");
    assert!(ev5.is_empty());

    let ev6 = parser.feed(b"\n");
    assert_eq!(ev6, vec!["world".to_string()]);
}

#[test]
fn test_sse_parser_comments_and_ignored_fields() {
    let mut parser = SseParser::new();
    let payload = b": this is a comment\r\nevent: message\r\nid: 12345\r\nretry: 3000\r\ndata: actual data\r\n\r\n";
    let events = parser.feed(payload);
    assert_eq!(events, vec!["actual data".to_string()]);
}

#[test]
fn test_sse_parser_multi_line_data() {
    let mut parser = SseParser::new();
    let payload = b"data: first line\ndata: second line\ndata: third line\n\n";
    let events = parser.feed(payload);
    assert_eq!(
        events,
        vec!["first line\nsecond line\nthird line".to_string()]
    );
}

#[test]
fn test_chunk_to_events_openai_tool_calls_across_chunks() {
    let mut state = ChunkState::default();

    // Chunk 1: Tool call start with name, empty arguments
    let chunk1 = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "index": 0,
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "edit_file",
                        "arguments": ""
                    }
                }]
            }
        }]
    });
    let ev1 = chunk_to_events(&chunk1, &mut state).expect("ok");
    assert_eq!(ev1.len(), 1);
    match &ev1[0] {
        StreamEvent::ToolCallStart { index, id, name } => {
            assert_eq!(*index, 0);
            assert_eq!(id.as_str(), "call_123");
            assert_eq!(name, "edit_file");
        }
        other => panic!("expected ToolCallStart, got {other:?}"),
    }

    // Chunk 2: First arguments delta
    let chunk2 = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": {
                        "arguments": "{\"path\": \""
                    }
                }]
            }
        }]
    });
    let ev2 = chunk_to_events(&chunk2, &mut state).expect("ok");
    assert_eq!(ev2.len(), 1);
    match &ev2[0] {
        StreamEvent::ToolCallArgsDelta { index, delta } => {
            assert_eq!(*index, 0);
            assert_eq!(delta, "{\"path\": \"");
        }
        other => panic!("expected ToolCallArgsDelta, got {other:?}"),
    }

    // Chunk 3: Second arguments delta + finish_reason
    let chunk3 = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": {
                        "arguments": "src/lib.rs\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    let ev3 = chunk_to_events(&chunk3, &mut state).expect("ok");
    assert_eq!(ev3.len(), 2);
    match &ev3[0] {
        StreamEvent::ToolCallArgsDelta { index, delta } => {
            assert_eq!(*index, 0);
            assert_eq!(delta, "src/lib.rs\"}");
        }
        other => panic!("expected ToolCallArgsDelta, got {other:?}"),
    }
    match &ev3[1] {
        StreamEvent::MessageEnd { stop } => {
            assert_eq!(*stop, StopReason::ToolUse);
        }
        other => panic!("expected MessageEnd, got {other:?}"),
    }
    assert!(state.any_tool_call());
    assert!(state.message_end_emitted());
}

#[test]
fn test_chunk_to_events_deepseek_reasoning_content() {
    let mut state = ChunkState::default();

    let chunk = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "reasoning_content": "DeepSeek is analyzing the code..."
            }
        }]
    });

    let events = chunk_to_events(&chunk, &mut state).expect("ok");
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::ReasoningDelta { text } => {
            assert_eq!(text, "DeepSeek is analyzing the code...");
        }
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn test_chunk_to_events_gemini_openai_compat() {
    let mut state = ChunkState::default();

    // Gemini OpenAI compat: reasoning in "reasoning", tool_calls without explicit index
    let chunk = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "reasoning": "Gemini thoughts here",
                "tool_calls": [{
                    "id": "gemini_call_1",
                    "function": {
                        "name": "search_code",
                        "arguments": "{\"query\": \"fn main\"}"
                    }
                }]
            }
        }]
    });

    let events = chunk_to_events(&chunk, &mut state).expect("ok");
    assert_eq!(events.len(), 3);
    match &events[0] {
        StreamEvent::ReasoningDelta { text } => {
            assert_eq!(text, "Gemini thoughts here");
        }
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
    match &events[1] {
        StreamEvent::ToolCallStart { index, id, name } => {
            assert_eq!(*index, 0);
            assert_eq!(id.as_str(), "gemini_call_1");
            assert_eq!(name, "search_code");
        }
        other => panic!("expected ToolCallStart, got {other:?}"),
    }
    match &events[2] {
        StreamEvent::ToolCallArgsDelta { index, delta } => {
            assert_eq!(*index, 0);
            assert_eq!(delta, "{\"query\": \"fn main\"}");
        }
        other => panic!("expected ToolCallArgsDelta, got {other:?}"),
    }
}

#[test]
fn test_chunk_to_events_usage_with_cached_tokens() {
    let mut state = ChunkState::default();

    let chunk = json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 150,
            "completion_tokens": 42,
            "completion_tokens_details": {
                "reasoning_tokens": 18
            },
            "prompt_tokens_details": {
                "cached_tokens": 80
            }
        }
    });

    let events = chunk_to_events(&chunk, &mut state).expect("ok");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Usage(Usage {
            input_tokens: 150,
            output_tokens: 42,
            reasoning_tokens: 18,
            cache_read_tokens: 80,
            cache_write_tokens: 0,
        })
    );
}

#[test]
fn test_chunk_to_events_finish_reason_mapping() {
    let test_cases = vec![
        ("stop", StopReason::EndTurn),
        ("tool_calls", StopReason::ToolUse),
        ("length", StopReason::MaxTokens),
        ("content_filter", StopReason::ContentFilter),
        (
            "custom_reason",
            StopReason::Other("custom_reason".to_string()),
        ),
    ];

    for (reason, expected_stop) in test_cases {
        let mut state = ChunkState::default();
        let chunk = json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": reason
            }]
        });

        let events = chunk_to_events(&chunk, &mut state).expect("ok");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::MessageEnd {
                stop: expected_stop
            }
        );
        assert!(state.message_end_emitted());
    }
}

#[test]
fn test_chunk_to_events_error_payload() {
    let mut state = ChunkState::default();
    let chunk = json!({
        "error": {
            "message": "Quota exceeded",
            "type": "insufficient_quota"
        }
    });

    let err = chunk_to_events(&chunk, &mut state).expect_err("should fail");
    match err {
        crate::ProviderError::Malformed(msg) => {
            assert_eq!(msg, "Quota exceeded");
        }
        other => panic!("expected Malformed error, got {other:?}"),
    }
}
