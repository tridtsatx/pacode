//! Injections: things that reach the model between steps (spec §6.2), never
//! mid-stream.

use std::collections::VecDeque;
use std::sync::Mutex;

use codeapp_types::{AgentInfo, Message, MessageKind, TaskInfo};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq)]
pub enum Injection {
    /// The user typed while a turn was running.
    UserSteer(String),
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

    pub fn drain(&self) -> Vec<Injection> {
        self.queue
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.lock().map(|q| q.is_empty()).unwrap_or(true)
    }

    /// Wait until at least one injection is queued.
    pub async fn wait_nonempty(&self) {
        while self.is_empty() {
            self.notify.notified().await;
        }
    }
}

/// Render a batch of injections into ONE user message of kind `Injected`, each item
/// wrapped in a marker tag and capped to `cap_chars` (spec §6.4):
///
/// ```text
/// <task_finished id="tsk_…" label="cargo build" exit="0" duration="3m02s">
/// …tail…
/// </task_finished>
/// <agent_finished id="agt_…" name="general-purpose">
/// …answer…
/// </agent_finished>
/// <user_message>…</user_message>
/// <notice>…</notice>
/// ```
pub fn render_injections(items: &[Injection], cap_chars: usize) -> Message {
    let _ = (items, cap_chars);
    let _ = MessageKind::Injected;
    todo!("inject::render_injections")
}
