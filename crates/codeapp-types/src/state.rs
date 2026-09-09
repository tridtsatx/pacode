//! Session state DTOs shared by daemon, store and clients: mode, agents, plan,
//! background tasks, usage, permissions.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, CallId, PermissionId, SessionId, TaskId};
use crate::model::{Effort, ModelRoute};

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
                    title = title.chars().take(39).collect::<String>() + "…";
                }
                title
            }
            None => "new session".to_string(),
        }
    }
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
        )
    }

    /// Working right now (not idle, not terminal).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            AgentStatus::Thinking | AgentStatus::RunningTool | AgentStatus::WaitingApproval
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
        let sum: u32 = self
            .counted()
            .map(|item| match item.status {
                PlanStatus::Done => 100,
                PlanStatus::Pending => 0,
                PlanStatus::Active => item.progress.map(u32::from).unwrap_or(50),
                PlanStatus::Cancelled => 0,
            })
            .sum();
        (sum / total as u32).min(100) as u8
    }

    pub fn is_complete(&self) -> bool {
        self.total() > 0 && self.done() == self.total()
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
        label.chars().take(31).collect::<String>() + "…"
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
            first_prompt: Some("разбери layout.rs и раскидай по модулям, тесты параллельно".into()),
        };
        // six words, exactly 40 chars: no truncation
        assert_eq!(meta.title(), "разбери layout.rs и раскидай по модулям,");
    }
}
