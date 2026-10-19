//! Plan reminders: the plan keeps the main agent going.
//!
//! When the main agent's model chooses to stop (a step with no tool calls and
//! nothing due to drain — point B of the turn loop) while the session plan
//! still has open items (pending or active), the reminder is folded into
//! history as an [`Injection::PlanReminder`] and the loop goes on, exactly as
//! it does for an injection that is due at point B. The turn does not end:
//! clients that wait for `TurnEnded` (`pacode run`, an ACP prompt) see the
//! continuation as part of it, and a fusion turn keeps its plan instead of
//! running the planner again.
//!
//! No reminder when any of these holds:
//! * the agent is not the main agent (subagents, and a detached turn: its
//!   agent stops being main at the detach, so the budget stays behind);
//! * a stop is requested, or the user interrupted (`may_start_own_turn`);
//!   interrupted and failed turns never reach point B, and a user message
//!   waiting at point B ends the turn before this is asked;
//! * the session is in Plan mode, or `[session] plan_reminders` is off;
//! * the budget is spent: `plan_reminders_max` per user message;
//! * the previous reminder is still awaiting progress: the plan has not
//!   changed since it was sent. Silence until the plan moves or the user writes;
//! * something else will wake the agent or needs the user: anything queued,
//!   a paced notice-lock release, a live subagent, a running background task,
//!   a watching monitor, a pending question or permission. Cron jobs do not
//!   count: they start a fresh prompt on their own schedule, possibly days
//!   away, and are not work this turn left in flight;
//! * the final assistant message hands the turn to the user
//!   ([`awaits_user`]).
//!
//! The budget lives on the [`Agent`] and is reset when a user message starts
//! a turn.

#[cfg(test)]
#[path = "plan_reminder_tests.rs"]
mod plan_reminder_tests;

#[cfg(test)]
#[path = "plan_reminder_flow_tests.rs"]
mod plan_reminder_flow_tests;

use std::sync::{Arc, LazyLock};

use pacode_types::{Mode, PlanItem, PlanStatus, Role, ToastLevel};
use regex::Regex;

use crate::agent::Agent;
use crate::inject::Injection;
use crate::session::Session;

/// Open items listed in one reminder; the rest are counted.
pub const MAX_LISTED: usize = 10;
/// Characters of one item's text in a reminder.
pub const ITEM_CHARS: usize = 200;

/// The main agent's reminder budget for the current user message.
#[derive(Debug, Default)]
pub struct Budget {
    sent: u32,
    /// Plan version the last reminder was sent against: while the plan still
    /// has it, the reminder is awaiting progress.
    awaiting: Option<u64>,
}

/// A user message starts a turn: a fresh budget.
pub(crate) fn on_user_turn(agent: &Agent) {
    *agent
        .plan_reminders
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Budget::default();
}

/// Point B: the model chose to stop and nothing was due. When a reminder is
/// owed it lands in history (persisted like any injection, so a resumed
/// session replays it) and `true` tells the loop to go on.
///
/// The reminder is optional: a store that refuses it ends the turn the way
/// the model chose, rather than failing a turn whose work is done.
pub(crate) async fn remind(session: &Arc<Session>, agent: &Arc<Agent>) -> bool {
    let Some(Due {
        open,
        attempt,
        version,
    }) = due(session, agent)
    else {
        return false;
    };
    let max = session.config.session.plan_reminders_max;
    let line = notice_line(open.len(), attempt, max);
    let message = crate::inject::render_injections(
        &[Injection::PlanReminder { open, attempt, max }],
        session.config.context.injection_cap_chars,
    );
    if let Err(error) = crate::turn::persist_history_message(session, agent, message).await {
        log::warn!(
            "plan reminder not delivered: session={} agent={}: {error}",
            session.id,
            agent.id()
        );
        return false;
    }
    *agent
        .plan_reminders
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Budget {
        sent: attempt,
        awaiting: Some(version),
    };
    session.emit_notice(agent, ToastLevel::Info, line);
    true
}

/// A reminder the stop owes: the open items, which reminder of the budget it
/// is, and the plan version it was sent against.
struct Due {
    open: Vec<PlanItem>,
    attempt: u32,
    version: u64,
}

fn due(session: &Session, agent: &Agent) -> Option<Due> {
    if !agent.id().is_main() || agent.stop_requested() || !agent.may_start_own_turn() {
        return None;
    }
    let config = &session.config.session;
    if !config.plan_reminders || session.meta().mode == Mode::Plan {
        return None;
    }
    let (version, open) = {
        let plan = session.plan.read().unwrap_or_else(|p| p.into_inner());
        let open: Vec<PlanItem> = plan
            .items
            .iter()
            .filter(|item| is_open(item.status))
            .cloned()
            .collect();
        (plan.version, open)
    };
    if open.is_empty() {
        return None;
    }
    let attempt = {
        let budget = agent
            .plan_reminders
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if budget.sent >= config.plan_reminders_max || budget.awaiting == Some(version) {
            return None;
        }
        budget.sent + 1
    };
    if !agent.injections.is_empty() || agent.notice_lock.wants_work() || work_in_flight(session) {
        return None;
    }
    if final_text(agent).is_some_and(|text| awaits_user(&text)) {
        return None;
    }
    Some(Due {
        open,
        attempt,
        version,
    })
}

fn is_open(status: PlanStatus) -> bool {
    match status {
        PlanStatus::Pending | PlanStatus::Active => true,
        PlanStatus::Done | PlanStatus::Cancelled => false,
    }
}

/// Work that wakes the main agent by itself, or a user who is being asked.
/// The main agent is excluded: it is the one running this turn.
fn work_in_flight(session: &Session) -> bool {
    let live_subagent = session
        .agents
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .any(|(id, agent)| !id.is_main() && (agent.is_running() || agent.status().is_active()));
    live_subagent
        || !session.questions.pending().is_empty()
        || !session.permissions.pending().is_empty()
        || session
            .scheduler
            .monitors()
            .iter()
            .any(|monitor| monitor.status.is_live())
        || session
            .tasks_of_session()
            .iter()
            .any(|task| !task.status.is_terminal())
}

/// Text of the last assistant message: the one the model stopped on.
fn final_text(agent: &Agent) -> Option<String> {
    agent
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(|message| message.text())
}

/// The transcript line the client shows for a reminder.
pub fn notice_line(open: usize, attempt: u32, max: u32) -> String {
    let items = if open == 1 { "item" } else { "items" };
    format!("↻ {open} plan {items} open — continuing ({attempt}/{max})")
}

/// The reminder as the model reads it. Bounded: at most [`MAX_LISTED`]
/// items of at most [`ITEM_CHARS`] characters each, one line apiece.
pub fn render(open: &[PlanItem], attempt: u32, max: u32) -> String {
    let mut out = format!(
        "<system-reminder>You stopped with {} open plan item(s):\n",
        open.len()
    );
    for item in open.iter().take(MAX_LISTED) {
        let marker = match item.status {
            PlanStatus::Active => "[active]",
            PlanStatus::Pending | PlanStatus::Done | PlanStatus::Cancelled => "[ ]",
        };
        out.push_str(&format!("- {marker} {}\n", one_line(&item.content)));
    }
    if open.len() > MAX_LISTED {
        out.push_str(&format!("- … and {} more\n", open.len() - MAX_LISTED));
    }
    out.push_str(&format!(
        "Continue working on them, or mark them done/cancelled with the `plan` tool if they \
         are finished or no longer needed. (Reminder {attempt}/{max})</system-reminder>"
    ));
    out
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= ITEM_CHARS {
        return flat;
    }
    let mut cut = pacode_types::truncate_chars(&flat, ITEM_CHARS - 1);
    cut.push('…');
    cut
}

// Ported from oh-my-pi's `isAwaitingUserAnswer`. The patterns are constants:
// a typo panics every `awaits_user` test, never a session.
fn compile(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => panic!("static regex {pattern:?}: {error}"),
    }
}

static MARKDOWN_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| compile(r"^(?:>\s*)?(?:(?:[-*+]|\d+[.)])\s+)*"));
static PROMPT_LABEL: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)^(?:q(?:uestion)?|ask)\s*\d*\s*[:.)-]\s*"));
static QUESTION_START: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"(?i)^(?:what|which|when|where|why|how|who|whom|whose|do|does|did|can|could|would|will|should|is|are|am|may|shall)\b",
    )
});
static USER_DIRECTED: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b(?:you|your|we|our)\b"));
static RESPONSE_CUE: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"(?i)^(?:please\s+)?(?:confirm|reply|choose|pick|decide|advise|answer|let\s+me\s+know|tell\s+me)\b",
    )
});

/// Whether the final assistant text ends by handing the turn to the user: its
/// last line is a question to them, or asks them to confirm / choose / reply.
///
/// Conservative on purpose: a stray `?` (a code tail like `parse(input)?`)
/// is not a question unless the line reads like one — an English question word
/// first, "you/your/we/our" in it, a `Q:` label, or any non-ASCII text (a
/// question in another language has no cheap word list, and a `?` ending
/// prose in it is a real one).
pub fn awaits_user(text: &str) -> bool {
    let Some(last) = text.trim().lines().last() else {
        return false;
    };
    let stripped = MARKDOWN_PREFIX.replace(last.trim(), "");
    let stripped = stripped.trim();
    let line = PROMPT_LABEL.replace(stripped, "");
    let line = line.trim();
    let labelled = line.len() != stripped.len();
    let is_question = (line.ends_with('?') || line.ends_with('？'))
        && (labelled
            || QUESTION_START.is_match(line)
            || USER_DIRECTED.is_match(line)
            || !line.is_ascii());
    let cue = line
        .trim_end_matches(['.', '!', '?', '。', '！', '？'])
        .trim();
    is_question || RESPONSE_CUE.is_match(cue)
}
