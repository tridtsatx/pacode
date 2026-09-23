//! Client ↔ daemon wire protocol. NDJSON: one JSON object per line.
//!
//! Client → daemon: [`Envelope`] (`id` + [`Request`]).
//! Daemon → client: [`ServerMessage`]: a [`Reply`] to an `id`, or an [`Event`] with a
//! per-session monotonic `seq`.

use std::path::PathBuf;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{
    AgentId, ClientId, CronJobId, MonitorId, PermissionId, QuestionId, SessionId, TaskId, TurnId,
};
use crate::message::ImageSource;
use crate::model::{Effort, ModelFlags, ModelInfo, ModelRoute};
use crate::question::{Question, QuestionAnswer};
use crate::schedule::{CronJob, CronSchedule, MonitorInfo};
use crate::state::{
    AgentInfo, Mode, PermissionDecision, PermissionRequest, Plan, SessionMeta, TaskInfo,
    ToastLevel, UsageTotals,
};
use crate::stream::Usage;
use crate::transcript::TranscriptItem;
use crate::vcs::{CommitRow, GitStatus};

/// Bumped on any wire-visible change: v2 added `SetZdr`/`SetModelFlag`,
/// `SessionMeta.zdr` and `ModelInfo.supported_efforts`; v3 added the git
/// requests (`GitLog`, `GitShow`, `GitDiff`) and their replies; v4 adds
/// `RefreshModels` + the `ModelsUpdated` broadcast; v5 adds
/// `ExplainPermission` + the `PermissionExplained` broadcast.
pub const PROTOCOL_VERSION: u32 = 5;

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
        /// Images attached to the prompt (`[Image N]` paste chips, `@`-refs);
        /// appended to the user message as `ContentBlock::Image` blocks.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageSource>,
    },
    /// Cancel the running turn (history is kept).
    Interrupt,
    /// Cancel every running agent in the session (main + subagents).
    InterruptAll,
    PermissionReply {
        permission: PermissionId,
        decision: PermissionDecision,
        /// Optional note attached to a `Deny` (e.g. why the call was refused);
        /// reaches the model and the transcript verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Ask the daemon to explain a pending permission: a detached one-shot
    /// model call describes the gated tool call and rates its danger. The
    /// reply is `Reply::Ok` once the task is accepted (or `Reply::Error` when
    /// the id is unknown/already resolved); the result arrives later as
    /// `Event::PermissionExplained` and never changes the decision itself.
    ExplainPermission {
        permission: PermissionId,
    },
    SetModel(ModelRoute),
    SetEffort(Effort),
    SetMode(Mode),
    /// Toggle zero-data-retention for the session (`zdr = "session"` config).
    SetZdr(bool),
    /// Update user flags for one model route (`provider/model` key); the session
    /// keeps them in memory, the TUI persists them to `prefs.toml`.
    SetModelFlag {
        model: String,
        flags: ModelFlags,
    },
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
    /// Force a re-fetch of every provider's model catalog, bypassing the cache
    /// TTL (`Reply::Models`; an `Event::ModelsUpdated` broadcast follows).
    RefreshModels,
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
    /// Plugins a marketplace offers (`Reply::MarketplacePlugins`).
    BrowseMarketplace {
        /// `owner/repo`, `owner/repo@ref`, or an https URL to a marketplace.json.
        source: String,
        /// Free-text filter over name, description, category and keywords.
        #[serde(default)]
        query: String,
    },
    /// Install one plugin from a marketplace (`Reply::MarketplacePlugins`).
    InstallPlugin {
        source: String,
        name: String,
    },
    /// Remove a plugin pacode installed (`Reply::MarketplacePlugins`).
    UninstallPlugin {
        name: String,
    },
    /// Answer a question the model asked (`Event::QuestionAsked`).
    AnswerQuestion {
        question: QuestionId,
        answer: QuestionAnswer,
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
    /// Providers with their credential status (`Reply::AuthStatus`).
    ListAuth,
    /// Start a login flow for one provider. Progress arrives as `Event::LoginProgress`,
    /// the immediate reply is `Reply::Ok`.
    Login {
        provider: String,
    },
    /// Drop stored credentials (`Reply::AuthStatus`). Without `label`, every account
    /// of the provider is removed.
    Logout {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Pick which stored account a provider uses (`Reply::AuthStatus`).
    SetAuthAccount {
        provider: String,
        label: String,
    },
    /// One page of `git log` for the session's cwd (`Reply::GitLog`).
    /// `after` is the hash of the last commit the client already holds.
    GitLog {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    /// `git show --stat` of one commit for the detail pane (`Reply::GitShow`).
    GitShow {
        hash: String,
    },
    /// Patch of one commit for the diff panel (`Reply::GitDiff`).
    GitDiff {
        hash: String,
    },
}

impl Request {
    /// Stable wire name (the serde `type` tag) for logs and error text —
    /// exhaustive so a new variant cannot ship without one.
    pub fn name(&self) -> &'static str {
        match self {
            Request::Hello(_) => "hello",
            Request::Attach(_) => "attach",
            Request::Detach => "detach",
            Request::UserMessage { .. } => "user_message",
            Request::Interrupt => "interrupt",
            Request::InterruptAll => "interrupt_all",
            Request::PermissionReply { .. } => "permission_reply",
            Request::ExplainPermission { .. } => "explain_permission",
            Request::SetModel(_) => "set_model",
            Request::SetEffort(_) => "set_effort",
            Request::SetMode(_) => "set_mode",
            Request::SetZdr(_) => "set_zdr",
            Request::SetModelFlag { .. } => "set_model_flag",
            Request::StopAgent(_) => "stop_agent",
            Request::KillTask(_) => "kill_task",
            Request::AckTask(_) => "ack_task",
            Request::GetSnapshot => "get_snapshot",
            Request::GetHistory { .. } => "get_history",
            Request::GetTaskOutput { .. } => "get_task_output",
            Request::ListSessions { .. } => "list_sessions",
            Request::ListModels => "list_models",
            Request::RefreshModels => "refresh_models",
            Request::Compact => "compact",
            Request::DetachTurn => "detach_turn",
            Request::Shutdown { .. } => "shutdown",
            Request::Ping => "ping",
            Request::ListMcpServers => "list_mcp_servers",
            Request::RestartMcpServer { .. } => "restart_mcp_server",
            Request::SetMcpServerEnabled { .. } => "set_mcp_server_enabled",
            Request::GetMcpPrompt { .. } => "get_mcp_prompt",
            Request::ListPlugins => "list_plugins",
            Request::RunPluginCommand { .. } => "run_plugin_command",
            Request::BrowseMarketplace { .. } => "browse_marketplace",
            Request::InstallPlugin { .. } => "install_plugin",
            Request::UninstallPlugin { .. } => "uninstall_plugin",
            Request::AnswerQuestion { .. } => "answer_question",
            Request::ListCronJobs => "list_cron_jobs",
            Request::AddCronJob { .. } => "add_cron_job",
            Request::RemoveCronJob(_) => "remove_cron_job",
            Request::SetCronEnabled { .. } => "set_cron_enabled",
            Request::RunCronJobNow(_) => "run_cron_job_now",
            Request::ListMonitors => "list_monitors",
            Request::StopMonitor(_) => "stop_monitor",
            Request::ListAuth => "list_auth",
            Request::Login { .. } => "login",
            Request::Logout { .. } => "logout",
            Request::SetAuthAccount { .. } => "set_auth_account",
            Request::GitLog { .. } => "git_log",
            Request::GitShow { .. } => "git_show",
            Request::GitDiff { .. } => "git_diff",
        }
    }
}

/// One provider row in the `/login` picker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderAuthInfo {
    pub id: String,
    pub display_name: String,
    /// `oauth` | `api_key` | `local`
    pub auth_kind: String,
    /// One line of help: what the provider needs.
    pub detail: String,
    pub recommended: bool,
    pub state: AuthState,
    /// Labels of stored accounts, and which one is active.
    #[serde(default)]
    pub accounts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthState {
    /// Usable right now.
    Configured,
    /// Credentials exist but are expired or rejected.
    NeedsAttention { reason: String },
    /// Nothing stored.
    NotConfigured,
}

/// Stages of a running login flow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoginStage {
    /// The browser was opened, or could not be and the user should follow the URL.
    OpenUrl {
        url: String,
        opened: bool,
    },
    /// Waiting for the loopback callback.
    Waiting,
    /// Trading the authorization code for tokens.
    Exchanging,
    Done {
        label: String,
    },
    Failed {
        message: String,
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

/// One plugin as the `/plugins` picker shows it: loaded into the runtime
/// (`plugin.toml`), installed from a marketplace (`.pacode-install.json` +
/// `.claude-plugin/plugin.json`), or both.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    /// `lua` | `wasm`; empty when the plugin is installed but not loaded.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True when the plugin is live in the runtime; `kind`/`tools`/`commands`
    /// only carry data then.
    #[serde(default)]
    pub loaded: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Marketplace the plugin was installed from (`owner/repo`); empty for one
    /// dropped into the plugins directory by hand.
    #[serde(default)]
    pub source: String,
    /// Install time in ms since the epoch; 0 when not marketplace-installed.
    #[serde(default)]
    pub installed_at_ms: u64,
    /// MCP server names the plugin manifest declares.
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// Skill directories under `<plugin>/skills/`.
    #[serde(default)]
    pub skill_dirs: u32,
    /// Manifest components pacode does not run (`hooks`, `agents`, `commands`).
    #[serde(default)]
    pub unsupported: Vec<String>,
}

impl PluginInfo {
    /// Whether a marketplace install record exists, so `x` can uninstall it.
    pub fn is_installed(&self) -> bool {
        !self.source.is_empty()
    }
}

/// One plugin as the marketplace screen shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplacePluginInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub category: String,
    /// Which marketplace it came from, in display form.
    #[serde(default)]
    pub marketplace: String,
    #[serde(default)]
    pub installed: bool,
    /// Version installed, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub update_available: bool,
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
    /// Questions the model is waiting on, so a reconnecting client can answer them.
    #[serde(default)]
    pub pending_questions: Vec<Question>,
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
    /// Answer to every marketplace request.
    MarketplacePlugins {
        plugins: Vec<MarketplacePluginInfo>,
        /// True when the listing came from a cached copy past its TTL because
        /// the fetch failed, so the client can say so.
        #[serde(default)]
        stale: bool,
    },
    /// Answer to `ListCronJobs` and to every mutating cron request.
    CronJobs {
        jobs: Vec<CronJob>,
    },
    /// Answer to `ListMonitors` and to `StopMonitor`.
    Monitors {
        monitors: Vec<MonitorInfo>,
    },
    AuthStatus {
        providers: Vec<ProviderAuthInfo>,
    },
    /// Answer to `Request::GitLog`. `status` reports a non-repo cwd or a
    /// missing git binary; both are normal outcomes, not errors.
    GitLog {
        commits: Vec<CommitRow>,
        has_more: bool,
        status: GitStatus,
    },
    /// Answer to `Request::GitShow`: the `git show --stat` block.
    GitShow {
        hash: String,
        stat: String,
    },
    /// Answer to `Request::GitDiff`: the commit's patch, size-capped.
    GitDiff {
        hash: String,
        patch: String,
        /// The patch exceeded the daemon's capture cap and was cut.
        truncated: bool,
    },
}

/// Danger rating a model assigns to a pending permission call. Display-only:
/// it colors the cell in the UI and is NEVER consulted by the permission
/// gate — the decision stays with the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainLevel {
    /// The model declined to rate, or rated the call harmless.
    None,
    Low,
    Medium,
    High,
    Catastrophic,
}

impl ExplainLevel {
    /// Parse the model's `LEVEL:` line value; anything unrecognized is `None`
    /// (the UI shows no badge rather than a wrong one).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "catastrophic" => Some(Self::Catastrophic),
            _ => None,
        }
    }
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
        /// Why a `Deny` landed when it was not a plain user refusal (policy
        /// reason, `zdr_blocked: …`, `interrupted`, or a client note).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The detached explanation requested by `Request::ExplainPermission`
    /// landed. `level` is display-only — it never feeds the permission gate;
    /// `None` means the model's rating was missing or unparseable. `text`
    /// carries the explanation, or an honest `explain failed: …`/`explain
    /// unavailable …` note so the client never waits forever.
    PermissionExplained {
        permission: PermissionId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<ExplainLevel>,
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
    /// The model asked the user something and its turn is waiting.
    QuestionAsked(Question),
    /// The question was answered or dismissed; clients drop their picker.
    QuestionResolved {
        question: QuestionId,
        answer: QuestionAnswer,
    },
    /// A cron job was added, edited, enabled/disabled or fired.
    CronUpdated(CronJob),
    CronRemoved(CronJobId),
    /// A monitor started, checked, fired or stopped.
    MonitorUpdated(MonitorInfo),
    /// One step of a running login flow.
    LoginProgress {
        provider: String,
        stage: LoginStage,
    },
    /// Credentials of a provider changed (login, logout, account switch).
    AuthUpdated(ProviderAuthInfo),
    /// The model catalog changed — a refresh landed. Carries the full list,
    /// so clients replace their copy wholesale like a `Reply::Models` fold.
    ModelsUpdated {
        models: Vec<ModelInfo>,
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
            req: Request::UserMessage {
                text: "hi".into(),
                images: Vec::new(),
            },
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

    #[test]
    fn user_message_with_images_round_trip() {
        let env = Envelope {
            id: 9,
            req: Request::UserMessage {
                text: "what is this".into(),
                images: vec![
                    crate::message::ImageSource::Path(std::path::PathBuf::from("/tmp/p.png")),
                    crate::message::ImageSource::Bytes {
                        mime: "image/png".into(),
                        data: vec![7, 8],
                    },
                ],
            },
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["images"].as_array().unwrap().len(), 2);
        let back: Envelope = serde_json::from_value(json).unwrap();
        assert_eq!(back, env);

        // Backwards compatible: a client that sends no `images` key still loads.
        let env: Envelope = serde_json::from_value(serde_json::json!({
            "id": 10,
            "type": "user_message",
            "text": "hi",
        }))
        .unwrap();
        assert_eq!(
            env.req,
            Request::UserMessage {
                text: "hi".into(),
                images: Vec::new(),
            }
        );
    }

    #[test]
    fn interrupt_all_round_trip() {
        let env = Envelope {
            id: 11,
            req: Request::InterruptAll,
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"interrupt_all\""));
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn explain_permission_round_trip() {
        let env = Envelope {
            id: 13,
            req: Request::ExplainPermission {
                permission: PermissionId::new("perm_42"),
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"explain_permission\""));
        assert!(json.contains("\"permission\":\"perm_42\""));
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn permission_explained_round_trip() {
        let event = Event::PermissionExplained {
            permission: PermissionId::new("perm_7"),
            text: "Deletes files recursively.".to_string(),
            level: Some(ExplainLevel::High),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "permission_explained");
        assert_eq!(json["level"], "high");
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);

        // A missing level stays absent on the wire and reads back as `None`.
        let event = Event::PermissionExplained {
            permission: PermissionId::new("perm_8"),
            text: "explain unavailable under zero-data-retention".to_string(),
            level: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("level").is_none());
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn explain_level_parses_all_variants() {
        assert_eq!(ExplainLevel::parse("none"), Some(ExplainLevel::None));
        assert_eq!(ExplainLevel::parse("low"), Some(ExplainLevel::Low));
        assert_eq!(ExplainLevel::parse("Medium"), Some(ExplainLevel::Medium));
        assert_eq!(ExplainLevel::parse(" HIGH "), Some(ExplainLevel::High));
        assert_eq!(
            ExplainLevel::parse("catastrophic"),
            Some(ExplainLevel::Catastrophic)
        );
        assert_eq!(ExplainLevel::parse("bogus"), None);
        assert_eq!(ExplainLevel::parse(""), None);
    }
}
