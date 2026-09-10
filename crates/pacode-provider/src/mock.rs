//! Scripted provider for core/daemon/TUI tests. Always compiled (small).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::{CallId, ModelInfo, StopReason, StreamEvent, Usage};
use serde_json::Value;

use crate::{CompletionRequest, EventStream, Provider, ProviderError};

#[cfg(test)]
#[path = "mock_tests.rs"]
mod mock_tests;

#[derive(Clone, Debug)]
pub enum MockResponse {
    /// Streams the text in ~8-char deltas, then `MessageEnd{EndTurn}`.
    Text(String),
    /// Optional text, then tool calls `(name, input)` with generated ids, `MessageEnd{ToolUse}`.
    ToolCalls {
        text: Option<String>,
        calls: Vec<(String, Value)>,
    },
    /// Reasoning deltas, then text.
    ReasoningThenText { reasoning: String, text: String },
    /// Fails the `complete` call.
    Error(String),
    /// Emits half the text, sleeps `delay`, then the rest (for interrupt tests).
    Slow { text: String, delay: Duration },
}

pub struct MockProvider {
    id: String,
    scripts: Mutex<VecDeque<MockResponse>>,
    requests: Mutex<Vec<CompletionRequest>>,
    usage: Usage,
    models: Mutex<Vec<ModelInfo>>,
    list_models_calls: std::sync::atomic::AtomicUsize,
    list_models_delay: Mutex<Option<Duration>>,
    list_models_error: Mutex<Option<String>>,
}

impl MockProvider {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            scripts: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..Usage::default()
            },
            models: Mutex::new(Vec::new()),
            list_models_calls: std::sync::atomic::AtomicUsize::new(0),
            list_models_delay: Mutex::new(None),
            list_models_error: Mutex::new(None),
        }
    }

    pub fn set_models(&self, models: Vec<ModelInfo>) {
        if let Ok(mut guard) = self.models.lock() {
            *guard = models;
        }
    }

    pub fn set_list_models_delay(&self, delay: Duration) {
        if let Ok(mut guard) = self.list_models_delay.lock() {
            *guard = Some(delay);
        }
    }

    pub fn set_list_models_error(&self, err: Option<String>) {
        if let Ok(mut guard) = self.list_models_error.lock() {
            *guard = err;
        }
    }

    pub fn list_models_count(&self) -> usize {
        self.list_models_calls
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn push(&self, response: MockResponse) {
        if let Ok(mut guard) = self.scripts.lock() {
            guard.push_back(response);
        }
    }

    /// Every request received so far (clones).
    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }

    pub fn pending(&self) -> usize {
        self.scripts.lock().map(|s| s.len()).unwrap_or(0)
    }
}

fn chunk_chars(s: &str, chunk_size: usize) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = s.chars().collect();
    chars
        .chunks(chunk_size)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

#[async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        self.requests
            .lock()
            .map_err(|_| ProviderError::Config("mock lock poisoned".to_string()))?
            .push(req);

        let script = self
            .scripts
            .lock()
            .map_err(|_| ProviderError::Config("mock lock poisoned".to_string()))?
            .pop_front()
            .ok_or_else(|| ProviderError::Config("mock: no scripted response".to_string()))?;

        match script {
            MockResponse::Text(text) => {
                let mut events = Vec::new();
                for chunk in chunk_chars(&text, 8) {
                    events.push(StreamEvent::TextDelta { text: chunk });
                }
                events.push(StreamEvent::Usage(self.usage));
                events.push(StreamEvent::MessageEnd {
                    stop: StopReason::EndTurn,
                });
                Ok(Box::pin(tokio_stream::iter(events.into_iter().map(Ok))))
            }
            MockResponse::ToolCalls { text, calls } => {
                let mut events = Vec::new();
                if let Some(t) = text {
                    for chunk in chunk_chars(&t, 8) {
                        events.push(StreamEvent::TextDelta { text: chunk });
                    }
                }
                for (i, (name, input)) in calls.into_iter().enumerate() {
                    let call_id = CallId::generate();
                    events.push(StreamEvent::ToolCallStart {
                        index: i as u32,
                        id: call_id,
                        name,
                    });
                    events.push(StreamEvent::ToolCallArgsDelta {
                        index: i as u32,
                        delta: input.to_string(),
                    });
                }
                events.push(StreamEvent::Usage(self.usage));
                events.push(StreamEvent::MessageEnd {
                    stop: StopReason::ToolUse,
                });
                Ok(Box::pin(tokio_stream::iter(events.into_iter().map(Ok))))
            }
            MockResponse::ReasoningThenText { reasoning, text } => {
                let mut events = Vec::new();
                for chunk in chunk_chars(&reasoning, 8) {
                    events.push(StreamEvent::ReasoningDelta { text: chunk });
                }
                for chunk in chunk_chars(&text, 8) {
                    events.push(StreamEvent::TextDelta { text: chunk });
                }
                events.push(StreamEvent::Usage(self.usage));
                events.push(StreamEvent::MessageEnd {
                    stop: StopReason::EndTurn,
                });
                Ok(Box::pin(tokio_stream::iter(events.into_iter().map(Ok))))
            }
            MockResponse::Error(msg) => Err(ProviderError::Http {
                status: 500,
                message: msg,
            }),
            MockResponse::Slow { text, delay } => {
                let chars: Vec<char> = text.chars().collect();
                let mid = chars.len() / 2;
                let first_half: String = chars[..mid].iter().collect();
                let second_half: String = chars[mid..].iter().collect();
                let first_chunks = chunk_chars(&first_half, 8);
                let second_chunks = chunk_chars(&second_half, 8);
                let usage = self.usage;

                enum SlowState {
                    First(VecDeque<String>),
                    Delay,
                    Second(VecDeque<String>),
                    Usage,
                    End,
                    Done,
                }

                let initial_state = SlowState::First(first_chunks.into());

                let stream = futures::stream::unfold(initial_state, move |mut state| {
                    let second_chunks = second_chunks.clone();
                    async move {
                        loop {
                            match state {
                                SlowState::First(ref mut chunks) => {
                                    if let Some(chunk) = chunks.pop_front() {
                                        return Some((
                                            Ok(StreamEvent::TextDelta { text: chunk }),
                                            state,
                                        ));
                                    }
                                    state = SlowState::Delay;
                                }
                                SlowState::Delay => {
                                    tokio::time::sleep(delay).await;
                                    state = SlowState::Second(second_chunks.clone().into());
                                }
                                SlowState::Second(ref mut chunks) => {
                                    if let Some(chunk) = chunks.pop_front() {
                                        return Some((
                                            Ok(StreamEvent::TextDelta { text: chunk }),
                                            state,
                                        ));
                                    }
                                    state = SlowState::Usage;
                                }
                                SlowState::Usage => {
                                    state = SlowState::End;
                                    return Some((Ok(StreamEvent::Usage(usage)), state));
                                }
                                SlowState::End => {
                                    state = SlowState::Done;
                                    return Some((
                                        Ok(StreamEvent::MessageEnd {
                                            stop: StopReason::EndTurn,
                                        }),
                                        state,
                                    ));
                                }
                                SlowState::Done => {
                                    return None;
                                }
                            }
                        }
                    }
                });

                Ok(Box::pin(stream))
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.list_models_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let delay = self.list_models_delay.lock().ok().and_then(|g| *g);
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        if let Some(err) = self.list_models_error.lock().ok().and_then(|g| g.clone()) {
            return Err(ProviderError::Transport(err));
        }
        let custom = self
            .models
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();
        if custom.is_empty() {
            Ok(vec![self.model_info("mock-model")])
        } else {
            Ok(custom)
        }
    }

    fn model_info(&self, model: &str) -> ModelInfo {
        ModelInfo {
            route: pacode_types::ModelRoute::new(self.id.clone(), model),
            display_name: model.to_string(),
            context_window: Some(32_000),
            supports_reasoning: true,
            pricing: None,
        }
    }
}
