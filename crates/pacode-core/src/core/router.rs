//! Background task router forwarding exec task events to sessions, plus the
//! machine-wide resource-guard notices (one toast per open session).

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
        | TaskEvent::Unreported(info)
        | TaskEvent::Stalled(info) => info.id.clone(),
        TaskEvent::Output { info, .. } => info.id.clone(),
        TaskEvent::Resource(notice) => {
            resource_toast(core, notice);
            return;
        }
    };
    // `Unreported` follows the `Ended` that tombstoned the task.
    if !matches!(event, TaskEvent::Unreported(_)) && core.is_task_tombstoned(&task_id) {
        return;
    }
    match event {
        TaskEvent::Unreported(info) => {
            // The call waiting on this task was dropped after the task ended
            // and before it delivered the output: the notice is the only
            // report the model gets.
            if let Some(session) = core.session(&info.session) {
                persist_task(&session, &info).await;
                session.events.emit(Event::TaskUpdated(info.clone()));
                announce_end(&session, info).await;
            }
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
            finish_task(core, info, ended).await;
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

/// Tell the task's owner that it ended, with the tail of its output.
async fn announce_end(session: &std::sync::Arc<crate::session::Session>, info: TaskInfo) {
    let tail_lines = session.tasks.tail(&info.id, 40).await.unwrap_or_default();
    let tail = tail_lines.join("\n");
    if let Some(agent) = session.agent_for_host(&info.owner) {
        session.inject_and_wake(
            &agent,
            crate::inject::Injection::TaskFinished { task: info, tail },
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
