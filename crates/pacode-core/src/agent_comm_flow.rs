//! Session-level flow of orchestrator ↔ subagent messages and questions. The pure
//! state is in `agent_comm.rs`.

use std::sync::Arc;
use std::time::Duration;

use pacode_tools::builtin::ask_parent::{ParentReply, clean_options};
use pacode_types::{AgentId, AgentStatus, Event, ToastLevel, TranscriptItem, TranscriptKind};

use crate::agent_comm::{
    AnswerRoute, AskOutcome, CommError, Delivery, MAX_QUEUED_PARENT_MESSAGES, MessageRoute,
    check_text, delivery, is_parent_message, short, should_wake,
};
use crate::inject::Injection;
use crate::session::Session;

/// Label the subagent sees for the sender of a message.
const ORCHESTRATOR: &str = "orchestrator";
/// Longest excerpt of a message shown in a transcript notice.
const NOTICE_CHARS: usize = 160;

impl Session {
    /// Show a line in an agent's transcript (`message ←`, `question →`, ...).
    fn comm_notice(&self, agent: &AgentId, text: String) {
        let Some(target) = self.agent(agent) else {
            return;
        };
        let seq = target
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq();
        let item = TranscriptItem {
            seq,
            agent: agent.clone(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::Notice {
                level: ToastLevel::Info,
                text,
            },
        };
        target
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        self.events.emit(Event::ItemAdded(item));
    }

    /// The running subagent `id`, or why it cannot take a message now.
    fn running_subagent(&self, id: &AgentId) -> Result<Arc<crate::agent::Agent>, CommError> {
        if id.is_main() {
            return Err(CommError::IsMain);
        }
        let target = self
            .agent(id)
            .ok_or_else(|| CommError::NotFound(id.clone()))?;
        match delivery(target.status().is_live(), target.is_running()) {
            Delivery::Accept => Ok(target),
            Delivery::Reject => Err(CommError::NotRunning(id.clone())),
        }
    }

    /// Orchestrator → subagent: queue `text` for its next step. A subagent blocked
    /// in `ask_parent` takes it as the answer instead.
    pub fn message_agent(&self, id: &AgentId, text: &str) -> Result<(), CommError> {
        let text = check_text(text)?;
        let target = self.running_subagent(id)?;
        let line = short(&text, NOTICE_CHARS);
        if let MessageRoute::Consumed(question) = target.comm.deliver_message(&text) {
            self.comm_notice(
                id,
                format!("message ← {ORCHESTRATOR} (answers {question}): {line}"),
            );
            return Ok(());
        }
        let queued = target.injections.push_limited(
            Injection::ParentMessage {
                from: ORCHESTRATOR.to_string(),
                text,
            },
            MAX_QUEUED_PARENT_MESSAGES,
            is_parent_message,
        );
        if !queued {
            return Err(CommError::QueueFull {
                agent: id.clone(),
                cap: MAX_QUEUED_PARENT_MESSAGES,
            });
        }
        target.comm.clear_unwaited();
        self.comm_notice(id, format!("message ← {ORCHESTRATOR}: {line}"));
        Ok(())
    }

    /// Orchestrator → subagent: answer its pending question.
    pub fn answer_agent(
        &self,
        id: &AgentId,
        question_id: &str,
        text: &str,
    ) -> Result<(), CommError> {
        let text = check_text(text)?;
        let target = self.running_subagent(id)?;
        let route = target.comm.answer(id, question_id, &text)?;
        let line = short(&text, NOTICE_CHARS);
        if route == AnswerRoute::Later {
            target.injections.push(Injection::ParentAnswer {
                question_id: question_id.to_string(),
                text,
            });
        }
        self.comm_notice(
            id,
            format!("answer ← {ORCHESTRATOR} ({question_id}): {line}"),
        );
        Ok(())
    }

    /// Drop a stopped or finished agent's pending question; a blocked call wakes
    /// with a cancelled outcome.
    pub fn drop_agent_questions(&self, id: &AgentId) {
        if let Some(agent) = self.agent(id)
            && let Some(question) = agent.comm.cancel()
        {
            log::info!("dropped pending question {question} of agent {id}");
        }
    }

    /// Subagent → orchestrator. Registers the question, delivers it to the parent
    /// (waking an idle main agent) and, with `wait`, blocks for the answer.
    pub async fn ask_parent(
        self: &Arc<Self>,
        me: &AgentId,
        question: String,
        options: Vec<String>,
        wait: bool,
    ) -> Result<ParentReply, CommError> {
        let agent = self
            .agent(me)
            .ok_or_else(|| CommError::NotFound(me.clone()))?;
        let info = agent.info();
        let parent_id = info.parent.clone().ok_or(CommError::NoParent)?;
        let parent = self.agent(&parent_id).ok_or(CommError::ParentGone)?;
        let options = clean_options(options);

        let (question_id, receiver) = agent
            .comm
            .begin_question(self.config.agents.ask_parent_max, wait)?;

        parent.injections.push(Injection::AgentQuestion {
            agent: info.clone(),
            question_id: question_id.clone(),
            question: question.clone(),
            options,
        });
        self.comm_notice(
            &parent_id,
            format!(
                "question ← {} ({question_id}): {}",
                info.name,
                short(&question, NOTICE_CHARS)
            ),
        );
        self.comm_notice(
            me,
            format!(
                "question → {ORCHESTRATOR} ({question_id}): {}",
                short(&question, NOTICE_CHARS)
            ),
        );
        if should_wake(parent.id().is_main(), parent.is_running()) {
            crate::turn::start_turn(self.clone(), parent);
        }

        let Some(mut receiver) = receiver else {
            return Ok(ParentReply::Pending { question_id });
        };

        let previous = agent.info();
        // Conditional: a stop that landed since the question was registered must keep
        // its terminal status, not be overwritten by "waiting".
        if agent.restore_status(
            previous.status,
            AgentStatus::WaitingOrchestrator,
            Some("waiting for orchestrator".to_string()),
        ) {
            self.events.emit(Event::AgentUpdated(agent.info()));
        }

        let cancel = agent
            .cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let timeout = Duration::from_secs(self.config.agents.ask_parent_timeout_secs.max(1));
        let outcome = tokio::select! {
            res = &mut receiver => Some(res.unwrap_or(AskOutcome::Cancelled)),
            _ = tokio::time::sleep(timeout) => None,
            _ = async {
                match &cancel {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => Some(AskOutcome::Cancelled),
        };

        // A stop in the meantime already set a terminal status: leave it alone.
        if agent.restore_status(
            AgentStatus::WaitingOrchestrator,
            previous.status,
            previous.activity,
        ) {
            self.events.emit(Event::AgentUpdated(agent.info()));
        }

        let reply = match outcome {
            Some(AskOutcome::Answered(text)) => ParentReply::Answered { question_id, text },
            Some(AskOutcome::Cancelled) => {
                agent.comm.cancel();
                ParentReply::Cancelled { question_id }
            }
            None => {
                if agent.comm.expire(&question_id) {
                    if let Some(parent) = self.agent(&parent_id) {
                        parent.injections.push(Injection::SystemNotice(format!(
                            "Question {question_id} from agent {} expired unanswered; it continued on its own assumption.",
                            info.name
                        )));
                    }
                    ParentReply::TimedOut { question_id }
                } else {
                    // Answered in the same instant the timer fired.
                    match receiver.try_recv() {
                        Ok(AskOutcome::Answered(text)) => {
                            ParentReply::Answered { question_id, text }
                        }
                        Ok(AskOutcome::Cancelled) | Err(_) => {
                            ParentReply::Cancelled { question_id }
                        }
                    }
                }
            }
        };
        Ok(reply)
    }
}

#[cfg(test)]
#[path = "agent_comm_flow_tests.rs"]
mod agent_comm_flow_tests;
