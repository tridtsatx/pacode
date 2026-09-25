use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::FutureExt;

use super::{ProviderGovernor, evict_idle_governor, governor_for_key, governor_key};
use crate::{
    DEFAULT_PROVIDER_CONCURRENCY, MAX_PROVIDER_CONCURRENCY, ProviderError, ProviderLimits,
    provider_concurrency_ceiling, set_provider_concurrency_ceiling,
};

fn limits(max_concurrency: usize) -> ProviderLimits {
    ProviderLimits {
        max_concurrency,
        ..ProviderLimits::default()
    }
}

fn key(name: &str) -> super::GovernorKey {
    governor_key(&format!("https://{name}.test/v1/chat"), None, &[])
}

/// The governor registry and the process-wide limits are singletons: tests that
/// touch either take this lock so they cannot observe each other halfway.
fn globals() -> MutexGuard<'static, ()> {
    static GLOBALS: Mutex<()> = Mutex::new(());
    GLOBALS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tokio::test]
async fn test_governor_admits_up_to_the_concurrency_limit() {
    let governor = ProviderGovernor::new(limits(1));
    let first = governor.acquire(0).await.expect("first permit");
    let mut blocked = Box::pin(governor.acquire(0));
    assert!(blocked.as_mut().now_or_never().is_none());

    governor.apply_limits(limits(2));
    let second = governor
        .acquire(0)
        .await
        .expect("second permit after resize");
    drop(first);
    drop(second);
    drop(blocked);
}

#[tokio::test]
async fn test_governor_queue_is_bounded_by_items_and_bytes() {
    let governor = ProviderGovernor::new(ProviderLimits {
        max_concurrency: 1,
        max_queue_items: 1,
        max_queue_bytes: 100,
    });
    let held = governor.acquire(0).await.expect("first permit");

    let mut queued = Box::pin(governor.acquire(60));
    assert!(queued.as_mut().now_or_never().is_none());
    assert_eq!(governor.queued_items.load(Ordering::SeqCst), 1);
    assert_eq!(governor.queued_bytes.load(Ordering::SeqCst), 60);

    let over_items = governor
        .acquire(1)
        .now_or_never()
        .expect("queue is full, so nothing waits")
        .err()
        .expect("queue item cap reached");
    assert!(
        matches!(&over_items, ProviderError::RateLimited(message) if message.contains("queue full")),
        "{over_items:?}"
    );

    let over_bytes = governor
        .acquire(60)
        .now_or_never()
        .expect("queue is full, so nothing waits")
        .err()
        .expect("queue byte cap reached");
    assert!(
        matches!(&over_bytes, ProviderError::RateLimited(_)),
        "{over_bytes:?}"
    );

    drop(queued);
    assert_eq!(governor.queued_items.load(Ordering::SeqCst), 0);
    assert_eq!(governor.queued_bytes.load(Ordering::SeqCst), 0);

    drop(held);
    let second = governor.acquire(0).await.expect("queue drained");
    drop(second);
}

#[tokio::test]
async fn test_zero_queue_cap_rejects_only_what_cannot_be_admitted() {
    let governor = ProviderGovernor::new(ProviderLimits {
        max_concurrency: 1,
        max_queue_items: 0,
        max_queue_bytes: 0,
    });
    let held = governor.acquire(0).await.expect("first permit");
    let error = governor
        .acquire(0)
        .now_or_never()
        .expect("no queue, so nothing waits")
        .err()
        .expect("saturated with no queue room");
    assert!(matches!(&error, ProviderError::RateLimited(_)), "{error:?}");

    drop(held);
    governor
        .acquire(0)
        .await
        .expect("a free slot needs no queue room");
}

#[tokio::test]
async fn test_cooldown_pauses_admission_for_the_whole_account() {
    let governor = ProviderGovernor::new(limits(1));
    governor.start_cooldown(Duration::from_millis(20));
    assert!(governor.cooldown_remaining() > Duration::ZERO);

    let mut delayed = Box::pin(governor.acquire(0));
    assert!(delayed.as_mut().now_or_never().is_none());
    let permit = tokio::time::timeout(Duration::from_millis(250), governor.acquire(0))
        .await
        .expect("cooldown expires")
        .expect("permit after cooldown");
    drop(permit);
    drop(delayed);
}

#[tokio::test]
async fn test_cooldown_started_while_waiting_is_honored() {
    let governor = ProviderGovernor::new(limits(1));
    let held = governor.acquire(0).await.expect("first permit");
    let mut waiting = Box::pin(governor.acquire(0));
    assert!(waiting.as_mut().now_or_never().is_none());

    governor.start_cooldown(Duration::from_millis(30));
    drop(held);

    let permit = tokio::time::timeout(Duration::from_millis(250), waiting)
        .await
        .expect("cooldown expires")
        .expect("permit after cooldown");
    assert!(governor.cooldown_remaining().is_zero());
    drop(permit);
}

#[test]
fn test_registry_shares_one_governor_per_provider_and_account() {
    let _guard = globals();
    let first = governor_for_key(key("a"));
    let same = governor_for_key(key("a"));
    let other = governor_for_key(key("b"));
    assert!(std::sync::Arc::ptr_eq(&first, &same));
    assert!(!std::sync::Arc::ptr_eq(&first, &other));
}

#[test]
fn test_registry_keeps_cooldown_state_across_lookups() {
    let _guard = globals();
    let shared = key("cooldown-state");
    let governor = governor_for_key(shared);
    governor.start_cooldown(Duration::from_secs(30));

    // The cooldown was learned by a request that has since returned: a later
    // request for the same account has to see it, which only holds while the
    // registry keeps the governor alive.
    drop(governor);
    let again = governor_for_key(shared);
    assert!(again.cooldown_remaining() > Duration::from_secs(29));
    assert!(std::sync::Arc::strong_count(&again) > 1);
    again.start_cooldown(Duration::ZERO);
    let _ = governor_for_key(shared);
}

#[tokio::test]
async fn test_registry_evicts_the_least_recently_used_idle_governor() {
    let (old_key, fresh_key) = (key("old"), key("fresh"));
    let old = ProviderGovernor::new(ProviderLimits::default());
    let fresh = ProviderGovernor::new(ProviderLimits::default());
    let mut registry: super::GovernorRegistry = super::GovernorRegistry::new();
    registry.insert(
        old_key,
        super::GovernorEntry {
            governor: Arc::clone(&old),
            last_used: Instant::now() - Duration::from_secs(1),
        },
    );
    registry.insert(
        fresh_key,
        super::GovernorEntry {
            governor: Arc::clone(&fresh),
            last_used: Instant::now(),
        },
    );

    assert!(evict_idle_governor(&mut registry));
    assert!(
        !registry.contains_key(&old_key),
        "the least recently used idle governor is dropped"
    );
    assert!(registry.contains_key(&fresh_key));

    registry.clear();
    let busy = ProviderGovernor::new(ProviderLimits::default());
    let permit = busy.acquire(0).await.expect("permit");
    busy.start_cooldown(Duration::from_secs(30));
    registry.insert(
        key("busy"),
        super::GovernorEntry {
            governor: Arc::clone(&busy),
            last_used: Instant::now() - Duration::from_secs(60),
        },
    );
    assert!(
        !evict_idle_governor(&mut registry),
        "a governor that is running or cooling down is never dropped"
    );
    assert!(registry.contains_key(&key("busy")));
    drop(permit);
}

/// The daemon hands `limits.max_provider_concurrency` to the runtime, and the
/// config layer clamps it to the range the config itself allows. If the runtime
/// and the config disagree, the daemon silently keeps a limit nobody asked for.
#[test]
fn test_runtime_ceiling_matches_the_config_range() {
    assert_eq!(
        pacode_types::DEFAULT_PROVIDER_CONCURRENCY as usize,
        DEFAULT_PROVIDER_CONCURRENCY
    );
    assert_eq!(
        pacode_types::MAX_PROVIDER_CONCURRENCY as usize,
        MAX_PROVIDER_CONCURRENCY
    );
    assert_eq!(
        pacode_types::LimitsConfig::default().max_provider_concurrency as usize,
        DEFAULT_PROVIDER_CONCURRENCY
    );
    assert!(set_provider_concurrency_ceiling(MAX_PROVIDER_CONCURRENCY).is_ok());
    crate::apply_provider_limits(ProviderLimits::default()).expect("defaults fit");
}

#[test]
fn test_concurrency_ceiling_is_bounded() {
    let _guard = globals();
    assert!(set_provider_concurrency_ceiling(0).is_err());
    assert!(set_provider_concurrency_ceiling(MAX_PROVIDER_CONCURRENCY + 1).is_err());
    assert_eq!(
        set_provider_concurrency_ceiling(MAX_PROVIDER_CONCURRENCY).expect("ceiling fits"),
        MAX_PROVIDER_CONCURRENCY
    );
    assert_eq!(provider_concurrency_ceiling(), MAX_PROVIDER_CONCURRENCY);
    assert_eq!(
        set_provider_concurrency_ceiling(8).expect("ceiling fits"),
        8
    );
    assert_eq!(provider_concurrency_ceiling(), 8);

    crate::apply_provider_limits(ProviderLimits::default()).expect("defaults fit");
}

/// The W128 target: a hundred and twenty-eight agents must be able to hold a
/// provider stream at once, so the default ceiling has to admit exactly that
/// many without queueing. The 129th is the one that waits — that is the
/// backstop, and it must engage rather than let the 128 through and hope.
#[tokio::test]
async fn the_default_ceiling_admits_128_concurrent_streams_and_queues_the_129th() {
    // No `globals()` guard: the governor is built from explicit limits and the
    // registry is untouched, so there is no shared state to serialise against —
    // and holding a std MutexGuard across an await would not compile anyway.
    assert_eq!(
        DEFAULT_PROVIDER_CONCURRENCY, 128,
        "the W128 target is the default, not an opt-in"
    );

    let governor = ProviderGovernor::new(limits(DEFAULT_PROVIDER_CONCURRENCY));
    let mut held = Vec::with_capacity(DEFAULT_PROVIDER_CONCURRENCY);
    for index in 0..DEFAULT_PROVIDER_CONCURRENCY {
        held.push(
            governor
                .acquire(0)
                .await
                .unwrap_or_else(|error| panic!("stream {index} was refused: {error}")),
        );
    }
    assert_eq!(
        held.len(),
        128,
        "all 128 concurrent streams must be admitted"
    );

    let stats = governor.stats();
    assert_eq!(stats.in_flight, 128, "all 128 are in flight at once");
    assert_eq!(stats.high_water_in_flight, 128);
    assert_eq!(stats.queued, 0, "none of the 128 had to queue");
    assert_eq!(stats.refused, 0);

    // The 129th has to wait for a permit rather than being admitted or refused.
    let mut overflow = Box::pin(governor.acquire(0));
    assert!(
        overflow.as_mut().now_or_never().is_none(),
        "the 129th stream must queue behind the ceiling"
    );

    drop(held.pop());
    let permit = tokio::time::timeout(Duration::from_secs(5), overflow)
        .await
        .expect("the queued stream was not admitted after a permit was released")
        .expect("queued acquisition failed");
    drop(permit);

    let stats = governor.stats();
    assert_eq!(stats.queued_admissions, 1, "exactly one caller queued");
    assert!(
        stats.wait.count >= 1,
        "the queueing wait must be recorded, or the ceiling is invisible"
    );
}

/// A 128-way fan-out must not lose or reorder admissions, and the counters have
/// to account for every one of them.
#[tokio::test]
async fn a_128_way_fan_out_is_admitted_exactly_once_each() {
    let governor = Arc::new(ProviderGovernor::new(limits(DEFAULT_PROVIDER_CONCURRENCY)));
    let admitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut tasks = Vec::with_capacity(128);
    for _ in 0..128 {
        let governor = Arc::clone(&governor);
        let admitted = Arc::clone(&admitted);
        tasks.push(tokio::spawn(async move {
            let permit = governor.acquire(0).await.expect("admitted");
            admitted.fetch_add(1, Ordering::SeqCst);
            // Hold briefly so the fan-out genuinely overlaps rather than
            // collapsing into a sequential walk through the ceiling.
            tokio::time::sleep(Duration::from_millis(5)).await;
            drop(permit);
        }));
    }
    for task in tasks {
        task.await.expect("task joined");
    }

    assert_eq!(admitted.load(Ordering::SeqCst), 128);
    let stats = governor.stats();
    assert_eq!(stats.admitted, 128, "every admission is accounted for");
    assert_eq!(
        stats.high_water_in_flight, 128,
        "the fan-out really did overlap"
    );
    assert_eq!(stats.in_flight, 0, "every permit was released");
    assert_eq!(stats.queued, 0, "the ceiling was never reached");
}
