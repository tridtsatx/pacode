//! Session and agent runtime (spec §6, §8, §10). The daemon owns one [`Core`]; every
//! session, agent, turn, injection, permission and plan lives here. No sockets, no
//! terminal types.
//!
//! Module map:
//! - `core`: [`Core`] — sessions, request routing, task-event routing, idle detection
//! - `session`: [`Session`] — agents, plan, usage, permissions, event sink
//! - `agent`: [`Agent`] — history, status, transcript bookkeeping
//! - `turn`: the turn loop (§6.2) with injection points B/C/D
//! - `inject`: [`InjectionQueue`] and rendering of injections into user messages
//! - `permissions`: the mode × kind × risk matrix and the AllowSession cache
//! - `prompt`: system prompt (static + dynamic), AGENTS.md/CLAUDE.md discovery
//! - `compaction`: threshold check and summary turn
//! - `transcript`: history → `TranscriptItem`, event sink with seq, delta coalescing
//! - `host`: `ToolHost` implementation handed to tools
//! - `naming`: session title generation after the first reply

pub mod agent;
pub mod compaction;
pub mod core;
pub mod expand;
pub mod host;
pub mod inject;
pub mod naming;
pub mod permissions;
pub mod prompt;
pub mod sampling;
pub mod session;
pub mod transcript;
pub mod turn;

pub use core::{Core, CoreDeps};
pub use sampling::CoreSamplingHandler;
pub use session::Session;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("session not found: {0}")]
    SessionNotFound(pacode_types::SessionId),
    #[error("agent not found: {0}")]
    AgentNotFound(pacode_types::AgentId),
    #[error("no model configured: set [provider].default or pass --model")]
    NoModel,
    #[error("provider: {0}")]
    Provider(#[from] pacode_provider::ProviderError),
    #[error("store: {0}")]
    Store(#[from] pacode_store::StoreError),
    #[error("exec: {0}")]
    Exec(#[from] pacode_exec::ExecError),
    #[error("{0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
