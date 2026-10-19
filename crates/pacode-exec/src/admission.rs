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
//! - Fairness: a freed slot is handed straight to the head of the queue (it never becomes
//!   free while someone waits), so new callers never overtake waiters and a release wakes
//!   exactly one waiter, through that waiter's own `Notify`.
//! - A waiter that leaves after its slot was handed to it (cancelled or dropped at the
//!   same moment) passes the slot on to the next waiter: nothing leaks, nothing starts.
//! - Nothing ticks while idle: waiters sleep on their `Notify`.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::manager::ExecError;

/// Maximum number of spawn calls waiting for a slot.
pub(crate) const MAX_QUEUED: usize = 1024;

#[derive(Debug)]
struct Waiter {
    ticket: u64,
    wake: Arc<Notify>,
}

#[derive(Debug, Default)]
struct AdmissionState {
    /// Slots held, including slots handed to a waiter that has not picked them up yet.
    active: usize,
    next_ticket: u64,
    waiters: VecDeque<Waiter>,
    /// Tickets a slot was handed to, not yet picked up.
    granted: HashSet<u64>,
}

impl AdmissionState {
    /// Give one slot back: to the head waiter when there is one (the slot stays counted
    /// in `active`), otherwise to the pool. Returns the waiter to wake, to be notified
    /// after the lock is released.
    fn release_slot(&mut self) -> Option<Arc<Notify>> {
        match self.waiters.pop_front() {
            Some(waiter) => {
                self.granted.insert(waiter.ticket);
                Some(waiter.wake)
            }
            None => {
                self.active = self.active.saturating_sub(1);
                None
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Admission {
    state: Mutex<AdmissionState>,
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
        let wake = self.admission.lock().release_slot();
        if let Some(wake) = wake {
            wake.notify_one();
        }
    }
}

/// Removes a waiter's ticket when its future ends without being admitted, and passes on
/// a slot that was handed to it in the meantime.
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
        let wake = {
            let mut st = self.admission.lock();
            if st.granted.remove(&self.ticket) {
                st.release_slot()
            } else {
                st.waiters.retain(|w| w.ticket != self.ticket);
                None
            }
        };
        if let Some(wake) = wake {
            wake.notify_one();
        }
    }
}

impl Admission {
    pub(crate) fn new(max_active: usize) -> Self {
        Self::with_queue_cap(max_active, MAX_QUEUED)
    }

    pub(crate) fn with_queue_cap(max_active: usize, max_queued: usize) -> Self {
        Self {
            state: Mutex::new(AdmissionState::default()),
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

    /// Number of slots held (including slots handed to a waiter not yet resumed).
    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.lock().active
    }

    /// A free slot right now, without queueing; `None` when every slot is taken or
    /// someone already waits (a caller that does not wait never overtakes one that does).
    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<SlotPermit> {
        let mut st = self.lock();
        if st.waiters.is_empty() && st.active < self.max_active {
            st.active += 1;
            return Some(SlotPermit {
                admission: Arc::clone(self),
            });
        }
        None
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
        let (ticket, wake) = {
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
            let wake = Arc::new(Notify::new());
            st.waiters.push_back(Waiter {
                ticket,
                wake: Arc::clone(&wake),
            });
            (ticket, wake)
        };
        let mut guard = TicketGuard {
            admission: self,
            ticket,
            armed: true,
        };

        loop {
            // A cancel wins over a slot handed over at the same moment: the guard passes
            // that slot on, and nothing starts.
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(ExecError::Cancelled);
            }
            if self.lock().granted.remove(&ticket) {
                guard.armed = false;
                return Ok(SlotPermit {
                    admission: Arc::clone(self),
                });
            }
            // `notify_one` stores a permit when nobody is parked yet, so a hand-over
            // between the check above and this await is not lost.
            tokio::select! {
                _ = wake.notified() => {}
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
