//! Where questions are, as the orchestrator's turns and its identity change:
//! shown (delivered), settled with the turn that showed them, failed when they
//! cannot be shown, and handed on when a detach replaces the orchestrator.
//! Plus the two transcript helpers both legs of the exchange post with.

use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_tools::ParentReply;
use pacode_types::{AgentId, Event, TranscriptItem, TranscriptKind};

use super::mailbox::question_key;
use crate::agent::Agent;
use crate::inject::Injection;
use crate::session::Session;

/// The parent's turn persisted `delivered`: the questions in it now count as
/// seen by the parent.
pub(crate) fn mark_delivered(session: &Session, delivered: &[Injection]) {
    for (agent, call_id) in delivered.iter().filter_map(question_key) {
        if let Some(asker) = session.agent(agent) {
            asker.mailbox.mark_delivered(call_id);
        }
    }
}

/// The questions in `taken` were drained but can never be shown (the turn
/// failed before they reached the history). Each one ends as failed rather
/// than vanishing with the turn and leaving its subagent waiting forever.
pub(crate) fn resolve_undeliverable(session: &Session, taken: &[Injection]) {
    for (agent, call_id) in taken.iter().filter_map(question_key) {
        if let Some(asker) = session.agent(agent) {
            asker
                .mailbox
                .resolve_call(call_id, ParentReply::ParentFailed);
        }
    }
}

/// `parent`'s turn ended in a way that settles the questions it was shown:
/// every one it delivered and did not answer ends with `reply`, and its
/// subagent goes on with its own judgment.
pub(crate) fn resolve_delivered(session: &Session, parent: &Agent, reply: ParentReply) {
    let parent_id = parent.id();
    let children: Vec<Arc<Agent>> = session
        .agents
        .read()
        .map(|agents| children_of(&agents, &parent_id).cloned().collect())
        .unwrap_or_default();
    for child in children {
        child.mailbox.resolve_delivered(reply.clone());
    }
}

/// A detach turned the old orchestrator into a subagent that cannot answer
/// (it has no `agent` tool). Its subagents' open questions — still queued on
/// it, or already shown in its history — go to the new orchestrator as
/// undelivered, so the one that can answer is shown them and its turns settle
/// them. Called with the session's agent map write-locked, after the swap.
pub(crate) fn reroute_questions(agents: &BTreeMap<AgentId, Arc<Agent>>, old: &Agent, new: &Agent) {
    old.injections.take_questions();
    let parent = new.id();
    for child in children_of(agents, &parent) {
        if let Some(question) = child.mailbox.undeliver() {
            new.injections.push(question);
        }
    }
}

fn children_of<'a>(
    agents: &'a BTreeMap<AgentId, Arc<Agent>>,
    parent: &'a AgentId,
) -> impl Iterator<Item = &'a Arc<Agent>> {
    agents.values().filter(move |agent| {
        let info = agent.info.read().unwrap_or_else(|p| p.into_inner());
        info.parent.as_ref() == Some(parent)
    })
}

/// Append an exchange item to `agent`'s transcript and announce it; the seq
/// is what a question's update replaces.
pub(super) fn post(session: &Session, agent: &Agent, kind: TranscriptKind) -> u64 {
    let item = {
        let mut transcript = agent.transcript.lock().unwrap_or_else(|p| p.into_inner());
        let item = TranscriptItem {
            seq: transcript.next_seq(),
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind,
        };
        transcript.upsert(item.clone());
        item
    };
    let seq = item.seq;
    session.events.emit(Event::ItemAdded(item));
    seq
}

pub(super) fn update(session: &Session, agent: &Agent, seq: u64, kind: TranscriptKind) {
    let item = TranscriptItem {
        seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind,
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemUpdated(item));
}
