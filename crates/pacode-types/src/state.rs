//! Session state DTOs shared by daemon, store and clients: mode, agents, plan,
//! background tasks, usage, permissions.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, CallId, PermissionId, SessionId, TaskId};
use crate::model::{Effort, ModelRoute};

/// Caps every session DTO field is held to. These DTOs are written by a model, a
/// tool, a plugin or a client, stored, rendered, and often handed straight back to
/// the model, so each one states the ceiling its contract has to survive: a name
/// in a rail, a command in a row, a diff preview in a pane, a detail block in a
/// prompt. [`check_chars`], [`check_bytes`] and [`check_count`] are what
/// [`LimitError`] comes from, and each `validate` names the field that broke.
pub const ID_MAX_CHARS: usize = 128;
pub const NAME_MAX_CHARS: usize = 120;
pub const TITLE_MAX_CHARS: usize = 200;
pub const TEXT_MAX_CHARS: usize = 4_000;
pub const SUMMARY_MAX_CHARS: usize = 2_000;
pub const PATH_MAX_BYTES: usize = 4_096;
pub const DETAIL_MAX_BYTES: usize = 64 * 1024;
/// Steps in a plan. A plan longer than this is a model looping, not a plan.
pub const PLAN_MAX_ITEMS: usize = 500;

/// How a field is measured against its cap: characters for what a human reads,
/// bytes for what is stored or fed to a model, items for what is collected,
/// seconds for what the scheduler waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitUnit {
    Chars,
    Bytes,
    Items,
    Seconds,
}

impl fmt::Display for LimitUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chars => f.write_str("chars"),
            Self::Bytes => f.write_str("bytes"),
            Self::Items => f.write_str("items"),
            Self::Seconds => f.write_str("seconds"),
        }
    }
}

/// A DTO field past the cap its contract sets. `pacode-types` carries no error
/// crate, so `Display` is spelled out the way `question` and `cron_expr` do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitError {
    pub field: &'static str,
    pub actual: usize,
    pub limit: usize,
    pub unit: LimitUnit,
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is {} {} over the limit of {} {}",
            self.field, self.actual, self.unit, self.limit, self.unit
        )
    }
}

impl std::error::Error for LimitError {}

/// Build the error a capped field produces, for callers that measure something
/// the three `check_*` helpers do not cover.
pub fn limit_error(
    field: &'static str,
    actual: usize,
    limit: usize,
    unit: LimitUnit,
) -> LimitError {
    LimitError {
        field,
        actual,
        limit,
        unit,
    }
}

/// Hold a human-facing string to `max` characters.
pub fn check_chars(field: &'static str, value: &str, max: usize) -> Result<(), LimitError> {
    let actual = value.chars().count();
    if actual > max {
        return Err(limit_error(field, actual, max, LimitUnit::Chars));
    }
    Ok(())
}

/// Hold a stored or model-facing string to `max` bytes.
pub fn check_bytes(field: &'static str, value: &str, max: usize) -> Result<(), LimitError> {
    let actual = value.len();
    if actual > max {
        return Err(limit_error(field, actual, max, LimitUnit::Bytes));
    }
    Ok(())
}

/// Hold a collection to `max` entries.
pub fn check_count(field: &'static str, actual: usize, max: usize) -> Result<(), LimitError> {
    if actual > max {
        return Err(limit_error(field, actual, max, LimitUnit::Items));
    }
    Ok(())
}

/// Cut a string to `max` characters, for the display paths that cannot refuse a
/// DTO they were handed.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// The single permission/behaviour axis, cycled with `shift+tab`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Ask before edits and non-trivial commands.
    #[default]
    Build,
    /// Auto-accept edits and low-risk commands; ask only for `Confirm` risk.
    Auto,
    /// Read-only tools and the plan tool.
    Plan,
    /// No prompts except the catastrophic deny.
    Bypass,
}

impl Mode {
    pub const CYCLE: [Mode; 4] = [Mode::Build, Mode::Auto, Mode::Plan, Mode::Bypass];

    pub fn next(self) -> Mode {
        match self {
            Mode::Build => Mode::Auto,
            Mode::Auto => Mode::Plan,
            Mode::Plan => Mode::Bypass,
            Mode::Bypass => Mode::Build,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Build => "build",
            Mode::Auto => "auto",
            Mode::Plan => "plan",
            Mode::Bypass => "bypass",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "build" => Some(Mode::Build),
            "auto" => Some(Mode::Auto),
            "plan" => Some(Mode::Plan),
            "bypass" => Some(Mode::Bypass),
            _ => None,
        }
    }

    /// Footer row-1 label.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Build => "Build",
            Mode::Auto => "Auto",
            Mode::Plan => "Plan",
            Mode::Bypass => "Bypass",
        }
    }

    /// Footer row-2 description.
    pub fn permission_line(self) -> &'static str {
        match self {
            Mode::Build => "ask before edits and commands",
            Mode::Auto => "auto-accept edits and safe commands",
            Mode::Plan => "read-only, plan mode",
            Mode::Bypass => "bypass permissions on",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub model: ModelRoute,
    pub effort: Effort,
    pub mode: Mode,
    /// First user prompt, used as the title before the model names the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    /// Zero-data-retention session toggle (`zdr = "session"`): blocks network
    /// tools, MCP calls and subagent spawns, and restricts model switching to
    /// routes flagged `zdr_capable`.
    #[serde(default)]
    pub zdr: bool,
}

impl SessionMeta {
    /// Title for the rail header: model-generated name, else first words of the prompt.
    pub fn title(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        match &self.first_prompt {
            Some(prompt) => {
                let words: Vec<&str> = prompt.split_whitespace().take(6).collect();
                let mut title = words.join(" ");
                if title.chars().count() > 40 {
                    title = format!("{}…", truncate_chars(&title, 39));
                }
                title
            }
            None => "new session".to_string(),
        }
    }

    /// Whether the session header is within the caps of the session DTOs.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_chars("session id", self.id.as_str(), ID_MAX_CHARS)?;
        check_chars(
            "session name",
            self.name.as_deref().unwrap_or(""),
            NAME_MAX_CHARS,
        )?;
        check_bytes("session cwd", &self.cwd.to_string_lossy(), PATH_MAX_BYTES)?;
        check_chars(
            "session git branch",
            self.git_branch.as_deref().unwrap_or(""),
            TITLE_MAX_CHARS,
        )?;
        check_chars(
            "session first prompt",
            self.first_prompt.as_deref().unwrap_or(""),
            TEXT_MAX_CHARS,
        )?;
        check_route(&self.model)
    }
}

/// Hold a model route's two halves to the caps of the session DTOs.
pub fn check_route(route: &ModelRoute) -> Result<(), LimitError> {
    check_chars("model provider", &route.provider, NAME_MAX_CHARS)?;
    check_chars("model name", &route.model, NAME_MAX_CHARS)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Main,
    Sub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Thinking,
    RunningTool,
    WaitingApproval,
    /// A subagent blocked in `ask_parent` until the orchestrator answers.
    /// `waiting_orchestrator` is the name a pre-merge branch build wrote.
    #[serde(alias = "waiting_orchestrator")]
    WaitingParent,
    Finished,
    Failed,
    Stopped,
}

impl AgentStatus {
    pub fn is_live(self) -> bool {
        matches!(
            self,
            AgentStatus::Idle
                | AgentStatus::Thinking
                | AgentStatus::RunningTool
                | AgentStatus::WaitingApproval
                | AgentStatus::WaitingParent
        )
    }

    /// Working right now (not idle, not terminal).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            AgentStatus::Thinking
                | AgentStatus::RunningTool
                | AgentStatus::WaitingApproval
                | AgentStatus::WaitingParent
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub status: AgentStatus,
    /// Last tool call or state text shown on the agent card (`Reading foo.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub model: ModelRoute,
    pub effort: Effort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<AgentId>,
    /// Final answer preview once finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AgentInfo {
    pub fn duration_ms(&self, now_ms: u64) -> u64 {
        self.finished_at_ms
            .unwrap_or(now_ms)
            .saturating_sub(self.started_at_ms)
    }

    /// Whether the agent card is within the caps of the session DTOs.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_chars("agent id", self.id.as_str(), ID_MAX_CHARS)?;
        check_chars("agent name", &self.name, NAME_MAX_CHARS)?;
        check_chars(
            "agent activity",
            self.activity.as_deref().unwrap_or(""),
            TITLE_MAX_CHARS,
        )?;
        check_chars(
            "agent summary",
            self.summary.as_deref().unwrap_or(""),
            SUMMARY_MAX_CHARS,
        )?;
        check_chars(
            "agent error",
            self.error.as_deref().unwrap_or(""),
            SUMMARY_MAX_CHARS,
        )?;
        check_route(&self.model)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    Active,
    Done,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub content: String,
    pub status: PlanStatus,
    /// 0..=100, only meaningful for the active item and only when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub version: u64,
    pub items: Vec<PlanItem>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Items that count (cancelled excluded).
    pub fn counted(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|item| item.status != PlanStatus::Cancelled)
    }

    pub fn total(&self) -> usize {
        self.counted().count()
    }

    pub fn done(&self) -> usize {
        self.counted()
            .filter(|item| item.status == PlanStatus::Done)
            .count()
    }

    pub fn active(&self) -> Option<&PlanItem> {
        self.items
            .iter()
            .find(|item| item.status == PlanStatus::Active)
    }

    /// Overall percent per the TUI spec: done = 100, pending = 0, active = its value
    /// or 50 when not reported; average over counted items.
    pub fn percent(&self) -> u8 {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        let sum: u64 = self
            .counted()
            .map(|item| match item.status {
                PlanStatus::Done => 100,
                PlanStatus::Pending => 0,
                PlanStatus::Active => u64::from(item.progress.unwrap_or(50)),
                PlanStatus::Cancelled => 0,
            })
            .sum();
        let total = u64::try_from(total).unwrap_or(u64::MAX);
        (sum / total).min(100) as u8
    }

    pub fn is_complete(&self) -> bool {
        self.total() > 0 && self.done() == self.total()
    }

    /// Whether the plan is within the caps of the session DTOs.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_count("plan items", self.items.len(), PLAN_MAX_ITEMS)?;
        for item in &self.items {
            check_chars("plan item id", &item.id, ID_MAX_CHARS)?;
            check_chars("plan item content", &item.content, TEXT_MAX_CHARS)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, TaskStatus::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressSource {
    /// The agent reported it through the `bg` tool.
    Reported,
    /// Parsed from the process output by a known pattern.
    Parsed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub source: ProgressSource,
    pub updated_at_ms: u64,
}

impl TaskProgress {
    /// Counts win over a conflicting percent (port of jcode's normalize).
    pub fn normalize(mut self) -> Self {
        if let (Some(current), Some(total)) = (self.current, self.total)
            && total > 0
        {
            self.percent = Some(((current as f64 / total as f64) * 100.0) as f32);
        }
        self.percent = self.percent.map(|p| p.clamp(0.0, 100.0));
        self
    }

    /// `214/380` or `60%` or the message.
    pub fn short_label(&self) -> Option<String> {
        if let (Some(current), Some(total)) = (self.current, self.total) {
            return Some(format!("{current}/{total}"));
        }
        if let Some(percent) = self.percent {
            return Some(format!("{}%", percent.round() as u32));
        }
        self.message.clone()
    }

    /// Whether the progress line is within the caps of the session DTOs.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_chars(
            "progress message",
            self.message.as_deref().unwrap_or(""),
            TITLE_MAX_CHARS,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: TaskId,
    pub session: SessionId,
    pub owner: AgentId,
    /// Short display label, e.g. `cargo build`.
    pub label: String,
    pub command: String,
    pub cwd: PathBuf,
    pub status: TaskStatus,
    /// True once the task outlived its foreground wait or was started with `background: true`.
    pub backgrounded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<TaskProgress>,
    pub warnings: u32,
    pub errors: u32,
    pub output_path: PathBuf,
    pub output_bytes: u64,
    /// A failed task keeps the red counter until acknowledged (opened or `AckTask`).
    pub acked: bool,
}

impl TaskInfo {
    pub fn duration_ms(&self, now_ms: u64) -> u64 {
        self.ended_at_ms
            .unwrap_or(now_ms)
            .saturating_sub(self.started_at_ms)
    }

    pub fn is_failed_unacked(&self) -> bool {
        matches!(self.status, TaskStatus::Failed) && !self.acked
    }

    /// Whether the task card is within the caps of the session DTOs.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_chars("task id", self.id.as_str(), ID_MAX_CHARS)?;
        check_chars("task session", self.session.as_str(), ID_MAX_CHARS)?;
        check_chars("task owner", self.owner.as_str(), ID_MAX_CHARS)?;
        check_chars("task label", &self.label, NAME_MAX_CHARS)?;
        check_chars("task command", &self.command, TEXT_MAX_CHARS)?;
        check_bytes("task cwd", &self.cwd.to_string_lossy(), PATH_MAX_BYTES)?;
        check_bytes(
            "task output path",
            &self.output_path.to_string_lossy(),
            PATH_MAX_BYTES,
        )?;
        if let Some(progress) = &self.progress {
            progress.validate()?;
        }
        Ok(())
    }
}

/// Derive the rail label from a command: first two words, `cargo build --release` → `cargo build`.
pub fn task_label_from_command(command: &str) -> String {
    let mut words = command
        .split_whitespace()
        .filter(|w| !w.contains('='))
        .take(2)
        .collect::<Vec<_>>();
    if words.len() == 2 && words[1].starts_with('-') {
        words.truncate(1);
    }
    if words.is_empty() {
        return "task".to_string();
    }
    let label = words.join(" ");
    if label.chars().count() > 32 {
        format!("{}…", truncate_chars(&label, 31))
    } else {
        label
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub turns: u32,
    /// Only when pricing is configured for the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Last known size of the main agent's context (tokens).
    pub context_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    pub started_at_ms: u64,
    pub last_activity_ms: u64,
}

impl UsageTotals {
    pub fn cache_hit_percent(&self) -> Option<f32> {
        if self.input == 0 {
            return None;
        }
        Some(self.cache_read as f32 / self.input as f32 * 100.0)
    }

    pub fn context_percent(&self) -> Option<u8> {
        let window = self.context_window?;
        if window == 0 {
            return None;
        }
        Some(((self.context_tokens as u64 * 100) / window as u64).min(100) as u8)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    Low,
    Confirm,
    Catastrophic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    AllowSession,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: PermissionId,
    /// The asking agent. Serialized as `asking_agent` (alias `agent` for
    /// older payloads): inside a flattened `TranscriptItem` the outer
    /// `agent` field is authoritative and an inner `agent` key would
    /// duplicate-collide on deserialize.
    #[serde(rename = "asking_agent", alias = "agent")]
    pub agent: AgentId,
    pub agent_name: String,
    pub call_id: CallId,
    pub tool: String,
    /// One line: `Bash: cargo build --release` or `Edit: src/main.rs`.
    pub title: String,
    /// Multi-line detail: the command, or a diff preview.
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskLevel>,
    pub created_at_ms: u64,
}

impl PermissionRequest {
    /// Whether the prompt is within the caps of the session DTOs. The detail is
    /// measured in bytes: it is a command or a diff on its way to the model.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_chars("permission id", self.id.as_str(), ID_MAX_CHARS)?;
        check_chars("permission agent", self.agent.as_str(), ID_MAX_CHARS)?;
        check_chars("permission call id", self.call_id.as_str(), ID_MAX_CHARS)?;
        check_chars("permission agent name", &self.agent_name, NAME_MAX_CHARS)?;
        check_chars("permission tool", &self.tool, NAME_MAX_CHARS)?;
        check_chars("permission title", &self.title, TITLE_MAX_CHARS)?;
        check_bytes("permission detail", &self.detail, DETAIL_MAX_BYTES)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastLevel {
    Info,
    Success,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names are what the store and old clients already hold: adding
    /// `waiting_parent` must not move any of them, and the new one is live.
    #[test]
    fn agent_status_wire_names_are_stable() {
        let names = [
            (AgentStatus::Idle, "idle"),
            (AgentStatus::Thinking, "thinking"),
            (AgentStatus::RunningTool, "running_tool"),
            (AgentStatus::WaitingApproval, "waiting_approval"),
            (AgentStatus::WaitingParent, "waiting_parent"),
            (AgentStatus::Finished, "finished"),
            (AgentStatus::Failed, "failed"),
            (AgentStatus::Stopped, "stopped"),
        ];
        for (status, name) in names {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            let back: AgentStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status);
        }
        assert!(AgentStatus::WaitingParent.is_live());
        assert!(AgentStatus::WaitingParent.is_active());
    }

    /// A pre-merge branch build serialized the same status as
    /// `waiting_orchestrator`; it still decodes, and re-encodes under the new name.
    #[test]
    fn agent_status_accepts_the_old_waiting_orchestrator_name() {
        let back: AgentStatus = serde_json::from_str("\"waiting_orchestrator\"").unwrap();
        assert_eq!(back, AgentStatus::WaitingParent);
        assert_eq!(serde_json::to_string(&back).unwrap(), "\"waiting_parent\"");
    }

    fn item(status: PlanStatus, progress: Option<u8>) -> PlanItem {
        PlanItem {
            id: "x".into(),
            content: "c".into(),
            status,
            progress,
        }
    }

    #[test]
    fn plan_percent_matches_spec_example() {
        // [✓] [•60%] [ ] [ ] → (100 + 60 + 0 + 0) / 4 = 40; the mockup's 43% used 70.
        let plan = Plan {
            version: 1,
            items: vec![
                item(PlanStatus::Done, None),
                item(PlanStatus::Active, Some(60)),
                item(PlanStatus::Pending, None),
                item(PlanStatus::Pending, None),
            ],
        };
        assert_eq!(plan.percent(), 40);
        assert_eq!(plan.done(), 1);
        assert_eq!(plan.total(), 4);
        let plan = Plan {
            version: 1,
            items: vec![item(PlanStatus::Done, None), item(PlanStatus::Active, None)],
        };
        assert_eq!(plan.percent(), 75);
        assert_eq!(Plan::default().percent(), 0);
    }

    #[test]
    fn mode_cycle() {
        assert_eq!(Mode::Build.next(), Mode::Auto);
        assert_eq!(Mode::Bypass.next(), Mode::Build);
    }

    #[test]
    fn task_labels() {
        assert_eq!(
            task_label_from_command("cargo build --release"),
            "cargo build"
        );
        assert_eq!(task_label_from_command("npm run lint"), "npm run");
        assert_eq!(task_label_from_command("FOO=1 cargo test"), "cargo test");
        assert_eq!(task_label_from_command("ls -la"), "ls");
    }

    #[test]
    fn progress_normalize() {
        let p = TaskProgress {
            current: Some(2),
            total: Some(10),
            percent: Some(80.0),
            message: None,
            source: ProgressSource::Reported,
            updated_at_ms: 0,
        }
        .normalize();
        assert_eq!(p.percent, Some(20.0));
        assert_eq!(p.short_label().as_deref(), Some("2/10"));
    }

    #[test]
    fn session_title_fallback() {
        let meta = SessionMeta {
            id: SessionId::new("ses_1"),
            name: None,
            cwd: PathBuf::from("/tmp"),
            git_branch: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            model: ModelRoute::new("p", "m"),
            effort: Effort::High,
            mode: Mode::Build,
            first_prompt: Some(
                "review layout.rs and split into modules, run tests in parallel".into(),
            ),
            zdr: false,
        };
        // six words, exactly 40 chars: no truncation
        assert_eq!(meta.title(), "review layout.rs and split into modules,");

        let meta = SessionMeta {
            first_prompt: Some(
                "refactoring the scheduler carefully, split it, and keep the tests green".into(),
            ),
            ..meta
        };
        let title = meta.title();
        assert_eq!(title.chars().count(), 40);
        assert!(title.ends_with('…'), "{title}");
    }

    fn meta() -> SessionMeta {
        SessionMeta {
            id: SessionId::new("ses_1"),
            name: None,
            cwd: PathBuf::from("/tmp"),
            git_branch: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            model: ModelRoute::new("p", "m"),
            effort: Effort::High,
            mode: Mode::Build,
            first_prompt: None,
            zdr: false,
        }
    }

    fn agent() -> AgentInfo {
        AgentInfo {
            id: AgentId::new("agt_1"),
            name: "main".into(),
            kind: AgentKind::Main,
            status: AgentStatus::Idle,
            activity: None,
            started_at_ms: 0,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: ModelRoute::new("p", "m"),
            effort: Effort::High,
            parent: None,
            summary: None,
            error: None,
        }
    }

    fn task() -> TaskInfo {
        TaskInfo {
            id: TaskId::new("tsk_1"),
            session: SessionId::new("ses_1"),
            owner: AgentId::main(),
            label: "cargo build".into(),
            command: "cargo build".into(),
            cwd: PathBuf::from("/tmp"),
            status: TaskStatus::Running,
            backgrounded: false,
            exit_code: None,
            started_at_ms: 0,
            ended_at_ms: None,
            progress: None,
            warnings: 0,
            errors: 0,
            output_path: PathBuf::from("/tmp/out.log"),
            output_bytes: 0,
            acked: false,
        }
    }

    fn permission() -> PermissionRequest {
        PermissionRequest {
            id: PermissionId::new("perm_1"),
            agent: AgentId::main(),
            agent_name: "main".into(),
            call_id: CallId::new("call_1"),
            tool: "Bash".into(),
            title: "Bash: cargo build".into(),
            detail: "cargo build --release".into(),
            risk: Some(RiskLevel::Low),
            created_at_ms: 0,
        }
    }

    #[test]
    fn limits_report_the_field_and_its_ceiling() {
        let error = check_chars(
            "session name",
            &"x".repeat(NAME_MAX_CHARS + 1),
            NAME_MAX_CHARS,
        )
        .expect_err("one char over the cap");
        assert_eq!(error.field, "session name");
        assert_eq!(error.actual, NAME_MAX_CHARS + 1);
        assert_eq!(error.limit, NAME_MAX_CHARS);
        assert_eq!(error.unit, LimitUnit::Chars);
        assert_eq!(
            error.to_string(),
            format!(
                "session name is {} chars over the limit of {} chars",
                NAME_MAX_CHARS + 1,
                NAME_MAX_CHARS
            )
        );

        let error = check_bytes("cwd", &"x".repeat(9), 8).expect_err("one byte over the cap");
        assert_eq!(error.unit, LimitUnit::Bytes);
        assert_eq!(error.actual, 9);

        let error = check_count("plan items", 3, 2).expect_err("one item over the cap");
        assert_eq!(error.unit, LimitUnit::Items);

        assert!(check_chars("name", "x", 1).is_ok());
        assert!(check_bytes("cwd", "x", 1).is_ok());
        assert!(check_count("items", 2, 2).is_ok());
    }

    #[test]
    fn caps_are_measured_in_the_unit_they_are_written_in() {
        let text = "éé";
        assert_eq!(text.len(), 4);
        assert!(check_bytes("detail", text, 4).is_ok());
        assert!(check_bytes("detail", text, 3).is_err());
        assert!(check_chars("title", text, 2).is_ok());
        assert!(check_chars("title", text, 1).is_err());
    }

    #[test]
    fn truncate_chars_keeps_the_prefix_and_the_characters_intact() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("", 10), "");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        let cut = truncate_chars("привет", 3);
        assert_eq!(cut, "при");
        assert_eq!(cut.chars().count(), 3);
    }

    #[test]
    fn session_dtos_within_the_caps_validate() {
        assert!(meta().validate().is_ok());
        assert!(agent().validate().is_ok());
        assert!(task().validate().is_ok());
        assert!(permission().validate().is_ok());
        assert!(Plan::default().validate().is_ok());
    }

    #[test]
    fn session_validate_names_the_field_that_broke_the_cap() {
        let error = SessionMeta {
            name: Some("x".repeat(NAME_MAX_CHARS + 1)),
            ..meta()
        }
        .validate()
        .expect_err("overlong session name");
        assert_eq!(error.field, "session name");

        let error = SessionMeta {
            first_prompt: Some("x".repeat(TEXT_MAX_CHARS + 1)),
            ..meta()
        }
        .validate()
        .expect_err("overlong first prompt");
        assert_eq!(error.field, "session first prompt");

        let error = SessionMeta {
            model: ModelRoute::new("provider", "m".repeat(NAME_MAX_CHARS + 1)),
            ..meta()
        }
        .validate()
        .expect_err("overlong model name");
        assert_eq!(error.field, "model name");

        let error = AgentInfo {
            activity: Some("x".repeat(TITLE_MAX_CHARS + 1)),
            ..agent()
        }
        .validate()
        .expect_err("overlong activity");
        assert_eq!(error.field, "agent activity");

        let error = TaskInfo {
            command: "x".repeat(TEXT_MAX_CHARS + 1),
            ..task()
        }
        .validate()
        .expect_err("overlong command");
        assert_eq!(error.field, "task command");

        let error = PermissionRequest {
            detail: "x".repeat(DETAIL_MAX_BYTES + 1),
            ..permission()
        }
        .validate()
        .expect_err("overlong detail");
        assert_eq!(error.field, "permission detail");
    }

    #[test]
    fn plan_validate_caps_items_and_step_text() {
        let plan = Plan {
            version: 1,
            items: vec![item(PlanStatus::Pending, None); PLAN_MAX_ITEMS],
        };
        assert!(plan.validate().is_ok());

        let plan = Plan {
            version: 1,
            items: vec![item(PlanStatus::Pending, None); PLAN_MAX_ITEMS + 1],
        };
        let error = plan.validate().expect_err("one step over the cap");
        assert_eq!(error.field, "plan items");
        assert_eq!(error.unit, LimitUnit::Items);

        let plan = Plan {
            version: 1,
            items: vec![PlanItem {
                content: "x".repeat(TEXT_MAX_CHARS + 1),
                ..item(PlanStatus::Pending, None)
            }],
        };
        assert_eq!(
            plan.validate().expect_err("overlong step").field,
            "plan item content"
        );
    }

    #[test]
    fn plan_percent_averages_over_a_long_plan() {
        let plan = Plan {
            version: 1,
            items: vec![item(PlanStatus::Done, None); 9_000],
        };
        assert_eq!(plan.percent(), 100);

        let mut items = vec![item(PlanStatus::Done, None); 9_000];
        items.push(item(PlanStatus::Active, Some(0)));
        let plan = Plan { version: 1, items };
        assert_eq!(plan.percent(), 99);
    }
}
