//! The foreground hand-off: who tells the model that a task ended.
//!
//! A foreground call starts its task and waits on it. When the task ends
//! inside that wait, the call's own result carries the output; when the wait
//! gives up first, the task moves to the background and its end reaches the
//! model later as a `<task_finished>` notice. `TaskInfo::backgrounded` as it
//! stands at the moment the task ends is that verdict, and the core's task
//! router reads it from the `Ended` event.
//!
//! The call holds that right for its whole length, not only while it waits: a
//! [`ForegroundGuard`] is created with the task ([`TaskManager::spawn_foreground`])
//! and released only once the call has its result in hand
//! ([`ForegroundGuard::delivered`]). A call dropped anywhere before that — while
//! its task row is being written, while it waits, while it reads the output
//! back — hands the task over in the guard's `Drop`:
//! - still running: the task is backgrounded and its end is announced; or,
//!   for a plain read nobody needs once its call is gone
//!   ([`ForegroundGuard::kill_on_drop`]), killed and never announced;
//! - already ended: its result reached nobody, so [`TaskEvent::Unreported`]
//!   carries it to the router, which announces it like a background end.
//!
//! Every switch is atomic with the end: the guard and the wait's give-up check
//! the status and set the flag under the task lock, and the supervisor's
//! terminal transition clones the info under the same lock. So each task's end
//! is reported exactly once — by its call, or by one notice.

use std::sync::Arc;
use std::time::Duration;

use pacode_types::{TaskId, TaskInfo};

use super::{ExecError, TaskEvent, TaskManager, WaitResult};
use crate::spec::TaskSpec;
use crate::state::SharedState;

/// The right to report a foreground task's end; see the module docs.
#[must_use = "dropping the guard hands the task to the background at once"]
pub struct ForegroundGuard {
    state: Arc<SharedState>,
    id: TaskId,
    armed: bool,
    kill_on_drop: bool,
}

impl std::fmt::Debug for ForegroundGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForegroundGuard")
            .field("id", &self.id)
            .field("armed", &self.armed)
            .field("kill_on_drop", &self.kill_on_drop)
            .finish()
    }
}

impl ForegroundGuard {
    pub fn id(&self) -> &TaskId {
        &self.id
    }

    /// If the call is dropped while the task runs, kill it instead of
    /// backgrounding it: for a read whose output only its call wanted.
    pub fn kill_on_drop(&mut self) {
        self.kill_on_drop = true;
    }

    /// The call has its result: it reports the task itself (an end inside the
    /// wait), or the wait already handed it to the background.
    pub fn delivered(mut self) {
        self.armed = false;
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let unreported = {
            let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = tasks.get_mut(&self.id) else {
                return;
            };
            if entry.info.backgrounded {
                // The wait gave up first: the router reports the end.
                return;
            }
            if !entry.info.status.is_terminal() {
                if self.kill_on_drop {
                    entry.kill_requested = true;
                    entry.kill_tx.send_modify(|requested| *requested = true);
                } else {
                    entry.info.backgrounded = true;
                }
                return;
            }
            if self.kill_on_drop {
                return;
            }
            entry.info.backgrounded = true;
            entry.info.clone()
        };
        let _ = self.state.events.send(TaskEvent::Unreported(unreported));
    }
}

impl TaskManager {
    /// [`Self::spawn`] for a call that waits on the task in the foreground,
    /// with the guard that makes the call the one that reports it. No await
    /// separates the two, so no cancellation can leave a task that nobody
    /// reports.
    pub async fn spawn_foreground(
        &self,
        mut spec: TaskSpec,
    ) -> Result<(TaskInfo, ForegroundGuard), ExecError> {
        spec.background = false;
        let info = self.spawn(spec).await?;
        let guard = ForegroundGuard {
            state: Arc::clone(&self.state),
            id: info.id.clone(),
            armed: true,
            kill_on_drop: false,
        };
        Ok((info, guard))
    }

    /// [`Self::wait`] for the caller that owns the task's foreground.
    ///
    /// `Ended` means the task ended while still in the foreground, so the
    /// caller reports its output and no `<task_finished>` follows. `Timeout`
    /// and `Progress` mean the task is now backgrounded, decided in the same
    /// lock step that saw it still running. A wait dropped before it resolves
    /// leaves the decision to the caller's [`ForegroundGuard`].
    pub async fn wait_foreground(
        &self,
        id: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitResult {
        let result = self.wait(id, timeout, return_on_progress).await;
        match result {
            WaitResult::Ended(_) | WaitResult::NotFound => result,
            WaitResult::Timeout(_) | WaitResult::Progress(_) => match background(&self.state, id) {
                GiveUp::Ended(info) => WaitResult::Ended(info),
                GiveUp::Backgrounded(info) => match result {
                    WaitResult::Progress(_) => WaitResult::Progress(info),
                    WaitResult::Timeout(_) | WaitResult::Ended(_) | WaitResult::NotFound => {
                        WaitResult::Timeout(info)
                    }
                },
                GiveUp::Gone => result,
            },
        }
    }
}

enum GiveUp {
    /// The task ended between the wait's deadline and the lock: it is still
    /// the foreground's to report.
    Ended(TaskInfo),
    Backgrounded(TaskInfo),
    Gone,
}

fn background(state: &SharedState, id: &TaskId) -> GiveUp {
    let mut tasks = state.tasks.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = tasks.get_mut(id) else {
        return GiveUp::Gone;
    };
    if entry.info.status.is_terminal() {
        return GiveUp::Ended(entry.info.clone());
    }
    entry.info.backgrounded = true;
    GiveUp::Backgrounded(entry.info.clone())
}

#[cfg(test)]
#[path = "foreground_tests.rs"]
mod foreground_tests;
