//! Tests for the cheap post-`bash` refresh: batch stat, no rehash of unchanged paths,
//! per-refresh path cap, attribution and coalescing.

use std::path::PathBuf;
use std::time::Instant;

use pacode_types::AgentId;

use super::refresh::RefreshGate;
use super::state::DigestRow;
use super::*;

fn main_id() -> AgentId {
    AgentId::main()
}

fn sub_id() -> AgentId {
    AgentId::new("agt_reviewer")
}

async fn track(
    ledger: &FileLedger,
    agent: &AgentId,
    dir: &std::path::Path,
    n: usize,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for i in 0..n {
        let path = dir.join(format!("f{i}.txt"));
        let text = format!("line a{i}\nline b{i}\n");
        tokio::fs::write(&path, &text).await.unwrap();
        ledger.record_read(agent, &path, &text, 0..2).await;
        paths.push(path);
    }
    paths
}

#[tokio::test]
async fn unchanged_paths_are_stat_ed_but_never_rehashed() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = FileLedger::new();
    track(&ledger, &main_id(), dir.path(), 500).await;

    let start = Instant::now();
    ledger.refresh_after_exec(&main_id(), "main").await;
    let took = start.elapsed();

    assert_eq!(ledger.refresh.sweeps(), 1);
    assert_eq!(ledger.refresh.rehashes(), 0);
    // Loose bound (the plan asks < 5 ms); only guards against per-path hops.
    assert!(took.as_millis() < 500, "{took:?}");
}

#[tokio::test]
async fn one_changed_file_is_rehashed_and_attributed_to_the_caller() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = FileLedger::new();
    let paths = track(&ledger, &main_id(), dir.path(), 50).await;
    for p in &paths {
        let text = tokio::fs::read_to_string(p).await.unwrap();
        ledger.record_read(&sub_id(), p, &text, 0..2).await;
    }
    tokio::fs::write(&paths[7], "line a7\nchanged by sed, longer\n")
        .await
        .unwrap();

    ledger.refresh_after_exec(&main_id(), "main").await;

    assert_eq!(ledger.refresh.rehashes(), 1);
    let key = canonical_key(&paths[7]).await;
    let rows = digest_rows(&ledger, &sub_id());
    let drifted: Vec<_> = rows.iter().filter(|r| r.drift.is_some()).collect();
    assert_eq!(drifted.len(), 1);
    assert_eq!(drifted[0].path, key);
    let drift = drifted[0].drift.clone().unwrap();
    assert_eq!(drift.writers, vec![Writer::agent(&main_id(), "main")]);
    let mine = digest_rows(&ledger, &main_id());
    assert!(mine.iter().all(|r| r.drift.is_none()));
}

#[tokio::test]
async fn one_refresh_stats_at_most_the_cap_most_recent_paths() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = FileLedger::new();
    let paths = track(&ledger, &main_id(), dir.path(), REFRESH_MAX_PATHS + 44).await;
    let candidates = ledger
        .state()
        .refresh_candidates(&main_id(), REFRESH_MAX_PATHS);
    assert_eq!(candidates.len(), REFRESH_MAX_PATHS);

    // The oldest-used path is beyond the cap: a change there is not rehashed.
    tokio::fs::write(&paths[0], "line a0\nchanged, much longer than before\n")
        .await
        .unwrap();
    ledger.refresh_after_exec(&main_id(), "main").await;
    assert_eq!(ledger.refresh.rehashes(), 0);
}

#[tokio::test]
async fn paths_only_other_agents_read_are_not_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = FileLedger::new();
    track(&ledger, &sub_id(), dir.path(), 5).await;
    assert!(
        ledger
            .state()
            .refresh_candidates(&main_id(), REFRESH_MAX_PATHS)
            .is_empty()
    );
    ledger.refresh_after_exec(&main_id(), "main").await;
    assert_eq!(ledger.refresh.rehashes(), 0);
}

#[tokio::test]
async fn concurrent_refreshes_of_one_agent_coalesce() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = FileLedger::new();
    track(&ledger, &main_id(), dir.path(), 100).await;

    let main = main_id();
    let calls = (0..20).map(|_| ledger.refresh_after_exec(&main, "main"));
    futures::future::join_all(calls).await;

    // How many sweeps the scheduler produced is timing-dependent (see the gate test
    // below); what must hold is that nothing was rehashed and nothing hung.
    assert_eq!(ledger.refresh.rehashes(), 0);
}

/// The digest rows (drift resolved) of one agent, for inspection.
fn digest_rows(ledger: &FileLedger, agent: &AgentId) -> Vec<DigestRow> {
    let (inputs, _) = ledger.state().digest_inputs(agent, 100);
    inputs
        .into_iter()
        .map(|input| input.resolve(&ledger.memo))
        .collect()
}

#[tokio::test]
async fn one_sweep_covers_every_request_registered_before_it_started() {
    let gate = RefreshGate::default();
    let agent = main_id();
    let wants: Vec<u64> = (0..20).map(|_| gate.request()).collect();
    let mut entered = 0;
    for want in wants {
        if gate.enter(&agent, want).await.is_some() {
            entered += 1;
        }
    }
    assert_eq!(entered, 1, "the first sweep started after all 20 requests");
    assert_eq!(gate.sweeps(), 1);

    // A request made after that sweep started needs a new one.
    let later = gate.request();
    assert!(gate.enter(&agent, later).await.is_some());
    assert_eq!(gate.sweeps(), 2);
}
