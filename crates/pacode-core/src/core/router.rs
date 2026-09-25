//! Background task router forwarding exec task events to sessions.

use std::collections::{HashMap, HashSet};
use std::sync::Weak;
use std::time::{Duration, Instant};

use pacode_exec::TaskEvent;
use pacode_types::{Event, TaskId, TaskInfo, ToastLevel};
use tokio::sync::broadcast;

use crate::core::Core;

async fn route_task_event(
    core: &Core,
    event: TaskEvent,
    last_progress: &mut HashMap<TaskId, Instant>,
    ended: &mut HashSet<TaskId>,
) {
    let task_id = match &event {
        TaskEvent::Started(info)
        | TaskEvent::Progress(info)
        | TaskEvent::Ended(info)
        | TaskEvent::Stalled(info) => info.id.clone(),
    };
    if core.is_task_tombstoned(&task_id) {
        return;
    }
    match event {
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
            finish_task(core, info, ended).await;
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
                let owner_agent = session.agent_for_host(&info.owner);
                if let Some(agent) = owner_agent {
                    session.inject_and_wake(
                        &agent,
                        crate::inject::Injection::TaskStalled {
                            task: info.clone(),
                            tail,
                        },
                    );
                }
            }
        }
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

async fn finish_task(core: &Core, info: TaskInfo, ended: &mut HashSet<TaskId>) {
    let Some(session) = core.session(&info.session) else {
        return;
    };
    if !ended.insert(info.id.clone()) {
        return;
    }
    if !persist_task(&session, &info).await {
        ended.remove(&info.id);
        return;
    }
    core.tombstone_tasks([info.id.clone()]);
    session.events.emit(Event::TaskUpdated(info.clone()));

    // No toast here: `TaskUpdated` above is what the client
    // reports a finished task from, as one transcript line.
    // A popup on top of it said the same thing twice.

    let tail_lines = session.tasks.tail(&info.id, 40).await.unwrap_or_default();
    let tail = tail_lines.join("\n");

    let owner_agent = session.agent_for_host(&info.owner);
    if let Some(agent) = owner_agent {
        session.inject_and_wake(
            &agent,
            crate::inject::Injection::TaskFinished {
                task: info.clone(),
                tail,
            },
        );
    }
}

async fn recover_task_state(
    core: &Core,
    receiver: &mut broadcast::Receiver<TaskEvent>,
    last_progress: &mut HashMap<TaskId, Instant>,
    ended: &mut HashSet<TaskId>,
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
        route_task_event(core, event, last_progress, ended).await;
    }

    for session in core
        .sessions
        .read()
        .map(|sessions| sessions.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if let Ok(tasks) = session.store.list_tasks(&session.id).await {
            ended.extend(
                tasks
                    .into_iter()
                    .filter(|task| task.status.is_terminal())
                    .map(|task| task.id),
            );
        }
    }
    let tasks = core.deps.tasks.list(None);
    let task_ids = tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    for info in tasks {
        let Some(session) = core.session(&info.session) else {
            continue;
        };
        if info.status.is_terminal() {
            finish_task(core, info, ended).await;
            continue;
        }
        last_progress.insert(info.id.clone(), Instant::now());
        session.events.emit(Event::TaskUpdated(info));
    }
    ended.retain(|id| task_ids.contains(id));
}

pub(crate) fn start_task_router(
    core_weak: Weak<Core>,
    mut rx: broadcast::Receiver<TaskEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_progress: HashMap<TaskId, Instant> = HashMap::new();
        let mut ended: HashSet<TaskId> = HashSet::new();
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    let Some(core) = core_weak.upgrade() else {
                        break;
                    };
                    log::warn!("task event receiver lagged by {count} events");
                    recover_task_state(&core, &mut rx, &mut last_progress, &mut ended).await;
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let Some(core) = core_weak.upgrade() else {
                break;
            };
            route_task_event(&core, event, &mut last_progress, &mut ended).await;
        }
    })
}
