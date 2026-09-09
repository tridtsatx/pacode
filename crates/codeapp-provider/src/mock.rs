//! Scripted provider for core/daemon/TUI tests. Always compiled (small).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use codeapp_types::{ModelInfo, Usage};
use serde_json::Value;

use crate::{CompletionRequest, EventStream, Provider, ProviderError};

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
        }
    }

    pub fn push(&self, response: MockResponse) {
        self.scripts.lock().expect("mock lock").push_back(response);
    }

    /// Every request received so far (clones).
    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().expect("mock lock").clone()
    }

    pub fn pending(&self) -> usize {
        self.scripts.lock().expect("mock lock").len()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        let _ = req;
        let _ = &self.usage;
        todo!("MockProvider::complete")
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![self.model_info("mock-model")])
    }

    fn model_info(&self, model: &str) -> ModelInfo {
        ModelInfo {
            route: codeapp_types::ModelRoute::new(self.id.clone(), model),
            display_name: model.to_string(),
            context_window: Some(32_000),
            supports_reasoning: true,
            pricing: None,
        }
    }
}
