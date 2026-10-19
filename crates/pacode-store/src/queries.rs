//! Row types, filters, and query functions for `Store`.

use std::path::PathBuf;

use pacode_types::{AgentId, ClientId, Message, SessionId, SessionMeta, TaskId, TurnId};

pub(crate) mod clear;
pub(crate) mod cron;
pub(crate) mod message;
pub(crate) mod session;
pub(crate) mod state;

pub use clear::clear_context;
pub use cron::{delete_cron_job, list_cron_jobs, upsert_cron_job};
pub use message::{
    DEFAULT_ADMISSION_PAGE, DEFAULT_MESSAGE_PAGE, append_message, append_user_message,
    append_user_message_with_turn, cancelled_message_seqs, get_message_admission,
    load_message_admissions, load_message_admissions_for_session, load_message_admissions_page,
    load_messages, load_messages_after, load_messages_before, load_unresolved_message_admissions,
    mark_message_admission_running, mark_message_admission_terminal, max_message_seq, search,
};
pub use session::{
    DEFAULT_SESSION_PAGE, admit_attach, admit_task, delete_session, get_attach_admission,
    get_session, get_task_admission, get_task_admission_by_task, list_sessions, list_sessions_page,
    upsert_session,
};
pub use state::CompactionRecord;
pub use state::{
    add_usage, add_usage_for_turn, agent_restore, create_agent_with_history, detach_agent,
    list_agents, list_tasks, load_compaction, load_plan, reassign_task_owner, record_compaction,
    rollback_detach, save_compaction, save_plan, set_agent_stopped_by, upsert_agent, upsert_task,
    upsert_task_checked, usage_totals,
};

/// A subagent's restore state beyond its `AgentInfo` (see
/// [`agent_restore`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentRestore {
    /// The brief it was spawned with.
    pub prompt: Option<String>,
    /// Who stopped it, as recorded (`"user"` / `"orchestrator"`).
    pub stopped_by: Option<String>,
    /// The tool names it was spawned with; `None` for an agent created
    /// without a narrowing on record (older rows, a detached turn).
    pub tools: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionFilter {
    /// Only sessions created in this directory.
    pub cwd: Option<PathBuf>,
    pub limit: u32,
}

/// Keyset position in the `(updated_at DESC, id ASC)` session ordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCursor {
    pub updated_at_ms: u64,
    pub id: SessionId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionPage {
    pub sessions: Vec<SessionMeta>,
    pub next_cursor: Option<SessionCursor>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MessageRow {
    pub seq: u64,
    pub message: Message,
}

/// One bounded read of a message window, ascending by `seq`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessagePage {
    pub rows: Vec<MessageRow>,
    /// More rows exist past the returned window.
    pub has_more: bool,
}

/// Keyset position in the `(session_id, message_seq, message_id)` admission order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionCursor {
    pub session: SessionId,
    pub message_seq: Option<u64>,
    pub message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageAdmissionPage {
    pub admissions: Vec<MessageAdmission>,
    pub next_cursor: Option<AdmissionCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAdmissionResult {
    Appended,
    Duplicate,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAdmissionState {
    Admitted,
    Running,
    Terminal,
}

impl MessageAdmissionState {
    pub(crate) fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "admitted" => Ok(Self::Admitted),
            "running" => Ok(Self::Running),
            "terminal" => Ok(Self::Terminal),
            other => Err(format!("invalid message admission state {other:?}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageAdmission {
    pub session: SessionId,
    pub message_id: String,
    pub fingerprint: u64,
    pub agent: AgentId,
    pub message_seq: Option<u64>,
    pub state: MessageAdmissionState,
    pub turn_id: Option<TurnId>,
    pub terminal_stop: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachAdmission {
    pub client_id: ClientId,
    pub idempotency_key: String,
    pub session_id: SessionId,
    pub fingerprint: String,
    pub allow_create: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachAdmissionResult {
    Admitted(AttachAdmission),
    Duplicate(AttachAdmission),
    Conflict(AttachAdmission),
}

/// A task id reserved for one logical start request, so a replayed start cannot
/// spawn a second process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskAdmission {
    pub session: SessionId,
    pub idempotency_key: String,
    pub task_id: TaskId,
    pub owner: AgentId,
    pub fingerprint: String,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskAdmissionResult {
    Admitted(TaskAdmission),
    Duplicate(TaskAdmission),
    Conflict(TaskAdmission),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub session: SessionId,
    pub agent: AgentId,
    pub seq: u64,
    /// FTS snippet with `[` `]` around matches.
    pub snippet: String,
}
