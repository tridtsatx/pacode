//! Background task router forwarding exec task events to sessions, plus the
//! machine-wide resource-guard notices (one toast per open session).
//!
//! The receive loop awaits no IO but a task's `Started` persist, kept inline so
//! it lands before that task's `Ended` is handled. The IO half of `Ended`,
//! `Unreported` and `Stalled` (store upsert, output tail, injection, wake) runs
//! on a spawned task, and so does the store half of a lag recovery: with
//! hundreds of agents a store round trip per event in the loop is what makes it
//! lag. Ended tasks are deduplicated through a bounded [`RoutedSet`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use pacode_exec::TaskEvent;
use pacode_types::{Event, SessionId, TaskId, TaskInfo, ToastLevel};
use tokio::sync::broadcast;

use crate::core::Core;

/// Most ended task ids remembered as already routed.
const ROUTED_CAP: usize = 4096;

/// Ended tasks the router already took on (persisted and, when backgrounded,
/// announced to their owner), so a live `Ended` and a lag recovery never report
/// the same task twice, and a late event for an ended task is dropped.
///
/// Cap: `ROUTED_CAP` ids, evicted oldest-first (FIFO by routing order).
/// Invalidation: eviction, and `unmark` when the persist failed (a later
/// recovery may retry it). An evicted task's end time raises `floor_ms`, and a
/// recovery ignores tasks that ended at or before it, so eviction cannot cause
/// a duplicate. Owner: the router task; the handlers it spawns share it behind a
/// std mutex that is never held across an await.
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

    /// Forget `id` (its persist failed): a recovery may route it again.
    fn unmark(&mut self, id: &TaskId) {
        if self.ids.remove(id) {
            self.order.retain(|(queued, _)| queued != id);
        }
    }

    fn contains(&self, id: &TaskId) -> bool {
        self.ids.contains(id)
    }

    /// Whether a recovery may still route a task that ended at `ended_ms`.
    fn may_route(&self, id: &TaskId, ended_ms: u64) -> bool {
        ended_ms > self.floor_ms && !self.contains(id)
    }
}

type Routed = Arc<Mutex<RoutedSet>>;

fn lock(routed: &Routed) -> std::sync::MutexGuard<'_, RoutedSet> {
    routed.lock().unwrap_or_else(|p| p.into_inner())
}

fn ended_ms(info: &TaskInfo) -> u64 {
    info.ended_at_ms.unwrap_or_else(pacode_types::now_ms)
}

async fn route_task_event(
    core: &Arc<Core>,
    event: TaskEvent,
    last_progress: &mut HashMap<TaskId, Instant>,
    routed: &Routed,
) {
    let task_id = match &event {
        TaskEvent::Started(info)
        | TaskEvent::Progress(info)
        | TaskEvent::Ended(info)
        | TaskEvent::Unreported(info)
        | TaskEvent::Stalled(info) => info.id.clone(),
        TaskEvent::Output { info, .. } => info.id.clone(),
        TaskEvent::Resource(notice) => {
            resource_toast(core, notice);
            return;
        }
    };
    // `Unreported` follows the `Ended` that routed the task.
    if !matches!(event, TaskEvent::Unreported(_))
        && (core.is_task_tombstoned(&task_id) || lock(routed).contains(&task_id))
    {
        return;
    }
    match event {
        TaskEvent::Unreported(info) => {
            // The call waiting on this task was dropped after the task ended
            // and before it delivered the output: the notice is the only
            // report the model gets.
            tokio::spawn(report_unreported(Arc::downgrade(core), info));
        }
        TaskEvent::Started(info) => {
            if let Some(session) = core.session(&info.session)
                && persist_task(&session, &info).await
            {
                session.events.emit(Event::TaskAdded(info));
            }
        }
        TaskEvent::Progress(info) => {
            let now = Instant::now();
            let emit = last_progress
                .get(&info.id)
                .is_none_or(|last| now.duration_since(*last) >= Duration::from_secs(1));
            if emit {
                last_progress.insert(info.id.clone(), now);
                if let Some(session) = core.session(&info.session) {
                    session.events.emit(Event::TaskUpdated(info));
                }
            }
        }
        TaskEvent::Ended(info) => {
            last_progress.remove(&info.id);
            if core.session(&info.session).is_none() {
                return;
            }
            if lock(routed).mark(&info.id, ended_ms(&info)) {
                tokio::spawn(finish_task(Arc::downgrade(core), info, routed.clone()));
            }
        }
        TaskEvent::Output { info, call, text } => {
            // Only for a task the client is watching as a *foreground* call: a
            // background task already has its own live view, and sending its
            // tail into the transcript as well would put the same output in two
            // places.
            if info.backgrounded {
                return;
            }
            let Some(call) = call else {
                return;
            };
            let Some(session) = core.session(&info.session) else {
                return;
            };
            let Some(agent) = session.agent_for_host(&info.owner) else {
                return;
            };
            let Some(seq) = session.running_tool_call_seq(&info.owner, &call) else {
                return;
            };
            session.events.emit(Event::ToolOutput {
                agent: agent.id(),
                item_seq: seq,
                text,
            });
        }
        // Answered above, before the task lookup.
        TaskEvent::Resource(_) => {}
        TaskEvent::Stalled(info) => {
            if let Some(session) = core.session(&info.session) {
                session.events.emit(Event::Toast {
                    level: ToastLevel::Warn,
                    title: format!("Task stalled: {}", info.label),
                    detail: Some("No output or progress recently".to_string()),
                });
                tokio::spawn(report_stalled(Arc::downgrade(core), info));
            }
        }
    }
}

/// Machine-wide, so every open session gets the toast. A toast has no dismiss
/// event: `Clear` is a Success toast that replaces it.
fn resource_toast(core: &Core, notice: &pacode_exec::ResourceNotice) {
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
    let sessions = core
        .sessions
        .read()
        .map(|sessions| sessions.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for session in sessions {
        session.events.emit(Event::Toast {
            level,
            title: title.clone(),
            detail: detail.clone(),
        });
    }
}

async fn persist_task(session: &crate::session::Session, info: &TaskInfo) -> bool {
    for attempt in 0..3 {
        match session.store.upsert_task_checked(info).await {
            Ok(true) => return true,
            Ok(false) => return false,
            Err(error) if attempt < 2 => {
                log::warn!(
                    "failed to persist task {} (attempt {}): {error}",
                    info.id,
                    attempt + 1
                );
                tokio::task::yield_now().await;
            }
            Err(error) => {
                log::warn!("failed to persist task {}: {error}", info.id);
            }
        }
    }
    false
}

/// The IO half of `Ended` (the loop already marked it routed): persist, show it
/// ended, and tell a backgrounded task's owner.
async fn finish_task(core_weak: Weak<Core>, info: TaskInfo, routed: Routed) {
    let Some(core) = core_weak.upgrade() else {
        return;
    };
    let Some(session) = core.session(&info.session) else {
        return;
    };
    if !persist_task(&session, &info).await {
        lock(&routed).unmark(&info.id);
        return;
    }
    session.events.emit(Event::TaskUpdated(info.clone()));

    // No toast here: `TaskUpdated` above is what the client
    // reports a finished task from, as one transcript line.
    // A popup on top of it said the same thing twice.

    // A task that ended in the foreground was reported by the call waiting on
    // it (`TaskManager::wait_foreground`): its result already holds the output
    // and the exit code. A notice on top of that repeated up to 40 lines to the
    // model, and one landing after the turn's last drain cost a model round
    // trip of its own — or woke a new turn to say nothing new.
    if !info.backgrounded {
        return;
    }
    announce_end(&session, info).await;
}

/// The IO half of `Unreported`.
async fn report_unreported(core_weak: Weak<Core>, info: TaskInfo) {
    let Some(core) = core_weak.upgrade() else {
        return;
    };
    let Some(session) = core.session(&info.session) else {
        return;
    };
    persist_task(&session, &info).await;
    session.events.emit(Event::TaskUpdated(info.clone()));
    announce_end(&session, info).await;
}

/// The IO half of `Stalled`: tail the output and tell the task's owner.
async fn report_stalled(core_weak: Weak<Core>, info: TaskInfo) {
    let Some(core) = core_weak.upgrade() else {
        return;
    };
    let Some(session) = core.session(&info.session) else {
        return;
    };
    let tail_lines = session.tasks.tail(&info.id, 40).await.unwrap_or_default();
    let tail = tail_lines.join("\n");
    if let Some(agent) = session.agent_for_host(&info.owner) {
        session.inject_and_wake(
            &agent,
            crate::inject::Injection::TaskStalled { task: info, tail },
        );
    }
}

/// Tell the task's owner that it ended, with the tail of its output.
async fn announce_end(session: &Arc<crate::session::Session>, info: TaskInfo) {
    let tail_lines = session.tasks.tail(&info.id, 40).await.unwrap_or_default();
    let tail = tail_lines.join("\n");
    if let Some(agent) = session.agent_for_host(&info.owner) {
        session.inject_and_wake(
            &agent,
            crate::inject::Injection::TaskFinished { task: info, tail },
        );
    }
}

/// After a lag: route the events still retained, show the running tasks as
/// they are now, and hand the ended ones whose `Ended` may have been lost to
/// [`reconcile_ended`], off the loop.
async fn recover_task_state(
    core: &Arc<Core>,
    receiver: &mut broadcast::Receiver<TaskEvent>,
    last_progress: &mut HashMap<TaskId, Instant>,
    routed: &Routed,
) {
    let mut retained = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(event) => retained.push(event),
            Err(broadcast::error::TryRecvError::Lagged(count)) => {
                log::warn!("task event receiver remained behind by {count} events");
            }
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => break,
        }
    }
    for event in retained {
        route_task_event(core, event, last_progress, routed).await;
    }

    let tasks = core.deps.tasks.list(None);
    let mut live = HashSet::new();
    let mut ended = Vec::new();
    for info in tasks {
        let Some(session) = core.session(&info.session) else {
            continue;
        };
        if info.status.is_terminal() {
            if lock(routed).may_route(&info.id, ended_ms(&info)) {
                ended.push(info);
            }
            continue;
        }
        live.insert(info.id.clone());
        last_progress.insert(info.id.clone(), Instant::now());
        session.events.emit(Event::TaskUpdated(info));
    }
    // A task whose `Ended` was lost left its entry behind.
    last_progress.retain(|id, _| live.contains(id));
    if !ended.is_empty() {
        tokio::spawn(reconcile_ended(Arc::downgrade(core), ended, routed.clone()));
    }
}

/// Route ended tasks a lag may have hidden the `Ended` of — except those the
/// store already holds as ended: they were finished before (the router's
/// record of them may have been evicted, or another path persisted them).
async fn reconcile_ended(core_weak: Weak<Core>, ended: Vec<TaskInfo>, routed: Routed) {
    let Some(core) = core_weak.upgrade() else {
        return;
    };
    let mut by_session: HashMap<SessionId, Vec<TaskInfo>> = HashMap::new();
    for info in ended {
        by_session
            .entry(info.session.clone())
            .or_default()
            .push(info);
    }
    for (session_id, tasks) in by_session {
        let Some(session) = core.session(&session_id) else {
            continue;
        };
        // Dropped at the end of this session's pass: only the ids of this
        // batch are ever kept.
        let stored_terminal: HashSet<TaskId> = match session.store.list_tasks(&session_id).await {
            Ok(stored) => stored
                .into_iter()
                .filter(|task| task.status.is_terminal())
                .map(|task| task.id)
                .collect(),
            Err(error) => {
                log::warn!("task recovery: listing tasks of {session_id} failed: {error}");
                HashSet::new()
            }
        };
        for info in tasks {
            let fresh = lock(&routed).mark(&info.id, ended_ms(&info));
            if fresh && !stored_terminal.contains(&info.id) {
                finish_task(core_weak.clone(), info, routed.clone()).await;
            }
        }
    }
}

pub(crate) fn start_task_router(
    core_weak: Weak<Core>,
    mut rx: broadcast::Receiver<TaskEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Cap: one entry per running task (bounded by `exec.max_tasks`); removed
        // on `Ended`, pruned to the running tasks on a lag. Owner: this loop.
        let mut last_progress: HashMap<TaskId, Instant> = HashMap::new();
        let routed: Routed = Arc::new(Mutex::new(RoutedSet::default()));
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    let Some(core) = core_weak.upgrade() else {
                        break;
                    };
                    log::warn!("task event receiver lagged by {count} events");
                    recover_task_state(&core, &mut rx, &mut last_progress, &routed).await;
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let Some(core) = core_weak.upgrade() else {
                break;
            };
            route_task_event(&core, event, &mut last_progress, &routed).await;
        }
    })
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
