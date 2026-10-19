//! Injections: things that reach the model between steps (spec §6.2), never
//! mid-stream.

use std::collections::VecDeque;
use std::sync::Mutex;

use pacode_types::{AgentId, AgentInfo, CallId, Message, MessageKind, PlanItem, TaskInfo};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq)]
pub enum Injection {
    /// The user typed while a turn was running.
    UserSteer {
        admission: Option<String>,
    },
    /// A background task ended; `tail` is the last lines of its output.
    TaskFinished {
        task: TaskInfo,
        tail: String,
    },
    TaskStalled {
        task: TaskInfo,
        tail: String,
    },
    /// A subagent finished; `answer` is its final message (already capped).
    AgentFinished {
        agent: AgentInfo,
        answer: String,
    },
    SystemNotice(String),
    /// Orchestrator asked a subagent for its status.
    StatusRequest,
    /// A subagent reported its status to the parent.
    AgentStatus {
        agent: AgentInfo,
        text: String,
    },
    /// `agent send` to a subagent that is mid-turn (already capped).
    ParentMessage {
        text: String,
    },
    /// A subagent's `ask_parent` question; its call `call_id` is blocked until
    /// the parent answers it with `agent send` (already capped).
    AgentQuestion {
        agent: AgentId,
        name: String,
        call_id: CallId,
        question: String,
    },
    /// An `agent send` whose call was dropped (the orchestrator was
    /// interrupted) after its message had taken effect: what it did, so the
    /// orchestrator neither sends it again nor loses a report it took back.
    SendLanded {
        agent: AgentId,
        name: String,
        outcome: pacode_tools::SendOutcome,
    },
    /// The main agent stopped on its own with plan items still open
    /// (`crate::plan_reminder`). `attempt` of `max` per user message. Folded
    /// into history by the turn loop itself, never queued.
    PlanReminder {
        open: Vec<PlanItem>,
        attempt: u32,
        max: u32,
    },
}

impl Injection {
    /// What the notice lock may park. Everything that reports work *back* to
    /// the orchestrator is holdable; what the user or the orchestrator is
    /// waiting on is not.
    ///
    /// The rule is stated as an exclusion on purpose: a new variant is
    /// holdable by default, and the ones that must never wait in a backlog are
    /// the ones that hang a turn — a held steer is a turn the user believes is
    /// running and is not, a held question is a subagent blocked for nothing,
    /// and a held `SendLanded` is a message the orchestrator sends again. A
    /// held plan reminder is a turn that was meant to go on and stalls.
    pub fn is_holdable(&self) -> bool {
        !matches!(
            self,
            Injection::UserSteer { .. }
                | Injection::StatusRequest
                | Injection::ParentMessage { .. }
                | Injection::AgentQuestion { .. }
                | Injection::SendLanded { .. }
                | Injection::PlanReminder { .. }
        )
    }
}

#[derive(Default)]
pub struct InjectionQueue {
    queue: Mutex<VecDeque<Injection>>,
    notify: Notify,
}

impl InjectionQueue {
    pub fn push(&self, injection: Injection) {
        if let Ok(mut q) = self.queue.lock() {
            q.push_back(injection);
        }
        self.notify.notify_one();
    }

    /// Push unless `limit` queued items already satisfy `counts`; `false` = refused.
    pub fn push_limited(
        &self,
        injection: Injection,
        limit: usize,
        counts: impl Fn(&Injection) -> bool,
    ) -> bool {
        {
            let Ok(mut q) = self.queue.lock() else {
                return false;
            };
            if q.iter().filter(|i| counts(i)).count() >= limit {
                return false;
            }
            q.push_back(injection);
        }
        self.notify.notify_one();
        true
    }

    /// How many queued items satisfy `counts`.
    pub fn count_matching(&self, counts: impl Fn(&Injection) -> bool) -> usize {
        self.queue
            .lock()
            .map(|q| q.iter().filter(|i| counts(i)).count())
            .unwrap_or(0)
    }

    pub fn drain(&self) -> Vec<Injection> {
        self.queue
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn restore(&self, items: Vec<Injection>) {
        if items.is_empty() {
            return;
        }
        if let Ok(mut queue) = self.queue.lock() {
            for item in items.into_iter().rev() {
                queue.push_front(item);
            }
        }
        self.notify.notify_one();
    }

    pub fn has_user_admission(&self) -> bool {
        self.queue
            .lock()
            .map(|queue| {
                queue
                    .iter()
                    .any(|item| matches!(item, Injection::UserSteer { admission: Some(_) }))
            })
            .unwrap_or(false)
    }

    pub fn is_empty(&self) -> bool {
        self.queue.lock().map(|q| q.is_empty()).unwrap_or(true)
    }

    /// An `agent send` is waiting: the subagent owes the orchestrator a turn.
    pub fn has_parent_message(&self) -> bool {
        self.queue
            .lock()
            .map(|q| {
                q.iter()
                    .any(|item| matches!(item, Injection::ParentMessage { .. }))
            })
            .unwrap_or(false)
    }

    /// Drop `agent`'s queued question of `call_id`, if it is still
    /// undelivered: once answered or cancelled it would only show the parent a
    /// stale ask. Keyed by the asker too — call ids are only unique per agent
    /// (a provider without its own ids numbers them `call_0`, `call_1`, …).
    pub fn retract_question(&self, agent: &AgentId, call_id: &CallId) {
        if let Ok(mut q) = self.queue.lock() {
            q.retain(|item| {
                !matches!(
                    item,
                    Injection::AgentQuestion { agent: a, call_id: c, .. } if a == agent && c == call_id
                )
            });
        }
    }

    /// Take every queued question out (the agent can no longer answer them).
    pub fn take_questions(&self) -> Vec<Injection> {
        self.take_where(|item| matches!(item, Injection::AgentQuestion { .. }))
    }

    /// Take `agent`'s queued reports out.
    pub fn take_agent_finished(&self, agent: &AgentId) -> Vec<Injection> {
        self.take_where(|item| {
            matches!(item, Injection::AgentFinished { agent: info, .. } if &info.id == agent)
        })
    }

    fn take_where(&self, take: impl Fn(&Injection) -> bool) -> Vec<Injection> {
        let Ok(mut q) = self.queue.lock() else {
            return Vec::new();
        };
        let (taken, kept): (VecDeque<_>, VecDeque<_>) = q.drain(..).partition(|item| take(item));
        *q = kept;
        taken.into()
    }

    /// Wait until at least one injection is queued.
    pub async fn wait_nonempty(&self) {
        while self.is_empty() {
            self.notify.notified().await;
        }
    }
}

/// Default total character budget for one rendered batch of injections. The caller
/// takes the budget from `[context] injection_budget_chars` (this is its default).
pub const DEFAULT_INJECTION_BUDGET: usize = 24_000;

/// Render a batch of injections into ONE user message of kind `Injected`, each item
/// wrapped in a marker tag and capped to `cap_chars` (spec §6.4), the whole batch
/// held to roughly `budget_chars`:
///
/// ```text
/// <task_finished id="tsk_…" label="cargo build" exit="0" duration="3m02s">
/// …tail…
/// </task_finished>
/// <agent_finished id="agt_…" name="general-purpose">
/// …answer…
/// </agent_finished>
/// <notice>…</notice>
/// ```
///
/// Items render in order. Once the next full report would cross the budget, the
/// remaining reports (finished/stalled tasks, finished/status agents) become one-line
/// stubs and a single notice counts them. The first item is never dropped: if it alone
/// exceeds the budget it is head/tail truncated to the budget. Steers, notices and
/// questions are small and always rendered in full (per-item cap only).
pub fn render_injections(items: &[Injection], cap_chars: usize) -> Message {
    render_injections_budgeted(items, cap_chars, DEFAULT_INJECTION_BUDGET)
}

/// [`render_injections`] with an explicit batch budget (`[context]
/// injection_budget_chars`).
pub fn render_injections_budgeted(
    items: &[Injection],
    cap_chars: usize,
    budget_chars: usize,
) -> Message {
    let now = pacode_types::now_ms();
    let mut parts: Vec<String> = Vec::with_capacity(items.len() + 1);
    let mut used = 0usize;
    let mut truncated = 0usize;

    for item in items {
        let mut text = render_item(item, cap_chars, now);
        if text.is_empty() {
            // A steer carries its message in history, not in the batch.
            continue;
        }
        if used + text.len() > budget_chars
            && let Some(stub) = report_stub(item)
        {
            if parts.is_empty() {
                // A lone oversized report is shrunk, never dropped.
                text = render_item(item, cap_chars.min(budget_chars), now);
            } else {
                text = stub;
                truncated += 1;
            }
        }
        used += text.len() + 2;
        parts.push(text);
    }
    if truncated > 0 {
        parts.push(format!(
            "<notice>{truncated} reports truncated; use `agent list` to see each agent's status, `agent result <id>` for an answer</notice>"
        ));
    }

    let body = parts.join("\n\n");
    Message::user(body).with_kind(MessageKind::Injected)
}

/// One-line stub for a bulky report item; `None` for items that are always rendered.
fn report_stub(item: &Injection) -> Option<String> {
    match item {
        Injection::AgentFinished { agent, .. } => Some(format!(
            "<agent_finished id=\"{}\" name=\"{}\" status=\"{}\" truncated=\"true\"/>",
            agent.id,
            agent.name,
            format!("{:?}", agent.status).to_lowercase()
        )),
        Injection::AgentStatus { agent, .. } => Some(format!(
            "<agent_status id=\"{}\" name=\"{}\" truncated=\"true\"/>",
            agent.id, agent.name
        )),
        Injection::TaskFinished { task, .. } => Some(format!(
            "<task_finished id=\"{}\" label=\"{}\" truncated=\"true\"/>",
            task.id, task.label
        )),
        Injection::TaskStalled { task, .. } => Some(format!(
            "<task_stalled id=\"{}\" label=\"{}\" truncated=\"true\"/>",
            task.id, task.label
        )),
        Injection::UserSteer { .. }
        | Injection::SystemNotice(_)
        | Injection::StatusRequest
        | Injection::ParentMessage { .. }
        | Injection::AgentQuestion { .. }
        | Injection::SendLanded { .. }
        | Injection::PlanReminder { .. } => None,
    }
}

/// One item's text; empty for items that render nothing (a steer).
fn render_item(item: &Injection, cap_chars: usize, now: u64) -> String {
    match item {
        Injection::UserSteer { .. } => String::new(),
        Injection::TaskFinished { task, tail } => {
            let exit_str = task
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "0".to_string());
            let duration_str = pacode_types::time::format_duration_ms(task.duration_ms(now));
            let capped = pacode_types::truncate_head_tail(tail, cap_chars);
            if capped.trim().is_empty() {
                format!(
                    "<task_finished id=\"{}\" label=\"{}\" exit=\"{exit_str}\" duration=\"{duration_str}\">\n</task_finished>",
                    task.id, task.label
                )
            } else {
                format!(
                    "<task_finished id=\"{}\" label=\"{}\" exit=\"{exit_str}\" duration=\"{duration_str}\">\n{capped}\n</task_finished>",
                    task.id, task.label
                )
            }
        }
        Injection::TaskStalled { task, tail } => {
            let capped = pacode_types::truncate_head_tail(tail, cap_chars);
            if capped.trim().is_empty() {
                format!(
                    "<task_stalled id=\"{}\" label=\"{}\">\n</task_stalled>",
                    task.id, task.label
                )
            } else {
                format!(
                    "<task_stalled id=\"{}\" label=\"{}\">\n{capped}\n</task_stalled>",
                    task.id, task.label
                )
            }
        }
        Injection::AgentFinished { agent, answer } => {
            let capped = pacode_types::truncate_head_tail(answer, cap_chars);
            if capped.trim().is_empty() {
                format!(
                    "<agent_finished id=\"{}\" name=\"{}\">\n</agent_finished>",
                    agent.id, agent.name
                )
            } else {
                format!(
                    "<agent_finished id=\"{}\" name=\"{}\">\n{capped}\n</agent_finished>",
                    agent.id, agent.name
                )
            }
        }
        Injection::StatusRequest => "<status_request>Report your current status briefly with the `report_status` tool (done / in progress / blockers), then continue.</status_request>".to_string(),
        Injection::AgentStatus { agent, text } => {
            let capped = pacode_types::truncate_head_tail(text, cap_chars);
            format!(
                "<agent_status id=\"{}\" name=\"{}\">\n{capped}\n</agent_status>",
                agent.id, agent.name
            )
        }
        Injection::SystemNotice(text) => {
            let capped = pacode_types::truncate_head_tail(text, cap_chars);
            format!("<notice>{capped}</notice>")
        }
        // Both legs of the exchange were capped at the door
        // (`AGENT_MESSAGE_CAP_CHARS`); the injection cap is for reports,
        // and cutting an instruction or a question in half would change
        // what it says.
        Injection::ParentMessage { text } => {
            format!("<orchestrator_message>\n{text}\n</orchestrator_message>")
        }
        Injection::AgentQuestion {
            agent,
            name,
            question,
            ..
        } => format!(
            "<agent_question id=\"{agent}\" name=\"{name}\">\n{question}\n\
             (It is blocked until you answer with `agent send`; ending your turn \
             without an answer lets it proceed on its own judgment.)\n</agent_question>"
        ),
        // Whole, like the exchange itself: a report it carries was capped
        // when it was first queued.
        Injection::SendLanded {
            agent,
            name,
            outcome,
        } => format!(
            "<send_result id=\"{agent}\" name=\"{name}\">\nYour interrupted `agent send` \
             had already reached it; do not send the message again. {}\n</send_result>",
            outcome.describe(agent)
        ),
        // Bounded by its own caps (item count, item length).
        Injection::PlanReminder { open, attempt, max } => {
            crate::plan_reminder::render(open, *attempt, *max)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacode_types::{AgentId, AgentInfo, AgentKind, AgentStatus, TaskId, TaskInfo, TaskStatus};

    #[test]
    fn test_render_injections_tags_and_caps() {
        let task = TaskInfo {
            id: TaskId::generate(),
            session: pacode_types::SessionId::generate(),
            owner: AgentId::main(),
            label: "cargo test".into(),
            command: "cargo test".into(),
            cwd: std::path::PathBuf::from("/tmp"),
            status: TaskStatus::Completed,
            backgrounded: false,
            exit_code: Some(0),
            started_at_ms: 1000,
            ended_at_ms: Some(3000),
            progress: None,
            warnings: 0,
            errors: 0,
            output_path: std::path::PathBuf::from("/tmp/task.log"),
            output_bytes: 100,
            acked: true,
        };

        let agent = AgentInfo {
            id: AgentId::generate(),
            name: "sub-worker".into(),
            kind: AgentKind::Sub,
            status: AgentStatus::Finished,
            activity: None,
            started_at_ms: 1000,
            finished_at_ms: Some(2000),
            tokens_in: 50,
            tokens_out: 50,
            model: pacode_types::ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            },
            effort: pacode_types::Effort::Low,
            parent: Some(AgentId::main()),
            summary: Some("done".into()),
            error: None,
        };

        let items = vec![
            Injection::TaskFinished {
                task: task.clone(),
                tail: "test passed completely".into(),
            },
            Injection::AgentFinished {
                agent: agent.clone(),
                answer: "all files modified successfully".into(),
            },
            Injection::SystemNotice("system warning".into()),
        ];

        let msg = render_injections_budgeted(&items, 1000, DEFAULT_INJECTION_BUDGET);
        assert_eq!(msg.meta.kind, MessageKind::Injected);
        let text = msg.text();
        assert!(text.contains("<task_finished id="));
        assert!(text.contains("label=\"cargo test\""));
        assert!(text.contains("test passed completely"));
        assert!(text.contains("<agent_finished id="));
        assert!(text.contains("name=\"sub-worker\""));
        assert!(text.contains("all files modified successfully"));
        assert!(text.contains("<notice>system warning</notice>"));

        // Test capping
        let overlong = "A".repeat(500);
        let capped_msg = render_injections(&[Injection::SystemNotice(overlong)], 100);
        let capped_text = capped_msg.text();
        assert!(capped_text.contains("characters truncated"));
        assert!(
            render_injections(&[Injection::UserSteer { admission: None }], 100)
                .text()
                .is_empty()
        );
    }

    #[test]
    fn exchange_injections_are_tagged_whole_and_never_held() {
        let question = Injection::AgentQuestion {
            agent: AgentId::new("agt_1"),
            name: "tests".into(),
            call_id: CallId::new("call_1"),
            question: "Q".repeat(300),
        };
        let message = Injection::ParentMessage {
            text: "M".repeat(300),
        };
        let landed = Injection::SendLanded {
            agent: AgentId::new("agt_1"),
            name: "tests".into(),
            outcome: pacode_tools::SendOutcome::Resumed {
                previous_report: Some("R".repeat(300)),
            },
        };
        assert!(!question.is_holdable());
        assert!(!message.is_holdable());
        assert!(!landed.is_holdable());

        // Capped at the door, so the report cap does not cut them again.
        let text = render_injections(&[message, question, landed], 100).text();
        assert!(text.contains("<send_result id=\"agt_1\" name=\"tests\">"));
        assert!(text.contains("do not send the message again"));
        assert!(text.contains(&"R".repeat(300)));
        assert!(text.contains(&format!(
            "<orchestrator_message>\n{}\n</orchestrator_message>",
            "M".repeat(300)
        )));
        assert!(text.contains("<agent_question id=\"agt_1\" name=\"tests\">"));
        assert!(text.contains(&"Q".repeat(300)));
        assert!(text.contains("`agent send`"));
        assert!(!text.contains("truncated"));
    }

    #[test]
    fn queue_reports_messages_and_retracts_questions() {
        let question = |agent: &str, call: &str| Injection::AgentQuestion {
            agent: AgentId::new(agent),
            name: agent.into(),
            call_id: CallId::new(call),
            question: "q".into(),
        };
        let queue = InjectionQueue::default();
        queue.push(question("agt_1", "c1"));
        queue.push(Injection::SystemNotice("n".into()));
        assert!(!queue.has_parent_message());
        queue.push(Injection::ParentMessage { text: "m".into() });
        assert!(queue.has_parent_message());

        queue.retract_question(&AgentId::new("agt_1"), &CallId::new("other"));
        assert_eq!(queue.drain().len(), 3);
    }

    /// Call ids repeat across agents (`call_0`, …): retracting one agent's
    /// question must leave another's with the same id alone.
    #[test]
    fn a_retract_is_keyed_by_the_asking_agent() {
        let question = |agent: &str| Injection::AgentQuestion {
            agent: AgentId::new(agent),
            name: agent.into(),
            call_id: CallId::new("call_0"),
            question: "q".into(),
        };
        let queue = InjectionQueue::default();
        queue.push(question("agt_1"));
        queue.push(question("agt_2"));
        queue.retract_question(&AgentId::new("agt_1"), &CallId::new("call_0"));
        assert_eq!(queue.drain(), vec![question("agt_2")]);
    }

    fn finished(i: usize, answer_len: usize) -> Injection {
        Injection::AgentFinished {
            agent: AgentInfo {
                id: AgentId::new(format!("agt_{i:04}")),
                name: format!("worker-{i}"),
                kind: AgentKind::Sub,
                status: AgentStatus::Finished,
                activity: None,
                started_at_ms: 0,
                finished_at_ms: None,
                tokens_in: 0,
                tokens_out: 0,
                model: pacode_types::ModelRoute {
                    provider: "mock".into(),
                    model: "mock-model".into(),
                },
                effort: pacode_types::Effort::Low,
                parent: Some(AgentId::main()),
                summary: None,
                error: None,
            },
            answer: "x".repeat(answer_len),
        }
    }

    #[test]
    fn injection_budget_stubs_the_tail_but_keeps_every_id_in_order() {
        let items: Vec<Injection> = (0..256).map(|i| finished(i, 4000)).collect();
        let budget = 24_000;
        let text = render_injections_budgeted(&items, 4000, budget).text();
        assert!(text.len() <= budget + 300 + 256 * 120, "len {}", text.len());
        let mut last = 0;
        for i in 0..256 {
            let pos = text
                .find(&format!("id=\"agt_{i:04}\""))
                .expect("every agent id appears");
            assert!(pos >= last, "ordering preserved");
            last = pos;
        }
        assert!(text.contains("truncated=\"true\"/>"));
        assert!(text.contains("reports truncated; use `agent list`"));
    }

    #[test]
    fn injection_under_budget_has_no_stub_or_notice() {
        let items: Vec<Injection> = (0..3).map(|i| finished(i, 100)).collect();
        let text = render_injections_budgeted(&items, 4000, 24_000).text();
        assert!(!text.contains("truncated=\"true\""));
        assert!(!text.contains("reports truncated"));
    }

    #[test]
    fn single_item_over_budget_is_head_tail_truncated_not_dropped() {
        let text = render_injections_budgeted(&[finished(1, 50_000)], 100_000, 2_000).text();
        assert!(text.contains("id=\"agt_0001\""));
        assert!(text.contains("characters truncated"));
        assert!(!text.contains("truncated=\"true\""));
        assert!(text.len() < 3_000, "len {}", text.len());
    }
}
