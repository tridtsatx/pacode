//! The queue of unanswered questions.
//!
//! `pacode_types::SessionSnapshot::pending_questions` is already a list —
//! several agents can have a question open at once — but the picker used to
//! keep only the last one it saw, orphaning the rest. `PendingQuestions`
//! mirrors the queue contract inside the TUI: `QuestionAsked` pushes,
//! `QuestionResolved` removes by id, a reconnect snapshot replaces the whole
//! queue, and the picker keeps its option cursor plus half-typed custom
//! answer in a `QuestionFocus` per question so walking `Left`/`Right`
//! through the queue loses nothing.

use std::collections::{HashMap, VecDeque};

use pacode_types::{Question, QuestionId};

/// One question's picker state, kept while the user walks the queue so a
/// half-built answer survives navigating away and back.
#[derive(Debug, Clone, Default)]
pub struct QuestionFocus {
    /// Which row the cursor sits on. `0..options.len()` are the options;
    /// `options.len()` is the free-text plaque.
    pub index: usize,
    /// Chosen options, in the order they were picked (multi-select only).
    pub selected: Vec<usize>,
    /// The custom answer as typed so far.
    pub typed: String,
    /// Whether the next `Enter` submits `typed` — typing mode swallows
    /// printable keys instead of navigating.
    pub typing: bool,
}

impl QuestionFocus {
    /// Fit the stored cursor/selection to an option list of `len` — the
    /// plaque row at `index == len` is the largest legal index. A reconnect
    /// can swap a question for a shorter option list under the same id.
    fn clamp(&mut self, len: usize) {
        self.index = self.index.min(len);
        self.selected.retain(|i| *i < len);
    }
}

/// The live queue plus the cursor across it. Owner: `AppState`.
#[derive(Debug, Default)]
pub struct PendingQuestions {
    /// Waiting questions, oldest first — the same order `pending_questions`
    /// arrives on a snapshot.
    queue: VecDeque<Question>,
    /// Which queue slot the picker is showing.
    cursor: usize,
    /// Per-question picker state, keyed by `QuestionId` so it survives
    /// removals and reconnect reshuffles in the queue.
    focus: HashMap<QuestionId, QuestionFocus>,
}

impl PendingQuestions {
    /// The question under the cursor. `None` when the queue is empty — the
    /// overlay is dismissed as soon as that happens.
    pub fn current(&self) -> Option<&Question> {
        self.queue.get(self.cursor)
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// `1` when showing the first of N — for the `i/n` badge.
    pub fn position(&self) -> usize {
        self.cursor + 1
    }

    /// A `QuestionAsked` event. Never replaces what is already waiting —
    /// the old single-question overlay did that and orphaned the previous
    /// ask. A re-arriving id (reconnect replay) replaces its own entry so
    /// the queue cannot accumulate duplicates. Returns false when the new
    /// question is too malformed to present.
    pub fn push(&mut self, question: Question) -> bool {
        if question.question.trim().is_empty() || question.options.is_empty() {
            return false;
        }
        if let Some(slot) = self.queue.iter_mut().find(|q| q.id == question.id) {
            let len = question.options.len();
            let id = question.id.clone();
            *slot = question;
            if let Some(f) = self.focus.get_mut(&id) {
                f.clamp(len);
            }
            return true;
        }
        // The cursor opens on the recommended option, same as the old picker.
        self.focus.insert(
            question.id.clone(),
            QuestionFocus {
                index: question.recommended_index().unwrap_or(0),
                ..QuestionFocus::default()
            },
        );
        self.queue.push_back(question);
        true
    }

    /// A `QuestionResolved` event — or the optimistic local drop after the
    /// user answered here. Returns the question when it was waiting; `None`
    /// means a stale resolve for an id a reconnect already cleared.
    pub fn resolve(&mut self, id: &QuestionId) -> Option<Question> {
        let pos = self.queue.iter().position(|q| q.id == *id)?;
        let q = self.queue.remove(pos).expect("position just found");
        self.focus.remove(id);
        self.clamp_cursor();
        Some(q)
    }

    /// A reconnect snapshot is authoritative: it replaces the whole queue.
    /// Focus state survives for questions still waiting — a half-typed
    /// custom answer must not be lost to a reconnect — while state for ids
    /// that are gone (answered elsewhere, or resolved while detached) is
    /// dropped. The cursor re-anchors on the same question when it is still
    /// queued. Returns whether anything is still waiting.
    pub fn replace(&mut self, questions: Vec<Question>) -> bool {
        let current_id = self.current().map(|q| q.id.clone());
        self.queue = questions
            .into_iter()
            .filter(|q| !q.question.trim().is_empty() && !q.options.is_empty())
            .collect();
        for q in &self.queue {
            self.focus
                .entry(q.id.clone())
                .or_insert_with(|| QuestionFocus {
                    index: q.recommended_index().unwrap_or(0),
                    ..QuestionFocus::default()
                });
        }
        self.focus.retain(|id, f| {
            self.queue.iter().find(|q| q.id == *id).is_some_and(|q| {
                f.clamp(q.options.len());
                true
            })
        });
        self.cursor = current_id
            .and_then(|id| self.queue.iter().position(|q| q.id == id))
            .unwrap_or(0);
        self.clamp_cursor();
        !self.queue.is_empty()
    }

    /// Move the question cursor; wraps at both ends. The per-question focus
    /// state is untouched — that is the point of keying it by id.
    pub fn move_cursor(&mut self, delta: i32) {
        if self.queue.is_empty() {
            return;
        }
        let n = self.queue.len() as i32;
        self.cursor = (((self.cursor as i32 + delta) % n + n) % n) as usize;
    }

    /// The picker's stored state for the question under the cursor.
    pub fn focus(&self) -> Option<&QuestionFocus> {
        self.current().and_then(|q| self.focus.get(&q.id))
    }

    /// Mutable access; a never-touched question gets a fresh `QuestionFocus`
    /// clamped to its option list.
    pub fn focus_mut(&mut self) -> Option<&mut QuestionFocus> {
        let (id, len) = self.current().map(|q| (q.id.clone(), q.options.len()))?;
        let f = self.focus.entry(id).or_default();
        f.clamp(len);
        Some(f)
    }

    fn clamp_cursor(&mut self) {
        if self.cursor >= self.queue.len() {
            self.cursor = self.queue.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacode_types::{AgentId, CallId, Question, QuestionId, QuestionOption, QuestionOrigin};

    fn q(n: &str, text: &str) -> Question {
        Question::new(
            QuestionId::new(n),
            QuestionOrigin::new(AgentId::main(), "main", CallId::new(format!("call_{n}"))),
            "",
            text,
            vec![
                QuestionOption::new("a", "first"),
                QuestionOption::new("b", "second"),
            ],
            false,
            0,
        )
        .expect("question")
    }

    #[test]
    fn pushes_queue_up_instead_of_replacing() {
        let mut pq = PendingQuestions::default();
        assert!(pq.push(q("qst_1", "first?")));
        assert!(pq.push(q("qst_2", "second?")));
        assert_eq!(pq.len(), 2);
        assert_eq!(pq.current().map(|c| c.id.as_str()), Some("qst_1"));
        pq.move_cursor(1);
        assert_eq!(pq.current().map(|c| c.id.as_str()), Some("qst_2"));
        assert_eq!(pq.position(), 2);
        // Wraps at both ends.
        pq.move_cursor(1);
        assert_eq!(pq.current().map(|c| c.id.as_str()), Some("qst_1"));
        pq.move_cursor(-1);
        assert_eq!(pq.current().map(|c| c.id.as_str()), Some("qst_2"));
    }

    #[test]
    fn resolve_removes_and_reclamps_the_cursor() {
        let mut pq = PendingQuestions::default();
        pq.push(q("qst_1", "a?"));
        pq.push(q("qst_2", "b?"));
        pq.move_cursor(1);
        let gone = pq.resolve(&QuestionId::new("qst_1")).expect("was queued");
        assert_eq!(gone.question, "a?");
        assert_eq!(pq.len(), 1);
        // The cursor re-anchors on the same question (it slid into slot 0).
        assert_eq!(pq.current().map(|c| c.id.as_str()), Some("qst_2"));
        // A stale resolve does nothing.
        assert!(pq.resolve(&QuestionId::new("qst_99")).is_none());
    }

    #[test]
    fn a_replayed_ask_replaces_its_own_entry() {
        let mut pq = PendingQuestions::default();
        pq.push(q("qst_1", "old wording?"));
        pq.push(q("qst_2", "other?"));
        pq.push(q("qst_1", "new wording?"));
        assert_eq!(pq.len(), 2);
        assert_eq!(
            pq.current().map(|c| c.question.as_str()),
            Some("new wording?")
        );
    }

    #[test]
    fn replace_reanchors_and_keeps_focus_of_survivors() {
        let mut pq = PendingQuestions::default();
        pq.push(q("qst_1", "a?"));
        pq.push(q("qst_2", "b?"));
        pq.move_cursor(1);
        pq.focus_mut().expect("focus").typed = "half typed".to_string();

        // Question 1 left the queue; question 2 stays and keeps its state.
        assert!(pq.replace(vec![q("qst_2", "b?"), q("qst_3", "c?")]));
        assert_eq!(pq.current().map(|c| c.id.as_str()), Some("qst_2"));
        assert_eq!(pq.focus().expect("focus").typed, "half typed");
        pq.move_cursor(1);
        assert_eq!(pq.current().map(|c| c.id.as_str()), Some("qst_3"));
        assert_eq!(pq.focus().expect("focus").typed, "");
    }

    #[test]
    fn a_new_question_starts_on_its_recommended_option() {
        let mut pq = PendingQuestions::default();
        let mut recommended = q("qst_1", "pick one");
        recommended.options[1].recommended = true;
        pq.push(recommended);
        assert_eq!(pq.focus().expect("focus").index, 1);
    }

    #[test]
    fn malformed_questions_never_enter_the_queue() {
        let mut pq = PendingQuestions::default();
        let mut empty_opts = q("qst_1", "real?");
        empty_opts.options.clear();
        assert!(!pq.push(empty_opts));
        assert_eq!(pq.len(), 0);
    }
}
