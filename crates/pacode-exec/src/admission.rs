//! Admission queue for `TaskManager::spawn` (F4).
//!
//! Instead of failing with an error when `max_tasks` processes already run, callers wait
//! in FIFO order for a slot. A wait can be cancelled through the spec's token.
//!
//! - Owner: `SharedState::admission` (one per `TaskManager`).
//! - Cap: at most `max_active` live permits and `MAX_QUEUED` waiters; a spawn that would
//!   exceed the queue fails with `ExecError::TooManyTasks(MAX_QUEUED)`.
//! - Invalidation: a [`SlotPermit`] frees its slot on drop (the supervisor owns it for the
//!   task's whole life; a failed spawn drops it at once); a waiter's ticket leaves the
//!   queue when its future is dropped, cancelled or admitted.
//! - Fairness: a ticket number per waiter; only the head of the queue may take a slot and
//!   new callers never overtake waiters.
//! - Nothing ticks while idle: waiters sleep on a `Notify`, woken only by a release or a
//!   leaving ticket.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::manager::ExecError;

/// Maximum number of spawn calls waiting for a slot.
pub(crate) const MAX_QUEUED: usize = 1024;

#[derive(Debug, Default)]
struct AdmissionState {
    active: usize,
    next_ticket: u64,
    waiters: VecDeque<u64>,
}

#[derive(Debug)]
pub(crate) struct Admission {
    state: Mutex<AdmissionState>,
    notify: Notify,
    max_active: usize,
    max_queued: usize,
}

/// One running-process slot; frees itself on drop.
#[derive(Debug)]
pub(crate) struct SlotPermit {
    admission: Arc<Admission>,
}

impl Drop for SlotPermit {
    fn drop(&mut self) {
        {
            let mut st = self.admission.lock();
            st.active = st.active.saturating_sub(1);
        }
        self.admission.notify.notify_waiters();
    }
}

/// Removes a waiter's ticket when its future ends without being admitted.
struct TicketGuard<'a> {
    admission: &'a Admission,
    ticket: u64,
    armed: bool,
}

impl Drop for TicketGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        {
            let mut st = self.admission.lock();
            st.waiters.retain(|t| *t != self.ticket);
        }
        self.admission.notify.notify_waiters();
    }
}

impl Admission {
    pub(crate) fn new(max_active: usize) -> Self {
        Self::with_queue_cap(max_active, MAX_QUEUED)
    }

    pub(crate) fn with_queue_cap(max_active: usize, max_queued: usize) -> Self {
        Self {
            state: Mutex::new(AdmissionState::default()),
            notify: Notify::new(),
            max_active: max_active.max(1),
            max_queued,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AdmissionState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of callers currently waiting.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.lock().waiters.len()
    }

    /// Wait for a free slot. `Err(Cancelled)` when `cancel` fires first; nothing is held
    /// then. `Err(TooManyTasks)` when the queue itself is full.
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        cancel: Option<&CancellationToken>,
    ) -> Result<SlotPermit, ExecError> {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(ExecError::Cancelled);
        }
        let ticket = {
            let mut st = self.lock();
            if st.waiters.is_empty() && st.active < self.max_active {
                st.active += 1;
                return Ok(SlotPermit {
                    admission: Arc::clone(self),
                });
            }
            if st.waiters.len() >= self.max_queued {
                return Err(ExecError::TooManyTasks(self.max_queued));
            }
            let ticket = st.next_ticket;
            st.next_ticket += 1;
            st.waiters.push_back(ticket);
            ticket
        };
        let mut guard = TicketGuard {
            admission: self,
            ticket,
            armed: true,
        };

        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Register before checking the state so a release in between is not lost.
            notified.as_mut().enable();
            {
                let mut st = self.lock();
                if st.waiters.front() == Some(&ticket) && st.active < self.max_active {
                    st.waiters.pop_front();
                    st.active += 1;
                    guard.armed = false;
                    drop(st);
                    // The next waiter may be admissible too (several slots freed).
                    self.notify.notify_waiters();
                    return Ok(SlotPermit {
                        admission: Arc::clone(self),
                    });
                }
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = async {
                    match cancel {
                        Some(c) => c.cancelled().await,
                        None => std::future::pending().await,
                    }
                } => return Err(ExecError::Cancelled),
            }
        }
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod admission_tests;
