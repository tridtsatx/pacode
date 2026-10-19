//! Admission control for provider accounts: how many requests of one account
//! may be in flight, how much of the rest may wait, and for how long a
//! `Retry-After` parks the account.
//!
//! The configured `max_concurrency` is a CEILING: the limit actually enforced
//! adapts AIMD-style. A rate-limit episode (429, 503, 529, or one reported
//! inside the stream) halves it once, never below [`MIN_LIMIT`]; every `L`
//! streams that produced output outside an episode raise it by one, back up to
//! the ceiling. `active` is a plain counter, so a shrink needs no bookkeeping:
//! a release simply does not hand its slot on while `active` is over the limit.
//!
//! Waiters queue FIFO and are woken one at a time: a release hands its slot
//! straight to the oldest live waiter, and a newcomer never overtakes the queue.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use pacode_types::{LatencyHist, StreamEvent};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use futures::Stream;
use tokio::sync::oneshot;

use crate::{EventStream, ProviderError, ProviderLimits};

#[cfg(test)]
#[path = "governor_tests.rs"]
mod governor_tests;

const MAX_GOVERNORS: usize = 1024;

/// The adaptive limit never drops below this (unless the ceiling itself is lower).
pub(crate) const MIN_LIMIT: usize = 2;
/// Shortest rate-limit episode: further rate limits inside it do not shrink the
/// limit again (they come from requests sent before the first answer arrived).
pub(crate) const MIN_EPISODE: Duration = Duration::from_millis(250);
/// Longest episode, matching the longest cooldown a server hint may impose.
const MAX_EPISODE: Duration = Duration::from_secs(5 * 60);

/// AIMD bookkeeping beside the adaptive limit. Touched once per rate limit and
/// once per credited success, never on the admission path.
#[derive(Default)]
struct Aimd {
    /// Successes credited since the last limit change.
    successes: usize,
    /// No further decrease (and no success credit) before this instant.
    episode_until: Option<Instant>,
}

pub struct ProviderGovernor {
    max_concurrency: AtomicUsize,
    /// Adaptive limit, `1..=max_concurrency`; admission uses the smaller of the two.
    effective: AtomicUsize,
    max_queue_items: AtomicUsize,
    max_queue_bytes: AtomicU64,
    active: AtomicUsize,
    queued_items: AtomicUsize,
    queued_bytes: AtomicU64,
    cooldown_until: Mutex<Option<Instant>>,
    aimd: Mutex<Aimd>,
    /// Parked callers, oldest first. A grant is a `()` sent to one of them with
    /// its slot already counted in `active`. Non-empty only while `active` is at
    /// the limit: every path that frees capacity drains it first.
    waiters: Mutex<VecDeque<oneshot::Sender<()>>>,
    /// High-water marks and outcomes. Atomics on the admission path only: a
    /// caller that never reaches the governor costs nothing, and an idle daemon
    /// has no poller to wake.
    high_water_active: AtomicUsize,
    high_water_queued: AtomicUsize,
    admitted: AtomicU64,
    queued_admissions: AtomicU64,
    refused: AtomicU64,
    cooldown_entries: AtomicU64,
    /// How long callers waited for a permit. This is the number that says
    /// whether the concurrency ceiling is the binding constraint.
    wait: LatencyHist,
}

impl ProviderGovernor {
    fn new(limits: ProviderLimits) -> Arc<Self> {
        Arc::new(Self {
            max_concurrency: AtomicUsize::new(limits.max_concurrency),
            effective: AtomicUsize::new(limits.max_concurrency),
            max_queue_items: AtomicUsize::new(limits.max_queue_items),
            max_queue_bytes: AtomicU64::new(limits.max_queue_bytes),
            active: AtomicUsize::new(0),
            queued_items: AtomicUsize::new(0),
            queued_bytes: AtomicU64::new(0),
            cooldown_until: Mutex::new(None),
            aimd: Mutex::new(Aimd::default()),
            waiters: Mutex::new(VecDeque::new()),
            high_water_active: AtomicUsize::new(0),
            high_water_queued: AtomicUsize::new(0),
            admitted: AtomicU64::new(0),
            queued_admissions: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            cooldown_entries: AtomicU64::new(0),
            wait: LatencyHist::new(),
        })
    }

    fn apply_limits(&self, limits: ProviderLimits) {
        let old = self
            .max_concurrency
            .swap(limits.max_concurrency, Ordering::AcqRel);
        // A limit that never adapted follows the ceiling; an adapted one keeps
        // its place (it grows back on its own) and is only clamped.
        let _ = self
            .effective
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |effective| {
                Some(if effective >= old {
                    limits.max_concurrency
                } else {
                    effective.min(limits.max_concurrency)
                })
            });
        self.max_queue_items
            .store(limits.max_queue_items, Ordering::Release);
        self.max_queue_bytes
            .store(limits.max_queue_bytes, Ordering::Release);
        self.admit_waiters();
    }

    /// The limit admission enforces: the adaptive one, never above the ceiling.
    pub(crate) fn limit(&self) -> usize {
        self.effective
            .load(Ordering::Acquire)
            .min(self.max_concurrency.load(Ordering::Acquire))
            .max(1)
    }

    pub(crate) fn cooldown_remaining(&self) -> Duration {
        self.cooldown_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .map_or(Duration::ZERO, |until| {
                until
                    .checked_duration_since(Instant::now())
                    .unwrap_or(Duration::ZERO)
            })
    }

    pub(crate) fn start_cooldown(&self, delay: Duration) {
        self.cooldown_entries.fetch_add(1, Ordering::Relaxed);
        let Some(until) = Instant::now().checked_add(delay) else {
            return;
        };
        let mut current = self
            .cooldown_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.as_ref().is_none_or(|value| *value < until) {
            *current = Some(until);
        }
    }

    /// The server is pacing this account (429/503/529, or a rate limit reported
    /// inside the stream): park it for `delay` and, once per episode, halve the
    /// adaptive limit. The episode lasts as long as the cooldown (at least
    /// [`MIN_EPISODE`]): requests already in flight when the first answer came
    /// back meet the same limit and must not halve it again.
    pub(crate) fn note_rate_limited(&self, delay: Duration) {
        self.start_cooldown(delay);
        let now = Instant::now();
        let window = delay.clamp(MIN_EPISODE, MAX_EPISODE);
        let mut aimd = self.lock_aimd();
        let fresh = aimd.episode_until.is_none_or(|until| now >= until);
        if let Some(end) = now.checked_add(window) {
            aimd.episode_until = Some(aimd.episode_until.map_or(end, |until| until.max(end)));
        }
        if !fresh {
            return;
        }
        aimd.successes = 0;
        let ceiling = self.max_concurrency.load(Ordering::Acquire);
        let _ = self
            .effective
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |effective| {
                let current = effective.min(ceiling);
                Some((current / 2).max(MIN_LIMIT).min(current).max(1))
            });
    }

    /// A request this governor admitted produced its first event. Credits one
    /// success unless an episode is still running (those requests were sent
    /// before the decrease); after `L` credits the limit grows by one, up to
    /// the ceiling, and the slot goes straight to the oldest waiter.
    pub(crate) fn on_success(&self) {
        let ceiling = self.max_concurrency.load(Ordering::Acquire);
        // The steady state (never rate limited) costs two atomic loads.
        if self.effective.load(Ordering::Acquire) >= ceiling {
            return;
        }
        let mut aimd = self.lock_aimd();
        let effective = self.effective.load(Ordering::Acquire);
        if aimd
            .episode_until
            .is_some_and(|until| Instant::now() < until)
            || effective >= ceiling
        {
            return;
        }
        aimd.successes += 1;
        if aimd.successes < effective.max(MIN_LIMIT) {
            return;
        }
        aimd.successes = 0;
        let _ = self.effective.compare_exchange(
            effective,
            effective + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        drop(aimd);
        self.admit_waiters();
    }

    fn lock_aimd(&self) -> MutexGuard<'_, Aimd> {
        self.aimd
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_waiters(&self) -> MutexGuard<'_, VecDeque<oneshot::Sender<()>>> {
        self.waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_idle(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0 && self.cooldown_remaining().is_zero()
    }

    /// Take a slot if one is free under the adaptive limit. A limit shrinking
    /// concurrently can let a racer in over it; `release` absorbs that by not
    /// handing the slot on.
    fn try_admit(&self) -> bool {
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= self.limit() {
                return false;
            }
            match self.active.compare_exchange(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.high_water_active
                        .fetch_max(active + 1, Ordering::Relaxed);
                    return true;
                }
                Err(current) => active = current,
            }
        }
    }

    /// Wrap a slot already counted in `active`.
    fn grant(self: &Arc<Self>) -> GovernorPermit {
        self.admitted.fetch_add(1, Ordering::Relaxed);
        GovernorPermit {
            governor: Arc::clone(self),
        }
    }

    fn admit_waiters(&self) {
        let mut waiters = self.lock_waiters();
        self.admit_waiters_locked(&mut waiters);
    }

    /// Hand every free slot to the oldest live waiters, one wake per slot. A
    /// waiter whose acquire was cancelled refuses the grant and the slot moves
    /// on; with nobody left it is returned.
    fn admit_waiters_locked(&self, waiters: &mut VecDeque<oneshot::Sender<()>>) {
        while !waiters.is_empty() && self.try_admit() {
            let handed = loop {
                match waiters.pop_front() {
                    Some(waiter) => {
                        if waiter.send(()).is_ok() {
                            break true;
                        }
                    }
                    None => break false,
                }
            };
            if !handed {
                self.active.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    /// Join the FIFO, or take a slot at once (`None`) when nobody is ahead and
    /// one is free.
    fn enqueue(&self) -> Option<oneshot::Receiver<()>> {
        let mut waiters = self.lock_waiters();
        // Senders of cancelled acquires stay queued until a release skips them;
        // prune them before they pile up behind an account that is stuck.
        if waiters.len() >= self.max_queue_items.load(Ordering::Acquire).max(1) {
            waiters.retain(|waiter| !waiter.is_closed());
        }
        self.admit_waiters_locked(&mut waiters);
        if waiters.is_empty() && self.try_admit() {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        waiters.push_back(sender);
        Some(receiver)
    }

    fn reserve_queue(self: &Arc<Self>, bytes: u64) -> Result<QueueSlot, ProviderError> {
        let max_items = self.max_queue_items.load(Ordering::Acquire);
        let max_bytes = self.max_queue_bytes.load(Ordering::Acquire);
        let items = self.queued_items.fetch_add(1, Ordering::AcqRel) + 1;
        self.high_water_queued.fetch_max(items, Ordering::Relaxed);
        self.queued_admissions.fetch_add(1, Ordering::Relaxed);
        if items > max_items {
            self.queued_items.fetch_sub(1, Ordering::AcqRel);
            self.refused.fetch_add(1, Ordering::Relaxed);
            return Err(queue_full_error(items - 1, bytes, max_items, max_bytes));
        }
        let queued_bytes = self.queued_bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
        if queued_bytes > max_bytes {
            self.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
            self.queued_items.fetch_sub(1, Ordering::AcqRel);
            self.refused.fetch_add(1, Ordering::Relaxed);
            return Err(queue_full_error(items, bytes, max_items, max_bytes));
        }
        Ok(QueueSlot {
            governor: Arc::clone(self),
            bytes,
            released: false,
        })
    }

    /// Wait for a slot. Cancel-safe: dropping the future frees its queue room
    /// and returns a slot that was handed to it but not yet taken.
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        queued_bytes: u64,
    ) -> Result<GovernorPermit, ProviderError> {
        // A newcomer only skips the queue when nobody waits in it: FIFO.
        if self.queued_items.load(Ordering::Acquire) == 0
            && self.cooldown_remaining().is_zero()
            && self.try_admit()
        {
            return Ok(self.grant());
        }

        let mut slot = self.reserve_queue(queued_bytes)?;
        let queued_at = Instant::now();
        loop {
            self.wait_for_cooldown().await;
            let Some(receiver) = self.enqueue() else {
                break;
            };
            let mut waiter = Waiter {
                receiver: Some(receiver),
                governor: self,
            };
            if waiter.granted().await {
                break;
            }
        }
        let permit = self.grant();
        slot.release();
        // Only callers that actually queued are recorded: an immediate permit
        // has no wait worth a bucket, and counting it would drag the mean down
        // to zero and hide the queueing that matters.
        self.wait.record_duration(queued_at.elapsed());
        // A cooldown that started after the slot was handed over is waited out
        // holding it: the turn is kept and nothing fires into the rate limit.
        self.wait_for_cooldown().await;
        Ok(permit)
    }

    async fn wait_for_cooldown(&self) {
        loop {
            let delay = self.cooldown_remaining();
            if delay.is_zero() {
                return;
            }
            tokio::time::sleep(delay).await;
        }
    }

    fn release(&self) {
        let mut waiters = self.lock_waiters();
        if self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .is_ok()
        {
            self.admit_waiters_locked(&mut waiters);
        }
    }
}

/// A queued caller's end of the FIFO. Dropped before its grant was taken (the
/// acquire was cancelled), it closes the channel and returns a slot that had
/// already been handed to it.
struct Waiter<'a> {
    receiver: Option<oneshot::Receiver<()>>,
    governor: &'a ProviderGovernor,
}

impl Waiter<'_> {
    /// `true` once a slot was handed over; `false` when the grant was withdrawn
    /// and the caller has to queue again.
    async fn granted(&mut self) -> bool {
        let Some(receiver) = self.receiver.as_mut() else {
            return false;
        };
        let granted = receiver.await.is_ok();
        self.receiver = None;
        granted
    }
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        if let Some(mut receiver) = self.receiver.take() {
            // Close first: a release racing this drop then fails its send and
            // moves on instead of handing a slot to nobody.
            receiver.close();
            if receiver.try_recv().is_ok() {
                self.governor.release();
            }
        }
    }
}

fn queue_full_error(items: usize, bytes: u64, max_items: usize, max_bytes: u64) -> ProviderError {
    ProviderError::RateLimited(format!(
        "provider request queue full ({items}/{max_items} items, {bytes}/{max_bytes} bytes)"
    ))
}

struct QueueSlot {
    governor: Arc<ProviderGovernor>,
    bytes: u64,
    released: bool,
}

impl QueueSlot {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.governor.queued_items.fetch_sub(1, Ordering::AcqRel);
        self.governor
            .queued_bytes
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl Drop for QueueSlot {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) struct GovernorPermit {
    governor: Arc<ProviderGovernor>,
}

impl GovernorPermit {
    /// The request this permit admitted produced its first event: credit one
    /// success toward the AIMD increase. [`hold_permit`] calls it once per stream.
    pub(crate) fn record_success(&self) {
        self.governor.on_success();
    }
}

impl Drop for GovernorPermit {
    fn drop(&mut self) {
        self.governor.release();
    }
}

impl ProviderGovernor {
    pub fn stats(&self) -> pacode_types::GovernorStats {
        pacode_types::GovernorStats {
            max_concurrency: self.max_concurrency.load(Ordering::Acquire),
            in_flight: self.active.load(Ordering::Acquire),
            high_water_in_flight: self.high_water_active.load(Ordering::Relaxed),
            queued: self.queued_items.load(Ordering::Acquire),
            high_water_queued: self.high_water_queued.load(Ordering::Relaxed),
            admitted: self.admitted.load(Ordering::Relaxed),
            queued_admissions: self.queued_admissions.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            cooldown_entries: self.cooldown_entries.load(Ordering::Relaxed),
            cooldown_remaining_ms: self.cooldown_remaining().as_millis() as u64,
            wait: self.wait.snapshot(),
        }
    }
}

/// Every live governor, folded into one reading. Takes the registry lock, so
/// it is a diagnostic call and not something to put on a hot path.
pub(crate) fn provider_stats() -> pacode_types::ProviderStats {
    let registry = lock_registry();
    let mut total = pacode_types::ProviderStats {
        governors: registry.len(),
        ..pacode_types::ProviderStats::default()
    };
    for entry in registry.values() {
        let stats = entry.governor.stats();
        total.in_flight += stats.in_flight;
        total.high_water_in_flight = total.high_water_in_flight.max(stats.high_water_in_flight);
        total.queued += stats.queued;
        total.high_water_queued = total.high_water_queued.max(stats.high_water_queued);
        total.admitted += stats.admitted;
        total.queued_admissions += stats.queued_admissions;
        total.refused += stats.refused;
        total.cooldown_entries += stats.cooldown_entries;
        total.wait_total_micros += stats.wait.total_micros;
        total.wait_max_micros = total.wait_max_micros.max(stats.wait.max_micros);
        total.wait_count += stats.wait.count;
    }
    total
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GovernorKey {
    provider: u64,
    account: u64,
}

struct GovernorEntry {
    governor: Arc<ProviderGovernor>,
    last_used: Instant,
}

type GovernorRegistry = HashMap<GovernorKey, GovernorEntry>;

fn governor_registry() -> &'static Mutex<GovernorRegistry> {
    static REGISTRY: OnceLock<Mutex<GovernorRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_registry() -> MutexGuard<'static, GovernorRegistry> {
    governor_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn overflow_governor() -> &'static Arc<ProviderGovernor> {
    static OVERFLOW: OnceLock<Arc<ProviderGovernor>> = OnceLock::new();
    OVERFLOW.get_or_init(|| ProviderGovernor::new(crate::provider_limits()))
}

pub(crate) fn apply_provider_limits(limits: ProviderLimits) {
    let registry = lock_registry();
    for entry in registry.values() {
        entry.governor.apply_limits(limits);
    }
    drop(registry);
    overflow_governor().apply_limits(limits);
}

fn governor_for_key(key: GovernorKey) -> Arc<ProviderGovernor> {
    let mut registry = lock_registry();
    if let Some(entry) = registry.get_mut(&key) {
        entry.last_used = Instant::now();
        return Arc::clone(&entry.governor);
    }

    if registry.len() >= MAX_GOVERNORS && !evict_idle_governor(&mut registry) {
        return Arc::clone(overflow_governor());
    }

    let governor = ProviderGovernor::new(crate::provider_limits());
    registry.insert(
        key,
        GovernorEntry {
            governor: Arc::clone(&governor),
            last_used: Instant::now(),
        },
    );
    governor
}

fn evict_idle_governor(registry: &mut GovernorRegistry) -> bool {
    let victim = registry
        .iter()
        .filter(|(_, entry)| entry.governor.is_idle())
        .min_by_key(|(_, entry)| entry.last_used)
        .map(|(key, _)| *key);
    match victim {
        Some(key) => registry.remove(&key).is_some(),
        None => false,
    }
}

fn governor_key(url: &str, api_key: Option<&str>, headers: &[(String, String)]) -> GovernorKey {
    let mut provider_hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut provider_hasher);

    let mut account_hasher = std::collections::hash_map::DefaultHasher::new();
    match api_key {
        Some(key) if !key.is_empty() => key.hash(&mut account_hasher),
        _ => 0u8.hash(&mut account_hasher),
    }
    let mut identity_headers: Vec<&(String, String)> = headers
        .iter()
        .filter(|(name, _)| {
            let name = name.to_ascii_lowercase();
            name == "authorization"
                || name == "x-api-key"
                || name == "api-key"
                || name == "chatgpt-account-id"
        })
        .collect();
    identity_headers.sort();
    for (name, value) in identity_headers {
        name.to_ascii_lowercase().hash(&mut account_hasher);
        value.hash(&mut account_hasher);
    }

    GovernorKey {
        provider: provider_hasher.finish(),
        account: account_hasher.finish(),
    }
}

pub(crate) fn governor_for(
    url: &str,
    api_key: Option<&str>,
    headers: &[(String, String)],
) -> Arc<ProviderGovernor> {
    governor_for_key(governor_key(url, api_key, headers))
}

/// Tie `permit` to `inner`: the slot is freed as soon as the stream reports
/// `MessageEnd`, fails or ends, so a consumer that is slow to drop a finished
/// stream does not hold it idle; dropping the stream is the fallback. The
/// first event credits the AIMD increase.
pub(crate) fn hold_permit(inner: EventStream, permit: GovernorPermit) -> EventStream {
    Box::pin(HeldStream {
        inner,
        permit: Some(permit),
        credited: false,
    })
}

struct HeldStream {
    inner: EventStream,
    permit: Option<GovernorPermit>,
    credited: bool,
}

impl Stream for HeldStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let poll = this.inner.as_mut().poll_next(cx);
        if !this.credited
            && matches!(&poll, Poll::Ready(Some(Ok(_))))
            && let Some(permit) = &this.permit
        {
            // First event: the provider accepted the request (AIMD additive increase).
            permit.record_success();
            this.credited = true;
        }
        if matches!(
            &poll,
            Poll::Ready(None | Some(Err(_)) | Some(Ok(StreamEvent::MessageEnd { .. })))
        ) {
            this.permit = None;
        }
        poll
    }
}
