use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use pacode_types::TaskId;

use super::gc;

fn write_spool(dir: &std::path::Path, id: &TaskId, bytes: usize, age_secs: u64) {
    let path = dir.join(format!("{}.out", id.as_str()));
    std::fs::write(&path, vec![b'x'; bytes]).unwrap();
    let when = SystemTime::now() - Duration::from_secs(age_secs);
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_modified(when).unwrap();
}

#[test]
fn deletes_oldest_beyond_cap_and_keeps_running() {
    let tmp = tempfile::tempdir().unwrap();
    // 20 files of 5 bytes each = 100 bytes; file 0 is the oldest.
    let ids: Vec<TaskId> = (0..20).map(|_| TaskId::generate()).collect();
    for (i, id) in ids.iter().enumerate() {
        write_spool(tmp.path(), id, 5, 1000 - i as u64 * 10);
    }
    let keep: HashSet<TaskId> = [ids[0].clone()].into_iter().collect();
    let removed = gc(tmp.path(), 50, &keep);
    assert_eq!(removed, 10);
    // Oldest unprotected files are gone: ids[1..=10]; the protected oldest remains.
    assert!(tmp.path().join(format!("{}.out", ids[0].as_str())).exists());
    for id in &ids[1..=10] {
        assert!(!tmp.path().join(format!("{}.out", id.as_str())).exists());
    }
    for id in &ids[11..] {
        assert!(tmp.path().join(format!("{}.out", id.as_str())).exists());
    }
}

#[test]
fn under_cap_removes_nothing_and_ignores_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    let id = TaskId::generate();
    write_spool(tmp.path(), &id, 10, 5);
    std::fs::write(tmp.path().join("notes.txt"), vec![b'y'; 1000]).unwrap();
    assert_eq!(gc(tmp.path(), 100, &HashSet::new()), 0);
    assert_eq!(gc(tmp.path(), 5, &HashSet::new()), 1);
    assert!(tmp.path().join("notes.txt").exists());
}

#[test]
fn missing_dir_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(gc(&tmp.path().join("nope"), 0, &HashSet::new()), 0);
}
