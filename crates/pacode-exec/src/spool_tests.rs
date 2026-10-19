use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use pacode_types::TaskId;

use super::{GcLimits, gc};

const DAY: u64 = 24 * 3600;

fn write_spool(dir: &std::path::Path, id: &TaskId, bytes: usize, age_secs: u64) {
    let path = dir.join(format!("{}.out", id.as_str()));
    std::fs::write(&path, vec![b'x'; bytes]).unwrap();
    let when = SystemTime::now() - Duration::from_secs(age_secs);
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_modified(when).unwrap();
}

fn exists(dir: &std::path::Path, id: &TaskId) -> bool {
    dir.join(format!("{}.out", id.as_str())).exists()
}

/// Only the size cap applies.
fn cap_only(max_total_bytes: u64) -> GcLimits {
    GcLimits {
        max_total_bytes,
        stale_after: Duration::from_secs(365 * DAY),
        min_age: Duration::ZERO,
    }
}

fn known(ids: &[&TaskId]) -> impl Fn(&TaskId) -> bool {
    let set: HashSet<TaskId> = ids.iter().map(|id| (*id).clone()).collect();
    move |id| set.contains(id)
}

#[test]
fn deletes_oldest_beyond_cap_and_keeps_known_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    // 20 files of 5 bytes each = 100 bytes; file 0 is the oldest.
    let ids: Vec<TaskId> = (0..20).map(|_| TaskId::generate()).collect();
    for (i, id) in ids.iter().enumerate() {
        write_spool(tmp.path(), id, 5, 1000 - i as u64 * 10);
    }
    let removed = gc(
        tmp.path(),
        cap_only(50),
        known(&[&ids[0]]),
        SystemTime::now(),
    );
    assert_eq!(removed, 10);
    // Oldest unknown files are gone: ids[1..=10]; the known oldest remains.
    assert!(exists(tmp.path(), &ids[0]));
    for id in &ids[1..=10] {
        assert!(!exists(tmp.path(), id));
    }
    for id in &ids[11..] {
        assert!(exists(tmp.path(), id));
    }
}

#[test]
fn under_cap_removes_nothing_and_ignores_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    let id = TaskId::generate();
    write_spool(tmp.path(), &id, 10, 5);
    std::fs::write(tmp.path().join("notes.txt"), vec![b'y'; 1000]).unwrap();
    let now = SystemTime::now();
    assert_eq!(gc(tmp.path(), cap_only(100), known(&[]), now), 0);
    assert_eq!(gc(tmp.path(), cap_only(5), known(&[]), now), 1);
    assert!(tmp.path().join("notes.txt").exists());
}

#[test]
fn stale_spools_of_unknown_tasks_are_swept_under_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let orphan = TaskId::generate();
    let retained = TaskId::generate();
    let recent = TaskId::generate();
    write_spool(tmp.path(), &orphan, 5, 8 * DAY);
    write_spool(tmp.path(), &retained, 5, 9 * DAY);
    write_spool(tmp.path(), &recent, 5, DAY);
    let removed = gc(
        tmp.path(),
        GcLimits::DEFAULT,
        known(&[&retained]),
        SystemTime::now(),
    );
    assert_eq!(removed, 1);
    assert!(!exists(tmp.path(), &orphan));
    assert!(exists(tmp.path(), &retained));
    assert!(exists(tmp.path(), &recent));
}

#[test]
fn a_freshly_written_spool_survives_even_over_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let old = TaskId::generate();
    let fresh = TaskId::generate();
    write_spool(tmp.path(), &old, 10, 3600);
    write_spool(tmp.path(), &fresh, 10, 1);
    let limits = GcLimits {
        min_age: Duration::from_secs(60),
        ..cap_only(0)
    };
    assert_eq!(gc(tmp.path(), limits, known(&[]), SystemTime::now()), 1);
    assert!(!exists(tmp.path(), &old));
    assert!(exists(tmp.path(), &fresh));
}

#[test]
fn missing_dir_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        gc(
            &tmp.path().join("nope"),
            cap_only(0),
            known(&[]),
            SystemTime::now()
        ),
        0
    );
}
