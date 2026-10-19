//! Tests for the G4/G5 ledger paths: large-file (whole-hash-only) snapshots, deferred
//! ranged reads, the diff memo and concurrent checks/writes on one path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pacode_tools::WriteIntent;
use pacode_types::AgentId;

use super::snapshot::{MAX_LINE_SNAPSHOT_BYTES, diff_snapshots, touched_coarse};
use super::*;

const NAME: &str = "big.txt";

fn main_id() -> AgentId {
    AgentId::main()
}

fn sub_id() -> AgentId {
    AgentId::new("agt_reviewer")
}

fn line(i: usize) -> String {
    format!("line {i:07}")
}

/// `n` lines of 13 bytes each (`line 0000001\n`...).
fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("{}\n", line(i))).collect()
}

/// More than [`MAX_LINE_SNAPSHOT_BYTES`].
const BIG_LINES: usize = 700_000;

async fn write_big() -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(NAME);
    let text = numbered(BIG_LINES);
    assert!(text.len() > MAX_LINE_SNAPSHOT_BYTES);
    tokio::fs::write(&path, &text).await.unwrap();
    (dir, path, text)
}

async fn check_edit(
    ledger: &FileLedger,
    agent: &AgentId,
    path: &Path,
    current: &str,
    proposed: &str,
) -> Result<Option<String>, String> {
    ledger
        .check(
            agent,
            path,
            NAME,
            WriteIntent::Edit,
            Some(current),
            Some(proposed),
        )
        .await
}

#[test]
fn snapshots_above_the_limit_keep_only_the_whole_hash() {
    let big = numbered(BIG_LINES);
    let snap = Snapshot::of(&big);
    assert!(snap.whole_only);
    assert!(snap.lines.is_empty());
    assert_eq!(snap.line_count(), BIG_LINES);
    let small = Snapshot::of("a\nb\nc");
    assert!(!small.whole_only);
    assert_eq!(small.line_count(), 3);
    // Against a whole-only side a diff is one full-file change; equal hashes: none.
    let other = Snapshot::of(&format!("{big}x\n"));
    let changes = diff_snapshots(&snap, &other);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].new, 0..BIG_LINES + 1);
    assert!(diff_snapshots(&snap, &snap).is_empty());
}

#[test]
fn coarse_touched_is_the_span_between_common_prefix_and_suffix() {
    let cur = "a\nb\nc\nd\n";
    assert_eq!(touched_coarse(cur, "a\nB\nc\nd\n"), vec![1..2]);
    assert!(touched_coarse(cur, cur).is_empty());
    assert_eq!(touched_coarse("a\nb", "a\nX"), vec![1..2]);
    // Always a superset of the exact answer.
    let exact = snapshot::touched(cur, "a\nB\nB2\nd\n");
    let coarse = touched_coarse(cur, "a\nB\nB2\nd\n");
    assert!(
        exact
            .iter()
            .all(|e| coarse.iter().any(|c| c.start <= e.start && e.end <= c.end))
    );
}

#[tokio::test]
async fn a_large_file_reads_and_writes_and_conflicts_say_the_file_changed() {
    let (_dir, path, text) = write_big().await;
    let ledger = FileLedger::new();
    ledger
        .record_read(&main_id(), &path, &text, 0..BIG_LINES)
        .await;
    let edited = text.replacen(&line(10), "edited ten", 1);
    assert_eq!(
        check_edit(&ledger, &main_id(), &path, &text, &edited).await,
        Ok(None)
    );
    // Another agent rewrites the file: the next check cannot name lines.
    tokio::fs::write(&path, &edited).await.unwrap();
    ledger
        .record_write(&sub_id(), "reviewer", &path, &edited)
        .await;
    let again = edited.replacen(&line(20), "edited twenty", 1);
    let err = check_edit(&ledger, &main_id(), &path, &edited, &again)
        .await
        .unwrap_err();
    assert!(err.contains("too large"), "{err}");
    assert!(err.contains("reviewer"), "{err}");
    // The writer itself is in sync.
    assert_eq!(
        check_edit(&ledger, &sub_id(), &path, &edited, &again).await,
        Ok(None)
    );
}

#[tokio::test]
async fn a_ranged_read_of_a_large_file_can_be_edited() {
    let (_dir, path, text) = write_big().await;
    let ledger = FileLedger::new();
    let shown: Vec<String> = (101..=110).map(line).collect();
    let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
    ledger
        .record_read_range(&main_id(), &path, 100, &refs)
        .await;
    assert!(ledger.is_tracked(&canonical_key(&path).await));

    let edited = text.replacen(&line(105), "edited", 1);
    assert_eq!(
        check_edit(&ledger, &main_id(), &path, &text, &edited).await,
        Ok(None)
    );
    // An agent that read nothing is still refused.
    let err = check_edit(&ledger, &sub_id(), &path, &text, &edited)
        .await
        .unwrap_err();
    assert!(err.contains("first"), "{err}");
}

#[tokio::test]
async fn a_changed_large_file_passes_only_over_lines_the_agent_saw() {
    let (_dir, path, text) = write_big().await;
    let ledger = FileLedger::new();
    let shown: Vec<String> = (101..=110).map(line).collect();
    let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
    ledger
        .record_read_range(&main_id(), &path, 100, &refs)
        .await;

    // Something else appends to the file after the read.
    let grown = format!("{text}tail\n");
    tokio::fs::write(&path, &grown).await.unwrap();

    let unseen = grown.replacen(&line(5000), "edited", 1);
    let err = check_edit(&ledger, &main_id(), &path, &grown, &unseen)
        .await
        .unwrap_err();
    assert!(err.contains("too large"), "{err}");

    let seen = grown.replacen(&line(105), "edited", 1);
    let note = check_edit(&ledger, &main_id(), &path, &grown, &seen)
        .await
        .unwrap();
    assert!(note.is_some_and(|n| n.contains("as you saw")));
}

#[tokio::test]
async fn diffs_are_computed_before_the_lock_and_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    let text = numbered(1000);
    tokio::fs::write(&path, &text).await.unwrap();
    let ledger = FileLedger::new();
    ledger.record_read(&main_id(), &path, &text, 0..1000).await;
    let changed = text.replacen(&line(900), "other agent", 1);
    tokio::fs::write(&path, &changed).await.unwrap();
    ledger
        .record_write(&sub_id(), "reviewer", &path, &changed)
        .await;

    let edit = changed.replacen(&line(10), "mine", 1);
    assert!(
        check_edit(&ledger, &main_id(), &path, &changed, &edit)
            .await
            .is_ok()
    );
    let computed = ledger.memo.computed();
    // Further checks of the same state hit the memo.
    for _ in 0..5 {
        let _ = check_edit(&ledger, &main_id(), &path, &changed, &edit).await;
    }
    assert_eq!(ledger.memo.computed(), computed);
    assert!(ledger.memo.len() <= diff_memo::DIFF_MEMO_CAP);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_checks_and_writes_on_one_path_match_serial_execution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    let text = numbered(200);
    tokio::fs::write(&path, &text).await.unwrap();
    let ledger = Arc::new(FileLedger::new());
    for i in 0..32 {
        let agent = AgentId::new(format!("agt_{i}"));
        ledger.record_read(&agent, &path, &text, 0..200).await;
    }
    let mut tasks = Vec::new();
    for i in 0..32usize {
        let (ledger, path) = (ledger.clone(), path.clone());
        tasks.push(tokio::spawn(async move {
            let agent = AgentId::new(format!("agt_{i}"));
            let _guards = ledger.lock(std::slice::from_ref(&path)).await;
            let current = tokio::fs::read_to_string(&path).await.unwrap();
            // Each agent edits its own line: disjoint from everyone else's.
            let proposed = current.replacen(&line(i + 1), &format!("agent {i}"), 1);
            let verdict = check_edit(&ledger, &agent, &path, &current, &proposed).await;
            if verdict.is_ok() {
                tokio::fs::write(&path, &proposed).await.unwrap();
                ledger
                    .record_write(&agent, &format!("a{i}"), &path, &proposed)
                    .await;
            }
            verdict.is_ok()
        }));
    }
    let results = tokio::time::timeout(Duration::from_secs(30), async {
        let mut ok = Vec::new();
        for t in tasks {
            ok.push(t.await.unwrap());
        }
        ok
    })
    .await
    .expect("no deadlock");
    // Serial execution would let every one of them through, and none is lost.
    assert!(results.iter().all(|&ok| ok), "{results:?}");
    let final_text = tokio::fs::read_to_string(&path).await.unwrap();
    for i in 0..32 {
        assert!(
            final_text.contains(&format!("agent {i}\n")),
            "lost write {i}"
        );
    }
    assert_eq!(ledger.tracked_count(), 1);
}
