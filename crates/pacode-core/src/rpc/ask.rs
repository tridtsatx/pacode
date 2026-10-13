//! `ask_parent`: the subagent's side of a question, from queueing it on the
//! orchestrator to the waiting status and the two transcript items.

use std::sync::Arc;

use pacode_tools::{ParentReply, ToolError};
use pacode_types::{
    AGENT_MESSAGE_CAP_CHARS, AgentId, AgentMessageDirection, AgentReply, AgentStatus, CallId,
    Event, TranscriptKind,
};
use tokio::sync::oneshot;

use super::mailbox::Opened;
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
    let parent = session
        .agent(parent_id)
        .ok_or_else(|| ToolError::failed("the orchestrator is gone"))?;
    let question = pacode_types::truncate_head_tail(question.trim(), AGENT_MESSAGE_CAP_CHARS);
    let opened = me.mailbox.open(
        &parent,
        Injection::AgentQuestion {
            agent: info.id.clone(),
            name: info.name.clone(),
            call_id: call_id.clone(),
            question: question.clone(),
        },
    )?;
    let rx = match opened {
        Opened::Asked(rx) => rx,
        // Not queued, so nothing to wake and nothing to show: the call's own
        // result says why.
        Opened::NotAnswering => return Ok(ParentReply::NotAnswering),
    };
    // Every exit from here — answer, no answer, a stop dropping this future —
    // passes through the guard.
    let mut waiting = Waiting::start(session, me, &parent, call_id, &info, &question, rx);
    // It wakes an idle parent the way a finished report does.
    if parent.id().is_main() {
        crate::turn::lifecycle::request_turn(session.clone(), parent.clone());
    }
    match (&mut waiting.rx).await {
        Ok(reply) => {
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
    rx: oneshot::Receiver<ParentReply>,
    done: bool,
}

impl<'a> Waiting<'a> {
    fn start(
        session: &'a Arc<Session>,
        me: &'a Arc<Agent>,
        parent: &Arc<Agent>,
        call_id: &CallId,
        info: &pacode_types::AgentInfo,
        question: &str,
        rx: oneshot::Receiver<ParentReply>,
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
        me.set_status(
            AgentStatus::WaitingParent,
            Some("asking the orchestrator".to_string()),
        );
        session.events.emit(Event::AgentUpdated(me.info()));
        Self {
            session,
            me,
            shown_in: parent.clone(),
            parent_id: parent.id(),
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
        // A stop that landed during the wait wins over the restore.
        if !self.me.stop_requested() {
            let (status, activity) = self.prev.clone();
            self.me.set_status(status, activity);
        }
        self.session
            .events
            .emit(Event::AgentUpdated(self.me.info()));
    }
}

impl Drop for Waiting<'_> {
    /// The call was dropped (a stop cancelled the turn). An answer that had
    /// already been handed over stays on record as the answer it was; anything
    /// else ends as cancelled.
    fn drop(&mut self) {
        let reply = self
            .rx
            .try_recv()
            .map(|reply| shown(&reply))
            .unwrap_or(AgentReply::Cancelled);
        self.finish(reply);
    }
}
