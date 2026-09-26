//! Tool-gating tests: `tools.<name>.enabled = false` removes tools from the
//! registry and therefore from the `ToolDefinition` list sent to the model, plus
//! the file primitives the writing tools share.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pacode_types::{AgentId, SessionId};
use tokio_util::sync::CancellationToken;

use super::{
    MAX_PERMISSION_DETAIL_BYTES, MAX_SOURCE_FILE_BYTES, MAX_TOOL_OUTPUT_BYTES,
    MAX_TOOL_WORKERS_PER_AGENT, SourceReadError, ToolRegistry, acquire_path_write_lock,
    cap_output_bytes, read_source_file, write_atomic,
};
use crate::ToolError;
use crate::builtin::glob::GlobTool;
use crate::builtin::read::ReadTool;

fn names(registry: &ToolRegistry) -> Vec<String> {
    registry
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect()
}

#[test]
fn disabled_tool_absent_from_definitions() {
    let mut registry = ToolRegistry::new()
        .with(Arc::new(ReadTool))
        .with(Arc::new(GlobTool));
    registry.apply_disabled(["read".to_string()]);

    assert_eq!(names(&registry), ["glob"]);
    assert!(registry.get("read").is_none());
    assert!(registry.get("glob").is_some());
}

#[test]
fn register_refuses_disabled_names() {
    let mut registry = ToolRegistry::new();
    registry.apply_disabled(["read".to_string()]);
    registry.register(Arc::new(ReadTool));
    assert!(registry.get("read").is_none());
    assert!(names(&registry).is_empty());
}

#[test]
fn subset_and_clones_keep_disabled_set() {
    let mut registry = ToolRegistry::new().with(Arc::new(ReadTool));
    registry.apply_disabled(["read".to_string(), "mcp__tool".to_string()]);
    registry.register(Arc::new(ReadTool)); // refused

    let subset = registry.subset(["read", "glob"]);
    assert!(subset.names().next().is_none());
    // A subset refuses late registrations of disabled names as well.
    let mut subset = registry.subset([]);
    subset.register(Arc::new(ReadTool));
    assert!(subset.get("read").is_none());
}

#[test]
fn unknown_disabled_names_are_harmless() {
    let mut registry = ToolRegistry::new().with(Arc::new(ReadTool));
    registry.apply_disabled(["no_such_tool".to_string()]);
    assert_eq!(names(&registry), ["read"]);
}

#[test]
fn snapshot_does_not_change_when_registry_mutates() {
    let mut registry = ToolRegistry::new().with(Arc::new(ReadTool));
    let snapshot = registry.snapshot();

    registry.register(Arc::new(GlobTool));

    assert!(snapshot.get("glob").is_none());
    assert!(registry.get("glob").is_some());
    assert!(snapshot.get("read").is_some());
}

#[test]
fn hard_output_cap_is_exact_for_multibyte_text() {
    let text = "界".repeat(200_000);
    let capped = cap_output_bytes(text.as_str(), 4_096);

    assert!(capped.len() <= 4_096);
    assert!(capped.contains("output truncated"));
    assert!(capped.starts_with('界'));
    assert!(capped.ends_with('界'));
    assert!(text.len() > MAX_TOOL_OUTPUT_BYTES);
}

fn leftover_temp_files(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains("pacode-tmp"))
        .collect()
}

#[tokio::test]
async fn write_atomic_keeps_the_mode_and_leaves_no_temp() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("run.sh");
    tokio::fs::write(&target, b"old\n").await.expect("seed");
    tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
        .await
        .expect("chmod");

    write_atomic(&target, b"new\n").await.expect("write");

    assert_eq!(tokio::fs::read(&target).await.expect("read"), b"new\n");
    let mode = std::fs::metadata(&target)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o755,
        "an overwrite must not drop the exec bit"
    );
    assert!(
        leftover_temp_files(tmp.path()).is_empty(),
        "temp files left behind: {:?}",
        leftover_temp_files(tmp.path())
    );
}

#[tokio::test]
async fn write_atomic_gives_a_new_file_a_fresh_temp_each_time() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("new.txt");

    write_atomic(&target, b"one").await.expect("first");
    write_atomic(&target, b"two").await.expect("second");

    assert_eq!(tokio::fs::read(&target).await.expect("read"), b"two");
    assert!(leftover_temp_files(tmp.path()).is_empty());
}

#[tokio::test]
async fn read_source_file_refuses_what_it_cannot_hold() {
    use std::io::Write;

    let tmp = tempfile::tempdir().expect("tempdir");

    let missing = read_source_file(&tmp.path().join("nope.txt"), 1024)
        .await
        .expect_err("missing");
    assert!(matches!(missing, SourceReadError::Io(_)));
    assert!(missing.is_not_found());

    let binary = tmp.path().join("b.bin");
    let mut file = std::fs::File::create(&binary).expect("create");
    file.write_all(b"abc\0def").expect("write");
    drop(file);
    assert!(matches!(
        read_source_file(&binary, 1024).await,
        Err(SourceReadError::Binary)
    ));

    let big = tmp.path().join("big.txt");
    let file = std::fs::File::create(&big).expect("create");
    file.set_len(MAX_SOURCE_FILE_BYTES + 1).expect("grow");
    drop(file);
    let err = read_source_file(&big, MAX_SOURCE_FILE_BYTES)
        .await
        .expect_err("oversized");
    assert!(matches!(
        err,
        SourceReadError::TooLarge {
            max, ..
        } if max == MAX_SOURCE_FILE_BYTES
    ));
    assert!(err.message("edit", "big.txt").contains("byte limit"));

    let fine = tmp.path().join("ok.txt");
    tokio::fs::write(&fine, b"hello").await.expect("write");
    assert_eq!(
        read_source_file(&fine, MAX_SOURCE_FILE_BYTES)
            .await
            .expect("read"),
        "hello"
    );
}

#[tokio::test]
async fn path_write_lock_serialises_two_spellings_of_one_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("f.txt");
    tokio::fs::write(&target, b"x").await.expect("seed");
    let alias = tmp.path().join(".").join("f.txt");

    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let hold = |path: PathBuf, inside: Arc<AtomicUsize>, peak: Arc<AtomicUsize>| async move {
        let cancel = CancellationToken::new();
        let _guard = acquire_path_write_lock(&path, &cancel).await.expect("lock");
        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        inside.fetch_sub(1, Ordering::SeqCst);
    };

    tokio::join!(
        hold(target.clone(), inside.clone(), peak.clone()),
        hold(alias, inside.clone(), peak.clone())
    );

    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "two writers on one path must not overlap"
    );
}

#[tokio::test]
async fn path_write_lock_gives_up_when_the_call_is_cancelled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("f.txt");
    let held = acquire_path_write_lock(&target, &CancellationToken::new())
        .await
        .expect("first lock");

    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = match acquire_path_write_lock(&target, &cancel).await {
        Ok(_) => panic!("a cancelled wait must not hand out the lock"),
        Err(err) => err,
    };
    assert!(matches!(err, ToolError::Cancelled));
    drop(held);
}

#[test]
fn permission_detail_cap_keeps_head_and_tail_of_a_long_diff() {
    let diff = format!("{}\n{}", "+".repeat(200_000), "!".repeat(200_000));
    let capped = cap_output_bytes(diff.as_str(), MAX_PERMISSION_DETAIL_BYTES);

    assert!(capped.len() <= MAX_PERMISSION_DETAIL_BYTES);
    assert!(capped.contains("output truncated"));
    assert!(capped.starts_with('+'));
    assert!(capped.ends_with('!'));
}

/// How many agents the fleet test below runs. It must stay above the 16 that
/// used to be the process-wide cap, otherwise the test would pass even with the
/// cap back in place.
const FLEET_SIZE: usize = 24;

/// A worker cap that is per-agent, not per-process. The global cap this guards
/// against was 16: with 128 concurrent subagents, 112 of them could not run a
/// single tool — and the parent's own `agent spawn` call queued behind the
/// children it had just spawned, which is what made agents crawl to a start.
///
/// All 24 permits are still held after `join_all` returns, so the calls are
/// genuinely in flight at the same time; under a process-wide cap the join would
/// never finish and the timeout would fail instead of hanging the suite.
#[tokio::test]
async fn workers_from_different_agents_are_handed_out_in_parallel() {
    let registry = ToolRegistry::new();
    let session = SessionId::new("ses_fleet");
    let cancel = CancellationToken::new();

    let permits = tokio::time::timeout(
        Duration::from_secs(10),
        futures::future::join_all((0..FLEET_SIZE).map(|index| {
            let registry = registry.clone();
            let session = session.clone();
            let cancel = cancel.clone();
            async move {
                let agent = AgentId::new(format!("agt_{index}"));
                registry.acquire_worker(&session, &agent, &cancel).await
            }
        })),
    )
    .await
    .expect("agents queued behind each other: a process-wide worker cap is back")
    .into_iter()
    .collect::<Vec<_>>();

    assert_eq!(permits.len(), FLEET_SIZE);
    for (index, permit) in permits.iter().enumerate() {
        assert!(permit.is_ok(), "agent {index} never got a worker");
    }
}

/// The remaining cap is per agent: one agent fills its own slots and then waits,
/// which is what keeps a single agent from exhausting fds and processes.
#[tokio::test]
async fn one_agent_is_capped_at_its_own_slot_count() {
    let registry = ToolRegistry::new();
    let session = SessionId::new("ses_cap");
    let agent = AgentId::main();

    let held = futures::future::join_all((0..MAX_TOOL_WORKERS_PER_AGENT).map(|_| {
        let registry = registry.clone();
        let session = session.clone();
        let agent = agent.clone();
        let cancel = CancellationToken::new();
        async move { registry.acquire_worker(&session, &agent, &cancel).await }
    }))
    .await;
    assert!(held.iter().all(|permit| permit.is_ok()));

    let blocked = tokio::time::timeout(
        Duration::from_millis(50),
        registry.acquire_worker(&session, &agent, &CancellationToken::new()),
    )
    .await;
    assert!(
        blocked.is_err(),
        "a full per-agent pool must make the next call wait"
    );
}
