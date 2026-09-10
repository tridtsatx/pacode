//! Background task router forwarding exec task events to sessions.

use std::collections::HashMap;
use std::sync::Weak;
use std::time::{Duration, Instant};

use pacode_exec::TaskEvent;
use pacode_types::{Event, TaskId, ToastLevel};
use tokio::sync::broadcast;

use crate::core::Core;

pub(crate) fn start_task_router(
    core_weak: Weak<Core>,
    mut rx: broadcast::Receiver<TaskEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_progress: HashMap<TaskId, Instant> = HashMap::new();
        while let Ok(event) = rx.recv().await {
            let Some(core) = core_weak.upgrade() else {
                break;
            };
            match event {
                TaskEvent::Started(info) => {
                    if let Some(session) = core.session(&info.session) {
                        session.events.emit(Event::TaskAdded(info));
                    }
                }
                TaskEvent::Progress(info) => {
                    let now = Instant::now();
                    let emit = match last_progress.get(&info.id) {
                        Some(last) => now.duration_since(*last) >= Duration::from_secs(1),
                        None => true,
                    };
                    if emit {
                        last_progress.insert(info.id.clone(), now);
                        if let Some(session) = core.session(&info.session) {
                            session.events.emit(Event::TaskUpdated(info));
                        }
                    }
                }
                TaskEvent::Ended(info) => {
                    last_progress.remove(&info.id);
                    if let Some(session) = core.session(&info.session) {
                        session.events.emit(Event::TaskUpdated(info.clone()));
                        let _ = session.store.upsert_task(&info).await;

                        // No toast here: `TaskUpdated` above is what the client
                        // reports a finished task from, as one transcript line.
                        // A popup on top of it said the same thing twice.

                        let tail_lines = session.tasks.tail(&info.id, 40).await.unwrap_or_default();
                        let tail = tail_lines.join("\n");

                        let owner_agent =
                            session.agent(&info.owner).or_else(|| session.main_agent());
                        if let Some(agent) = owner_agent {
                            agent
                                .injections
                                .push(crate::inject::Injection::TaskFinished {
                                    task: info.clone(),
                                    tail,
                                });
                            if agent.id().is_main() && !agent.is_running() {
                                crate::turn::start_turn(session.clone(), agent);
                            }
                        }
                    }
                }
                TaskEvent::Stalled(info) => {
                    if let Some(session) = core.session(&info.session) {
                        let toast = Event::Toast {
                            level: ToastLevel::Warn,
                            title: format!("Task stalled: {}", info.label),
                            detail: Some("No output or progress recently".to_string()),
                        };
                        session.events.emit(toast);
                        let tail_lines = session.tasks.tail(&info.id, 40).await.unwrap_or_default();
                        let tail = tail_lines.join("\n");
                        let owner_agent =
                            session.agent(&info.owner).or_else(|| session.main_agent());
                        if let Some(agent) = owner_agent {
                            agent
                                .injections
                                .push(crate::inject::Injection::TaskStalled {
                                    task: info.clone(),
                                    tail,
                                });
                        }
                    }
                }
            }
        }
    })
}
