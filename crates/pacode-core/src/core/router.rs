//! Background task router forwarding exec task events to sessions.
//!
//! The receive loop only matches events, emits cheap client events and spawns the work
//! that awaits IO (store upsert, output tail, injection, turn start). It survives
//! broadcast lag: a `Lagged` triggers `reconcile`, which re-routes terminal tasks whose
//! `Ended` was lost.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use pacode_exec::TaskEvent;
use pacode_types::{Event, TaskId, TaskInfo, ToastLevel};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::core::Core;

/// Most terminal task ids remembered as already routed.
const ROUTED_CAP: usize = 4096;

/// Terminal tasks the router already turned into an injection, so a live `Ended` and a
/// `reconcile` pass never inject the same task twice.
///
/// Cap: `ROUTED_CAP` ids, evicted oldest-first (FIFO by routing order). Invalidation:
/// eviction only; an evicted task's end time raises `floor_ms`, and `reconcile` ignores
/// tasks that ended at or before it, so eviction cannot cause a duplicate. Owner: the
/// one router task that created it (handlers it spawns share it behind a std mutex that
/// is never held across an await).
#[derive(Default)]
pub(crate) struct RoutedSet {
    ids: HashSet<TaskId>,
    order: VecDeque<(TaskId, u64)>,
    floor_ms: u64,
}

impl RoutedSet {
    /// Record `id` (ended at `ended_ms`). Returns false when it was already routed.
    fn mark(&mut self, id: &TaskId, ended_ms: u64) -> bool {
        if !self.ids.insert(id.clone()) {
            return false;
        }
        self.order.push_back((id.clone(), ended_ms));
        while self.order.len() > ROUTED_CAP {
            if let Some((old, old_ms)) = self.order.pop_front() {
                self.ids.remove(&old);
                self.floor_ms = self.floor_ms.max(old_ms);
            }
        }
        true
    }

    fn contains(&self, id: &TaskId) -> bool {
        self.ids.contains(id)
    }
}

fn ended_ms(info: &TaskInfo) -> u64 {
    info.ended_at_ms.unwrap_or_else(pacode_types::now_ms)
}

/// Start the router. Tasks that started before this call are never reconciled.
pub(crate) fn start_task_router(
    core_weak: Weak<Core>,
    rx: broadcast::Receiver<TaskEvent>,
) -> tokio::task::JoinHandle<()> {
    start_task_router_since(core_weak, rx, pacode_types::now_ms())
}

/// Like `start_task_router`, but only tasks started at or after `since_ms` are
/// candidates for `reconcile`.
pub(crate) fn start_task_router_since(
    core_weak: Weak<Core>,
    mut rx: broadcast::Receiver<TaskEvent>,
    since_ms: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Cap: one entry per running task (bounded by `exec.max_tasks`); removed on
        // `Ended`, cleared on lag. Owner: this loop.
        let mut last_progress: HashMap<TaskId, Instant> = HashMap::new();
        let routed: Arc<Mutex<RoutedSet>> = Arc::new(Mutex::new(RoutedSet::default()));
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(RecvError::Lagged(missed)) => {
                    log::warn!("task router lagged, {missed} events dropped; reconciling");
                    last_progress.clear();
                    let Some(core) = core_weak.upgrade() else {
                        break;
                    };
                    reconcile(&core, &core_weak, &routed, since_ms);
                    continue;
                }
                Err(RecvError::Closed) => break,
            };
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
                        // No toast here: `TaskUpdated` is what the client reports a
                        // finished task from, as one transcript line.
                        session.events.emit(Event::TaskUpdated(info.clone()));
                    }
                    let fresh = routed
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .mark(&info.id, ended_ms(&info));
                    if fresh {
                        tokio::spawn(route_ended(core_weak.clone(), info));
                    }
                }
                TaskEvent::Resource(notice) => {
                    // Machine-wide, so every open session gets the toast. A toast has
                    // no dismiss event: `Clear` is a Success toast that replaces it.
                    let (level, title, detail) = match notice.level {
                        pacode_exec::NoticeLevel::Warn => (
                            ToastLevel::Warn,
                            format!("low resources: {}% free", notice.free_percent),
                            Some(notice.message.clone()),
                        ),
                        pacode_exec::NoticeLevel::Breach => (
                            ToastLevel::Error,
                            format!("low resources: {}% free", notice.free_percent),
                            Some(notice.message.clone()),
                        ),
                        pacode_exec::NoticeLevel::Clear => {
                            (ToastLevel::Success, "resources recovered".to_string(), None)
                        }
                    };
                    for id in core.session_ids() {
                        if let Some(session) = core.session(&id) {
                            session.events.emit(Event::Toast {
                                level,
                                title: title.clone(),
                                detail: detail.clone(),
                            });
                        }
                    }
                }
                TaskEvent::Stalled(info) => {
                    if let Some(session) = core.session(&info.session) {
                        session.events.emit(Event::Toast {
                            level: ToastLevel::Warn,
                            title: format!("Task stalled: {}", info.label),
                            detail: Some("No output or progress recently".to_string()),
                        });
                    }
                    tokio::spawn(route_stalled(core_weak.clone(), info));
                }
            }
        }
    })
}

/// Re-route terminal tasks whose `Ended` the lagged receiver never saw. Synchronous:
/// it lists, marks and spawns, so the receive loop still awaits no IO.
fn reconcile(
    core: &Arc<Core>,
    core_weak: &Weak<Core>,
    routed: &Arc<Mutex<RoutedSet>>,
    since_ms: u64,
) {
    for sid in core.session_ids() {
        let Some(session) = core.session(&sid) else {
            continue;
        };
        for info in session.tasks.list(Some(&sid)) {
            if !info.status.is_terminal() || info.started_at_ms < since_ms {
                continue;
            }
            let fresh = {
                let mut set = routed.lock().unwrap_or_else(|p| p.into_inner());
                let ended = ended_ms(&info);
                ended > set.floor_ms && !set.contains(&info.id) && set.mark(&info.id, ended)
            };
            if fresh {
                session.events.emit(Event::TaskUpdated(info.clone()));
                tokio::spawn(route_ended(core_weak.clone(), info));
            }
        }
    }
}

/// The IO half of `Ended`: persist, tail, inject into the owner and wake main.
async fn route_ended(core_weak: Weak<Core>, info: TaskInfo) {
    let Some(core) = core_weak.upgrade() else {
        return;
    };
    let Some(session) = core.session(&info.session) else {
        return;
    };
    let _ = session.store.upsert_task(&info).await;
    let tail = session
        .tasks
        .tail(&info.id, 40)
        .await
        .unwrap_or_default()
        .join("\n");
    let owner_agent = session.agent(&info.owner).or_else(|| session.main_agent());
    if let Some(agent) = owner_agent {
        agent
            .injections
            .push(crate::inject::Injection::TaskFinished {
                task: info.clone(),
                tail,
            });
        if agent.id().is_main() {
            // Idempotent: a no-op when a turn is running (it drains the injection).
            let _ = crate::turn::start_turn(session.clone(), agent);
        }
    }
}

/// The IO half of `Stalled`: tail the output and inject a notice into the owner.
async fn route_stalled(core_weak: Weak<Core>, info: TaskInfo) {
    let Some(core) = core_weak.upgrade() else {
        return;
    };
    let Some(session) = core.session(&info.session) else {
        return;
    };
    let tail = session
        .tasks
        .tail(&info.id, 40)
        .await
        .unwrap_or_default()
        .join("\n");
    let owner_agent = session.agent(&info.owner).or_else(|| session.main_agent());
    if let Some(agent) = owner_agent {
        agent
            .injections
            .push(crate::inject::Injection::TaskStalled {
                task: info.clone(),
                tail,
            });
    }
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
