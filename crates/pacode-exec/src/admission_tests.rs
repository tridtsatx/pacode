use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::Admission;
use crate::manager::ExecError;

#[tokio::test(flavor = "multi_thread")]
async fn free_slot_is_taken_at_once_and_released_on_drop() {
    let adm = Arc::new(Admission::new(1));
    let permit = adm.acquire(None).await.unwrap();
    drop(permit);
    let again = adm.acquire(None).await;
    assert!(again.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn waiter_is_admitted_when_a_slot_frees() {
    let adm = Arc::new(Admission::new(1));
    let first = adm.acquire(None).await.unwrap();
    let adm2 = Arc::clone(&adm);
    let waiter = tokio::spawn(async move { adm2.acquire(None).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(adm.queued(), 1);
    assert!(!waiter.is_finished());
    drop(first);
    let res = tokio::time::timeout(Duration::from_secs(2), waiter).await;
    assert!(matches!(res, Ok(Ok(Ok(())))));
    assert_eq!(adm.queued(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn waiters_are_served_in_fifo_order() {
    let adm = Arc::new(Admission::new(1));
    let first = adm.acquire(None).await.unwrap();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for n in 0..4u32 {
        let adm = Arc::clone(&adm);
        let order = Arc::clone(&order);
        handles.push(tokio::spawn(async move {
            let permit = adm.acquire(None).await.unwrap();
            order.lock().unwrap().push(n);
            drop(permit);
        }));
        // Let each task enqueue before the next one starts.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    drop(first);
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_waiter_returns_cancelled_and_leaves_the_queue() {
    let adm = Arc::new(Admission::new(1));
    let _held = adm.acquire(None).await.unwrap();
    let token = CancellationToken::new();
    let adm2 = Arc::clone(&adm);
    let t2 = token.clone();
    let waiter = tokio::spawn(async move { adm2.acquire(Some(&t2)).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(adm.queued(), 1);
    token.cancel();
    let res = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(res, Err(ExecError::Cancelled)));
    assert_eq!(adm.queued(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn already_cancelled_token_never_takes_a_slot() {
    let adm = Arc::new(Admission::new(1));
    let token = CancellationToken::new();
    token.cancel();
    let res = adm.acquire(Some(&token)).await;
    assert!(matches!(res, Err(ExecError::Cancelled)));
    assert!(adm.acquire(None).await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn full_queue_is_refused() {
    let adm = Arc::new(Admission::with_queue_cap(1, 1));
    let _held = adm.acquire(None).await.unwrap();
    let adm2 = Arc::clone(&adm);
    let _waiter = tokio::spawn(async move { adm2.acquire(None).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let res = adm.acquire(None).await;
    assert!(matches!(res, Err(ExecError::TooManyTasks(1))));
}
