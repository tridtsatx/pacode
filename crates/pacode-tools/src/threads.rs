//! Process-wide budget of extra threads for parallel tool work (`grep`'s
//! search, the shared walk).
//!
//! Every tool call already runs on its own blocking-pool thread; while it
//! works, that thread is counted here too ([`ThreadBudget::occupy`]). A call
//! that wants to fan out takes *extra* threads from the same budget, sized to
//! the machine's parallelism: `min(wanted, free)`, and with none free it stays
//! on its own thread. So concurrent calls (a
//! batch of greps, several agents) share the cores instead of multiplying
//! threads, and with them CPU contention and the per-thread malloc arenas that
//! keep freed memory resident.
//!
//! Owner: the [`shared`] singleton; a reservation returns its threads on drop.
//!
//! Outside the budget: `grep`'s long-line lane (`builtin/grep_lane.rs`), one
//! process-lifetime thread that is idle until a file has a line over the
//! search buffer cap.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct ThreadBudget {
    capacity: usize,
    in_use: AtomicUsize,
}

/// Extra threads held by one call; returned to the budget on drop.
#[must_use = "dropping the reservation returns its threads at once"]
pub struct Reservation<'a> {
    budget: &'a ThreadBudget,
    count: usize,
}

impl ThreadBudget {
    pub const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            in_use: AtomicUsize::new(0),
        }
    }

    /// Take up to `wanted` extra threads; possibly none.
    pub fn reserve(&self, wanted: usize) -> Reservation<'_> {
        let mut current = self.in_use.load(Ordering::Relaxed);
        loop {
            let take = wanted.min(self.capacity.saturating_sub(current));
            if take == 0 {
                return Reservation {
                    budget: self,
                    count: 0,
                };
            }
            match self.in_use.compare_exchange_weak(
                current,
                current + take,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Reservation {
                        budget: self,
                        count: take,
                    };
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// Count the calling thread itself while it does the work, whether or not
    /// a slot is free (it is already running). Concurrent calls then see the
    /// cores as taken and get no helpers, instead of each adding threads on
    /// top of the others.
    pub fn occupy(&self) -> Reservation<'_> {
        self.in_use.fetch_add(1, Ordering::AcqRel);
        Reservation {
            budget: self,
            count: 1,
        }
    }

    pub fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Relaxed)
    }
}

impl Reservation<'_> {
    pub fn count(&self) -> usize {
        self.count
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.count > 0 {
            self.budget.in_use.fetch_sub(self.count, Ordering::AcqRel);
        }
    }
}

/// The process-wide budget: one extra thread per available core.
pub fn shared() -> &'static ThreadBudget {
    static BUDGET: OnceLock<ThreadBudget> = OnceLock::new();
    BUDGET.get_or_init(|| {
        ThreadBudget::new(std::thread::available_parallelism().map_or(1, |n| n.get()))
    })
}

#[cfg(test)]
#[path = "threads_tests.rs"]
mod threads_tests;
