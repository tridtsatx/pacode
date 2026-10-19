use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[tokio::test]
async fn concurrent_callers_in_window_share_one_refresh() {
    let cache = Arc::new(TokenCache::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        tasks.push(tokio::spawn(async move {
            cache
                .ensure_fresh("openai", || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    Ok(("tok".to_string(), Some(now_secs() + 3600)))
                })
                .await
        }));
    }
    for t in tasks {
        assert_eq!(t.await.expect("join"), Ok("tok".to_string()));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expiry_inside_margin_forces_refresh_and_remove_invalidates() {
    let cache = TokenCache::default();
    cache.put("p", "old", Some(now_secs() + 10));
    assert_eq!(cache.get_fresh("p", now_secs()), None);
    let tok = cache
        .ensure_fresh("p", || async { Ok(("new".to_string(), None)) })
        .await;
    assert_eq!(tok, Ok("new".to_string()));
    assert_eq!(cache.get_fresh("p", now_secs()).as_deref(), Some("new"));
    cache.remove("p");
    assert_eq!(cache.get_fresh("p", now_secs()), None);
}

#[test]
fn millisecond_expiry_is_normalised() {
    let in_an_hour_ms = (now_secs() + 3600) * 1000;
    assert!(is_fresh(Some(in_an_hour_ms), now_secs()));
    assert!(!is_fresh(Some(now_secs() * 1000), now_secs()));
}

#[tokio::test]
async fn refresh_error_is_not_cached() {
    let cache = TokenCache::default();
    let r = cache
        .ensure_fresh("p", || async {
            Err::<(String, Option<i64>), _>("boom".to_string())
        })
        .await;
    assert_eq!(r, Err("boom".to_string()));
    assert_eq!(cache.get_fresh("p", now_secs()), None);
}
