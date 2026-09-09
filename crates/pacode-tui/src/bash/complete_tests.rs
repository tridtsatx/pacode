use std::fs;
use tempfile::tempdir;

use super::*;

#[test]
fn test_history_cap_and_eviction() {
    let mut history = VecDeque::new();
    for i in 0..BASH_HISTORY_CAP + 50 {
        record_history(&mut history, &format!("echo {i}"));
    }
    assert_eq!(history.len(), BASH_HISTORY_CAP);
    // Oldest entries (0..50) evicted
    assert_eq!(history.front().unwrap(), "echo 50");
    assert_eq!(
        history.back().unwrap(),
        &format!("echo {}", BASH_HISTORY_CAP + 49)
    );
}

#[test]
fn test_complete_from_history() {
    let mut history = VecDeque::new();
    record_history(&mut history, "git status");
    record_history(&mut history, "git log -n 5");
    record_history(&mut history, "cargo build");

    let dir = tempdir().unwrap();
    let candidates = complete("git", 3, &history, dir.path());

    let displays: Vec<&str> = candidates.iter().map(|c| c.display.as_str()).collect();
    assert!(displays.contains(&"git log -n 5"));
    assert!(displays.contains(&"git status"));
    assert!(!displays.contains(&"cargo build"));
}

#[test]
fn test_complete_filesystem_directory_suffix_and_cap() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("sub_dir")).unwrap();
    fs::write(dir.path().join("file1.txt"), "1").unwrap();
    fs::write(dir.path().join("file2.rs"), "2").unwrap();

    let candidates = complete_filesystem("f", dir.path());
    let displays: Vec<&str> = candidates.iter().map(|c| c.display.as_str()).collect();
    assert_eq!(displays, vec!["file1.txt", "file2.rs"]);

    let dir_candidates = complete_filesystem("sub", dir.path());
    assert_eq!(dir_candidates.len(), 1);
    assert_eq!(dir_candidates[0].display, "sub_dir/");

    // Test cap
    for i in 0..100 {
        fs::write(dir.path().join(format!("many_{i:03}.txt")), "").unwrap();
    }
    let capped = complete_filesystem("many", dir.path());
    assert_eq!(capped.len(), MAX_COMPLETIONS);
}

#[test]
fn test_complete_filesystem_nested() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("crates");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("Cargo.toml"), "").unwrap();

    let candidates = complete_filesystem("crates/", dir.path());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].display, "crates/Cargo.toml");
}
