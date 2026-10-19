//! Per-connection event interest (`Request::Subscribe`).
//!
//! A TUI draws the main agent and at most one panel target, yet used to receive every
//! delta of every subagent. [`Interest`] says which agents' high-volume events
//! (`TextDelta`, `ReasoningDelta`, `ToolOutput`, `ItemAdded`, `ItemUpdated`,
//! `AgentUpdated`) a connection wants; [`Router`] drops or coalesces the rest inside the
//! session forwarder (`connection::spawn_session_forwarder`).
//!
//! Default is `all = true` (a client that never sends `Subscribe` sees everything).
//! Cap: [`MAX_SUBSCRIBED_AGENTS`] explicit ids, extra ids are ignored.
//! Invalidation: each `Subscribe` replaces the whole set. Owner: one per connection,
//! shared between the request loop (writer) and the session forwarder (reader).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use pacode_types::protocol::Event;
use pacode_types::{AgentId, AgentInfo, AgentStatus, TranscriptKind};

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

/// Filtered seqs kept as carriers for coalesced updates (see [`Router::flush`]).
const MAX_HOLES: usize = 256;

/// Interest filter and `AgentUpdated` coalescing for one session forwarder.
///
/// What always passes: session-level events, anything of a wanted agent, tool-call
/// items (the files list follows every agent), and every `AgentUpdated` that changes an
/// agent's status (a finished or failed subagent is never late). An agent is filtered
/// only after its `AgentAdded` or an `AgentUpdated` went out on this forwarder, so a
/// client knows every agent whose events it is missing.
///
/// The seq contract the client relies on is kept: what goes out is strictly increasing
/// and an event never carries a seq older than one already sent. Filtering leaves holes;
/// loss is still reported only by `ServerMessage::Resync`. A held `AgentUpdated` is the
/// latest state of its agent, so [`Router::flush`] re-stamps it with a filtered seq at or
/// above its own (one that is never sent otherwise): "the agent looked like this as of
/// that seq" stays true, and snapshot fencing (`seq <= snapshot.seq`) still applies.
///
/// Caps: `delivered` and `held` hold one entry per live agent of the session (a
/// terminal status removes it), `holes` at most [`MAX_HOLES`] seqs. Owner: the
/// forwarder's receiver task; [`Router::reset`] after broadcast lag.
pub struct Router {
    interest: SharedInterest,
    /// Status last sent per live agent.
    delivered: HashMap<AgentId, AgentStatus>,
    /// Latest unsent `AgentUpdated` per unwanted agent, with its original seq.
    held: HashMap<AgentId, (u64, AgentInfo)>,
    /// Seqs above `sent` that were filtered or held, ascending.
    holes: VecDeque<u64>,
    /// Highest seq handed out.
    sent: u64,
}

impl Router {
    pub fn new(interest: SharedInterest) -> Self {
        Self {
            interest,
            delivered: HashMap::new(),
            held: HashMap::new(),
            holes: VecDeque::new(),
            sent: 0,
        }
    }

    /// The event to send for `(seq, event)`, or `None` when it was filtered or held.
    pub fn route(&mut self, seq: u64, event: Event) -> Option<(u64, Event)> {
        let event = match event {
            Event::AgentUpdated(info) => return self.route_update(seq, info),
            event => event,
        };
        let agent = match &event {
            Event::AgentAdded(info) => {
                if info.status.is_live() {
                    self.delivered.insert(info.id.clone(), info.status);
                }
                None
            }
            Event::TextDelta { agent, .. }
            | Event::ReasoningDelta { agent, .. }
            | Event::ToolOutput { agent, .. } => Some(agent),
            Event::ItemAdded(item) | Event::ItemUpdated(item)
                if !matches!(item.kind, TranscriptKind::ToolCall { .. }) =>
            {
                Some(&item.agent)
            }
            _ => None,
        };
        if agent.is_some_and(|agent| self.filters(agent)) {
            self.skip(seq);
            return None;
        }
        Some(self.send(seq, event))
    }

    /// Whether coalesced updates wait for [`Router::flush`].
    pub fn has_held(&self) -> bool {
        !self.held.is_empty()
    }

    /// Held updates that fit into the holes seen so far, re-stamped and in seq order.
    /// An update with no hole at or above its seq stays held for a later flush.
    pub fn flush(&mut self) -> Vec<(u64, Event)> {
        if self.held.is_empty() || self.holes.is_empty() {
            return Vec::new();
        }
        let mut order: Vec<(u64, AgentId)> = self
            .held
            .iter()
            .map(|(agent, (seq, _))| (*seq, agent.clone()))
            .collect();
        order.sort_unstable();
        let mut out = Vec::new();
        let mut next = 0;
        for (seq, agent) in order {
            while self.holes.get(next).is_some_and(|hole| *hole < seq) {
                next += 1;
            }
            let Some(&carrier) = self.holes.get(next) else {
                break;
            };
            next += 1;
            if let Some((_, info)) = self.held.remove(&agent) {
                out.push((carrier, Event::AgentUpdated(info)));
            }
        }
        if let Some((last, _)) = out.last() {
            self.sent = *last;
            self.holes.drain(..next);
        }
        out
    }

    /// Events were lost (broadcast lag): a held update may be older than one that was
    /// dropped, and the client re-reads everything from a snapshot anyway.
    pub fn reset(&mut self) {
        self.delivered.clear();
        self.held.clear();
        self.holes.clear();
    }

    fn filters(&self, agent: &AgentId) -> bool {
        self.delivered.contains_key(agent) && !wants(&self.interest, agent)
    }

    fn route_update(&mut self, seq: u64, info: AgentInfo) -> Option<(u64, Event)> {
        let changed = self.delivered.get(&info.id) != Some(&info.status);
        if changed || wants(&self.interest, &info.id) {
            self.held.remove(&info.id);
            if info.status.is_live() {
                self.delivered.insert(info.id.clone(), info.status);
            } else {
                self.delivered.remove(&info.id);
            }
            return Some(self.send(seq, Event::AgentUpdated(info)));
        }
        self.held.insert(info.id.clone(), (seq, info));
        self.skip(seq);
        None
    }

    fn send(&mut self, seq: u64, event: Event) -> (u64, Event) {
        self.sent = seq;
        self.holes.clear();
        (seq, event)
    }

    fn skip(&mut self, seq: u64) {
        if seq <= self.sent {
            return;
        }
        if self.holes.len() == MAX_HOLES {
            self.holes.pop_front();
        }
        self.holes.push_back(seq);
    }
}

#[cfg(test)]
#[path = "interest_tests.rs"]
mod interest_tests;
