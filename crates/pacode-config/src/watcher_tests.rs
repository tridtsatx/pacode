use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{Event, EventKind};
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn test_debounce_multiple_rapid_events_triggers_single_reload() {
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(100);
    let reload_count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&reload_count);

    let handle = tokio::spawn(async move {
        run_debounced(
            rx,
            Duration::from_millis(50),
            |s| s == "config.toml",
            move || {
                count_clone.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;
    });

    // Send 5 rapid events within 20ms
    for _ in 0..5 {
        tx.send("config.toml".to_string()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(4)).await;
    }

    // Wait for debounce window (50ms after the last event) to settle
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(
        reload_count.load(Ordering::SeqCst),
        1,
        "rapid writes should debounce to exactly 1 reload"
    );

    drop(tx);
    let _ = handle.await;
}

#[tokio::test]
async fn test_is_relevant_event_filtering() {
    let target = std::ffi::OsStr::new("config.toml");

    let event_match = Event {
        kind: EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
        paths: vec![PathBuf::from("/some/path/config.toml")],
        attrs: Default::default(),
    };
    assert!(is_relevant_event(&event_match, target));

    let event_other_file = Event {
        kind: EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
        paths: vec![PathBuf::from("/some/path/other.toml")],
        attrs: Default::default(),
    };
    assert!(!is_relevant_event(&event_other_file, target));

    let event_access = Event {
        kind: EventKind::Access(AccessKind::Open(AccessMode::Read)),
        paths: vec![PathBuf::from("/some/path/config.toml")],
        attrs: Default::default(),
    };
    assert!(!is_relevant_event(&event_access, target));
}

#[tokio::test]
async fn test_rewritten_file_reloads_once_for_multiple_touches() {
    let dir = tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    std::fs::write(&config_file, "provider.default = 'model-a'\n").unwrap();

    let reloads = Arc::new(AtomicUsize::new(0));
    let reloads_clone = Arc::clone(&reloads);

    let watcher = ConfigWatcher::watch(
        config_file.clone(),
        Duration::from_millis(100),
        move |res| {
            if res.is_ok() {
                reloads_clone.fetch_add(1, Ordering::SeqCst);
            }
        },
    )
    .unwrap();

    // Rapidly write to the file multiple times (simulating editor chunk writes)
    for i in 0..4 {
        std::fs::write(
            &config_file,
            format!("provider.default = 'model-batch-{i}'\n"),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Wait for the quiet period to elapse
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Should have reloaded once for this batch of touches
    assert_eq!(
        reloads.load(Ordering::SeqCst),
        1,
        "multiple rapid touches should reload once"
    );

    drop(watcher);
}

#[tokio::test]
async fn test_rename_in_place_picked_up() {
    let dir = tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    let temp_file = dir.path().join("config.toml.tmp");

    std::fs::write(&config_file, "provider.default = 'old-model'\n").unwrap();

    let last_model = Arc::new(Mutex::new(None));
    let model_clone = Arc::clone(&last_model);

    let watcher =
        ConfigWatcher::watch(config_file.clone(), Duration::from_millis(50), move |res| {
            if let Ok(cfg) = res {
                *model_clone.lock().unwrap() = cfg.provider.default;
            }
        })
        .unwrap();

    // Simulate atomic save: write to temp file, then rename in place over target
    std::fs::write(&temp_file, "provider.default = 'renamed-model'\n").unwrap();
    std::fs::rename(&temp_file, &config_file).unwrap();

    // Wait for debounce window
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert_eq!(
        last_model.lock().unwrap().as_deref(),
        Some("renamed-model"),
        "atomic rename-in-place must be detected by watcher"
    );

    drop(watcher);
}

#[tokio::test]
async fn test_unparseable_file_leaves_previous_config_in_force_and_surfaces_error() {
    let dir = tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    std::fs::write(&config_file, "provider.default = 'good-model'\n").unwrap();

    // Running config held in daemon / client
    let active_config = Arc::new(Mutex::new(
        crate::load_from_path(&config_file).expect("initial load should succeed"),
    ));
    let active_clone = Arc::clone(&active_config);

    let last_error = Arc::new(Mutex::new(None));
    let err_clone = Arc::clone(&last_error);

    let watcher = ConfigWatcher::watch(
        config_file.clone(),
        Duration::from_millis(50),
        move |res| match res {
            Ok(new_cfg) => {
                *active_clone.lock().unwrap() = new_cfg;
            }
            Err(e) => {
                *err_clone.lock().unwrap() = Some(e.to_string());
            }
        },
    )
    .unwrap();

    // Write corrupted unparseable TOML
    std::fs::write(&config_file, "[[[invalid unclosed toml syntax :::").unwrap();

    // Wait for debounce window
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Error must be reported
    let err = last_error.lock().unwrap().take();
    assert!(err.is_some(), "parse error must be surfaced");
    assert!(
        err.as_ref().unwrap().contains("invalid config"),
        "error message should detail config failure"
    );

    // Active config must NOT be wiped or corrupted; remains previous config
    assert_eq!(
        active_config.lock().unwrap().provider.default.as_deref(),
        Some("good-model"),
        "previous valid config must remain in force when parse fails"
    );

    drop(watcher);
}
