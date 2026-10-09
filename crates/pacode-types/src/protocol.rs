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
use crate::question::{FREE_TEXT_MAX_CHARS, Question, QuestionAnswer};
use crate::schedule::{CronJob, CronSchedule, MonitorInfo};
use crate::state::{
    AgentInfo, ID_MAX_CHARS, LimitError, LimitUnit, Mode, NAME_MAX_CHARS, PATH_MAX_BYTES,
    PermissionDecision, PermissionRequest, Plan, SessionMeta, TEXT_MAX_CHARS, TITLE_MAX_CHARS,
    TaskInfo, ToastLevel, UsageTotals, check_bytes, check_chars, check_count, check_route,
    limit_error,
};
use crate::stream::Usage;
use crate::transcript::TranscriptItem;
use crate::vcs::{CommitRow, GitStatus};

/// Prompt text in a `UserMessage`: a pasted file plus the images that came with
/// it. A request travels inside one NDJSON line and the line has its own bound
/// enforced where lines are read; this is the per-field one, so an oversized
/// prompt is refused by name instead of being carried and rejected downstream.
pub const PROMPT_MAX_BYTES: usize = 512 * 1024;
/// Images pasted into one `UserMessage`; each is capped by the provider layer.
pub const PROMPT_MAX_IMAGES: usize = 16;
/// `message_id` the client may attach to its own prompt.
pub const MESSAGE_ID_MAX_CHARS: usize = 128;
/// `idempotency_key` of an attach request.
pub const IDEMPOTENCY_KEY_MAX_CHARS: usize = 128;
/// Note a client attaches to a `Deny`; it reaches the model verbatim.
pub const REASON_MAX_CHARS: usize = 2_000;
/// Provider, MCP server, plugin and marketplace names.
pub const PROVIDER_MAX_CHARS: usize = 64;
pub const SERVER_NAME_MAX_CHARS: usize = 120;
pub const PLUGIN_NAME_MAX_CHARS: usize = 200;
pub const MARKETPLACE_SOURCE_MAX_CHARS: usize = 500;
/// Free-text filters over a marketplace listing.
pub const QUERY_MAX_CHARS: usize = 200;
/// Tool arguments of a plugin command and of an MCP prompt, summed.
pub const ARGS_MAX_BYTES: usize = 64 * 1024;
/// Git revision a request names.
pub const GIT_HASH_MAX_CHARS: usize = 64;
/// Page sizes a client may ask for. `0` keeps each store caller's own default.
pub const MAX_HISTORY_PAGE: u32 = 1_000;
pub const MAX_SESSION_PAGE: u32 = 1_000;
pub const MAX_TASK_TAIL_LINES: u32 = 2_000;
/// Collections one snapshot may carry.
pub const SNAPSHOT_MAX_AGENTS: usize = 64;
pub const SNAPSHOT_MAX_TASKS: usize = 256;
pub const SNAPSHOT_MAX_TRANSCRIPT: usize = 1_000;
pub const SNAPSHOT_MAX_PENDING_PERMISSIONS: usize = 64;

/// Bumped on any wire-visible change: v2 added `SetZdr`/`SetModelFlag`,
/// `SessionMeta.zdr` and `ModelInfo.supported_efforts`; v3 added the git
/// requests (`GitLog`, `GitShow`, `GitDiff`) and their replies; v4 adds
/// `RefreshModels` + the `ModelsUpdated` broadcast; v5 adds
/// `ExplainPermission` + the `PermissionExplained` broadcast; v6 adds
/// the daemon-wide `SessionStatus` event for footer slot badges; v7 makes
/// `Attach::New` idempotent for one client/envelope id and orders attach
/// replies before session events; v8 adds the session `Resync` control marker
/// and separates attach request ids from stable idempotency keys; v9 adds
/// `SetNoticeLock` and the `NoticeLockChanged` broadcast; v12 adds
/// `ClearContext` and the `ContextCleared` broadcast.
pub const PROTOCOL_VERSION: u32 = 12;

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

impl Attach {
    /// Whether the attach request fits the caps the wire contract sets.
    pub fn validate(&self) -> Result<(), LimitError> {
        match self {
            Self::New { cwd, model, .. } => {
                check_bytes("attach cwd", &cwd.to_string_lossy(), PATH_MAX_BYTES)?;
                if let Some(route) = model {
                    check_route(route)?;
                }
                Ok(())
            }
            Self::Resume { session } => check_id("resumed session", session.as_str()),
            Self::Latest { cwd } => {
                check_bytes("attach cwd", &cwd.to_string_lossy(), PATH_MAX_BYTES)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello(ClientHello),
    Attach(Attach),
    Detach,
    /// Close the attached Core session and release its runtime resources.
    CloseSession,
    /// Starts a turn when idle; queued as a steer injection while a turn runs.
    UserMessage {
        text: String,
        /// Images attached to the prompt (`[Image N]` paste chips, `@`-refs);
        /// appended to the user message as `ContentBlock::Image` blocks.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
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
    /// Hold the injections the orchestrator would otherwise receive, then let
    /// them out one at a time. Session-wide like `SetZdr`, with the held
    /// backlog kept per agent underneath it; `Event::NoticeLockChanged` carries
    /// the badge numbers back on the flip and on every count that moves.
    SetNoticeLock(bool),
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
    /// Read the runtime's own instrumentation: resident sessions and agents,
    /// transcript bytes, session-open and history-page timings, the single store
    /// writer, and the provider governors. Read-only and on demand — nothing
    /// polls these, so an idle daemon pays for them not at all.
    RuntimeStats,
    /// Where this session's tokens went: by category, per tool, with latency
    /// quantiles and the context-quality scorecard. Session-scoped and read-only.
    /// The records behind it are fed on the turn path, so a client that never
    /// looks at them pays for the page it does not read.
    Analyze,
    ListSessions {
        limit: u32,
    },
    ListModels,
    /// Force a re-fetch of every provider's model catalog, bypassing the cache
    /// TTL (`Reply::Models`; an `Event::ModelsUpdated` broadcast follows).
    RefreshModels,
    Compact,
    /// Reset the conversation in place: same session id, model context wiped.
    /// Running agents are interrupted first; subagents are dropped. Every
    /// attached client resets on the `Event::ContextCleared` that follows.
    ClearContext,
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
            Request::CloseSession => "close_session",
            Request::UserMessage { .. } => "user_message",
            Request::Interrupt => "interrupt",
            Request::InterruptAll => "interrupt_all",
            Request::PermissionReply { .. } => "permission_reply",
            Request::ExplainPermission { .. } => "explain_permission",
            Request::SetModel(_) => "set_model",
            Request::SetEffort(_) => "set_effort",
            Request::SetMode(_) => "set_mode",
            Request::SetZdr(_) => "set_zdr",
            Request::SetNoticeLock(_) => "set_notice_lock",
            Request::SetModelFlag { .. } => "set_model_flag",
            Request::StopAgent(_) => "stop_agent",
            Request::KillTask(_) => "kill_task",
            Request::AckTask(_) => "ack_task",
            Request::GetSnapshot => "get_snapshot",
            Request::GetHistory { .. } => "get_history",
            Request::RuntimeStats => "runtime_stats",
            Request::Analyze => "analyze",
            Request::GetTaskOutput { .. } => "get_task_output",
            Request::ListSessions { .. } => "list_sessions",
            Request::ListModels => "list_models",
            Request::RefreshModels => "refresh_models",
            Request::Compact => "compact",
            Request::ClearContext => "clear_context",
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

    /// Whether the request fits the caps the wire contract sets. A daemon calls
    /// this before it acts: every field here arrives from a client, and one
    /// unbounded field is enough to blow up a transcript, a pane or the store.
    pub fn validate(&self) -> Result<(), LimitError> {
        match self {
            Self::Hello(hello) => {
                check_id("hello client id", hello.client_id.as_str())?;
                check_chars("hello app version", &hello.app_version, NAME_MAX_CHARS)
            }
            Self::Attach(attach) => attach.validate(),
            Self::Detach
            | Self::CloseSession
            | Self::Interrupt
            | Self::InterruptAll
            | Self::GetSnapshot
            | Self::ListModels
            | Self::RefreshModels
            | Self::Compact
            | Self::ClearContext
            | Self::DetachTurn
            | Self::Ping
            | Self::ListMcpServers
            | Self::ListPlugins
            | Self::ListCronJobs
            | Self::ListMonitors
            | Self::ListAuth
            | Self::Shutdown { .. } => Ok(()),
            Self::UserMessage {
                text,
                images,
                message_id,
            } => {
                check_bytes("prompt text", text, PROMPT_MAX_BYTES)?;
                check_count("prompt images", images.len(), PROMPT_MAX_IMAGES)?;
                for image in images {
                    if let ImageSource::Bytes { mime, .. } = image {
                        check_chars("image mime", mime, NAME_MAX_CHARS)?;
                    }
                }
                check_chars(
                    "prompt message id",
                    message_id.as_deref().unwrap_or(""),
                    MESSAGE_ID_MAX_CHARS,
                )
            }
            Self::PermissionReply {
                permission, reason, ..
            } => {
                check_id("permission reply", permission.as_str())?;
                check_chars(
                    "permission reason",
                    reason.as_deref().unwrap_or(""),
                    REASON_MAX_CHARS,
                )
            }
            Self::ExplainPermission { permission } => {
                check_id("explained permission", permission.as_str())
            }
            Self::SetModel(route) => check_route(route),
            Self::SetEffort(_) | Self::SetMode(_) | Self::SetZdr(_) | Self::SetNoticeLock(_) => {
                Ok(())
            }
            Self::SetModelFlag { model, .. } => check_chars("model flag", model, TITLE_MAX_CHARS),
            Self::StopAgent(agent) => check_id("stopped agent", agent.as_str()),
            Self::KillTask(task) | Self::AckTask(task) => check_id("task", task.as_str()),
            Self::GetHistory { agent, limit, .. } => {
                check_id("history agent", agent.as_str())?;
                check_page("history page", *limit, MAX_HISTORY_PAGE)
            }
            Self::GetTaskOutput { task, tail_lines } => {
                check_id("task output", task.as_str())?;
                check_page("task tail lines", *tail_lines, MAX_TASK_TAIL_LINES)
            }
            // Carries no caller input: it reads counters the daemon already owns.
            Self::RuntimeStats | Self::Analyze => Ok(()),
            Self::ListSessions { limit } => check_page("session page", *limit, MAX_SESSION_PAGE),
            Self::RestartMcpServer { server } | Self::SetMcpServerEnabled { server, .. } => {
                check_chars("mcp server", server, SERVER_NAME_MAX_CHARS)
            }
            Self::GetMcpPrompt { server, name, args } => {
                check_chars("mcp server", server, SERVER_NAME_MAX_CHARS)?;
                check_chars("mcp prompt", name, PLUGIN_NAME_MAX_CHARS)?;
                check_args(args)
            }
            Self::RunPluginCommand { name, args } => {
                check_chars("plugin name", name, PLUGIN_NAME_MAX_CHARS)?;
                check_bytes("plugin args", args, ARGS_MAX_BYTES)
            }
            Self::BrowseMarketplace { source, query } => {
                check_chars("marketplace source", source, MARKETPLACE_SOURCE_MAX_CHARS)?;
                check_chars("marketplace query", query, QUERY_MAX_CHARS)
            }
            Self::InstallPlugin { source, name } => {
                check_chars("marketplace source", source, MARKETPLACE_SOURCE_MAX_CHARS)?;
                check_chars("plugin name", name, PLUGIN_NAME_MAX_CHARS)
            }
            Self::UninstallPlugin { name } => {
                check_chars("plugin name", name, PLUGIN_NAME_MAX_CHARS)
            }
            Self::AnswerQuestion { question, answer } => {
                check_id("answered question", question.as_str())?;
                if let Some(text) = &answer.free_text {
                    check_chars("typed answer", text, FREE_TEXT_MAX_CHARS)?;
                }
                Ok(())
            }
            Self::AddCronJob {
                name,
                schedule,
                prompt,
            } => {
                check_chars("cron name", name, NAME_MAX_CHARS)?;
                check_bytes("cron prompt", prompt, TEXT_MAX_CHARS)?;
                schedule.check_size()
            }
            Self::RemoveCronJob(job)
            | Self::SetCronEnabled { id: job, .. }
            | Self::RunCronJobNow(job) => check_id("cron job", job.as_str()),
            Self::StopMonitor(monitor) => check_id("monitor", monitor.as_str()),
            Self::Login { provider } | Self::SetAuthAccount { provider, .. } => {
                check_chars("provider", provider, PROVIDER_MAX_CHARS)
            }
            Self::Logout { provider, label } => {
                check_chars("provider", provider, PROVIDER_MAX_CHARS)?;
                check_chars(
                    "account label",
                    label.as_deref().unwrap_or(""),
                    TITLE_MAX_CHARS,
                )
            }
            Self::GitLog { after } => check_chars(
                "git revision",
                after.as_deref().unwrap_or(""),
                GIT_HASH_MAX_CHARS,
            ),
            Self::GitShow { hash } | Self::GitDiff { hash } => {
                check_chars("git hash", hash, GIT_HASH_MAX_CHARS)
            }
        }
    }
}

fn check_id(field: &'static str, id: &str) -> Result<(), LimitError> {
    check_chars(field, id, ID_MAX_CHARS)
}

fn check_page(field: &'static str, requested: u32, max: u32) -> Result<(), LimitError> {
    if requested > max {
        return Err(limit_error(
            field,
            requested as usize,
            max as usize,
            LimitUnit::Items,
        ));
    }
    Ok(())
}

fn check_args(args: &BTreeMap<String, String>) -> Result<(), LimitError> {
    let total: usize = args
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum();
    if total > ARGS_MAX_BYTES {
        return Err(limit_error(
            "mcp prompt args",
            total,
            ARGS_MAX_BYTES,
            LimitUnit::Bytes,
        ));
    }
    Ok(())
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachEnvelope {
    #[serde(flatten)]
    pub envelope: Envelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl AttachEnvelope {
    pub fn new(id: u64, attach: Attach, idempotency_key: impl Into<String>) -> Self {
        Self {
            envelope: Envelope {
                id,
                req: Request::Attach(attach),
            },
            idempotency_key: Some(idempotency_key.into()),
        }
    }

    /// Whether the wrapped request and the idempotency key fit the caps the wire
    /// contract sets. The key is a client-chosen string a daemon keeps per
    /// connection, so it is capped like every other free-form field.
    pub fn validate(&self) -> Result<(), LimitError> {
        self.envelope.req.validate()?;
        check_chars(
            "attach idempotency key",
            self.idempotency_key.as_deref().unwrap_or(""),
            IDEMPOTENCY_KEY_MAX_CHARS,
        )
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_turn: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<String>,
    /// Cron jobs of this session, with `next_run_ms` already computed.
    #[serde(default)]
    pub cron_jobs: Vec<CronJob>,
    /// Live and terminal monitors of this session (in memory only).
    #[serde(default)]
    pub monitors: Vec<MonitorInfo>,
    /// Last event seq included in this snapshot; later events carry larger seqs.
    pub seq: u64,
}

impl SessionSnapshot {
    /// Whether the snapshot fits the caps the wire contract sets, collections
    /// first and then every entry they hold. A snapshot is what a reconnecting
    /// client rebuilds a whole session from, so its size is a rendering cost the
    /// sender has to bound, not something the receiver discovers later.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_count("snapshot agents", self.agents.len(), SNAPSHOT_MAX_AGENTS)?;
        check_count("snapshot tasks", self.tasks.len(), SNAPSHOT_MAX_TASKS)?;
        check_count(
            "snapshot transcript",
            self.transcript.len(),
            SNAPSHOT_MAX_TRANSCRIPT,
        )?;
        check_count(
            "snapshot permissions",
            self.pending_permissions.len(),
            SNAPSHOT_MAX_PENDING_PERMISSIONS,
        )?;
        check_count(
            "snapshot cron jobs",
            self.cron_jobs.len(),
            crate::schedule::MAX_CRON_JOBS,
        )?;
        check_count(
            "snapshot monitors",
            self.monitors.len(),
            crate::schedule::MAX_MONITORS,
        )?;
        self.meta.validate()?;
        self.plan.validate()?;
        for agent in &self.agents {
            agent.validate()?;
        }
        for task in &self.tasks {
            task.validate()?;
        }
        for permission in &self.pending_permissions {
            permission.validate()?;
        }
        for job in &self.cron_jobs {
            job.validate()?;
        }
        for monitor in &self.monitors {
            monitor.validate()?;
        }
        Ok(())
    }
}

/// The payload of `Reply::RuntimeStats`: what the core holds, and what the
/// provider governors are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatsReply {
    pub core: crate::metrics::RuntimeStats,
    pub providers: crate::metrics::ProviderStats,
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
    /// Boxed: the reading is two large structs, and inlining them would make
    /// every other `Reply` variant carry that size on every reply.
    RuntimeStats {
        stats: Box<RuntimeStatsReply>,
    },
    /// The reading behind `/analyze`. Boxed for the same reason as
    /// `RuntimeStats`: it carries up to `REQUEST_RECORD_CAP` per-request rows
    /// and inlining that would make every other reply carry the size.
    Analyze {
        report: Box<crate::analyze::AnalyzeReport>,
    },
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

/// The state a session's slot badge should show. The daemon reports
/// transitions on the daemon-wide stream so a client attached to a different
/// session can still paint the slot. Priority when several apply at once:
/// `NeedsInput` > `Working` > `Done` > `Idle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusKind {
    /// Nothing pending, nothing running — the client's resting color.
    Idle,
    /// A turn or a live subagent is running.
    Working,
    /// A permission request or a model question is waiting for the user.
    NeedsInput,
    /// A turn ended while the session was not being viewed. The badge stays
    /// until the user looks at the slot; the daemon only reports the
    /// transition, "viewed" is the client's call.
    Done,
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
    /// A session's slot-badge status changed. Emitted on the daemon-wide
    /// stream only — never on the session's own seq space — so a client
    /// attached to another session still sees the transition without
    /// subscribing to that session's events.
    SessionStatus {
        session: SessionId,
        status: SessionStatusKind,
    },
    /// Output a running foreground tool call has produced, on whole lines.
    ///
    /// The cell grows as the process writes instead of appearing all at once
    /// when it exits. `item_seq` is the transcript cell, so the client appends to
    /// exactly the row the call is drawing into.
    ToolOutput {
        agent: AgentId,
        item_seq: u64,
        text: String,
    },
    /// The session's notice lock moved: the switch flipped, or the parked
    /// backlog changed size. Fired on transitions only — a twenty-notice
    /// backlog costs two events, not twenty — so a client can render the badge
    /// from the wire without polling for it.
    NoticeLockChanged {
        session: SessionId,
        locked: bool,
        /// Injections parked across the session's resident agents.
        held: usize,
        /// Injections a cap refused since the daemon started.
        dropped: u64,
    },
    /// The conversation was reset in place (`Request::ClearContext`): the
    /// transcript, plan and subagents are gone and the context meter is back
    /// at zero. Clients drop everything they drew from them.
    ContextCleared,
}

impl Event {
    pub fn is_lossy_delta(&self) -> bool {
        matches!(self, Event::TextDelta { .. } | Event::ReasoningDelta { .. })
    }

    pub fn is_global(&self) -> bool {
        matches!(
            self,
            Event::LoginProgress { .. } | Event::AuthUpdated(_) | Event::SessionStatus { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ServerMessage {
    Reply {
        id: u64,
        reply: Reply,
    },
    Event {
        seq: u64,
        event: Event,
    },
    Resync {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_seq: Option<u64>,
    },
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
                message_id: Some("msg-envelope".into()),
            },
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["id"], 7);
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["text"], "hi");
        assert_eq!(json["message_id"], "msg-envelope");
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
        let resync = ServerMessage::Resync {
            current_seq: Some(9),
        };
        let line = serde_json::to_string(&resync).unwrap();
        assert!(line.contains("\"channel\":\"resync\""));
        assert!(line.contains("\"current_seq\":9"));
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&line).unwrap(),
            resync
        );
        let minimal = ServerMessage::Resync { current_seq: None };
        let line = serde_json::to_string(&minimal).unwrap();
        assert_eq!(line, r#"{"channel":"resync"}"#);
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&line).unwrap(),
            minimal
        );
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
    fn clear_context_protocol_round_trip() {
        let env = Envelope {
            id: 5,
            req: Request::ClearContext,
        };
        assert_eq!(env.req.name(), "clear_context");
        assert!(env.req.validate().is_ok());
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"clear_context\""));
        assert_eq!(serde_json::from_str::<Envelope>(&json).unwrap(), env);

        let event = ServerMessage::Event {
            seq: 3,
            event: Event::ContextCleared,
        };
        let line = serde_json::to_string(&event).unwrap();
        assert!(line.contains("\"type\":\"context_cleared\""));
        assert_eq!(serde_json::from_str::<ServerMessage>(&line).unwrap(), event);
        assert!(!Event::ContextCleared.is_global());
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
                message_id: Some("msg-images".into()),
            },
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["images"].as_array().unwrap().len(), 2);
        assert_eq!(json["message_id"], "msg-images");
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
                message_id: None,
            }
        );
        let json = serde_json::to_value(&env).unwrap();
        assert!(json.get("message_id").is_none());
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
    fn session_snapshot_optional_turn_fences_are_wire_compatible() {
        let snapshot = SessionSnapshot {
            meta: SessionMeta {
                id: SessionId::new("ses_snapshot"),
                name: None,
                cwd: PathBuf::from("/tmp"),
                git_branch: None,
                created_at_ms: 1,
                updated_at_ms: 1,
                model: ModelRoute::new("mock", "model"),
                effort: Effort::Low,
                mode: Mode::Build,
                first_prompt: None,
                zdr: false,
            },
            agents: Vec::new(),
            plan: Plan::default(),
            tasks: Vec::new(),
            usage: UsageTotals::default(),
            transcript: Vec::new(),
            has_more_history: false,
            pending_permissions: Vec::new(),
            pending_questions: Vec::new(),
            turn_active: false,
            active_turn: None,
            terminal_turn: None,
            terminal_error: None,
            cron_jobs: Vec::new(),
            monitors: Vec::new(),
            seq: 1,
        };
        let mut value = serde_json::to_value(&snapshot).unwrap();
        assert!(value.get("active_turn").is_none());
        assert!(value.get("terminal_turn").is_none());
        assert!(value.get("terminal_error").is_none());
        let object = value.as_object_mut().unwrap();
        object.remove("active_turn");
        object.remove("terminal_turn");
        object.remove("terminal_error");
        let legacy: SessionSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.active_turn, None);
        assert_eq!(legacy.terminal_turn, None);
        assert_eq!(legacy.terminal_error, None);

        let turn = TurnId::new("trn_snapshot");
        let snapshot = SessionSnapshot {
            active_turn: Some(turn.clone()),
            terminal_turn: Some(turn.clone()),
            terminal_error: Some("failed".into()),
            ..snapshot
        };
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["active_turn"], "trn_snapshot");
        assert_eq!(value["terminal_turn"], "trn_snapshot");
        assert_eq!(value["terminal_error"], "failed");
    }

    #[test]
    fn close_session_round_trip() {
        let env = Envelope {
            id: 14,
            req: Request::CloseSession,
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"close_session\""));
        assert_eq!(serde_json::from_str::<Envelope>(&json).unwrap(), env);
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
    fn session_status_round_trip() {
        for status in [
            SessionStatusKind::Idle,
            SessionStatusKind::Working,
            SessionStatusKind::NeedsInput,
            SessionStatusKind::Done,
        ] {
            let event = Event::SessionStatus {
                session: SessionId::new("ses_test1"),
                status,
            };
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json["type"], "session_status");
            assert_eq!(json["session"], "ses_test1");
            let back: Event = serde_json::from_value(json).unwrap();
            assert_eq!(back, event);
        }
        // The wire names are snake_case and stable.
        let json = serde_json::to_value(Event::SessionStatus {
            session: SessionId::new("ses_x"),
            status: SessionStatusKind::NeedsInput,
        })
        .unwrap();
        assert_eq!(json["status"], "needs_input");
    }

    #[test]
    fn attach_new_keeps_legacy_wire_shape() {
        let env = Envelope {
            id: 17,
            req: Request::Attach(Attach::New {
                cwd: PathBuf::from("/tmp/project"),
                model: None,
                effort: None,
                mode: None,
            }),
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["id"], 17);
        assert_eq!(json["type"], "attach");
        assert_eq!(json["kind"], "new");
        assert_eq!(json["cwd"], "/tmp/project");
        assert!(json.get("session").is_none());
        assert_eq!(serde_json::from_value::<Envelope>(json).unwrap(), env);
    }

    #[test]
    fn attach_idempotency_wrapper_is_wire_compatible() {
        let env = AttachEnvelope::new(
            18,
            Attach::New {
                cwd: PathBuf::from("/tmp/project"),
                model: None,
                effort: None,
                mode: None,
            },
            "logical-attach-1",
        );
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["id"], 18);
        assert_eq!(json["type"], "attach");
        assert_eq!(json["kind"], "new");
        assert_eq!(json["idempotency_key"], "logical-attach-1");
        assert_eq!(
            serde_json::from_value::<AttachEnvelope>(json.clone()).unwrap(),
            env
        );
        assert_eq!(
            serde_json::from_value::<Envelope>(json).unwrap(),
            env.envelope
        );

        let legacy = serde_json::json!({
            "id": 19,
            "type": "attach",
            "kind": "latest",
            "cwd": "/tmp/project"
        });
        let legacy: AttachEnvelope = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.idempotency_key, None);
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

    #[test]
    fn requests_a_client_can_send_validate() {
        let cases = [
            Request::Hello(ClientHello {
                client_id: ClientId::new("cli_1"),
                app_version: "1.1.0".into(),
                protocol: PROTOCOL_VERSION,
            }),
            Request::Attach(Attach::New {
                cwd: PathBuf::from("/tmp/project"),
                model: Some(ModelRoute::new("mock", "model")),
                effort: Some(Effort::High),
                mode: Some(Mode::Build),
            }),
            Request::Attach(Attach::Resume {
                session: SessionId::new("pacode-12345678-abcd"),
            }),
            Request::UserMessage {
                text: "hi".into(),
                images: vec![ImageSource::Path(PathBuf::from("/tmp/p.png"))],
                message_id: Some("msg-1".into()),
            },
            Request::GetHistory {
                agent: AgentId::main(),
                before_seq: None,
                limit: MAX_HISTORY_PAGE,
            },
            Request::GetTaskOutput {
                task: TaskId::new("tsk_1"),
                tail_lines: MAX_TASK_TAIL_LINES,
            },
            Request::ListSessions { limit: 0 },
            Request::GitLog {
                after: Some("abc123".into()),
            },
            Request::Login {
                provider: "mock".into(),
            },
            Request::GetMcpPrompt {
                server: "files".into(),
                name: "read".into(),
                args: BTreeMap::from([("path".to_string(), "/tmp/a".to_string())]),
            },
        ];
        for request in cases {
            assert!(request.validate().is_ok(), "{}", request.name());
        }
    }

    #[test]
    fn a_request_past_a_cap_names_the_field() {
        let cases: Vec<(Request, &'static str)> = vec![
            (
                Request::UserMessage {
                    text: "x".repeat(PROMPT_MAX_BYTES + 1),
                    images: Vec::new(),
                    message_id: None,
                },
                "prompt text",
            ),
            (
                Request::UserMessage {
                    text: "hi".into(),
                    images: vec![
                        ImageSource::Path(PathBuf::from("/tmp/p.png"));
                        PROMPT_MAX_IMAGES + 1
                    ],
                    message_id: None,
                },
                "prompt images",
            ),
            (
                Request::UserMessage {
                    text: "hi".into(),
                    images: Vec::new(),
                    message_id: Some("m".repeat(MESSAGE_ID_MAX_CHARS + 1)),
                },
                "prompt message id",
            ),
            (
                Request::GetHistory {
                    agent: AgentId::main(),
                    before_seq: None,
                    limit: MAX_HISTORY_PAGE + 1,
                },
                "history page",
            ),
            (
                Request::ListSessions {
                    limit: MAX_SESSION_PAGE + 1,
                },
                "session page",
            ),
            (
                Request::GetTaskOutput {
                    task: TaskId::new("tsk_1"),
                    tail_lines: MAX_TASK_TAIL_LINES + 1,
                },
                "task tail lines",
            ),
            (
                Request::PermissionReply {
                    permission: PermissionId::new("perm_1"),
                    decision: PermissionDecision::Deny,
                    reason: Some("r".repeat(REASON_MAX_CHARS + 1)),
                },
                "permission reason",
            ),
            (
                Request::SetModel(ModelRoute::new("p", "m".repeat(NAME_MAX_CHARS + 1))),
                "model name",
            ),
            (
                Request::GetMcpPrompt {
                    server: "files".into(),
                    name: "read".into(),
                    args: BTreeMap::from([("path".to_string(), "x".repeat(ARGS_MAX_BYTES + 1))]),
                },
                "mcp prompt args",
            ),
            (
                Request::RunPluginCommand {
                    name: "p".repeat(PLUGIN_NAME_MAX_CHARS + 1),
                    args: String::new(),
                },
                "plugin name",
            ),
            (
                Request::BrowseMarketplace {
                    source: "s".repeat(MARKETPLACE_SOURCE_MAX_CHARS + 1),
                    query: String::new(),
                },
                "marketplace source",
            ),
            (
                Request::BrowseMarketplace {
                    source: "owner/repo".into(),
                    query: "q".repeat(QUERY_MAX_CHARS + 1),
                },
                "marketplace query",
            ),
            (
                Request::Login {
                    provider: "p".repeat(PROVIDER_MAX_CHARS + 1),
                },
                "provider",
            ),
            (
                Request::RestartMcpServer {
                    server: "s".repeat(SERVER_NAME_MAX_CHARS + 1),
                },
                "mcp server",
            ),
            (
                Request::GitShow {
                    hash: "h".repeat(GIT_HASH_MAX_CHARS + 1),
                },
                "git hash",
            ),
            (
                Request::AddCronJob {
                    name: "n".into(),
                    schedule: CronSchedule::cron("x".repeat(201)),
                    prompt: "p".into(),
                },
                "cron expression",
            ),
            (
                Request::StopMonitor(MonitorId::new("m".repeat(ID_MAX_CHARS + 1))),
                "monitor",
            ),
            (
                Request::Attach(Attach::Latest {
                    cwd: PathBuf::from("/x".repeat(PATH_MAX_BYTES)),
                }),
                "attach cwd",
            ),
        ];
        for (request, field) in cases {
            let error = request.validate().expect_err(request.name());
            assert_eq!(error.field, field, "{}", request.name());
        }
    }

    #[test]
    fn an_attach_key_is_capped_like_every_other_client_string() {
        let env = AttachEnvelope::new(
            1,
            Attach::New {
                cwd: PathBuf::from("/tmp"),
                model: None,
                effort: None,
                mode: None,
            },
            "k".repeat(IDEMPOTENCY_KEY_MAX_CHARS + 1),
        );
        assert_eq!(
            env.validate().expect_err("overlong key").field,
            "attach idempotency key"
        );
        let env = AttachEnvelope::new(
            1,
            Attach::Latest {
                cwd: PathBuf::from("/tmp"),
            },
            "k",
        );
        assert!(env.validate().is_ok());
    }

    #[test]
    fn a_snapshot_validates_its_collections_and_their_entries() {
        let mut over = snapshot();
        assert!(over.validate().is_ok());

        over.agents = vec![AgentInfo {
            name: "a".repeat(NAME_MAX_CHARS + 1),
            ..agent_info()
        }];
        assert_eq!(
            over.validate().expect_err("overlong agent").field,
            "agent name"
        );

        let mut over = snapshot();
        over.transcript = vec![transcript_item(); SNAPSHOT_MAX_TRANSCRIPT + 1];
        assert_eq!(
            over.validate().expect_err("long transcript").field,
            "snapshot transcript"
        );

        let mut over = snapshot();
        over.cron_jobs = vec![cron_job(); crate::schedule::MAX_CRON_JOBS + 1];
        assert_eq!(
            over.validate().expect_err("too many jobs").field,
            "snapshot cron jobs"
        );
    }

    fn session_meta() -> SessionMeta {
        SessionMeta {
            id: SessionId::new("ses_1"),
            name: None,
            cwd: PathBuf::from("/tmp"),
            git_branch: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            model: ModelRoute::new("mock", "model"),
            effort: Effort::High,
            mode: Mode::Build,
            first_prompt: None,
            zdr: false,
        }
    }

    fn agent_info() -> AgentInfo {
        AgentInfo {
            id: AgentId::new("agt_1"),
            name: "main".into(),
            kind: crate::state::AgentKind::Main,
            status: crate::state::AgentStatus::Idle,
            activity: None,
            started_at_ms: 0,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: ModelRoute::new("mock", "model"),
            effort: Effort::High,
            parent: None,
            summary: None,
            error: None,
        }
    }

    fn transcript_item() -> TranscriptItem {
        TranscriptItem {
            agent: AgentId::main(),
            seq: 1,
            ts_ms: 0,
            kind: crate::transcript::TranscriptKind::Notice {
                level: ToastLevel::Info,
                text: String::new(),
            },
        }
    }

    fn cron_job() -> CronJob {
        CronJob {
            id: CronJobId::new("cron_1"),
            name: "nightly".into(),
            schedule: CronSchedule::every(60),
            prompt: "run the suite".into(),
            enabled: true,
            created_at_ms: 0,
            last_run_ms: None,
            next_run_ms: None,
            last_status: None,
        }
    }

    fn snapshot() -> SessionSnapshot {
        SessionSnapshot {
            meta: session_meta(),
            agents: vec![agent_info()],
            plan: Plan::default(),
            tasks: Vec::new(),
            usage: UsageTotals::default(),
            transcript: vec![transcript_item()],
            has_more_history: false,
            pending_permissions: Vec::new(),
            pending_questions: Vec::new(),
            turn_active: false,
            active_turn: None,
            terminal_turn: None,
            terminal_error: None,
            cron_jobs: vec![cron_job()],
            monitors: Vec::new(),
            seq: 1,
        }
    }
}
