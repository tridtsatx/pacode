//! Client-facing transcript items. The daemon converts model history into these;
//! the TUI never sees provider message formats.

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, CallId, TaskId};
use crate::state::{PermissionRequest, ToastLevel};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Running,
    Ok,
    Error,
    /// Command moved to the background; `task` on the item points at it.
    Backgrounded,
    Denied,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptKind {
    User {
        text: String,
    },
    Assistant {
        text: String,
        complete: bool,
    },
    Reasoning {
        text: String,
        complete: bool,
    },
    ToolCall {
        call_id: CallId,
        name: String,
        /// `Bash cargo build --release`, `Edit src/tui/sidebar.rs`.
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        status: ToolStatus,
        /// Output tail for the transcript row (bounded by the daemon).
        preview: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<DiffStat>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
    },
    Notice {
        level: ToastLevel,
        text: String,
    },
    Permission(PermissionRequest),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptItem {
    /// Monotonic per agent; the daemon assigns it and clients key deltas on it.
    pub seq: u64,
    pub agent: AgentId,
    pub ts_ms: u64,
    #[serde(flatten)]
    pub kind: TranscriptKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattened_kind_tag() {
        let item = TranscriptItem {
            seq: 3,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::User { text: "hi".into() },
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["kind"], "user");
        assert_eq!(json["text"], "hi");
        assert_eq!(json["seq"], 3);
        let back: TranscriptItem = serde_json::from_value(json).unwrap();
        assert_eq!(back, item);
    }
}
