//! Client ↔ daemon wire protocol. NDJSON: one JSON object per line.
//!
//! Client → daemon: [`Envelope`] (`id` + [`Request`]).
//! Daemon → client: [`ServerMessage`]: a [`Reply`] to an `id`, or an [`Event`] with a
//! per-session monotonic `seq`.

use std::path::PathBuf;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{
    AgentId, ClientId, CronJobId, MonitorId, PermissionId, SessionId, TaskId, TurnId,
};
use crate::model::{Effort, ModelInfo, ModelRoute};
use crate::schedule::{CronJob, CronSchedule, MonitorInfo};
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
    /// Detach a running turn on the main agent into a background subagent.
    DetachTurn,
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
    /// Cron jobs of the attached session (`Reply::CronJobs`).
    ListCronJobs,
    /// Register a scheduled prompt (`Reply::CronJobs` with the new job appended).
    AddCronJob {
        name: String,
        schedule: CronSchedule,
        prompt: String,
    },
    RemoveCronJob(CronJobId),
    SetCronEnabled {
        id: CronJobId,
        enabled: bool,
    },
    /// Fire a job outside its schedule (`Reply::Ok`).
    RunCronJobNow(CronJobId),
    /// Monitors of the attached session (`Reply::Monitors`).
    ListMonitors,
    StopMonitor(MonitorId),
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
    /// Cron jobs of this session, with `next_run_ms` already computed.
    #[serde(default)]
    pub cron_jobs: Vec<CronJob>,
    /// Live and terminal monitors of this session (in memory only).
    #[serde(default)]
    pub monitors: Vec<MonitorInfo>,
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
    TurnDetached {
        agent: AgentId,
    },
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
    /// Answer to `ListCronJobs` and to every mutating cron request.
    CronJobs {
        jobs: Vec<CronJob>,
    },
    /// Answer to `ListMonitors` and to `StopMonitor`.
    Monitors {
        monitors: Vec<MonitorInfo>,
    },
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
    /// A cron job was added, edited, enabled/disabled or fired.
    CronUpdated(CronJob),
    CronRemoved(CronJobId),
    /// A monitor started, checked, fired or stopped.
    MonitorUpdated(MonitorInfo),
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

    #[test]
    fn detach_turn_protocol_round_trip() {
        let env = Envelope {
            id: 42,
            req: Request::DetachTurn,
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"detach_turn\""));
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);

        let sub_id = AgentId::new("agt_test123");
        let reply_msg = ServerMessage::Reply {
            id: 42,
            reply: Reply::TurnDetached {
                agent: sub_id.clone(),
            },
        };
        let line = serde_json::to_string(&reply_msg).unwrap();
        assert!(line.contains("\"type\":\"turn_detached\""));
        assert!(line.contains("\"agent\":\"agt_test123\""));
        let back_reply: ServerMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(back_reply, reply_msg);
    }
}
