//! Per-provider admission control shared by every agent that talks to the provider.
//!
//! A [`ProviderGate`] bundles two things:
//! - a [`tokio::sync::Semaphore`] sized from `provider.max_concurrent` (0 = unlimited)
//!   whose permit is held for the whole streaming response (see [`GatedStream`]);
//! - a provider-wide cooldown (`Retry-After` / `x-ratelimit-reset-*`): while it is in the
//!   future every request waits instead of retrying on its own schedule.
//!
//! Ownership and bounds:
//! - [`GateMap`] is owned by `ProviderRegistry` (shared across its clones); it holds one
//!   gate per provider id, so it is bounded by the number of configured providers. A gate
//!   whose configured limit changed is replaced (in-flight requests keep the old gate and
//!   release into it); there is no other invalidation.
//! - The cooldown is a stored `Instant`, never a timer task: nothing runs while idle, and
//!   a hint is clamped to [`MAX_COOLDOWN`].
//! - Cancellation: `acquire` is cancel-safe; dropping the future frees its queue slot and
//!   any permit it had not yet returned.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures::Stream;
use pacode_types::StreamEvent;
use reqwest::header::HeaderMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::{EventStream, ProviderError};

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;

/// Largest cooldown a single server hint may impose.
pub const MAX_COOLDOWN: Duration = Duration::from_secs(300);
/// Total time a request may spend waiting out server-requested cooldowns before it gives
/// up with `RateLimited`.
pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(600);

/// Admission gate for one provider id.
#[derive(Debug)]
pub struct ProviderGate {
    permits: Option<Arc<Semaphore>>,
    limit: u32,
    cooldown_until: Mutex<Option<Instant>>,
    queued: AtomicUsize,
}

/// One slot of a [`ProviderGate`]; dropping it frees the slot. Unlimited gates hand out
/// permits that hold nothing.
#[derive(Debug)]
pub struct GatePermit {
    _permit: Option<OwnedSemaphorePermit>,
}

/// Decrements the queue counter even when the waiting future is dropped.
struct QueueGuard<'a>(&'a AtomicUsize);

impl<'a> QueueGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for QueueGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl ProviderGate {
    /// `max_concurrent == 0` means unlimited (the cooldown still applies).
    pub fn new(max_concurrent: u32) -> Self {
        let permits =
            (max_concurrent > 0).then(|| Arc::new(Semaphore::new(max_concurrent as usize)));
        Self {
            permits,
            limit: max_concurrent,
            cooldown_until: Mutex::new(None),
            queued: AtomicUsize::new(0),
        }
    }

    /// Configured permit count (0 = unlimited).
    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Requests currently waiting for a permit or for the cooldown to elapse.
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    /// Permits not currently held (`None` when unlimited).
    pub fn available(&self) -> Option<usize> {
        self.permits.as_ref().map(|s| s.available_permits())
    }

    /// Time left on the provider-wide cooldown, if any.
    pub fn cooldown_remaining(&self) -> Option<Duration> {
        let until = (*self
            .cooldown_until
            .lock()
            .unwrap_or_else(|p| p.into_inner()))?;
        let left = until.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    /// Start (or extend, never shorten) the provider-wide cooldown.
    pub fn start_cooldown(&self, delay: Duration) {
        let until = Instant::now() + delay.min(MAX_COOLDOWN);
        let mut slot = self
            .cooldown_until
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let current: Option<Instant> = *slot;
        *slot = Some(current.map_or(until, |cur| cur.max(until)));
    }

    /// Record a failed response. For 429/503 carrying a usable hint the shared cooldown is
    /// started and the hint returned: the caller should retry without its own backoff
    /// (the next `acquire` waits). Anything else returns `None`.
    pub fn note_response(&self, status: u16, headers: &HeaderMap) -> Option<Duration> {
        if status != 429 && status != 503 {
            return None;
        }
        let delay = crate::retry_after::delay_from_headers(headers, SystemTime::now())?;
        self.start_cooldown(delay);
        Some(delay)
    }

    /// Wait for the cooldown to elapse and for a free slot. Cancel-safe.
    pub async fn acquire(&self) -> GatePermit {
        let _queued = QueueGuard::new(&self.queued);
        loop {
            while let Some(left) = self.cooldown_remaining() {
                tokio::time::sleep(left).await;
            }
            let permit = match &self.permits {
                Some(sem) => sem.clone().acquire_owned().await.ok(),
                None => None,
            };
            // A 429 may have landed while we queued: do not fire into it.
            if self.cooldown_remaining().is_some() {
                drop(permit);
                continue;
            }
            return GatePermit { _permit: permit };
        }
    }
}

/// One gate per provider id; see the module docs for cap and invalidation.
#[derive(Debug, Default)]
pub(crate) struct GateMap {
    gates: Mutex<BTreeMap<String, Arc<ProviderGate>>>,
}

impl GateMap {
    /// The gate for `id`, created (or replaced when `max_concurrent` changed) on demand.
    pub(crate) fn gate_for(&self, id: &str, max_concurrent: u32) -> Arc<ProviderGate> {
        let mut gates = self.gates.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = gates.get(id)
            && existing.limit() == max_concurrent
        {
            return Arc::clone(existing);
        }
        let gate = Arc::new(ProviderGate::new(max_concurrent));
        gates.insert(id.to_string(), Arc::clone(&gate));
        gate
    }
}

/// Event stream that owns its gate permit: the slot is freed when the stream ends, fails,
/// reports `MessageEnd`, or is dropped.
pub(crate) struct GatedStream {
    inner: EventStream,
    permit: Option<GatePermit>,
}

impl GatedStream {
    pub(crate) fn wrap(inner: EventStream, permit: GatePermit) -> EventStream {
        Box::pin(Self {
            inner,
            permit: Some(permit),
        })
    }
}

impl Stream for GatedStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let poll = this.inner.as_mut().poll_next(cx);
        if matches!(
            &poll,
            Poll::Ready(None | Some(Err(_)) | Some(Ok(StreamEvent::MessageEnd { .. })))
        ) {
            this.permit = None;
        }
        poll
    }
}
