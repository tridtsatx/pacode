use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::{FutureExt, StreamExt};
use pacode_types::{StopReason, StreamEvent};

use super::{
    GovernorPermit, MIN_EPISODE, MIN_LIMIT, ProviderGovernor, evict_idle_governor,
    governor_for_key, governor_key, hold_permit,
};
use crate::{
    DEFAULT_PROVIDER_CONCURRENCY, EventStream, MAX_PROVIDER_CONCURRENCY, ProviderError,
    ProviderLimits, provider_concurrency_ceiling, set_provider_concurrency_ceiling,
};

const SHORT: Duration = Duration::from_millis(40);

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
    // The new slot goes to the caller already waiting, not to a newcomer.
    let second = blocked
        .as_mut()
        .now_or_never()
        .expect("resize admits the waiter")
        .expect("second permit after resize");
    let mut third = Box::pin(governor.acquire(0));
    assert!(third.as_mut().now_or_never().is_none());
    drop(first);
    let third = third.await.expect("third permit after a release");
    drop(second);
    drop(third);
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

async fn hold(governor: &Arc<ProviderGovernor>, n: usize) -> Vec<GovernorPermit> {
    let mut held = Vec::with_capacity(n);
    for _ in 0..n {
        held.push(governor.acquire(0).await.expect("permit"));
    }
    held
}

/// Waiters are served oldest first, one per freed slot, and a newcomer queues
/// behind them instead of taking the slot a release just freed.
#[tokio::test]
async fn test_waiters_are_admitted_in_arrival_order_one_per_release() {
    let governor = ProviderGovernor::new(limits(1));
    let held = governor.acquire(0).await.expect("first permit");
    let mut first = Box::pin(governor.acquire(0));
    let mut second = Box::pin(governor.acquire(0));
    assert!(first.as_mut().now_or_never().is_none());
    assert!(second.as_mut().now_or_never().is_none());

    drop(held);
    assert!(
        second.as_mut().now_or_never().is_none(),
        "one release wakes one waiter, and not the later one"
    );
    let first = first
        .as_mut()
        .now_or_never()
        .expect("the oldest waiter holds the slot")
        .expect("permit");

    let mut late = Box::pin(governor.acquire(0));
    assert!(late.as_mut().now_or_never().is_none());
    drop(first);
    assert!(
        late.as_mut().now_or_never().is_none(),
        "a newcomer never overtakes the queue"
    );
    let second = second
        .as_mut()
        .now_or_never()
        .expect("next in line")
        .expect("permit");
    drop(second);
    let late = tokio::time::timeout(Duration::from_secs(1), late)
        .await
        .expect("last in line")
        .expect("permit");
    drop(late);
    assert_eq!(governor.stats().in_flight, 0);
}

/// A slot handed to a waiter whose acquire is then cancelled moves on to the
/// next waiter instead of leaking.
#[tokio::test]
async fn test_a_grant_to_a_cancelled_waiter_moves_on() {
    let governor = ProviderGovernor::new(limits(1));
    let held = governor.acquire(0).await.expect("first permit");
    let mut first = Box::pin(governor.acquire(0));
    let mut second = Box::pin(governor.acquire(0));
    assert!(first.as_mut().now_or_never().is_none());
    assert!(second.as_mut().now_or_never().is_none());

    drop(held);
    drop(first);
    let second = second
        .as_mut()
        .now_or_never()
        .expect("the cancelled grant reached the next waiter")
        .expect("permit");
    assert_eq!(governor.stats().in_flight, 1);
    assert_eq!(governor.stats().queued, 0);
    drop(second);
    assert_eq!(governor.stats().in_flight, 0);
}

#[tokio::test]
async fn test_rate_limits_halve_the_limit_once_per_episode() {
    let governor = ProviderGovernor::new(limits(64));
    for _ in 0..64 {
        governor.note_rate_limited(Duration::from_millis(150));
    }
    assert_eq!(governor.limit(), 32, "64 simultaneous 429s halve once");
    assert_eq!(governor.stats().max_concurrency, 64, "the ceiling stays");
    tokio::time::sleep(MIN_EPISODE + Duration::from_millis(50)).await;
    governor.note_rate_limited(Duration::from_millis(150));
    assert_eq!(governor.limit(), 16, "a new episode halves again");
}

#[tokio::test]
async fn test_rate_limits_floor_at_the_minimum_and_the_ceiling() {
    let governor = ProviderGovernor::new(limits(64));
    for _ in 0..10 {
        governor.note_rate_limited(Duration::from_millis(1));
    }
    assert_eq!(governor.limit(), 32);
    for _ in 0..5 {
        tokio::time::sleep(MIN_EPISODE + Duration::from_millis(20)).await;
        governor.note_rate_limited(Duration::from_millis(1));
    }
    assert_eq!(governor.limit(), MIN_LIMIT);

    let tiny = ProviderGovernor::new(limits(1));
    tiny.note_rate_limited(Duration::from_millis(1));
    assert_eq!(tiny.limit(), 1, "never below one, never above the ceiling");
}

#[tokio::test]
async fn additive_increase_stops_at_ceiling() {
    let governor = ProviderGovernor::new(limits(8));
    assert_eq!(governor.limit(), 8);
    // Streams that started before the decrease and succeed inside the episode
    // earn no credit. (`acquire` waits out the cooldown, so take them first.)
    let early = hold(&governor, 4).await;
    governor.note_rate_limited(Duration::from_millis(30));
    assert_eq!(governor.limit(), 4);
    for permit in &early {
        permit.record_success();
    }
    assert_eq!(governor.limit(), 4);
    drop(early);

    tokio::time::sleep(MIN_EPISODE + Duration::from_millis(50)).await;
    for _ in 0..3 {
        governor.acquire(0).await.expect("permit").record_success();
    }
    assert_eq!(governor.limit(), 4);
    governor.acquire(0).await.expect("permit").record_success();
    assert_eq!(governor.limit(), 5, "+1 after L successes");

    for _ in 0..200 {
        governor.acquire(0).await.expect("permit").record_success();
    }
    assert_eq!(governor.limit(), 8, "never above the ceiling");
    assert_eq!(governor.stats().in_flight, 0);
}

/// Shrinking below the permits already out needs no bookkeeping: releases
/// simply stop handing slots on until `in_flight` is under the new limit.
#[tokio::test]
async fn test_shrink_below_the_permits_in_use_takes_effect_as_they_return() {
    let governor = ProviderGovernor::new(limits(8));
    let permits = hold(&governor, 8).await;
    governor.note_rate_limited(Duration::from_millis(20));
    tokio::time::sleep(MIN_EPISODE + Duration::from_millis(20)).await;
    governor.note_rate_limited(Duration::from_millis(20));
    assert_eq!(governor.limit(), 2);
    assert_eq!(governor.stats().in_flight, 8);

    drop(permits);
    assert_eq!(governor.stats().in_flight, 0);
    let _a = governor.acquire(0).await.expect("permit");
    let _b = governor.acquire(0).await.expect("permit");
    let third = tokio::time::timeout(SHORT, governor.acquire(0)).await;
    assert!(third.is_err(), "limit is 2");
}

#[tokio::test]
async fn test_cancelled_waiters_during_a_decrease_corrupt_nothing() {
    let governor = ProviderGovernor::new(limits(8));
    let permits = hold(&governor, 8).await;
    let mut waiters = Vec::new();
    for _ in 0..3 {
        let governor = Arc::clone(&governor);
        waiters.push(tokio::spawn(async move { governor.acquire(0).await }));
    }
    tokio::time::sleep(SHORT).await;
    assert_eq!(governor.stats().queued, 3);
    governor.note_rate_limited(Duration::from_millis(20));
    for waiter in waiters {
        waiter.abort();
        let _ = waiter.await;
    }
    assert_eq!(governor.stats().queued, 0);
    assert_eq!(governor.stats().in_flight, 8);
    drop(permits);
    assert_eq!(governor.stats().in_flight, 0);
    assert_eq!(governor.limit(), 4);
    let again = tokio::time::timeout(Duration::from_secs(1), hold(&governor, 4))
        .await
        .expect("four slots are free again");
    drop(again);
}

/// Only the server pacing the account shrinks it: 429/529 (`RateLimited`) and
/// 503. Any other failure with a `Retry-After` only parks it.
#[test]
fn test_only_rate_limits_and_503_shrink_the_limit() {
    let governor = ProviderGovernor::new(limits(8));
    let server_error = ProviderError::Http {
        status: 500,
        message: "boom".to_string(),
    };
    crate::retry::note_failure(
        &governor,
        &server_error,
        Some(Duration::from_secs(5)),
        Duration::from_secs(5),
    );
    assert!(governor.cooldown_remaining() > Duration::from_secs(4));
    assert_eq!(governor.limit(), 8);

    let unavailable = ProviderError::Http {
        status: 503,
        message: "overloaded".to_string(),
    };
    crate::retry::note_failure(&governor, &unavailable, None, Duration::from_millis(1));
    assert_eq!(governor.limit(), 4);

    let fresh = ProviderGovernor::new(limits(8));
    let rate_limited = ProviderError::RateLimited("slow down".to_string());
    crate::retry::note_failure(&fresh, &rate_limited, None, Duration::from_millis(1));
    assert_eq!(fresh.limit(), 4);
}

fn events(items: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
    futures::stream::iter(items)
        .chain(futures::stream::pending())
        .boxed()
}

/// The slot comes back at `MessageEnd` even while the consumer still holds the
/// stream; error, end and drop release it as well.
#[tokio::test]
async fn test_held_stream_frees_its_slot_at_message_end() {
    let governor = ProviderGovernor::new(limits(1));
    let permit = governor.acquire(0).await.expect("permit");
    let mut stream = hold_permit(
        events(vec![
            Ok(StreamEvent::TextDelta {
                text: "hi".to_string(),
            }),
            Ok(StreamEvent::MessageEnd {
                stop: StopReason::EndTurn,
            }),
        ]),
        permit,
    );
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::TextDelta { .. }))
    ));
    assert_eq!(governor.stats().in_flight, 1);
    assert!(matches!(
        stream.next().await,
        Some(Ok(StreamEvent::MessageEnd { .. }))
    ));
    assert_eq!(
        governor.stats().in_flight,
        0,
        "released at MessageEnd, stream alive"
    );

    let permit = governor.acquire(0).await.expect("permit");
    let mut failing = hold_permit(
        events(vec![Err(ProviderError::Transport("reset".to_string()))]),
        permit,
    );
    assert!(matches!(failing.next().await, Some(Err(_))));
    assert_eq!(governor.stats().in_flight, 0, "released on error");

    let permit = governor.acquire(0).await.expect("permit");
    let pending = hold_permit(events(vec![]), permit);
    assert_eq!(governor.stats().in_flight, 1);
    drop(pending);
    assert_eq!(governor.stats().in_flight, 0, "released on drop");
    drop(stream);
}

/// 64 agents x 4 requests against a provider that serves 32 concurrent streams
/// and answers 429 above that: the shared limit converges instead of herding.
#[tokio::test]
async fn test_limit_converges_to_provider_capacity_without_herd() {
    const CAPACITY: usize = 32;
    let governor = ProviderGovernor::new(limits(64));
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rejects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..64 {
        let (governor, live, rejects) = (
            Arc::clone(&governor),
            Arc::clone(&live),
            Arc::clone(&rejects),
        );
        tasks.push(tokio::spawn(async move {
            for _ in 0..4 {
                loop {
                    let permit = governor.acquire(0).await.expect("admitted");
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    if now > CAPACITY {
                        live.fetch_sub(1, Ordering::SeqCst);
                        rejects.fetch_add(1, Ordering::SeqCst);
                        governor.note_rate_limited(Duration::from_millis(30));
                        drop(permit);
                        continue;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    permit.record_success();
                    live.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                    break;
                }
            }
        }));
    }
    for task in tasks {
        task.await.expect("agent joins");
    }
    let total_429 = rejects.load(Ordering::SeqCst);
    assert!(total_429 <= 2 * 256, "{total_429} 429s for 256 requests");
    let limit = governor.limit();
    assert!(
        (8..=40).contains(&limit),
        "limit {limit} should hover near {CAPACITY}"
    );
    let stats = governor.stats();
    assert_eq!(stats.in_flight, 0);
    assert_eq!(stats.queued, 0);
}
