//! Admission control for provider accounts: how many requests of one account
//! may be in flight, how much of the rest may wait, and for how long a
//! `Retry-After` parks the account.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use pacode_types::LatencyHist;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::sync::Notify;

use crate::{EventStream, ProviderError, ProviderLimits};

#[cfg(test)]
#[path = "governor_tests.rs"]
mod governor_tests;

const MAX_GOVERNORS: usize = 1024;

enum Acquire {
    Permit(GovernorPermit),
    Saturated,
    Raced,
}

pub struct ProviderGovernor {
    max_concurrency: AtomicUsize,
    max_queue_items: AtomicUsize,
    max_queue_bytes: AtomicU64,
    active: AtomicUsize,
    queued_items: AtomicUsize,
    queued_bytes: AtomicU64,
    cooldown_until: Mutex<Option<Instant>>,
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
    notify: Notify,
}

impl ProviderGovernor {
    fn new(limits: ProviderLimits) -> Arc<Self> {
        Arc::new(Self {
            max_concurrency: AtomicUsize::new(limits.max_concurrency),
            max_queue_items: AtomicUsize::new(limits.max_queue_items),
            max_queue_bytes: AtomicU64::new(limits.max_queue_bytes),
            active: AtomicUsize::new(0),
            queued_items: AtomicUsize::new(0),
            queued_bytes: AtomicU64::new(0),
            cooldown_until: Mutex::new(None),
            high_water_active: AtomicUsize::new(0),
            high_water_queued: AtomicUsize::new(0),
            admitted: AtomicU64::new(0),
            queued_admissions: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            cooldown_entries: AtomicU64::new(0),
            wait: LatencyHist::new(),
            notify: Notify::new(),
        })
    }

    fn apply_limits(&self, limits: ProviderLimits) {
        self.max_concurrency
            .store(limits.max_concurrency, Ordering::Release);
        self.max_queue_items
            .store(limits.max_queue_items, Ordering::Release);
        self.max_queue_bytes
            .store(limits.max_queue_bytes, Ordering::Release);
        self.notify.notify_waiters();
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

    fn is_idle(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0 && self.cooldown_remaining().is_zero()
    }

    fn step(self: &Arc<Self>) -> Acquire {
        if !self.cooldown_remaining().is_zero() {
            return Acquire::Saturated;
        }
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= self.max_concurrency.load(Ordering::Acquire) {
                return Acquire::Saturated;
            }
            match self.active.compare_exchange(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let now_active = self.active.load(Ordering::Acquire);
                    self.high_water_active
                        .fetch_max(now_active, Ordering::Relaxed);
                    self.admitted.fetch_add(1, Ordering::Relaxed);
                    let within_limit = now_active <= self.max_concurrency.load(Ordering::Acquire);
                    if within_limit && self.cooldown_remaining().is_zero() {
                        return Acquire::Permit(GovernorPermit {
                            governor: Arc::clone(self),
                        });
                    }
                    self.release();
                    return Acquire::Raced;
                }
                Err(current) => active = current,
            }
        }
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

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        queued_bytes: u64,
    ) -> Result<GovernorPermit, ProviderError> {
        loop {
            match self.step() {
                Acquire::Permit(permit) => return Ok(permit),
                Acquire::Saturated => break,
                Acquire::Raced => continue,
            }
        }

        let mut slot = self.reserve_queue(queued_bytes)?;
        let queued_at = Instant::now();
        let permit = loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            self.wait_for_cooldown().await;
            match self.step() {
                Acquire::Permit(permit) => break permit,
                Acquire::Saturated => {}
                Acquire::Raced => continue,
            }
            notified.await;
        };
        slot.release();
        // Only callers that actually queued are recorded: an immediate permit
        // has no wait worth a bucket, and counting it would drag the mean down
        // to zero and hide the queueing that matters.
        self.wait.record_duration(queued_at.elapsed());
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
        if self
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .is_ok()
        {
            self.notify.notify_waiters();
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

pub(crate) fn hold_permit(inner: EventStream, permit: GovernorPermit) -> EventStream {
    let state = HeldStreamState {
        inner,
        _permit: permit,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        state.inner.next().await.map(|item| (item, state))
    }))
}

struct HeldStreamState {
    inner: EventStream,
    _permit: GovernorPermit,
}
