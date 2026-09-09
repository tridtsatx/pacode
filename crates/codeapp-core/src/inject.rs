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
    /// Orchestrator asked a subagent for its status.
    StatusRequest,
    /// A subagent reported its status to the parent.
    AgentStatus {
        agent: AgentInfo,
        text: String,
    },
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
    let now = codeapp_types::now_ms();
    let mut parts = Vec::new();

    for item in items {
        match item {
            Injection::UserSteer(text) => {
                let capped = codeapp_types::truncate_head_tail(text, cap_chars);
                parts.push(format!("<user_message>{capped}</user_message>"));
            }
            Injection::TaskFinished { task, tail } => {
                let exit_str = task
                    .exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "0".to_string());
                let duration_str = codeapp_types::time::format_duration_ms(task.duration_ms(now));
                let capped = codeapp_types::truncate_head_tail(tail, cap_chars);
                if capped.trim().is_empty() {
                    parts.push(format!(
                        "<task_finished id=\"{}\" label=\"{}\" exit=\"{exit_str}\" duration=\"{duration_str}\">\n</task_finished>",
                        task.id, task.label
                    ));
                } else {
                    parts.push(format!(
                        "<task_finished id=\"{}\" label=\"{}\" exit=\"{exit_str}\" duration=\"{duration_str}\">\n{capped}\n</task_finished>",
                        task.id, task.label
                    ));
                }
            }
            Injection::TaskStalled { task, tail } => {
                let capped = codeapp_types::truncate_head_tail(tail, cap_chars);
                if capped.trim().is_empty() {
                    parts.push(format!(
                        "<task_stalled id=\"{}\" label=\"{}\">\n</task_stalled>",
                        task.id, task.label
                    ));
                } else {
                    parts.push(format!(
                        "<task_stalled id=\"{}\" label=\"{}\">\n{capped}\n</task_stalled>",
                        task.id, task.label
                    ));
                }
            }
            Injection::AgentFinished { agent, answer } => {
                let capped = codeapp_types::truncate_head_tail(answer, cap_chars);
                if capped.trim().is_empty() {
                    parts.push(format!(
                        "<agent_finished id=\"{}\" name=\"{}\">\n</agent_finished>",
                        agent.id, agent.name
                    ));
                } else {
                    parts.push(format!(
                        "<agent_finished id=\"{}\" name=\"{}\">\n{capped}\n</agent_finished>",
                        agent.id, agent.name
                    ));
                }
            }
            Injection::StatusRequest => {
                parts.push(
                    "<status_request>Report your current status briefly with the `report_status` tool (done / in progress / blockers), then continue.</status_request>"
                        .to_string(),
                );
            }
            Injection::AgentStatus { agent, text } => {
                let capped = codeapp_types::truncate_head_tail(text, cap_chars);
                parts.push(format!(
                    "<agent_status id=\"{}\" name=\"{}\">\n{capped}\n</agent_status>",
                    agent.id, agent.name
                ));
            }
            Injection::SystemNotice(text) => {
                let capped = codeapp_types::truncate_head_tail(text, cap_chars);
                parts.push(format!("<notice>{capped}</notice>"));
            }
        }
    }

    let body = parts.join("\n\n");
    Message::user(body).with_kind(MessageKind::Injected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeapp_types::{AgentId, AgentInfo, AgentKind, AgentStatus, TaskId, TaskInfo, TaskStatus};

    #[test]
    fn test_render_injections_tags_and_caps() {
        let task = TaskInfo {
            id: TaskId::generate(),
            session: codeapp_types::SessionId::generate(),
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
            model: codeapp_types::ModelRoute {
                provider: "mock".into(),
                model: "mock-model".into(),
            },
            effort: codeapp_types::Effort::Low,
            parent: Some(AgentId::main()),
            summary: Some("done".into()),
            error: None,
        };

        let items = vec![
            Injection::UserSteer("please do this first".into()),
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

        let msg = render_injections(&items, 1000);
        assert_eq!(msg.meta.kind, MessageKind::Injected);
        let text = msg.text();
        assert!(text.contains("<user_message>please do this first</user_message>"));
        assert!(text.contains("<task_finished id="));
        assert!(text.contains("label=\"cargo test\""));
        assert!(text.contains("test passed completely"));
        assert!(text.contains("<agent_finished id="));
        assert!(text.contains("name=\"sub-worker\""));
        assert!(text.contains("all files modified successfully"));
        assert!(text.contains("<notice>system warning</notice>"));

        // Test capping
        let overlong = "A".repeat(500);
        let capped_msg = render_injections(&[Injection::UserSteer(overlong)], 100);
        let capped_text = capped_msg.text();
        assert!(capped_text.contains("characters truncated"));
    }
}
