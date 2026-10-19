//! `ask_parent`: the subagent's side of a question, from queueing it on the
//! orchestrator to the waiting status and the two transcript items.

use std::sync::Arc;

use pacode_tools::{ParentReply, ToolError};
use pacode_types::{
    AGENT_MESSAGE_CAP_CHARS, AgentId, AgentMessageDirection, AgentReply, AgentStatus, CallId,
    Event, TranscriptKind,
};
use tokio::sync::oneshot;

use super::landed;
use super::mailbox::{Opened, Reply};
use super::route::{post, update};
use crate::agent::Agent;
use crate::inject::Injection;
use crate::session::Session;

/// `ask_parent` from subagent `me`: queue the question on its parent and wait.
pub(crate) async fn ask_parent(
    session: &Arc<Session>,
    me: &Arc<Agent>,
    call_id: &CallId,
    question: &str,
) -> Result<ParentReply, ToolError> {
    let info = me.info();
    let parent_id = info
        .parent
        .as_ref()
        .ok_or_else(|| ToolError::invalid("only subagents can ask the orchestrator"))?;
    let question = pacode_types::truncate_head_tail(question.trim(), AGENT_MESSAGE_CAP_CHARS);
    let (parent, opened) = {
        // Found and queued on under one read of the agent map, in the order
        // a detach hands questions over in (map, mailbox, queue): a detach
        // swaps the orchestrator under the write lock, so the question is
        // either queued on the new one or found on the old one by the
        // hand-over — never left on an orchestrator that can no longer answer.
        let agents = session.agents.read().unwrap_or_else(|p| p.into_inner());
        let parent = agents
            .get(parent_id)
            .cloned()
            .ok_or_else(|| ToolError::failed("the orchestrator is gone"))?;
        #[cfg(test)]
        super::hooks::run(&info.id, super::hooks::Point::AskLookedUp);
        let opened = me.mailbox.open(
            &parent,
            Injection::AgentQuestion {
                agent: info.id.clone(),
                name: info.name.clone(),
                call_id: call_id.clone(),
                question: question.clone(),
            },
        )?;
        (parent, opened)
    };
    let rx = match opened {
        Opened::Asked(rx) => rx,
        // Not queued, so nothing to wake and nothing to show: the call's own
        // result says why.
        Opened::NotAnswering => return Ok(ParentReply::NotAnswering),
    };
    // Every exit from here — answer, no answer, a stop dropping this future —
    // passes through the guard.
    let mut waiting = Waiting::start(
        session, me, &parent, parent_id, call_id, &info, &question, rx,
    );
    // It wakes an idle parent the way a finished report does — whoever holds
    // the parent's place by now.
    if parent_id.is_main()
        && let Some(parent) = session.agent(parent_id)
    {
        crate::turn::lifecycle::request_turn(session.clone(), parent);
    }
    match (&mut waiting.rx).await {
        Ok(Reply { reply, taken }) => {
            // Taken here, in the same poll the call returns it in: from now
            // on the call's result is this reply, whatever cancels the turn.
            if let Some(taken) = taken {
                landed::deliver(taken, (), landed::answered, session, me);
            }
            waiting.finish(shown(&reply));
            Ok(reply)
        }
        Err(_) => Err(ToolError::Cancelled),
    }
}

/// How a reply reads on the question's transcript items.
fn shown(reply: &ParentReply) -> AgentReply {
    match reply {
        ParentReply::Answered(text) => AgentReply::Answered { text: text.clone() },
        ParentReply::Unanswered | ParentReply::NotAnswering => AgentReply::Unanswered,
        ParentReply::ParentFailed => AgentReply::Failed,
    }
}

/// The open question's side effects: the waiting status and the two
/// transcript items, undone exactly once however the wait ends.
struct Waiting<'a> {
    session: &'a Arc<Session>,
    me: &'a Arc<Agent>,
    /// The agent the question was first shown in: its transcript item lives
    /// there, even if a detach handed the question to a new orchestrator.
    shown_in: Arc<Agent>,
    parent_id: AgentId,
    call_id: CallId,
    item: TranscriptKind,
    seqs: [u64; 2],
    prev: (AgentStatus, Option<String>),
    rx: oneshot::Receiver<Reply>,
    done: bool,
}

impl<'a> Waiting<'a> {
    #[allow(clippy::too_many_arguments)]
    fn start(
        session: &'a Arc<Session>,
        me: &'a Arc<Agent>,
        parent: &Arc<Agent>,
        parent_id: &AgentId,
        call_id: &CallId,
        info: &pacode_types::AgentInfo,
        question: &str,
        rx: oneshot::Receiver<Reply>,
    ) -> Self {
        let item = TranscriptKind::AgentMessage {
            subagent: info.id.clone(),
            subagent_name: info.name.clone(),
            direction: AgentMessageDirection::Question,
            text: question.to_string(),
            reply: None,
        };
        let seqs = [
            post(session, parent, item.clone()),
            post(session, me, item.clone()),
        ];
        let prev = (info.status, info.activity.clone());
        // From the status read above, in one step: a stop that landed since
        // keeps `Stopped`, and `finish` then has no `WaitingParent` to undo.
        me.restore_status(
            info.status,
            AgentStatus::WaitingParent,
            Some("asking the orchestrator".to_string()),
        );
        session.events.emit(Event::AgentUpdated(me.info()));
        Self {
            session,
            me,
            shown_in: parent.clone(),
            parent_id: parent_id.clone(),
            call_id: call_id.clone(),
            item,
            seqs,
            prev,
            rx,
            done: false,
        }
    }

    fn finish(&mut self, reply: AgentReply) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        self.me.mailbox.take_question(&self.call_id);
        // Whoever holds the parent's place now (a detach may have handed the
        // question to a new orchestrator) must not be shown it any more.
        if let Some(parent) = self.session.agent(&self.parent_id) {
            parent
                .injections
                .retract_question(&self.me.id(), &self.call_id);
        }
        if let TranscriptKind::AgentMessage { reply: slot, .. } = &mut self.item {
            *slot = Some(reply);
        }
        update(
            self.session,
            &self.shown_in,
            self.seqs[0],
            self.item.clone(),
        );
        update(self.session, self.me, self.seqs[1], self.item.clone());
        #[cfg(test)]
        super::hooks::run(&self.me.id(), super::hooks::Point::WaitRestoring);
        // A stop that landed during the wait wins over the restore: only a
        // status still `WaitingParent` is put back, checked and set under one
        // lock (a flag check before a plain set would let a stop in between
        // be overwritten).
        let (status, activity) = self.prev.clone();
        self.me
            .restore_status(AgentStatus::WaitingParent, status, activity);
        self.session
            .events
            .emit(Event::AgentUpdated(self.me.info()));
    }
}

impl Drop for Waiting<'_> {
    /// The call was dropped (a stop cancelled the turn) before it took a
    /// reply: its result says cancelled, and so does every view of the
    /// question. A reply waiting in the channel was never taken — an answer
    /// among them tells its `send` so by dropping its `taken` unconfirmed.
    fn drop(&mut self) {
        self.finish(AgentReply::Cancelled);
    }
}
