use std::time::Duration;

use futures::StreamExt;
use pacode_types::{StopReason, StreamEvent};
use serde_json::json;

use crate::CompletionRequest;
use crate::Provider;
use crate::mock::{MockProvider, MockResponse};

#[tokio::test]
async fn test_mock_provider_text() {
    let mock = MockProvider::new("mock");
    mock.push(MockResponse::Text("Hello, world!".to_string()));

    let req = CompletionRequest {
        model: "mock-model".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = mock.complete(req).await.expect("complete succeeds");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("stream event ok"));
    }

    // "Hello, world!" is 13 chars -> 8 chars ("Hello, w") + 5 chars ("orld!")
    assert_eq!(
        events[0],
        StreamEvent::TextDelta {
            text: "Hello, w".to_string()
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::TextDelta {
            text: "orld!".to_string()
        }
    );
    // Usage before MessageEnd
    assert!(matches!(events[2], StreamEvent::Usage(_)));
    assert_eq!(
        events[3],
        StreamEvent::MessageEnd {
            stop: StopReason::EndTurn
        }
    );

    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn test_mock_provider_tool_calls() {
    let mock = MockProvider::new("mock");
    mock.push(MockResponse::ToolCalls {
        text: Some("Calling tool".to_string()),
        calls: vec![("read_file".to_string(), json!({"path": "Cargo.toml"}))],
    });

    let req = CompletionRequest {
        model: "mock-model".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = mock.complete(req).await.expect("ok");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("ok"));
    }

    // Text "Calling tool" -> "Calling " (8) + "tool" (4)
    assert_eq!(
        events[0],
        StreamEvent::TextDelta {
            text: "Calling ".to_string()
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::TextDelta {
            text: "tool".to_string()
        }
    );

    // ToolCallStart
    match &events[2] {
        StreamEvent::ToolCallStart { index, name, .. } => {
            assert_eq!(*index, 0);
            assert_eq!(name, "read_file");
        }
        other => panic!("expected ToolCallStart, got {other:?}"),
    }

    // ToolCallArgsDelta
    match &events[3] {
        StreamEvent::ToolCallArgsDelta { index, delta } => {
            assert_eq!(*index, 0);
            assert_eq!(delta, &json!({"path": "Cargo.toml"}).to_string());
        }
        other => panic!("expected ToolCallArgsDelta, got {other:?}"),
    }

    // Usage before MessageEnd
    assert!(matches!(events[4], StreamEvent::Usage(_)));

    // MessageEnd ToolUse
    assert_eq!(
        events[5],
        StreamEvent::MessageEnd {
            stop: StopReason::ToolUse
        }
    );
}

#[tokio::test]
async fn test_mock_provider_reasoning_then_text() {
    let mock = MockProvider::new("mock");
    mock.push(MockResponse::ReasoningThenText {
        reasoning: "Thinking...".to_string(),
        text: "Answer".to_string(),
    });

    let req = CompletionRequest {
        model: "mock-model".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let mut stream = mock.complete(req).await.expect("ok");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("ok"));
    }

    // Reasoning deltas
    assert_eq!(
        events[0],
        StreamEvent::ReasoningDelta {
            text: "Thinking".to_string()
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::ReasoningDelta {
            text: "...".to_string()
        }
    );

    // Text deltas
    assert_eq!(
        events[2],
        StreamEvent::TextDelta {
            text: "Answer".to_string()
        }
    );

    // Usage + MessageEnd
    assert!(matches!(events[3], StreamEvent::Usage(_)));
    assert_eq!(
        events[4],
        StreamEvent::MessageEnd {
            stop: StopReason::EndTurn
        }
    );
}

#[tokio::test]
async fn test_mock_provider_error() {
    let mock = MockProvider::new("mock");
    mock.push(MockResponse::Error("internal server error".to_string()));

    let req = CompletionRequest {
        model: "mock-model".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let err = match mock.complete(req).await {
        Err(e) => e,
        Ok(_) => panic!("should error"),
    };
    match err {
        crate::ProviderError::Http { status, message } => {
            assert_eq!(status, 500);
            assert_eq!(message, "internal server error");
        }
        other => panic!("expected Http 500 error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_mock_provider_slow() {
    let mock = MockProvider::new("mock");
    mock.push(MockResponse::Slow {
        text: "Part1Part2".to_string(),
        delay: Duration::from_millis(50),
    });

    let req = CompletionRequest {
        model: "mock-model".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let start = std::time::Instant::now();
    let mut stream = mock.complete(req).await.expect("ok");

    // First event available immediately
    let first = stream.next().await.expect("has first").expect("ok");
    assert_eq!(
        first,
        StreamEvent::TextDelta {
            text: "Part1".to_string()
        }
    );

    // Second event requires sleep(delay)
    let second = stream.next().await.expect("has second").expect("ok");
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(40),
        "should have delayed lazily"
    );
    assert_eq!(
        second,
        StreamEvent::TextDelta {
            text: "Part2".to_string()
        }
    );

    let third = stream.next().await.expect("has third").expect("ok");
    assert!(matches!(third, StreamEvent::Usage(_)));

    let fourth = stream.next().await.expect("has fourth").expect("ok");
    assert_eq!(
        fourth,
        StreamEvent::MessageEnd {
            stop: StopReason::EndTurn
        }
    );
}

#[tokio::test]
async fn test_mock_provider_no_scripted_response() {
    let mock = MockProvider::new("mock");
    let req = CompletionRequest {
        model: "mock-model".to_string(),
        system_static: "".to_string(),
        system_dynamic: "".to_string(),
        messages: vec![],
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    };

    let err = match mock.complete(req).await {
        Err(e) => e,
        Ok(_) => panic!("should error"),
    };
    match err {
        crate::ProviderError::Config(msg) => {
            assert!(msg.contains("no scripted response"));
        }
        other => panic!("expected Config error, got {other:?}"),
    }
}
