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
        let decision = self
            .host
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
            .await;
        match decision {
            PermissionDecision::AllowOnce | PermissionDecision::AllowSession => Ok(decision),
            PermissionDecision::Deny => Err(ToolError::Denied("user denied the request".into())),
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
    /// Tool names allowed for the subagent; `None` = the default subagent set.
    pub tools: Option<Vec<String>>,
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

/// Load agent kinds from `*.md` files in `dirs`. On a name clash the file from
/// the earlier dir wins, so pass the project dir before the global one.
/// Missing directories are normal; unreadable or invalid files are skipped and
/// reported as [`AgentKindWarning`]s.
pub fn discover_agent_kinds(dirs: &[PathBuf]) -> (Vec<AgentKindDef>, Vec<AgentKindWarning>) {
    let mut kinds = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();

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

    kinds.sort_by(|a, b| a.name.cmp(&b.name));
    (kinds, warnings)
}

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
    /// permission matrix and the `AllowSession` cache before prompting.
    async fn request_permission(&self, draft: PermissionDraft) -> PermissionDecision;

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
    /// Wait until the task ends, `timeout` elapses, or (when `return_on_progress`) its
    /// progress changes.
    async fn wait_task(
        &self,
        task: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitOutcome;
    async fn kill_task(&self, task: &TaskId) -> Result<(), ToolError>;
    /// Last `lines` lines of the spooled output.
    async fn task_tail(&self, task: &TaskId, lines: usize) -> Result<Vec<String>, ToolError>;
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
    /// Orchestrator → subagent: ask for a brief status. Delivered to the subagent as a
    /// `<status_request>` injection at its next step; it answers with `report_status`.
    fn request_agent_status(&self, agent: &AgentId) -> Result<(), ToolError>;
    /// Subagent → parent: a short status line, delivered as `<agent_status>` at the
    /// parent's next step.
    fn report_status(&self, text: String) -> Result<(), ToolError>;

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
