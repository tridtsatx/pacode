use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test]
async fn test_single_flight_runs_once_for_concurrent_callers() {
    let flight = SingleFlight::<String>::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let f1 = {
        let flight = flight.clone();
        let counter = counter.clone();
        tokio::spawn(async move {
            flight
                .run("openai:default", || async {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok("refreshed_access_token".to_string())
                })
                .await
        })
    };

    let f2 = {
        let flight = flight.clone();
        let counter = counter.clone();
        tokio::spawn(async move {
            // Give f1 a head start so it becomes the leader
            tokio::time::sleep(Duration::from_millis(10)).await;
            flight
                .run("openai:default", || async {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok("refreshed_access_token".to_string())
                })
                .await
        })
    };

    let (res1, res2) = tokio::join!(f1, f2);
    let token1 = res1.expect("task 1 join").expect("task 1 result");
    let token2 = res2.expect("task 2 join").expect("task 2 result");

    assert_eq!(token1, "refreshed_access_token");
    assert_eq!(token2, "refreshed_access_token");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "underlying refresh closure should have executed exactly once"
    );
}

#[tokio::test]
async fn test_single_flight_different_keys_run_independently() {
    let flight = SingleFlight::<String>::new();
    let counter = Arc::new(AtomicUsize::new(0));

    let f1 = {
        let flight = flight.clone();
        let counter = counter.clone();
        tokio::spawn(async move {
            flight
                .run("key_a", || async {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok("token_a".to_string())
                })
                .await
        })
    };

    let f2 = {
        let flight = flight.clone();
        let counter = counter.clone();
        tokio::spawn(async move {
            flight
                .run("key_b", || async {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok("token_b".to_string())
                })
                .await
        })
    };

    let (res1, res2) = tokio::join!(f1, f2);
    assert_eq!(res1.unwrap().unwrap(), "token_a");
    assert_eq!(res2.unwrap().unwrap(), "token_b");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[test]
fn test_refresh_state_tracks_terminal_failures() {
    let state = RefreshState::new();
    let key = "claude:active";

    // Initially allowed
    assert!(state.ensure_allowed(key).is_ok());

    // Record terminal failure
    state.record_outcome(key, Err("invalid_grant: token has been revoked"));

    // Now blocked
    let err = state.ensure_allowed(key).unwrap_err();
    assert!(format!("{err}").contains("invalid_grant"));

    // Clearing/success unblocks
    state.record_outcome(key, Ok(()));
    assert!(state.ensure_allowed(key).is_ok());
}
