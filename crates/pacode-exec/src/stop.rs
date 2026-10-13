//! Stopping tasks: `kill` (SIGTERM to the group, grace, SIGKILL), a whole
//! session, and the daemon's `shutdown`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use pacode_types::{SessionId, TaskId, TaskStatus};

use super::{ExecError, TaskEvent, TaskManager};
use crate::state::TaskEntry;

impl TaskManager {
    fn request_kill(
        &self,
        id: &TaskId,
    ) -> Result<Option<tokio::sync::watch::Receiver<u64>>, ExecError> {
        let mut tasks = self
            .state
            .tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = tasks
            .get_mut(id)
            .ok_or_else(|| ExecError::NotFound(id.clone()))?;
        if entry.info.status.is_terminal() {
            return Ok(None);
        }
        entry.kill_requested = true;
        entry.kill_tx.send_modify(|requested| *requested = true);
        Ok(Some(entry.version_tx.subscribe()))
    }

    fn force_kill_if_running(&self, id: &TaskId) {
        let child_pid = {
            let mut tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match tasks.get_mut(id) {
                Some(entry) if !entry.info.status.is_terminal() => entry.child_pid,
                _ => return,
            }
        };
        crate::supervisor::force_kill(child_pid);
    }

    pub async fn kill(&self, id: &TaskId) -> Result<(), ExecError> {
        let Some(mut rx) = self.request_kill(id)? else {
            return Ok(());
        };
        let timeout = Duration::from_secs(self.state.config.kill_grace_secs.saturating_add(2));
        wait_terminal_until(
            id,
            &self.state.tasks,
            &mut rx,
            tokio::time::Instant::now() + timeout,
        )
        .await;
        self.force_kill_if_running(id);
        Ok(())
    }

    pub async fn kill_session(&self, session: &SessionId) {
        let to_kill: Vec<TaskId> = {
            let tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            tasks
                .values()
                .filter(|entry| {
                    entry.info.session == *session && entry.info.status == TaskStatus::Running
                })
                .map(|entry| entry.info.id.clone())
                .collect()
        };
        let waiters = to_kill
            .iter()
            .filter_map(|id| {
                self.request_kill(id)
                    .ok()
                    .flatten()
                    .map(|rx| (id.clone(), rx))
            })
            .collect::<Vec<_>>();
        let timeout = Duration::from_secs(self.state.config.kill_grace_secs.saturating_add(2));
        let deadline = tokio::time::Instant::now() + timeout;
        for (id, mut rx) in waiters {
            wait_terminal_until(&id, &self.state.tasks, &mut rx, deadline).await;
        }
        for id in &to_kill {
            self.force_kill_if_running(id);
        }
    }

    pub async fn shutdown(&self) {
        let live = self.take_live_supervisors();
        for task in &live {
            let _ = self.request_kill(&task.id);
        }

        let grace = Duration::from_secs(self.state.config.kill_grace_secs)
            + crate::supervisor::DRAIN_GRACE
            + Duration::from_secs(2);
        let mut stuck = await_supervisors(live, tokio::time::Instant::now() + grace).await;
        for task in &stuck {
            crate::supervisor::force_kill(task.child_pid);
        }
        stuck = await_supervisors(
            stuck,
            tokio::time::Instant::now() + crate::supervisor::DRAIN_GRACE,
        )
        .await;
        for task in &stuck {
            task.handle.abort();
        }

        self.finalize_running();
    }

    fn take_live_supervisors(&self) -> Vec<LiveTask> {
        let mut tasks = self
            .state
            .tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        tasks
            .values_mut()
            .filter(|entry| !entry.info.status.is_terminal())
            .filter_map(|entry| {
                let handle = entry.supervisor.take()?;
                Some(LiveTask {
                    id: entry.info.id.clone(),
                    handle,
                    child_pid: entry.child_pid.take(),
                })
            })
            .collect()
    }

    fn finalize_running(&self) {
        let mut ended = Vec::new();
        {
            let mut tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let now = pacode_types::now_ms();
            for entry in tasks
                .values_mut()
                .filter(|entry| !entry.info.status.is_terminal())
            {
                crate::supervisor::force_kill(entry.child_pid.take());
                entry.info.status = TaskStatus::Killed;
                entry.info.exit_code = None;
                entry.info.ended_at_ms = Some(now);
                entry.version_tx.send_modify(|version| *version += 1);
                ended.push(entry.info.clone());
            }
        }
        for info in ended {
            let _ = self.state.events.send(TaskEvent::Ended(info));
        }
    }
}

struct LiveTask {
    id: TaskId,
    handle: tokio::task::JoinHandle<()>,
    child_pid: Option<u32>,
}

async fn await_supervisors(live: Vec<LiveTask>, deadline: tokio::time::Instant) -> Vec<LiveTask> {
    let mut stuck = Vec::new();
    for mut task in live {
        if tokio::time::timeout_at(deadline, &mut task.handle)
            .await
            .is_ok()
        {
            continue;
        }
        stuck.push(task);
    }
    stuck
}

async fn wait_terminal_until(
    id: &TaskId,
    tasks: &Mutex<HashMap<TaskId, TaskEntry>>,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    deadline: tokio::time::Instant,
) {
    loop {
        {
            let tasks = tasks.lock().unwrap_or_else(|error| error.into_inner());
            if tasks
                .get(id)
                .is_none_or(|entry| entry.info.status.is_terminal())
            {
                return;
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero()
            || !matches!(
                tokio::time::timeout(remaining, rx.changed()).await,
                Ok(Ok(()))
            )
        {
            return;
        }
    }
}
