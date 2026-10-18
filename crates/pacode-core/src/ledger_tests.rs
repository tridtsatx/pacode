//! Tests for the file ledger: agents interleaving reads and writes on one path,
//! never-read rejections, new files, external changes, command side effects, lock
//! serialization, caps and the compaction digest.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use pacode_tools::WriteIntent;
use pacode_types::AgentId;

use super::ranges::overlaps;
use super::*;

const MESSAGE_PATH: &str = "f.txt";

fn main_id() -> AgentId {
    AgentId::main()
}

fn sub_id() -> AgentId {
    AgentId::new("agt_reviewer")
}

fn name_of(agent: &AgentId) -> &'static str {
    if agent.is_main() { "main" } else { "reviewer" }
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// `text` with 1-based line `line` replaced by `with`.
fn replace_line(text: &str, line: usize, with: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| {
            if i + 1 == line {
                format!("{with}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect()
}

async fn setup(n: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MESSAGE_PATH);
    tokio::fs::write(&path, numbered(n)).await.unwrap();
    (dir, path)
}

async fn read_all(ledger: &FileLedger, agent: &AgentId, path: &Path) -> String {
    let text = tokio::fs::read_to_string(path).await.unwrap();
    let n = text.lines().count();
    ledger.record_read(agent, path, &text, 0..n).await;
    text
}

/// What a write tool does: lock, read the current content, derive the new content,
/// check, write, record.
async fn edit_line(
    ledger: &FileLedger,
    agent: &AgentId,
    path: &Path,
    line: usize,
    with: &str,
) -> Result<Option<String>, String> {
    let _lock = ledger.lock(&[path.to_path_buf()]).await;
    let current = tokio::fs::read_to_string(path).await.ok();
    let base = current.clone().unwrap_or_default();
    let proposed = replace_line(&base, line, with);
    let note = ledger
        .check(
            agent,
            path,
            MESSAGE_PATH,
            WriteIntent::Edit,
            current.as_deref(),
            Some(proposed.as_str()),
        )
        .await?;
    tokio::fs::write(path, &proposed).await.unwrap();
    ledger
        .record_write(agent, name_of(agent), path, &proposed)
        .await;
    Ok(note)
}

#[tokio::test]
async fn single_agent_repeated_edits_never_rejected() {
    let (_dir, path) = setup(30).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;
    for line in [3, 3, 10, 29, 1, 10] {
        let res = edit_line(&ledger, &main_id(), &path, line, "changed by main").await;
        assert_eq!(res, Ok(None), "edit of line {line}");
    }
}

#[tokio::test]
async fn never_read_existing_file_is_rejected() {
    let (_dir, path) = setup(5).await;
    let ledger = FileLedger::new();
    let err = edit_line(&ledger, &main_id(), &path, 1, "x")
        .await
        .unwrap_err();
    assert!(err.starts_with("read f.txt first"), "{err}");

    // Someone else's read does not count for this agent.
    read_all(&ledger, &sub_id(), &path).await;
    let err = edit_line(&ledger, &main_id(), &path, 1, "x")
        .await
        .unwrap_err();
    assert!(err.starts_with("read f.txt first"), "{err}");
    assert_eq!(edit_line(&ledger, &sub_id(), &path, 1, "x").await, Ok(None));
}

#[tokio::test]
async fn new_file_needs_no_read_and_counts_as_seen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("new.txt");
    let ledger = FileLedger::new();
    let _lock = ledger.lock(std::slice::from_ref(&path)).await;
    let res = ledger
        .check(
            &main_id(),
            &path,
            "new.txt",
            WriteIntent::Write,
            None,
            Some("a\nb\n"),
        )
        .await;
    assert_eq!(res, Ok(None));
    drop(_lock);
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&path, "a\nb\n").await.unwrap();
    ledger
        .record_write(&main_id(), "main", &path, "a\nb\n")
        .await;

    // The creator may edit it right away; anyone else must read it first.
    assert_eq!(
        edit_line(&ledger, &main_id(), &path, 2, "B").await,
        Ok(None)
    );
    let err = edit_line(&ledger, &sub_id(), &path, 1, "A")
        .await
        .unwrap_err();
    assert!(err.starts_with("read f.txt first"), "{err}");
}

#[tokio::test]
async fn overlapping_change_is_rejected_then_one_retry_succeeds() {
    let (_dir, path) = setup(30).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;
    read_all(&ledger, &sub_id(), &path).await;

    assert_eq!(
        edit_line(&ledger, &main_id(), &path, 10, "main was here").await,
        Ok(None)
    );
    let err = edit_line(&ledger, &sub_id(), &path, 10, "reviewer was here")
        .await
        .unwrap_err();
    assert!(
        err.starts_with(
            "f.txt was modified by agent main (1 lines changed at L10) after you read it. \
             Read f.txt lines 10 again before writing."
        ),
        "{err}"
    );
    assert!(err.contains("Current lines"), "{err}");
    assert!(err.contains("main was here"), "fresh lines included: {err}");
    let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(on_disk.contains("main was here"));
    assert!(!on_disk.contains("reviewer was here"));

    // The rejection showed the changed lines: the retry passes without a read.
    assert_eq!(
        edit_line(&ledger, &sub_id(), &path, 10, "reviewer after main").await,
        Ok(None)
    );
    // Main now sees the reviewer's change as drift on the same line.
    let err = edit_line(&ledger, &main_id(), &path, 10, "main again")
        .await
        .unwrap_err();
    assert!(err.contains("modified by agent reviewer"), "{err}");
}

#[tokio::test]
async fn disjoint_lines_pass_with_a_note() {
    let (_dir, path) = setup(60).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;
    read_all(&ledger, &sub_id(), &path).await;

    assert_eq!(
        edit_line(&ledger, &main_id(), &path, 3, "main").await,
        Ok(None)
    );
    let note = edit_line(&ledger, &sub_id(), &path, 40, "reviewer")
        .await
        .unwrap()
        .unwrap();
    assert!(
        note.starts_with(
            "Note: f.txt was also modified by agent main since you read it (1 lines changed at L3)"
        ),
        "{note}"
    );
    let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(on_disk.contains("main\n") && on_disk.contains("reviewer\n"));

    let note = edit_line(&ledger, &main_id(), &path, 50, "main 2")
        .await
        .unwrap()
        .unwrap();
    assert!(note.contains("agent reviewer"), "{note}");
    assert!(note.contains("L40"), "{note}");
}

#[tokio::test]
async fn failed_edit_is_explained_only_when_the_file_drifted() {
    let (_dir, path) = setup(10).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;
    read_all(&ledger, &sub_id(), &path).await;
    let current = tokio::fs::read_to_string(&path).await.unwrap();
    let res = ledger
        .check(
            &sub_id(),
            &path,
            MESSAGE_PATH,
            WriteIntent::Hashline,
            Some(current.as_str()),
            None,
        )
        .await;
    assert_eq!(res, Ok(None), "no drift: the tool's own error stands");

    edit_line(&ledger, &main_id(), &path, 5, "moved")
        .await
        .unwrap();
    let current = tokio::fs::read_to_string(&path).await.unwrap();
    let err = ledger
        .check(
            &sub_id(),
            &path,
            MESSAGE_PATH,
            WriteIntent::Hashline,
            Some(current.as_str()),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.contains("modified by agent main"), "{err}");
}

#[tokio::test]
async fn external_changes_are_attributed_to_external() {
    let (_dir, path) = setup(30).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;

    let text = tokio::fs::read_to_string(&path).await.unwrap();
    tokio::fs::write(&path, replace_line(&text, 5, "user edit"))
        .await
        .unwrap();
    let note = edit_line(&ledger, &main_id(), &path, 20, "main")
        .await
        .unwrap()
        .unwrap();
    assert!(note.contains("modified by external"), "{note}");

    let text = tokio::fs::read_to_string(&path).await.unwrap();
    tokio::fs::write(&path, replace_line(&text, 6, "user again"))
        .await
        .unwrap();
    let err = edit_line(&ledger, &main_id(), &path, 6, "main")
        .await
        .unwrap_err();
    assert!(
        err.starts_with("f.txt was modified by external (an editor or a process outside pacode)"),
        "{err}"
    );
}

#[tokio::test]
async fn full_write_reverting_unseen_lines_is_rejected() {
    let (_dir, path) = setup(10).await;
    let ledger = FileLedger::new();
    let stale = read_all(&ledger, &sub_id(), &path).await;
    read_all(&ledger, &main_id(), &path).await;
    edit_line(&ledger, &main_id(), &path, 2, "main")
        .await
        .unwrap();

    let current = tokio::fs::read_to_string(&path).await.unwrap();
    let proposed = format!("{stale}appended\n");
    let err = ledger
        .check(
            &sub_id(),
            &path,
            MESSAGE_PATH,
            WriteIntent::Write,
            Some(current.as_str()),
            Some(proposed.as_str()),
        )
        .await
        .unwrap_err();
    assert!(err.contains("lines 2 again"), "{err}");
    assert!(err.contains("prefer `edit`"), "{err}");
}

#[tokio::test]
async fn bash_side_effects_are_attributed_to_the_agent_that_ran_it() {
    let (_dir, path) = setup(20).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;
    read_all(&ledger, &sub_id(), &path).await;

    // As if `sed -i` ran in main's bash call (the length changes, so the stat moves).
    let text = tokio::fs::read_to_string(&path).await.unwrap();
    tokio::fs::write(&path, replace_line(&text, 7, "formatted by a command"))
        .await
        .unwrap();
    ledger.refresh_after_exec(&main_id(), "main").await;

    // Main caused it and stays in sync; the reviewer is told main changed line 7.
    assert_eq!(
        edit_line(&ledger, &main_id(), &path, 1, "main").await,
        Ok(None)
    );
    let err = edit_line(&ledger, &sub_id(), &path, 7, "reviewer")
        .await
        .unwrap_err();
    assert!(err.contains("modified by agent main"), "{err}");
}

#[tokio::test]
async fn untracked_paths_are_not_refreshed() {
    let (_dir, path) = setup(3).await;
    let ledger = FileLedger::new();
    ledger.refresh_after_exec(&main_id(), "main").await;
    assert_eq!(ledger.tracked_count(), 0);
    let key = canonical_key(&path).await;
    assert!(!ledger.is_tracked(&key));
}

#[tokio::test]
async fn writes_to_one_path_are_serialized() {
    let (_dir, path) = setup(5).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;
    read_all(&ledger, &sub_id(), &path).await;

    let (main, sub) = (main_id(), sub_id());
    let (a, b) = tokio::join!(
        edit_line(&ledger, &main, &path, 2, "from main"),
        edit_line(&ledger, &sub, &path, 2, "from reviewer"),
    );
    // Exactly one wins; the other saw the winner's change under the lock.
    assert!(a.is_ok() != b.is_ok(), "{a:?} / {b:?}");
    let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
    let winner = if a.is_ok() {
        "from main"
    } else {
        "from reviewer"
    };
    assert!(on_disk.contains(winner), "{on_disk}");
    assert_eq!(ledger.locked_count(), 0, "idle locks are dropped");
}

#[tokio::test]
async fn lock_holds_one_path_and_lets_others_through() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.txt");
    let two = dir.path().join("two.txt");
    let ledger = FileLedger::new();
    let log: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let push = |s: &'static str| log.lock().unwrap().push(s);

    let first = async {
        let _g = ledger.lock(std::slice::from_ref(&one)).await;
        push("first locked");
        tokio::time::sleep(Duration::from_millis(50)).await;
        push("first done");
    };
    let second = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _g = ledger.lock(&[one.clone(), one.clone()]).await;
        push("second locked");
    };
    let other = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _g = ledger.lock(std::slice::from_ref(&two)).await;
        push("other path locked");
    };
    tokio::join!(first, second, other);
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "first locked",
            "other path locked",
            "first done",
            "second locked"
        ]
    );
    assert_eq!(ledger.locked_count(), 0);
}

#[tokio::test]
async fn lru_cap_forgets_the_oldest_path() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = FileLedger::with_cap(3);
    let mut paths = Vec::new();
    for i in 0..4 {
        let p = dir.path().join(format!("f{i}.txt"));
        tokio::fs::write(&p, numbered(3)).await.unwrap();
        paths.push(p);
    }
    for p in paths.iter().take(3) {
        read_all(&ledger, &main_id(), p).await;
    }
    // Touch f0 so f1 is the least recently used one.
    read_all(&ledger, &main_id(), &paths[0]).await;
    read_all(&ledger, &main_id(), &paths[3]).await;
    assert_eq!(ledger.tracked_count(), 3);
    assert!(ledger.is_tracked(&canonical_key(&paths[0]).await));
    assert!(!ledger.is_tracked(&canonical_key(&paths[1]).await));
    // Its seen state went with it: read first again.
    let err = edit_line(&ledger, &main_id(), &paths[1], 1, "x")
        .await
        .unwrap_err();
    assert!(err.starts_with("read f.txt first"), "{err}");
}

#[test]
fn default_cap_is_2048_paths() {
    let mut state = LedgerState::default();
    let stat = Stat {
        len: 1,
        mtime: None,
    };
    for i in 0..=MAX_PATHS {
        let path = PathBuf::from(format!("/virtual/{i}"));
        state.record_read(&main_id(), &path, Snapshot::of("x\n"), stat, 0..1);
    }
    assert_eq!(MAX_PATHS, 2048);
    assert_eq!(state.tracked_count(), MAX_PATHS);
    assert!(!state.is_tracked(Path::new("/virtual/0")));
    assert!(state.is_tracked(Path::new("/virtual/2048")));
}

#[test]
fn line_ranges_merge_adjacent_and_cap_intervals() {
    let mut r = LineRanges::default();
    r.insert(0..2);
    r.insert(2..4);
    r.insert(10..12);
    r.insert(3..5);
    assert_eq!(r.spans(), &[0..5, 10..12]);
    assert!(r.covers(&(1..5)));
    assert!(!r.covers(&(4..6)));
    r.insert(5..5);
    assert_eq!(r.spans().len(), 2);

    let mut many = LineRanges::default();
    for i in 0..200 {
        many.insert(i * 3..i * 3 + 1);
    }
    assert_eq!(many.spans().len(), MAX_INTERVALS);
    assert_eq!(MAX_INTERVALS, 64);
}

#[test]
fn overlap_rules_for_points_and_ranges() {
    assert!(overlaps(&(2..4), &(3..5)));
    assert!(!overlaps(&(2..4), &(4..6)), "adjacent lines do not collide");
    assert!(overlaps(&(4..4), &(2..4)), "insertion right after a change");
    assert!(overlaps(&(3..3), &(3..3)));
    assert!(!overlaps(&(1..1), &(3..5)));
}

#[tokio::test]
async fn digest_lists_files_and_outstanding_warnings() {
    let (dir, path) = setup(10).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &sub_id(), &path).await;
    read_all(&ledger, &main_id(), &path).await;
    edit_line(&ledger, &sub_id(), &path, 4, "reviewer")
        .await
        .unwrap();

    let digest = ledger.digest(&main_id(), dir.path()).unwrap();
    assert!(digest.contains("- f.txt @"), "{digest}");
    assert!(
        digest.contains("MODIFIED by agent reviewer after your read (1 lines at L4)"),
        "{digest}"
    );
    let theirs = ledger.digest(&sub_id(), dir.path()).unwrap();
    assert!(theirs.contains("last written by you; in sync"), "{theirs}");
    assert!(
        ledger
            .digest(&AgentId::new("agt_none"), dir.path())
            .is_none()
    );

    for i in 0..30 {
        let p = dir.path().join(format!("g{i}.txt"));
        tokio::fs::write(&p, "x\n").await.unwrap();
        read_all(&ledger, &main_id(), &p).await;
    }
    let digest = ledger.digest(&main_id(), dir.path()).unwrap();
    assert!(digest.lines().count() <= 20, "{digest}");
    assert!(digest.contains("older files omitted"), "{digest}");
}

#[tokio::test]
async fn copy_agent_keeps_seen_state_under_a_new_id() {
    let (_dir, path) = setup(5).await;
    let ledger = FileLedger::new();
    read_all(&ledger, &main_id(), &path).await;
    let detached = AgentId::new("agt_detached");
    ledger.copy_agent(&main_id(), &detached);
    assert_eq!(edit_line(&ledger, &detached, &path, 1, "x").await, Ok(None));
}
