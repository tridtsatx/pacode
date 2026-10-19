//! Adaptive (AIMD) concurrency limit behind a [`super::ProviderGate`].
//!
//! The configured `max_concurrent` is the CEILING; the effective limit `L` starts there,
//! is halved (never below [`MIN_LIMIT`]) on a 429/503, and grows by one after every `L`
//! successful streams, back up to the ceiling. `max_concurrent = 0` (unlimited) stays
//! unlimited until the first 429/503; from then on it behaves like a ceiling of
//! [`IMPLICIT_CEILING`].
//!
//! Tokio's `Semaphore` cannot shrink while permits are out, so shrinking uses a debt:
//! `permits_in_existence = L + debt`. A decrease forgets as many AVAILABLE permits as it
//! can and records the rest as `debt`; a returning permit pays the debt (is forgotten)
//! instead of going back. An increase pays debt first, then `add_permits`.
//!
//! Bounds and ownership: a handful of integers per provider id, owned by the
//! `ProviderGate` (one per configured provider, see `GateMap`). Nothing is invalidated: a
//! gate replaced because its ceiling changed starts a fresh limiter. The `Mutex` is a
//! std mutex, never held across an await.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// The effective limit never drops below this (unless the ceiling itself is lower).
pub const MIN_LIMIT: usize = 2;
/// Ceiling assumed for an unlimited gate once a 429/503 taught it that a limit exists.
pub const IMPLICIT_CEILING: usize = 256;
/// Semaphore size of an unlimited gate before the first 429 (practically unbounded, and
/// below `Semaphore::MAX_PERMITS` on 32-bit targets).
const UNLIMITED_PERMITS: usize = 1 << 28;
/// Length of a decrease episode when the 429 carried no cooldown hint: further 429s inside
/// it do not halve again.
pub const NO_HINT_EPISODE: Duration = Duration::from_millis(250);

#[derive(Debug)]
struct State {
    /// Effective limit `L`.
    limit: usize,
    /// Current ceiling (`UNLIMITED_PERMITS` until an unlimited gate learned a limit).
    ceiling: usize,
    /// Permits to swallow as they come back (see module docs).
    debt: usize,
    /// Successes since the last limit change.
    successes: usize,
    /// No further decrease (and no success credit) before this instant.
    episode_until: Option<Instant>,
    /// `true` when the gate was configured unlimited and has not seen a 429 yet.
    unlimited_unlearned: bool,
}

/// Semaphore plus AIMD bookkeeping.
#[derive(Debug)]
pub(super) struct Limiter {
    sem: Arc<Semaphore>,
    state: Mutex<State>,
}

impl Limiter {
    /// `ceiling == 0` means unlimited.
    pub(super) fn new(ceiling: usize) -> Self {
        let unlimited = ceiling == 0;
        let size = if unlimited {
            UNLIMITED_PERMITS
        } else {
            ceiling
        };
        Self {
            sem: Arc::new(Semaphore::new(size)),
            state: Mutex::new(State {
                limit: size,
                ceiling: size,
                debt: 0,
                successes: 0,
                episode_until: None,
                unlimited_unlearned: unlimited,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(super) fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.sem)
    }

    /// `None` while an unlimited gate has not learned a limit.
    pub(super) fn effective_limit(&self) -> Option<usize> {
        let st = self.lock();
        (!st.unlimited_unlearned).then_some(st.limit)
    }

    /// `None` while an unlimited gate has not learned a limit.
    pub(super) fn ceiling(&self) -> Option<usize> {
        let st = self.lock();
        (!st.unlimited_unlearned).then_some(st.ceiling)
    }

    pub(super) fn is_unlimited(&self) -> bool {
        self.lock().unlimited_unlearned
    }

    /// Permits currently held (tracked ones only; debt-forgotten permits are already gone).
    pub(super) fn in_flight(&self) -> usize {
        let st = self.lock();
        (st.limit + st.debt).saturating_sub(self.sem.available_permits())
    }

    /// A 429/503 arrived. Halves the limit unless this response belongs to the episode
    /// that already did; `window` is how long the episode lasts (the cooldown hint).
    pub(super) fn on_rate_limited(&self, now: Instant, window: Duration) {
        let mut st = self.lock();
        let end = now + window;
        let fresh = st.episode_until.is_none_or(|until| now >= until);
        st.episode_until = Some(st.episode_until.map_or(end, |cur| cur.max(end)));
        if !fresh {
            return;
        }
        if st.unlimited_unlearned {
            st.unlimited_unlearned = false;
            st.ceiling = IMPLICIT_CEILING;
        }
        let target = (st.limit.min(st.ceiling) / 2).max(MIN_LIMIT).min(st.limit);
        let target = target.min(st.ceiling);
        self.shrink_to(&mut st, target);
        st.successes = 0;
    }

    /// Lower `L` to `target`, forgetting available permits and recording the rest as debt.
    fn shrink_to(&self, st: &mut State, target: usize) {
        let delta = st.limit.saturating_sub(target);
        if delta == 0 {
            return;
        }
        let forgotten = self.sem.forget_permits(delta);
        st.debt += delta - forgotten.min(delta);
        st.limit = target;
    }

    /// A stream delivered its first event. Credits the success unless an episode is
    /// still running (those streams started before the decrease); after `L` credits
    /// the limit grows by one, up to the ceiling.
    pub(super) fn on_success(&self, now: Instant) {
        let mut st = self.lock();
        if st.episode_until.is_some_and(|until| now < until) || st.limit >= st.ceiling {
            return;
        }
        st.successes += 1;
        if st.successes < st.limit.max(MIN_LIMIT) {
            return;
        }
        st.successes = 0;
        st.limit += 1;
        if st.debt > 0 {
            st.debt -= 1;
        } else {
            self.sem.add_permits(1);
        }
    }

    /// Give a permit back, or swallow it when the limit shrank below the permits in use.
    pub(super) fn release(&self, permit: OwnedSemaphorePermit) {
        let mut st = self.lock();
        if st.debt > 0 {
            st.debt -= 1;
            permit.forget();
        } else {
            drop(permit);
        }
    }
}
