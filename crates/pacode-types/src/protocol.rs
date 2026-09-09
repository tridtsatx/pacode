//! Client ↔ daemon wire protocol. NDJSON: one JSON object per line.
//!
//! Client → daemon: [`Envelope`] (`id` + [`Request`]).
//! Daemon → client: [`ServerMessage`]: a [`Reply`] to an `id`, or an [`Event`] with a
//! per-session monotonic `seq`.

use std::path::PathBuf;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, ClientId, PermissionId, SessionId, TaskId, TurnId};
use crate::model::{Effort, ModelInfo, ModelRoute};
use crate::state::{
    AgentInfo, Mode, PermissionDecision, PermissionRequest, Plan, SessionMeta, TaskInfo,
    ToastLevel, UsageTotals,
};
use crate::stream::Usage;
use crate::transcript::TranscriptItem;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientHello {
    pub client_id: ClientId,
    pub app_version: String,
    pub protocol: u32,
}

/// How a connection binds to a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attach {
    New {
        cwd: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<ModelRoute>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<Effort>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<Mode>,
    },
    Resume {
        session: SessionId,
    },
    /// Most recently updated session for this cwd, else a new one.
    Latest {
        cwd: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello(ClientHello),
    Attach(Attach),
    Detach,
    /// Starts a turn when idle; queued as a steer injection while a turn runs.
    UserMessage {
        text: String,
    },
    /// Cancel the running turn (history is kept).
    Interrupt,
    PermissionReply {
        permission: PermissionId,
        decision: PermissionDecision,
    },
    SetModel(ModelRoute),
    SetEffort(Effort),
    SetMode(Mode),
    StopAgent(AgentId),
    KillTask(TaskId),
    /// Mark a failed task as seen (clears the red counter).
    AckTask(TaskId),
    GetSnapshot,
    GetHistory {
        agent: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_seq: Option<u64>,
        limit: u32,
    },
    GetTaskOutput {
        task: TaskId,
        tail_lines: u32,
    },
    ListSessions {
        limit: u32,
    },
    ListModels,
    Compact,
    /// Ask the daemon to exit. Without `force` it exits only when idle.
    Shutdown {
        force: bool,
    },
    Ping,
    /// MCP servers with their status (`Reply::McpServers`).
    ListMcpServers,
    /// Restart one MCP server.
    RestartMcpServer {
        server: String,
    },
    /// Enable/disable one MCP server for the daemon lifetime.
    SetMcpServerEnabled {
        server: String,
        enabled: bool,
    },
    /// Render an MCP prompt (`Reply::McpPrompt`).
    GetMcpPrompt {
        server: String,
        name: String,
        #[serde(default)]
        args: BTreeMap<String, String>,
    },
    /// Loaded plugins (`Reply::Plugins`).
    ListPlugins,
    /// Run a plugin slash command (`Reply::PluginCommand`).
    RunPluginCommand {
        name: String,
        args: String,
    },
}

/// One MCP server as shown in the `/mcp` picker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    /// `stopped` | `starting` | `ready` | `failed` | `disabled`
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub tools: u32,
    pub resources: u32,
    pub prompts: u32,
    /// Prompt names exposed as `/mcp:<server>:<prompt>`.
    #[serde(default)]
    pub prompt_names: Vec<String>,
}

/// One loaded plugin as shown in the `/plugins` picker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    /// `lua` | `wasm`
    pub kind: String,
    pub tools: Vec<String>,
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of a plugin slash command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginCommandOutcome {
    InsertText { text: String },
    SendPrompt { text: String },
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub req: Request,
}

/// Everything a client needs to render a session from scratch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub meta: SessionMeta,
    pub agents: Vec<AgentInfo>,
    pub plan: Plan,
    pub tasks: Vec<TaskInfo>,
    pub usage: UsageTotals,
    /// Tail of the main agent's transcript.
    pub transcript: Vec<TranscriptItem>,
    pub has_more_history: bool,
    pub pending_permissions: Vec<PermissionRequest>,
    pub turn_active: bool,
    /// Last event seq included in this snapshot; later events carry larger seqs.
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Ok,
    Error {
        message: String,
    },
    Hello {
        daemon_version: String,
        protocol: u32,
        pid: u32,
    },
    Attached(SessionSnapshot),
    Snapshot(SessionSnapshot),
    History {
        agent: AgentId,
        items: Vec<TranscriptItem>,
        has_more: bool,
    },
    TaskOutput {
        task: TaskId,
        lines: Vec<String>,
        total_lines: u64,
    },
    Sessions {
        sessions: Vec<SessionMeta>,
    },
    Models {
        models: Vec<ModelInfo>,
    },
    Pong,
    McpServers {
        servers: Vec<McpServerInfo>,
    },
    McpPrompt {
        text: String,
    },
    Plugins {
        plugins: Vec<PluginInfo>,
    },
    PluginCommand(PluginCommandOutcome),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnStop {
    Completed,
    Interrupted,
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    SessionUpdated(SessionMeta),
    TurnStarted {
        agent: AgentId,
        turn: TurnId,
    },
    TurnEnded {
        agent: AgentId,
        turn: TurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        stop: TurnStop,
    },
    ItemAdded(TranscriptItem),
    /// Same `seq` as an earlier `ItemAdded`; replaces it wholesale.
    ItemUpdated(TranscriptItem),
    /// Coalesced assistant text for the item `item_seq` of `agent`.
    TextDelta {
        agent: AgentId,
        item_seq: u64,
        text: String,
    },
    ReasoningDelta {
        agent: AgentId,
        item_seq: u64,
        text: String,
    },
    PermissionRequested(PermissionRequest),
    PermissionResolved {
        permission: PermissionId,
        decision: PermissionDecision,
    },
    PlanUpdated(Plan),
    AgentAdded(AgentInfo),
    AgentUpdated(AgentInfo),
    TaskAdded(TaskInfo),
    TaskUpdated(TaskInfo),
    UsageUpdated(UsageTotals),
    Toast {
        level: ToastLevel,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    DaemonShuttingDown,
    /// Plugin asked for a toast (`pacode.toast`).
    PluginToast {
        plugin: String,
        text: String,
    },
    /// Plugin status text shown in the footer (`pacode.status`); empty clears it.
    PluginStatus {
        plugin: String,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum ServerMessage {
    Reply { id: u64, reply: Reply },
    Event { seq: u64, event: Event },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_flattens_request() {
        let env = Envelope {
            id: 7,
            req: Request::UserMessage { text: "hi".into() },
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["id"], 7);
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["text"], "hi");
        let back: Envelope = serde_json::from_value(json).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn server_message_round_trip() {
        let msg = ServerMessage::Event {
            seq: 1,
            event: Event::Toast {
                level: ToastLevel::Success,
                title: "cargo build".into(),
                detail: Some("3m02s".into()),
            },
        };
        let line = serde_json::to_string(&msg).unwrap();
        assert!(line.contains("\"channel\":\"event\""));
        let back: ServerMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(back, msg);
        let reply = ServerMessage::Reply {
            id: 1,
            reply: Reply::Sessions {
                sessions: Vec::new(),
            },
        };
        let back: ServerMessage =
            serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }
}
