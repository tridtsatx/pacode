//! Unfinished-todo guard: when the model ends its turn while the plan still has open
//! items, inject a reminder and let it continue instead of finishing.
//!
//! The decision is a pure function ([`should_remind`]); [`TodoGuard::remind`] does the
//! side effects (history, persistence, transcript notice). Counters live in the turn
//! loop's local [`TodoGuard`], so every new user turn starts from zero. The plan is
//! session-wide (not per agent), so only the main agent is ever guarded.

use std::sync::Arc;

use pacode_types::{
    Event, Message, MessageKind, Mode, PlanItem, PlanStatus, ToastLevel, TranscriptItem,
    TranscriptKind,
};

use crate::agent::Agent;
use crate::session::Session;

const ASK_TOOL: &str = "ask_question";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenItem {
    pub id: String,
    pub content: String,
    pub status: PlanStatus,
}

/// Everything the decision depends on.
pub(crate) struct GuardInput<'a> {
    pub enabled: bool,
    pub max_reminders: u32,
    pub reminders_sent: u32,
    pub mode: Mode,
    pub is_main: bool,
    pub interrupted: bool,
    pub plan: &'a [PlanItem],
    /// Text of the final assistant message.
    pub last_text: &'a str,
    /// `ask_question` was called earlier in this turn.
    pub asked_question: bool,
    /// The open set of the previous reminder, if one was sent this turn.
    pub last_reminded: Option<&'a [OpenItem]>,
}

pub(crate) fn open_items(plan: &[PlanItem]) -> Vec<OpenItem> {
    plan.iter()
        .filter(|i| matches!(i.status, PlanStatus::Pending | PlanStatus::Active))
        .map(|i| OpenItem {
            id: i.id.clone(),
            content: i.content.clone(),
            status: i.status,
        })
        .collect()
}

/// `Some(open items)` when a reminder should be sent now.
pub(crate) fn should_remind(input: &GuardInput<'_>) -> Option<Vec<OpenItem>> {
    if !input.enabled
        || !input.is_main
        || input.interrupted
        || input.mode == Mode::Plan
        || input.reminders_sent >= input.max_reminders
    {
        return None;
    }
    let open = open_items(input.plan);
    if open.is_empty() {
        return None;
    }
    let asked = input.asked_question || input.last_text.trim_end().ends_with('?');
    if asked && input.last_reminded == Some(open.as_slice()) {
        return None;
    }
    Some(open)
}

fn status_label(status: PlanStatus) -> &'static str {
    match status {
        PlanStatus::Pending => "pending",
        PlanStatus::Active => "in_progress",
        PlanStatus::Done => "done",
        PlanStatus::Cancelled => "cancelled",
    }
}

/// Deterministic reminder text.
pub(crate) fn format_reminder(open: &[OpenItem]) -> String {
    let mut out = String::from("You stopped, but these plan items are still open:\n");
    for (n, item) in open.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] {}: {}\n",
            n + 1,
            status_label(item.status),
            item.id,
            item.content
        ));
    }
    out.push_str(
        "Either finish them, or mark each as done/cancelled with the plan tool and explain \
         why it is no longer needed. Do not stop with open items.",
    );
    out
}

pub(crate) fn notice_text(count: usize, sent: u32, max: u32) -> String {
    let noun = if count == 1 { "item" } else { "items" };
    format!("todo guard: {count} {noun} open \u{2014} asked the agent to continue ({sent}/{max})")
}

/// Per-turn guard state; created at the top of `run_turn`.
#[derive(Default)]
pub(crate) struct TodoGuard {
    sent: u32,
    asked_question: bool,
    last_reminded: Option<Vec<OpenItem>>,
}

impl TodoGuard {
    /// Record the tool calls of one step (to know whether the agent asked the user).
    pub fn note_calls(
        &mut self,
        calls: &[(
            pacode_types::CallId,
            String,
            serde_json::Value,
            Option<String>,
        )],
    ) {
        if calls.iter().any(|(_, name, _, _)| name == ASK_TOOL) {
            self.asked_question = true;
        }
    }

    /// Called where the turn would end normally. Returns true when a reminder was
    /// injected and the loop must continue.
    pub async fn remind(
        &mut self,
        session: &Arc<Session>,
        agent: &Arc<Agent>,
        interrupted: bool,
        last_text: &str,
    ) -> bool {
        let cfg = &session.config.agents;
        let plan_items = session
            .plan
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .items
            .clone();
        let input = GuardInput {
            enabled: cfg.todo_guard,
            max_reminders: cfg.todo_guard_max_reminders,
            reminders_sent: self.sent,
            mode: session.meta().mode,
            is_main: agent.id().is_main(),
            interrupted,
            plan: &plan_items,
            last_text,
            asked_question: self.asked_question,
            last_reminded: self.last_reminded.as_deref(),
        };
        let Some(open) = should_remind(&input) else {
            return false;
        };
        self.sent += 1;
        let text = format_reminder(&open);
        let count = open.len();
        self.last_reminded = Some(open);

        let msg = Message::user(format!("<pacode_reminder>\n{text}\n</pacode_reminder>"))
            .with_kind(MessageKind::Injected);
        let (seq, arc_msg) = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(msg);
        let _ = session
            .store
            .append_message(&session.id, &agent.id(), seq, &arc_msg)
            .await;

        let item = TranscriptItem {
            seq: agent
                .transcript
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .next_seq(),
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::Notice {
                level: ToastLevel::Info,
                text: notice_text(count, self.sent, cfg.todo_guard_max_reminders),
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        session.events.emit(Event::ItemAdded(item));
        true
    }
}

#[cfg(test)]
#[path = "todo_guard_tests.rs"]
mod todo_guard_tests;
