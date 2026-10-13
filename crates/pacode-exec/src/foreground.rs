//! The foreground hand-off: who tells the model that a task ended.
//!
//! A foreground call waits on its task. When the task ends inside that wait,
//! the call's own result carries the output; when the wait gives up first, the
//! task moves to the background and its end reaches the model later as a
//! `<task_finished>` notice. `TaskInfo::backgrounded` as it stands at the
//! moment the task ends is that verdict, and the core's task router reads it
//! from the `Ended` event.
//!
//! So the switch has to be atomic with the end. The give-up below checks the
//! status and sets the flag under the task lock, and the supervisor's terminal
//! transition clones the info under the same lock: either the task ended
//! first and the waiter reports it, or it was backgrounded first and the router
//! does. A waiter dropped mid-wait (an interrupted call) hands the task over
//! the same way, since nobody is left to report it.

use std::time::Duration;

use pacode_types::{TaskId, TaskInfo};

use super::{TaskManager, WaitResult};
use crate::state::SharedState;

impl TaskManager {
    /// [`Self::wait`] for the caller that owns the task's foreground.
    ///
    /// `Ended` means the task ended while still in the foreground, so the
    /// caller reports its output and no `<task_finished>` follows. `Timeout`
    /// and `Progress` mean the task is now backgrounded, decided in the same
    /// lock step that saw it still running. Dropping the future before it
    /// resolves backgrounds the task as well.
    pub async fn wait_foreground(
        &self,
        id: &TaskId,
        timeout: Duration,
        return_on_progress: bool,
    ) -> WaitResult {
        let mut handoff = Handoff {
            state: &self.state,
            id,
            armed: true,
        };
        let result = self.wait(id, timeout, return_on_progress).await;
        handoff.armed = false;
        match result {
            WaitResult::Ended(_) | WaitResult::NotFound => result,
            WaitResult::Timeout(_) | WaitResult::Progress(_) => match background(&self.state, id) {
                GiveUp::Ended(info) => WaitResult::Ended(info),
                GiveUp::Backgrounded(info) => match result {
                    WaitResult::Progress(_) => WaitResult::Progress(info),
                    _ => WaitResult::Timeout(info),
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

/// Backgrounds the task if the foreground wait is dropped before it resolves.
struct Handoff<'a> {
    state: &'a SharedState,
    id: &'a TaskId,
    armed: bool,
}

impl Drop for Handoff<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = background(self.state, self.id);
        }
    }
}

#[cfg(test)]
#[path = "foreground_tests.rs"]
mod foreground_tests;
