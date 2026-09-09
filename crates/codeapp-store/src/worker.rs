//! `Store`: async facade over a single SQLite writer thread.
//!
//! Implementation: `std::sync::mpsc` of boxed closures `FnOnce(&mut Connection) ->
//! Result<..>` with a `tokio::sync::oneshot` for the reply; the thread opens the
//! connection (WAL, `synchronous=NORMAL`, `cache_size=-2000`, `foreign_keys=ON`),
//! runs `schema::migrate`, then loops. `Store` is `Clone` (cheap); dropping the last
//! clone stops the thread.

use std::path::Path;

use codeapp_types::{
    AgentId, AgentInfo, Message, Plan, SessionId, SessionMeta, TaskInfo, Usage, UsageTotals,
};

use crate::StoreError;
use crate::queries::{MessageRow, SearchHit, SessionFilter};

#[derive(Clone)]
pub struct Store {
    _private: (),
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let _ = path;
        todo!("Store::open")
    }

    pub fn open_in_memory() -> Result<Store, StoreError> {
        todo!("Store::open_in_memory")
    }

    // --- sessions ---
    pub async fn upsert_session(&self, meta: &SessionMeta) -> Result<(), StoreError> {
        let _ = meta;
        todo!("Store::upsert_session")
    }

    pub async fn get_session(&self, id: &SessionId) -> Result<Option<SessionMeta>, StoreError> {
        let _ = id;
        todo!("Store::get_session")
    }

    /// Most recently updated first.
    pub async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionMeta>, StoreError> {
        let _ = filter;
        todo!("Store::list_sessions")
    }

    /// Deletes the session and everything owned by it.
    pub async fn delete_session(&self, id: &SessionId) -> Result<(), StoreError> {
        let _ = id;
        todo!("Store::delete_session")
    }

    // --- messages ---
    /// `seq` is per (session, agent) and assigned by the core.
    pub async fn append_message(
        &self,
        session: &SessionId,
        agent: &AgentId,
        seq: u64,
        msg: &Message,
    ) -> Result<(), StoreError> {
        let _ = (session, agent, seq, msg);
        todo!("Store::append_message")
    }

    /// All messages of an agent in seq order.
    pub async fn load_messages(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<Vec<MessageRow>, StoreError> {
        let _ = (session, agent);
        todo!("Store::load_messages")
    }

    /// Up to `limit` messages with `seq < before` (or the last ones), ascending.
    pub async fn load_messages_before(
        &self,
        session: &SessionId,
        agent: &AgentId,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRow>, StoreError> {
        let _ = (session, agent, before, limit);
        todo!("Store::load_messages_before")
    }

    // --- agents ---
    pub async fn upsert_agent(
        &self,
        session: &SessionId,
        info: &AgentInfo,
        prompt: Option<&str>,
    ) -> Result<(), StoreError> {
        let _ = (session, info, prompt);
        todo!("Store::upsert_agent")
    }

    pub async fn list_agents(&self, session: &SessionId) -> Result<Vec<AgentInfo>, StoreError> {
        let _ = session;
        todo!("Store::list_agents")
    }

    // --- tasks ---
    pub async fn upsert_task(&self, info: &TaskInfo) -> Result<(), StoreError> {
        let _ = info;
        todo!("Store::upsert_task")
    }

    pub async fn list_tasks(&self, session: &SessionId) -> Result<Vec<TaskInfo>, StoreError> {
        let _ = session;
        todo!("Store::list_tasks")
    }

    // --- plan ---
    pub async fn save_plan(&self, session: &SessionId, plan: &Plan) -> Result<(), StoreError> {
        let _ = (session, plan);
        todo!("Store::save_plan")
    }

    pub async fn load_plan(&self, session: &SessionId) -> Result<Plan, StoreError> {
        let _ = session;
        todo!("Store::load_plan")
    }

    // --- usage ---
    pub async fn add_usage(
        &self,
        session: &SessionId,
        agent: &AgentId,
        turn: u32,
        usage: &Usage,
        cost_usd: Option<f64>,
    ) -> Result<(), StoreError> {
        let _ = (session, agent, turn, usage, cost_usd);
        todo!("Store::add_usage")
    }

    /// Sums over the session; `context_*` fields stay zero (the core fills them).
    pub async fn usage_totals(&self, session: &SessionId) -> Result<UsageTotals, StoreError> {
        let _ = session;
        todo!("Store::usage_totals")
    }

    // --- compaction ---
    pub async fn save_compaction(
        &self,
        session: &SessionId,
        agent: &AgentId,
        summary: &str,
        upto_seq: u64,
    ) -> Result<(), StoreError> {
        let _ = (session, agent, summary, upto_seq);
        todo!("Store::save_compaction")
    }

    pub async fn load_compaction(
        &self,
        session: &SessionId,
        agent: &AgentId,
    ) -> Result<Option<(String, u64)>, StoreError> {
        let _ = (session, agent);
        todo!("Store::load_compaction")
    }

    // --- search ---
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, StoreError> {
        let _ = (query, limit);
        todo!("Store::search")
    }

    /// Flush and stop the writer thread.
    pub async fn close(self) {
        todo!("Store::close")
    }
}
