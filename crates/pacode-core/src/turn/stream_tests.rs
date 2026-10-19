use std::time::{Duration, Instant};

use futures::stream;

use super::*;

#[tokio::test]
async fn silent_stream_without_buffered_data_never_wakes() {
    let mut s = stream::pending::<u8>();
    let res = tokio::time::timeout(Duration::from_millis(150), next_or_flush(&mut s, None)).await;
    assert!(res.is_err(), "no deadline: the call must stay pending");
}

#[tokio::test]
async fn buffered_data_arms_the_timer() {
    let mut s = stream::pending::<u8>();
    let at = Instant::now() + Duration::from_millis(20);
    let res = tokio::time::timeout(Duration::from_secs(2), next_or_flush(&mut s, Some(at)))
        .await
        .expect("timer must fire");
    assert!(res.is_err());
    assert!(Instant::now() >= at);
}

#[tokio::test]
async fn ready_item_wins_over_a_far_deadline() {
    let mut s = stream::iter(vec![7u8]);
    let at = Instant::now() + Duration::from_secs(60);
    assert!(matches!(next_or_flush(&mut s, Some(at)).await, Ok(Some(7))));
}

#[test]
fn coalescer_deadline_only_while_buffered() {
    let mut c = crate::transcript::DeltaCoalescer::new();
    assert!(c.deadline().is_none());
    let now = Instant::now();
    c.push("x", now);
    assert_eq!(c.deadline(), Some(now + c.max_age));
    c.take();
    assert!(c.deadline().is_none());
}
