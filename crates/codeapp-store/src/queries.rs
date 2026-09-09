//! Row types, filters, and query functions for `Store`.

use std::path::PathBuf;

use codeapp_types::{AgentId, Message, SessionId};

pub(crate) mod message;
pub(crate) mod session;
pub(crate) mod state;

pub use message::{append_message, load_messages, load_messages_before, search};
pub use session::{delete_session, get_session, list_sessions, upsert_session};
pub use state::{
    add_usage, list_agents, list_tasks, load_compaction, load_plan, save_compaction, save_plan,
    upsert_agent, upsert_task, usage_totals,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionFilter {
    /// Only sessions created in this directory.
    pub cwd: Option<PathBuf>,
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MessageRow {
    pub seq: u64,
    pub message: Message,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub session: SessionId,
    pub agent: AgentId,
    pub seq: u64,
    /// FTS snippet with `[` `]` around matches.
    pub snippet: String,
}
