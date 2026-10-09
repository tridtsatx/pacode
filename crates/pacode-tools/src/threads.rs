//! Process-wide budget of extra threads for parallel tool work (`grep`'s
//! search, the shared walk).
//!
//! Every tool call already runs on its own blocking-pool thread; a call that
//! wants to fan out takes *extra* threads from this one budget, sized to the
//! machine's parallelism. A call gets `min(wanted, free)` and runs with what it
//! got — with none free it stays on its own thread. So concurrent calls (a
//! batch of greps, several agents) share the cores instead of multiplying
//! threads, and with them CPU contention and the per-thread malloc arenas that
//! keep freed memory resident.
//!
//! Owner: the [`shared`] singleton; a reservation returns its threads on drop.

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
