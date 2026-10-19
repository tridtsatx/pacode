use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[test]
fn second_request_for_the_same_hash_does_not_load() {
    let cache = Base64Cache::new(1024);
    let loads = AtomicUsize::new(0);
    let load = || {
        loads.fetch_add(1, Ordering::SeqCst);
        Ok(vec![1u8, 2, 3])
    };
    let a = cache.get_or_load("h1", load).expect("first");
    let b = cache
        .get_or_load("h1", || panic!("must be served from cache"))
        .expect("second");
    assert_eq!(&*a, "AQID");
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[test]
fn lru_evicts_least_recent_past_the_cap() {
    // Each 6-byte image encodes to 8 base64 bytes; the cap holds two.
    let cache = Base64Cache::new(16);
    let loads = AtomicUsize::new(0);
    let load = || {
        loads.fetch_add(1, Ordering::SeqCst);
        Ok(vec![7u8; 6])
    };
    cache.get_or_load("a", load).expect("a");
    cache.get_or_load("b", load).expect("b");
    // Touch "a" so "b" is the least recently used.
    cache.get_or_load("a", load).expect("a again");
    cache.get_or_load("c", load).expect("c");
    assert_eq!(cache.total_bytes(), 16);
    assert_eq!(loads.load(Ordering::SeqCst), 3);
    cache.get_or_load("a", load).expect("a still cached");
    assert_eq!(loads.load(Ordering::SeqCst), 3);
    cache.get_or_load("b", load).expect("b was evicted");
    assert_eq!(loads.load(Ordering::SeqCst), 4);
}

#[test]
fn oversized_image_is_not_cached() {
    let cache = Base64Cache::new(4);
    let v = cache
        .get_or_load("big", || Ok(vec![0u8; 30]))
        .expect("load");
    assert!(v.len() > 4);
    assert_eq!(cache.total_bytes(), 0);
}

#[test]
fn load_error_is_propagated_and_not_cached() {
    let cache = Base64Cache::new(64);
    let err = cache.get_or_load("x", || Err(ProviderError::Config("gone".into())));
    assert!(err.is_err());
    assert_eq!(cache.total_bytes(), 0);
}
