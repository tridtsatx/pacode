//! Events a provider yields while streaming one assistant message.

use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

use crate::ids::CallId;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ReasoningSignature {
        signature: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    /// A tool call begins. `index` orders parallel calls inside one message.
    ToolCallStart {
        index: u32,
        id: CallId,
        name: String,
    },
    /// Raw JSON argument fragment for the call at `index`; concatenate in order.
    ToolCallArgsDelta {
        index: u32,
        delta: String,
    },
    Usage(Usage),
    MessageEnd {
        stop: StopReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    ContentFilter,
    Other(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn is_empty(&self) -> bool {
        *self == Usage::default()
    }
}

impl Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            reasoning_tokens: self.reasoning_tokens + rhs.reasoning_tokens,
            cache_read_tokens: self.cache_read_tokens + rhs.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens + rhs.cache_write_tokens,
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Usage) {
        *self = *self + rhs;
    }
}
