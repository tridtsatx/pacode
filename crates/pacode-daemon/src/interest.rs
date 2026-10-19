//! Per-connection event interest (`Request::Subscribe`).
//!
//! A TUI draws the main agent and at most one panel target, yet used to receive every
//! delta of every subagent. [`Interest`] says which agents' high-volume events
//! (`TextDelta`, `ReasoningDelta`, `ItemAdded`, `ItemUpdated`, `AgentUpdated`) a
//! connection wants; [`crate::fanout`] drops or coalesces the rest.
//!
//! Default is `all = true` (a client that never sends `Subscribe` sees everything).
//! Cap: [`MAX_SUBSCRIBED_AGENTS`] explicit ids, extra ids are ignored.
//! Invalidation: each `Subscribe` replaces the whole set. Owner: one per connection,
//! shared between the request loop (writer) and the session forwarder (reader).

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use pacode_types::AgentId;

/// Most explicit agent ids one connection can subscribe to.
pub const MAX_SUBSCRIBED_AGENTS: usize = 64;

#[derive(Debug)]
pub struct Interest {
    all: bool,
    agents: HashSet<AgentId>,
}

impl Default for Interest {
    fn default() -> Self {
        Self {
            all: true,
            agents: HashSet::new(),
        }
    }
}

impl Interest {
    /// Replace the subscription.
    pub fn set(&mut self, agents: Vec<AgentId>, all: bool) {
        self.all = all;
        self.agents = agents.into_iter().take(MAX_SUBSCRIBED_AGENTS).collect();
    }

    /// Whether the connection wants high-volume events of `agent`.
    pub fn wants(&self, agent: &AgentId) -> bool {
        self.all || self.agents.contains(agent)
    }
}

/// Shared handle; locks are never held across an await.
pub type SharedInterest = Arc<RwLock<Interest>>;

/// A new handle with the default (everything) interest.
pub fn new_shared() -> SharedInterest {
    Arc::new(RwLock::new(Interest::default()))
}

/// Replace the subscription behind `shared`.
pub fn apply(shared: &SharedInterest, agents: Vec<AgentId>, all: bool) {
    match shared.write() {
        Ok(mut g) => g.set(agents, all),
        Err(p) => p.into_inner().set(agents, all),
    }
}

/// Read helper that tolerates a poisoned lock.
pub fn wants(shared: &SharedInterest, agent: &AgentId) -> bool {
    match shared.read() {
        Ok(g) => g.wants(agent),
        Err(p) => p.into_inner().wants(agent),
    }
}
