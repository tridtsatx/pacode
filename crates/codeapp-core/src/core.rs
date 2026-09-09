//! `Core`: the daemon-facing API.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use codeapp_config::Paths;
use codeapp_exec::TaskManager;
use codeapp_mcp::McpPool;
use codeapp_provider::ProviderRegistry;
use codeapp_store::Store;
use codeapp_tools::ToolRegistry;
use codeapp_types::{Attach, Config, Event, Reply, Request, SessionId, SessionSnapshot};
use tokio::sync::broadcast;

use crate::CoreError;
use crate::session::Session;

/// Everything the core needs, built by the daemon.
pub struct CoreDeps {
    pub config: Arc<Config>,
    pub paths: Paths,
    pub providers: Arc<ProviderRegistry>,
    /// Built-in tools; MCP tools are appended per session from `mcp`.
    pub tools: ToolRegistry,
    pub tasks: Arc<TaskManager>,
    pub mcp: Arc<McpPool>,
    pub store: Store,
    /// Shown in the rail anchor and in `SessionMeta` logs.
    pub app_version: String,
}

pub struct Core {
    pub(crate) deps: CoreDeps,
    pub(crate) sessions: RwLock<BTreeMap<SessionId, Arc<Session>>>,
    /// Routes `TaskEvent`s to the owning session (injections + UI events).
    pub(crate) task_router: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Core {
    /// Build the core and start the task-event router.
    pub async fn new(deps: CoreDeps) -> Arc<Core> {
        let _ = deps;
        todo!("Core::new")
    }

    /// `Attach::New` creates a session (persisted immediately); `Resume` loads meta,
    /// plan, agents (as finished), tasks (as finished/killed) and the main agent's
    /// history (compaction summary + messages after it) from the store; `Latest`
    /// resumes the most recently updated session for `cwd` or creates one.
    pub async fn open_session(&self, attach: Attach) -> Result<SessionId, CoreError> {
        let _ = attach;
        todo!("Core::open_session")
    }

    pub fn session(&self, id: &SessionId) -> Option<Arc<Session>> {
        self.sessions.read().ok()?.get(id).cloned()
    }

    pub fn session_ids(&self) -> Vec<SessionId> {
        self.sessions
            .read()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Event stream of a session: `(seq, event)`, seq monotonic per session.
    pub fn subscribe(&self, id: &SessionId) -> Option<broadcast::Receiver<(u64, Event)>> {
        self.session(id).map(|s| s.events.subscribe())
    }

    /// Handle every request except the connection-level ones (`Hello`, `Attach`,
    /// `Detach`, `Shutdown`, `Ping`), which the daemon answers itself. Unknown or
    /// inapplicable requests return `Reply::Error`.
    pub async fn handle(&self, id: &SessionId, req: Request) -> Reply {
        let _ = (id, req);
        todo!("Core::handle")
    }

    pub fn snapshot(&self, id: &SessionId) -> Option<SessionSnapshot> {
        self.session(id).map(|s| s.snapshot())
    }

    /// No running turns, no live subagents, no running tasks in any session.
    pub fn is_idle(&self) -> bool {
        todo!("Core::is_idle")
    }

    /// Stop agents, kill the session's tasks, persist, drop from memory.
    pub async fn close_session(&self, id: &SessionId) {
        let _ = id;
        todo!("Core::close_session")
    }

    /// Close every session, stop the router, shut down tasks and MCP servers, close
    /// the store.
    pub async fn shutdown(&self) {
        todo!("Core::shutdown")
    }
}
