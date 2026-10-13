//! `ToolHost`: the only interface tools use to reach the core (permissions, background
//! tasks, subagents, plan, UI previews). The core implements it; tests use a stub.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_exec::TaskSpec;
use pacode_types::{
    AgentId, AgentInfo, CallId, Effort, Mode, ModelRoute, PermissionDecision, Plan, RiskLevel,
    SessionId, TaskId, TaskInfo, TaskProgress, ToastLevel,
};
use tokio_util::sync::CancellationToken;

use crate::output::ToolError;

/// Per-call context handed to [`crate::Tool::call`].
#[derive(Clone)]
pub struct ToolCtx {
    pub session: SessionId,
    pub agent: AgentId,
    pub agent_name: String,
    pub call_id: CallId,
    pub cwd: PathBuf,
    pub mode: Mode,
    pub host: Arc<dyn ToolHost>,
    pub cancel: CancellationToken,
    /// From `[context].tool_output_cap_chars`.
    pub output_cap_chars: usize,
    /// From `[exec].yield_after_secs`: foreground wait before a command is backgrounded.
    pub exec_yield_after: Duration,
    /// From `[exec].default_timeout_secs`.
    pub exec_default_timeout: Duration,
    /// Explicit tool name when called through the runtime.
    pub tool_name: Option<String>,
    /// Explicit tool kind when called through the runtime.
    pub tool_kind: Option<crate::ToolKind>,
    /// Snapshot of the agent's registry for this call — `batch` resolves its
    /// ops through it, so a disabled or unavailable tool fails inside a batch
    /// exactly as it would standalone.
    pub tools: crate::registry::ToolRegistry,
}

impl ToolCtx {
    /// Attach explicit tool identity to this context.
    pub fn with_tool(mut self, name: impl Into<String>, kind: crate::ToolKind) -> Self {
        self.tool_name = Some(name.into());
        self.tool_kind = Some(kind);
        self
    }
    /// Resolve a model-supplied path against the working directory.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    /// Ask for permission with a one-line title and a detail block. Returns `Err(Denied)`
    /// when refused, so tools can `?` it.
    pub async fn require_permission(
        &self,
        title: impl Into<String>,
        detail: impl Into<String>,
        risk: Option<RiskLevel>,
    ) -> Result<PermissionDecision, ToolError> {
        self.host
            .request_permission(PermissionDraft {
                agent: self.agent.clone(),
                agent_name: self.agent_name.clone(),
                call_id: self.call_id.clone(),
                title: title.into(),
                detail: detail.into(),
                risk,
                tool_name: self.tool_name.clone(),
                tool_kind: self.tool_kind,
            })
            .await
            .into_result("user denied the request")
    }
}

/// The host's answer to a permission request. `reason` explains a `Deny` that
/// was NOT the user's refusal: the policy gate's reason, `zdr_blocked: …`,
/// `interrupted`, or a client-supplied note. `None` on `Deny` means the user
/// pressed deny without a comment.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionOutcome {
    pub decision: PermissionDecision,
    pub reason: Option<String>,
}

impl PermissionOutcome {
    /// `Ok(decision)` for `Allow*`; `Err(ToolError::Denied)` for `Deny`.
    /// `user_denied` is the model-facing text for a bare user refusal; a
    /// `Some` reason is quoted back verbatim and marked as an automatic deny
    /// so the model does not retry the same call expecting a user answer.
    pub fn into_result(self, user_denied: &str) -> Result<PermissionDecision, ToolError> {
        match self.decision {
            PermissionDecision::AllowOnce | PermissionDecision::AllowSession => Ok(self.decision),
            PermissionDecision::Deny => {
                let message = match self.reason {
                    Some(reason) => {
                        let reason = reason.trim().trim_end_matches('.');
                        format!(
                            "{reason}. This call was denied automatically, not refused by the \
                             user — do not retry it; choose a different approach or ask the user."
                        )
                    }
                    None => user_denied.to_string(),
                };
                Err(ToolError::Denied(message))
            }
        }
    }
}

impl From<PermissionDecision> for PermissionOutcome {
    fn from(decision: PermissionDecision) -> Self {
        Self {
            decision,
            reason: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionDraft {
    pub agent: AgentId,
    pub agent_name: String,
    pub call_id: CallId,
    pub title: String,
    pub detail: String,
    pub risk: Option<RiskLevel>,
    pub tool_name: Option<String>,
    pub tool_kind: Option<crate::ToolKind>,
}

impl PermissionDraft {
    pub fn new(
        agent: AgentId,
        agent_name: impl Into<String>,
        call_id: CallId,
        title: impl Into<String>,
        detail: impl Into<String>,
        risk: Option<RiskLevel>,
    ) -> Self {
        Self {
            agent,
            agent_name: agent_name.into(),
            call_id,
            title: title.into(),
            detail: detail.into(),
            risk,
            tool_name: None,
            tool_kind: None,
        }
    }

    pub fn with_tool(mut self, name: impl Into<String>, kind: crate::ToolKind) -> Self {
        self.tool_name = Some(name.into());
        self.tool_kind = Some(kind);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub prompt: String,
    pub name: Option<String>,
    pub model: Option<ModelRoute>,
    pub effort: Option<Effort>,
    /// `true` = copy of the parent's history at spawn time.
    pub fork: bool,
    /// Tool names allowed for the subagent; `None` = the default subagent set,
    /// which is the orchestrator's own set. Purely a narrowing: naming a tool
    /// that does not exist is ignored, never an error.
    pub tools: Option<Vec<String>>,
    /// Tool names to drop, applied after [`Self::tools`]. This is how an
    /// orchestrator trims a subagent without restating the tools it wants: a
    /// subagent that edits files gains nothing from every browser schema in
    /// the set, and at 128 subagents those schemas are sent on every step.
    /// Exclude wins where both lists name the same tool.
    pub exclude_tools: Option<Vec<String>>,
}

/// What `agent send` did with the message — the tool output names it, so the
/// orchestrator knows whether the subagent is reading it now or starting over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// The subagent is mid-turn and reads the message at its next step.
    Delivered,
    /// The subagent was blocked in `ask_parent`: the message is its answer.
    Answered,
    /// The subagent had ended (finished or stopped): the message started a
    /// new turn on its retained context. `previous_report` is the report it
    /// ended with when the orchestrator had not read it yet — handed back here
    /// so it cannot arrive after the resume, posing as the reply.
    Resumed { previous_report: Option<String> },
}

/// How an `ask_parent` question ended, when it did not end in a stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentReply {
    /// The orchestrator's `agent send`, capped.
    Answered(String),
    /// The orchestrator completed a turn that showed it the question without
    /// answering.
    Unanswered,
    /// The orchestrator's turn failed after it was shown the question, or the
    /// question could not be delivered at all.
    ParentFailed,
    /// Too many questions in a row went unanswered: this one was not sent.
    NotAnswering,
}

/// Result of waiting on tasks.
#[derive(Clone, Debug, PartialEq)]
pub enum WaitOutcome {
    Finished,
    Timeout,
    Progress,
    Cancelled,
}

/// A named subagent type discovered from an `agents/*.md` file (Claude Code
/// `.claude/agents` parity): frontmatter fenced by `---` (YAML-style) or `+++`
/// (TOML) carries `name` (default: file stem), `description` (required — it is
/// what the model sees when picking a kind), `tools` (restrict the subagent's
/// tool set) and `model` (a route string); the markdown body is prepended to
/// the spawn prompt as the agent's brief.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentKindDef {
    pub name: String,
    pub description: String,
    /// `None` = the default subagent tool set.
    pub tools: Option<Vec<String>>,
    /// Route string resolved via `ToolHost::parse_model_route` at spawn time.
    pub model: Option<String>,
    /// Markdown body prepended to the spawn prompt.
    pub prompt: String,
    /// File the kind was loaded from.
    pub path: PathBuf,
}

/// A file skipped during [`discover_agent_kinds`]; discovery never fails, it
/// reports these instead.
#[derive(Debug, thiserror::Error)]
pub enum AgentKindWarning {
    #[error("failed to read agents directory at {path}: {source}")]
    DirReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read agent definition at {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid agent definition at {path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

/// Load agent kinds: the built-in ones first, then `*.md` files from `dirs`. On
/// a name clash the earlier source wins, so a file replaces a built-in of the
/// same name outright and the project dir beats the global one. Missing
/// directories are normal; unreadable or invalid files are skipped and reported
/// as [`AgentKindWarning`]s.
pub fn discover_agent_kinds(dirs: &[PathBuf]) -> (Vec<AgentKindDef>, Vec<AgentKindWarning>) {
    let mut kinds = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    // Files are collected first and win every name clash, so a project file
    // beats a global one and either beats a built-in of the same name. The
    // built-ins go in afterwards as the fallback for names nothing claimed —
    // which is the whole point: a fresh install with no `agents/` directory
    // still has kinds to offer, instead of every spawn being a refusal.

    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    warnings.push(AgentKindWarning::DirReadFailed {
                        path: dir.clone(),
                        source: err,
                    });
                }
                continue;
            }
        };

        let mut files = Vec::new();
        for entry in entries {
            match entry {
                Ok(e) => {
                    let path = e.path();
                    if path.is_file() && path.extension().is_some_and(|x| x == "md") {
                        files.push(path);
                    }
                }
                Err(err) => warnings.push(AgentKindWarning::DirReadFailed {
                    path: dir.clone(),
                    source: err,
                }),
            }
        }
        files.sort();

        for path in files {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(err) => {
                    warnings.push(AgentKindWarning::ReadFailed { path, source: err });
                    continue;
                }
            };
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            match parse_agent_kind(&content, stem, path.clone()) {
                Ok(def) => {
                    if seen.insert(def.name.clone()) {
                        kinds.push(def);
                    }
                }
                Err(reason) => warnings.push(AgentKindWarning::Invalid { path, reason }),
            }
        }
    }
    // Built-ins last, and only for names no file claimed: the file is the more
    // specific statement of intent and wins the name outright rather than
    // contributing field by field to a built-in.
    for (name, description, tools, prompt) in BUILTIN_AGENT_KINDS {
        if !seen.insert((*name).to_string()) {
            continue;
        }
        kinds.push(AgentKindDef {
            name: (*name).to_string(),
            description: (*description).to_string(),
            tools: Some(tools.iter().map(|t| (*t).to_string()).collect()),
            model: None,
            prompt: (*prompt).to_string(),
            path: PathBuf::from(format!("<builtin>/{name}")),
        });
    }
    kinds.sort_by(|a, b| a.name.cmp(&b.name));
    (kinds, warnings)
}

/// The kinds that ship with pacode, as `(name, description, tools, brief)`.
///
/// These exist because the tool schema takes `kind` as a free-form string: a
/// model asked for a subagent reaches for a plausible name, and with nothing on
/// offer every such call was either a refusal or a guess. Two names cover
/// nearly all of it — `explore` for finding things, `implement` for changing
/// them — and both pin a *narrow* tool list, which is the other half of the
/// reason to have them: every tool schema is re-sent on every step of every
/// subagent, so a fleet of readers stops paying for write and browser tools it
/// has no use for.
///
/// An explicit list rather than `None` (which would mean "the orchestrator's
/// full set") is the point. A user file of the same name replaces a built-in
/// whole, not field by field — a half-overridden kind is worse than either.
const BUILTIN_AGENT_KINDS: &[(&str, &str, &[&str], &str)] = &[
    (
        "explore",
        "Read-only search of the codebase. Cannot edit, cannot run commands.",
        &[
            "read",
            "grep",
            "glob",
            "ls",
            "symbols",
            "webfetch",
            "websearch",
        ],
        "You are exploring a codebase on someone's behalf. Search widely before \
         concluding, cite file:line for every claim, and report what you found \
         rather than what you would change. You cannot edit anything.",
    ),
    (
        "implement",
        "Makes changes: edits files, runs tests, iterates until the task is done.",
        &[
            "read", "edit", "write", "grep", "glob", "ls", "symbols", "bash", "plan",
        ],
        "You are implementing a change. Read enough to be sure of the shape \
         before editing, then make the change and verify it. Report what you did \
         and what you verified, and say so plainly if you could not.",
    ),
];

/// Flat frontmatter of an `agents/*.md` file; every field is optional at this
/// stage and validated in [`parse_agent_kind`].
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct RawKindFrontmatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<Vec<String>>,
    model: Option<String>,
}

/// Parse one `agents/*.md` file. `Err` carries the reason the file is skipped.
fn parse_agent_kind(
    content: &str,
    fallback_name: &str,
    path: PathBuf,
) -> Result<AgentKindDef, String> {
    let Some((is_toml, frontmatter, body)) = split_kind_frontmatter(content) else {
        return Err("missing frontmatter (expected a `---` or `+++` fenced block)".to_string());
    };
    let raw = if is_toml {
        toml::from_str::<RawKindFrontmatter>(frontmatter)
            .map_err(|e| format!("invalid TOML frontmatter: {e}"))?
    } else {
        parse_yaml_frontmatter(frontmatter)
    };

    let name = raw
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fallback_name.to_string());
    if name.is_empty() {
        return Err("missing `name` and the file has no usable stem".to_string());
    }
    let description = raw
        .description
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .ok_or_else(|| "missing required `description`".to_string())?;

    Ok(AgentKindDef {
        name,
        description,
        // An empty list reads as "not set": the default subagent set applies.
        tools: raw.tools.filter(|t| !t.is_empty()),
        model: raw
            .model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty()),
        prompt: body.trim().to_string(),
        path,
    })
}

/// Split `---` (YAML-style) or `+++` (TOML) frontmatter off the body. Returns
/// `(is_toml, frontmatter_text, body)`; `None` when there is no fenced block.
fn split_kind_frontmatter(content: &str) -> Option<(bool, &str, &str)> {
    let clean = content.strip_prefix('\u{feff}').unwrap_or(content);
    let (fence, is_toml) = if clean.starts_with("+++") {
        ("+++", true)
    } else if clean.starts_with("---") {
        ("---", false)
    } else {
        return None;
    };
    let after_open = &clean[fence.len()..];
    let after_open = after_open
        .strip_prefix("\r\n")
        .or_else(|| after_open.strip_prefix('\n'))?;

    let mut offset = 0;
    while offset < after_open.len() {
        let slice = &after_open[offset..];
        if let Some(rest) = slice.strip_prefix(fence) {
            let frontmatter = &after_open[..offset];
            if let Some(body) = rest
                .strip_prefix("\r\n")
                .or_else(|| rest.strip_prefix('\n'))
            {
                return Some((is_toml, frontmatter, body));
            }
            if rest.is_empty() {
                return Some((is_toml, frontmatter, ""));
            }
            // The fence followed by more text on the same line is not a closer.
        }
        match slice.find('\n') {
            Some(pos) => offset += pos + 1,
            None => break,
        }
    }
    None
}

/// Hand-parse the flat `key: value` frontmatter subset — the same approach as
/// `pacode_skills`, so no YAML dependency is pulled in. `tools` accepts both
/// `tools: [a, b]` and a `- item` block list; unknown keys are ignored.
fn parse_yaml_frontmatter(frontmatter: &str) -> RawKindFrontmatter {
    let mut raw = RawKindFrontmatter::default();
    let mut lines = frontmatter.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = strip_quotes(value);
        match key.trim() {
            "name" if raw.name.is_none() && !value.is_empty() => {
                raw.name = Some(value.to_string());
            }
            "description" if raw.description.is_none() && !value.is_empty() => {
                raw.description = Some(value.to_string());
            }
            "model" if raw.model.is_none() && !value.is_empty() => {
                raw.model = Some(value.to_string());
            }
            "tools" if raw.tools.is_none() => {
                raw.tools = Some(parse_yaml_tool_list(value, &mut lines));
            }
            _ => {}
        }
    }
    raw
}

/// `tools` frontmatter value: inline `[a, b]`, a single bare name, or a
/// `- item` block list consumed from the following lines.
fn parse_yaml_tool_list<'a>(
    inline: &str,
    lines: &mut std::iter::Peekable<std::str::Lines<'a>>,
) -> Vec<String> {
    let mut items = Vec::new();
    if let Some(inner) = inline.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        for part in inner.split(',') {
            let item = strip_quotes(part);
            if !item.is_empty() {
                items.push(item.to_string());
            }
        }
        return items;
    }
    if !inline.is_empty() {
        return vec![inline.to_string()];
    }
    while let Some(next) = lines.peek() {
        let t = next.trim();
        if t == "-" || t.starts_with("- ") {
            let item = strip_quotes(t.trim_start_matches('-'));
            if !item.is_empty() {
                items.push(item.to_string());
            }
            lines.next();
        } else {
            break;
        }
    }
    items
}

fn strip_quotes(s: &str) -> &str {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].trim()
    } else {
        trimmed
    }
}

/// Everything a tool may ask the core for. Implemented by `pacode-core`.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Ask the user (or the mode) whether the call may proceed. The host applies the
    /// permission matrix and the `AllowSession` cache before prompting. The returned
    /// [`PermissionOutcome`] carries the reason whenever a `Deny` was decided by
    /// policy or the environment rather than by the user's refusal.
    async fn request_permission(&self, draft: PermissionDraft) -> PermissionOutcome;

    /// Ask the user a question and wait for the answer. The turn blocks exactly
    /// as it does for a permission prompt; a dismissed question comes back as a
    /// cancelled answer rather than hanging.
    async fn ask_question(
        &self,
        call_id: &CallId,
        header: String,
        question: String,
        options: Vec<pacode_types::QuestionOption>,
        multi_select: bool,
    ) -> Result<pacode_types::QuestionAnswer, ToolError>;

    // --- scheduling (owner = session) ---
    /// Register a scheduled prompt. The schedule is validated here, so an
    /// unusable expression is an error the model can correct rather than a job
    /// that silently never fires.
    async fn add_cron_job(
        &self,
        name: String,
        schedule: pacode_types::CronSchedule,
        prompt: String,
    ) -> Result<pacode_types::CronJob, ToolError>;
    fn list_cron_jobs(&self) -> Vec<pacode_types::CronJob>;
    async fn remove_cron_job(&self, id: &pacode_types::CronJobId) -> Result<(), ToolError>;

    /// Start watching a condition. The monitor belongs to the session and stops
    /// with it; it fires at most once and then stops polling.
    fn add_monitor(
        &self,
        label: String,
        condition: pacode_types::MonitorCondition,
        poll_interval_secs: Option<u64>,
    ) -> Result<pacode_types::MonitorInfo, ToolError>;
    fn list_monitors(&self) -> Vec<pacode_types::MonitorInfo>;
    fn stop_monitor(&self, id: &pacode_types::MonitorId) -> Result<(), ToolError>;

    // --- background tasks (owner = session) ---
    async fn spawn_task(&self, spec: TaskSpec) -> Result<TaskId, ToolError>;
    fn task_info(&self, task: &TaskId) -> Option<TaskInfo>;
    fn list_tasks(&self) -> Vec<TaskInfo>;
    /// Wait, as the call that owns the task's foreground, until the task ends,
    /// `timeout` elapses, or (when `return_on_progress`) its progress changes.
    /// `Finished` makes the caller the one that reports the output: no
    /// `<task_finished>` notice follows. Any other outcome, or dropping the
    /// future, leaves the task backgrounded, and its end arrives as that
    /// notice.
    async fn wait_task(
        &self,
        task: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitOutcome;
    async fn kill_task(&self, task: &TaskId) -> Result<(), ToolError>;
    /// Last `lines` lines of the spooled output.
    async fn task_tail(&self, task: &TaskId, lines: usize) -> Result<Vec<String>, ToolError>;
    /// The task's output as the exec buffer holds it — the first and the last
    /// bytes with a `[... N bytes omitted ...]` marker between them when the
    /// middle was dropped — cut head+tail to `max_chars`.
    async fn task_output(&self, task: &TaskId, max_chars: usize) -> Result<String, ToolError>;
    fn report_task_progress(&self, task: &TaskId, progress: TaskProgress) -> Result<(), ToolError>;

    // --- subagents (depth 1) ---
    async fn spawn_agent(&self, spec: AgentSpec) -> Result<AgentId, ToolError>;
    fn agent_info(&self, agent: &AgentId) -> Option<AgentInfo>;
    fn list_agents(&self) -> Vec<AgentInfo>;
    /// Named subagent types the `agent` tool may spawn: discovered fresh from
    /// `agents/*.md` files on each call (spawn/list are rare), project dir
    /// first so it shadows the global one on a name clash.
    fn agent_kinds(&self) -> Vec<AgentKindDef>;
    /// Resolve a `provider/model` route — or a bare model on the default
    /// provider — against the session's provider registry.
    fn parse_model_route(&self, s: &str) -> Option<ModelRoute>;
    async fn stop_agent(&self, agent: &AgentId) -> Result<(), ToolError>;
    /// Re-drive a `Stopped` agent: appends a resume nudge to its retained
    /// history and starts a fresh turn. Errors while the agent is still running
    /// or already in a terminal state.
    async fn continue_agent(&self, agent: &AgentId) -> Result<(), ToolError>;
    /// Orchestrator → subagent: ask for a brief status. Delivered to the subagent as a
    /// `<status_request>` injection at its next step; it answers with `report_status`.
    fn request_agent_status(&self, agent: &AgentId) -> Result<(), ToolError>;
    /// Subagent → parent: a short status line, delivered as `<agent_status>` at the
    /// parent's next step.
    fn report_status(&self, text: String) -> Result<(), ToolError>;
    /// Orchestrator → its own subagent. A running subagent gets the message as
    /// an `<orchestrator_message>` at its next step; one blocked in
    /// `ask_parent` gets it as the answer; one that has ended is resumed with
    /// it. A failed subagent cannot be resumed and is an error.
    async fn send_to_agent(
        &self,
        agent: &AgentId,
        message: String,
    ) -> Result<SendOutcome, ToolError>;
    /// Subagent → orchestrator: deliver the question as `<agent_question>` and
    /// wait until the orchestrator answers it with `agent send` or completes a
    /// turn without doing so. A stop ends the wait as `Cancelled`.
    async fn ask_parent(
        &self,
        call_id: &CallId,
        question: String,
    ) -> Result<ParentReply, ToolError>;

    // --- plan ---
    fn plan(&self) -> Plan;
    fn set_plan(&self, plan: Plan);

    // --- UI ---
    /// Stream a bounded preview of in-progress output to the transcript row.
    fn emit_preview(&self, call_id: &CallId, preview: String);
    /// Emit a user-visible notice in the session transcript.
    fn emit_notice(&self, level: ToastLevel, text: String);
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod host_tests;
