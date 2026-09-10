//! Questions the model asked and is waiting on.
//!
//! The same shape as the permission registry: the tool call parks on a oneshot,
//! the client answers by id, and an agent that stops takes its pending questions
//! down with it so nothing waits forever.

#[cfg(test)]
#[path = "questions_tests.rs"]
mod questions_tests;

use std::collections::BTreeMap;
use std::sync::Mutex;

use pacode_types::{AgentId, Question, QuestionAnswer, QuestionId};
use tokio::sync::oneshot;

/// Most questions that may be open at once. A model that asks more than this is
/// not asking, it is spamming; the cap keeps the client's picker finite.
pub const MAX_PENDING: usize = 8;

#[derive(Default)]
pub struct QuestionState {
    pending: Mutex<BTreeMap<QuestionId, (Question, oneshot::Sender<QuestionAnswer>)>>,
}

impl QuestionState {
    /// Register a question; the receiver resolves when the user answers.
    /// `None` when too many are already open.
    pub fn register(&self, question: Question) -> Option<oneshot::Receiver<QuestionAnswer>> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        if pending.len() >= MAX_PENDING {
            log::warn!(
                "refusing question {}: {MAX_PENDING} are already waiting",
                question.id
            );
            return None;
        }
        log::info!(
            "question asked: id={} agent={} header={:?}",
            question.id,
            question.agent,
            question.header
        );
        pending.insert(question.id.clone(), (question, tx));
        Some(rx)
    }

    /// Answer a pending question. Returns the question when it was still open.
    pub fn resolve(&self, id: &QuestionId, answer: QuestionAnswer) -> Option<Question> {
        let entry = self
            .pending
            .lock()
            .ok()
            .and_then(|mut p| p.remove(id))
            .or_else(|| {
                log::warn!("answer for unknown question id={id}");
                None
            })?;
        let (question, tx) = entry;
        // A client that vanished between asking and answering is not an error:
        // the waiting side has already been dropped.
        let _ = tx.send(answer);
        Some(question)
    }

    pub fn pending(&self) -> Vec<Question> {
        self.pending
            .lock()
            .map(|p| p.values().map(|(q, _)| q.clone()).collect())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &QuestionId) -> Option<Question> {
        self.pending.lock().ok()?.get(id).map(|(q, _)| q.clone())
    }

    /// Drop every question an agent is waiting on (it stopped or was interrupted).
    /// The waiting tool sees a cancelled answer rather than hanging.
    pub fn cancel_for_agent(&self, agent: &AgentId) -> Vec<Question> {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let ids: Vec<QuestionId> = pending
            .iter()
            .filter(|(_, (q, _))| &q.agent == agent)
            .map(|(id, _)| id.clone())
            .collect();
        let mut cancelled = Vec::new();
        for id in ids {
            if let Some((question, tx)) = pending.remove(&id) {
                let _ = tx.send(QuestionAnswer::cancelled());
                cancelled.push(question);
            }
        }
        cancelled
    }
}
