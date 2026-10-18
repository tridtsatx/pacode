use super::*;

#[test]
fn put_is_content_addressed_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = ImageStore::new(dir.path());
    let a = store.put(b"abc", "image/png").unwrap();
    let b = store.put(b"abc", "image/png").unwrap();
    assert_eq!(a, b);
    assert_eq!(a.len(), 64);
    assert_eq!(store.load(&a).unwrap(), b"abc");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn load_rejects_path_traversal_and_missing() {
    let dir = tempfile::tempdir().unwrap();
    let store = ImageStore::new(dir.path());
    assert!(matches!(
        store.load("../../etc/passwd"),
        Err(StoreError::InvalidHash(_))
    ));
    assert!(matches!(
        store.load(&"0".repeat(64)),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn evict_drops_oldest_first_and_stale_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = ImageStore::new(dir.path());
    let h1 = store.put(&[1u8; 100], "image/png").unwrap();
    let h2 = store.put(&[2u8; 100], "image/jpeg").unwrap();
    let h3 = store.put(&[3u8; 100], "image/png").unwrap();
    let old = SystemTime::now() - std::time::Duration::from_secs(3600);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(store.path_of(&h1).unwrap())
        .unwrap();
    f.set_modified(old).unwrap();
    std::fs::write(dir.path().join("x.tmp-1"), b"junk").unwrap();

    let report = store.evict(250, 10).unwrap();
    assert_eq!(report.removed_files, 1);
    assert!(store.path_of(&h1).is_none());
    assert!(store.path_of(&h2).is_some());
    assert!(store.path_of(&h3).is_some());
    assert!(!dir.path().join("x.tmp-1").exists());

    let report = store.evict(u64::MAX, 1).unwrap();
    assert_eq!(report.kept_files, 1);
}

#[test]
fn evict_on_missing_dir_is_ok() {
    let store = ImageStore::new("/nonexistent/pacode-images-test");
    assert_eq!(store.evict(0, 0).unwrap(), EvictReport::default());
}
