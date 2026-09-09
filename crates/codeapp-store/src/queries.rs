//! Row types and filters for `Store`.

use std::path::PathBuf;

use codeapp_types::{AgentId, Message, SessionId};

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
