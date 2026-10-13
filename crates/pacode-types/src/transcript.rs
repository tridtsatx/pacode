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
    /// A question the model asked, with the answer once it has one.
    Question {
        question: crate::question::Question,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<crate::question::QuestionAnswer>,
    },
    BashCommand {
        command: String,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        truncated: bool,
    },
    /// One leg of the orchestrator <-> subagent exchange (`agent send`,
    /// `ask_parent`). The same exchange lands in both agents' transcripts,
    /// each under its own seq; a question's item is updated with its reply.
    AgentMessage {
        /// The subagent on the other end, in both transcripts.
        subagent: AgentId,
        subagent_name: String,
        direction: AgentMessageDirection,
        /// The message or the question, capped at [`AGENT_MESSAGE_CAP_CHARS`].
        text: String,
        /// How a question ended; `None` while it is open and for every send.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<AgentReply>,
    },
}

/// Cap of one orchestrator <-> subagent message or question (spec §6.4): what
/// one side can put into the other's context in a single exchange.
pub const AGENT_MESSAGE_CAP_CHARS: usize = 8_000;

/// Which way an [`TranscriptKind::AgentMessage`] went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageDirection {
    /// `agent send`: the orchestrator wrote to the subagent.
    ToSubagent,
    /// `ask_parent`: the subagent asked the orchestrator and waits for it.
    Question,
}

/// How a subagent's question ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentReply {
    /// The orchestrator answered with `agent send`; `text` is capped.
    Answered { text: String },
    /// The orchestrator ended a turn without answering; the subagent goes on
    /// with its own judgment.
    Unanswered,
    /// The orchestrator's turn failed with the question in it (or the
    /// question could not be delivered); the subagent goes on with its own
    /// judgment.
    Failed,
    /// The subagent was stopped (or its turn interrupted) while it waited.
    Cancelled,
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

    /// `Permission`/`Question` carry their own `agent` which used to collide
    /// with the item's on flatten — serde rejected the duplicate key and the
    /// client dropped the whole `item_added` event. The inner field now
    /// serializes as `asking_agent`.
    #[test]
    fn nested_agent_does_not_duplicate_on_flatten() {
        let perm = TranscriptItem {
            seq: 5,
            agent: AgentId::main(),
            ts_ms: 3,
            kind: TranscriptKind::Permission(PermissionRequest {
                id: crate::ids::PermissionId::new("perm_1"),
                agent: AgentId::new("sub-1"),
                agent_name: "sub".into(),
                call_id: CallId::new("call_1"),
                tool: "bash".into(),
                title: "Bash: ls".into(),
                detail: String::new(),
                risk: None,
                created_at_ms: 1,
            }),
        };
        let json = serde_json::to_string(&perm).unwrap();
        assert_eq!(json.matches("\"agent\":").count(), 1, "{json}");
        assert_eq!(json.matches("\"asking_agent\":").count(), 1, "{json}");
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, perm);

        let question = TranscriptItem {
            seq: 6,
            agent: AgentId::main(),
            ts_ms: 4,
            kind: TranscriptKind::Question {
                question: crate::question::Question {
                    id: crate::ids::QuestionId::new("q_1"),
                    agent: AgentId::new("sub-1"),
                    agent_name: "sub".into(),
                    call_id: CallId::new("call_2"),
                    header: "h".into(),
                    question: "which?".into(),
                    options: Vec::new(),
                    multi_select: false,
                    created_at_ms: 2,
                },
                answer: None,
            },
        };
        let json = serde_json::to_string(&question).unwrap();
        assert_eq!(json.matches("\"agent\":").count(), 1, "{json}");
        let back: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, question);
    }

    /// The exchange item flattens into the item without an `agent` key clash
    /// and carries its reply as a nested, tagged object.
    #[test]
    fn agent_message_kind_tag() {
        let item = TranscriptItem {
            seq: 7,
            agent: AgentId::main(),
            ts_ms: 5,
            kind: TranscriptKind::AgentMessage {
                subagent: AgentId::new("agt_1"),
                subagent_name: "tests".into(),
                direction: AgentMessageDirection::Question,
                text: "which db?".into(),
                reply: Some(AgentReply::Answered {
                    text: "postgres".into(),
                }),
            },
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["kind"], "agent_message");
        assert_eq!(json["agent"], "main");
        assert_eq!(json["direction"], "question");
        assert_eq!(json["reply"]["outcome"], "answered");
        let back: TranscriptItem = serde_json::from_value(json).unwrap();
        assert_eq!(back, item);

        let open = serde_json::json!({
            "seq": 8, "agent": "agt_1", "ts_ms": 6, "kind": "agent_message",
            "subagent": "agt_1", "subagent_name": "tests",
            "direction": "to_subagent", "text": "go on"
        });
        let back: TranscriptItem = serde_json::from_value(open).unwrap();
        assert!(matches!(
            back.kind,
            TranscriptKind::AgentMessage { reply: None, .. }
        ));
    }

    #[test]
    fn bash_command_kind_tag() {
        let item = TranscriptItem {
            seq: 4,
            agent: AgentId::main(),
            ts_ms: 2,
            kind: TranscriptKind::BashCommand {
                command: "cargo test".into(),
                output: "ok\n".into(),
                exit_code: Some(0),
                truncated: false,
            },
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["kind"], "bash_command");
        assert_eq!(json["command"], "cargo test");
        assert_eq!(json["exit_code"], 0);
        let back: TranscriptItem = serde_json::from_value(json).unwrap();
        assert_eq!(back, item);
    }
}
