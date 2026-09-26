//! `pacode stats`: what the running daemon is holding and what it is paying.
//!
//! Read-only, on demand, and never polled. Every number here is an atomic the
//! daemon already keeps because the work needed it anyway — queue depth, admit
//! waits, store exec time — so an idle daemon pays nothing to be measurable, and
//! a spike is visible even if it lasted a single call.

use std::path::PathBuf;

use anyhow::{Context, bail};
use pacode_client::{Client, ClientOptions};
use pacode_config::Paths;
use pacode_types::{
    AgentLaunchStats, HistogramSnapshot, ProviderStats, Reply, Request, RuntimeStats, StoreStats,
};

pub fn run(socket: Option<PathBuf>, paths: Paths, json: bool) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    rt.block_on(async move {
        let mut client_opts = ClientOptions::new(paths, pacode_config::APP_VERSION);
        client_opts.socket = socket;
        client_opts.spawn_daemon = true;

        let (client, _events_rx) = Client::connect(client_opts)
            .await
            .context("failed to connect to daemon")?;

        let reply = client
            .request(Request::RuntimeStats)
            .await
            .context("failed to read runtime stats")?;

        let (core, providers) = match reply {
            Reply::RuntimeStats { stats } => (stats.core, stats.providers),
            Reply::Error { message } => bail!("daemon error: {message}"),
            other => bail!("unexpected reply: {other:?}"),
        };

        if json {
            let payload = serde_json::json!({ "core": core, "providers": providers });
            println!("{}", serde_json::to_string_pretty(&payload)?);
            return Ok(());
        }
        print_report(&core, &providers);
        Ok(())
    })
}

fn print_report(core: &RuntimeStats, providers: &ProviderStats) {
    println!("resident");
    println!(
        "  sessions {}   agents {} ({} running)   transcript {} in {} items",
        core.sessions_resident,
        core.agents_resident,
        core.agents_running,
        human_bytes(core.transcript_bytes),
        core.transcript_items
    );
    println!();

    println!("store (single writer, group commit)");
    println!(
        "  calls {} done, {} refused, {} faults   queue {} now, peak {}",
        core.store.completed,
        core.store.refused,
        core.store.faults,
        core.store.queue_depth,
        core.store.queue_high_water
    );
    println!(
        "  {} commits for {} calls — mean batch {}",
        core.store.batches,
        core.store.completed,
        mean_batch_size(&core.store)
    );
    print_hist("  wait (queued behind other calls)", &core.store.wait);
    print_hist("  exec (the call's own statements)", &core.store.exec);
    print_hist(
        "  commit (one per batch, shared by all of it)",
        &core.store.commit,
    );
    println!("  {}", explain_store(&core.store));
    println!();

    println!("providers ({} governors)", providers.governors);
    println!(
        "  ceiling reached: peak {} in flight, peak {} queued   {} admitted, {} queued, {} refused",
        providers.high_water_in_flight,
        providers.high_water_queued,
        providers.admitted,
        providers.queued_admissions,
        providers.refused
    );
    if providers.wait_count == 0 {
        println!("  permit wait: none — the ceiling is not the constraint");
    } else {
        println!(
            "  permit wait: {} over {} queued admissions, worst {}us, mean {}us",
            providers.wait_count,
            providers.queued_admissions,
            providers.wait_max_micros,
            providers.wait_total_micros / providers.wait_count.max(1)
        );
    }
    if providers.cooldown_entries > 0 {
        println!("  cooldowns: {}", providers.cooldown_entries);
    }
    println!();

    println!("session opens");
    if core.resume.count == 0 {
        println!("  none yet");
    } else {
        print_hist("  open (resume included)", &core.resume);
    }
    println!("  history pages served: {}", core.history_pages);
    print_launch(&core.launches);
}

fn print_launch(launch: &AgentLaunchStats) {
    println!();
    println!("subagent launch (4 stages, same spawn measured twice)");
    if launch.spawns == 0 && launch.refused == 0 {
        println!("  no subagents spawned yet");
        return;
    }
    println!("  {} spawned, {} refused", launch.spawns, launch.refused);
    print_hist(
        "  1 resolve     (kind lookup, tool selection)",
        &launch.resolve,
    );
    print_hist(
        "  2 session open(store write, registration)  ",
        &launch.session_open,
    );
    print_hist(
        "  3 ready       (whole agent call)            ",
        &launch.ready,
    );
    print_hist(
        "  4 first event (spawn -> provider)          ",
        &launch.prompt_to_first,
    );
    println!("  {}", explain_launch(launch));
}

/// Which stage is the slow one. The distinction is the whole point of the
/// split: stages 1-3 are local work this codebase controls, and stage 4 is the
/// model round-trip plus whatever queue was ahead of it, which no local tuning
/// moves.
///
/// Both bounds are absolute rather than pure ratios, because the ratios alone
/// are misleading: on a fast launch the local work is most of the call, and
/// calling that "local slowness" would name a cause for a cost nobody is
/// waiting on. The thresholds are the point at which a human would actually
/// notice — a second to hand a subagent off, two seconds to hear from it.
fn explain_launch(launch: &AgentLaunchStats) -> String {
    const SLOW_READY_MICROS: u64 = 1_000_000;
    const SLOW_FIRST_MICROS: u64 = 2_000_000;

    if launch.ready.count == 0 {
        return "launch: idle".to_string();
    }
    let local = launch.resolve.mean_micros() + launch.session_open.mean_micros();
    let ready = launch.ready.mean_micros();
    let first = launch.prompt_to_first.mean_micros();
    if first >= SLOW_FIRST_MICROS {
        return format!(
            "launch: provider-bound — {first}us to first token against {ready}us to hand off. \
             The model round-trip and the queue ahead of it dominate; trim tool schemas or \
             spread over more provider keys rather than tuning the store"
        );
    }
    if ready >= SLOW_READY_MICROS && local.saturating_mul(2) > ready {
        return format!(
            "launch: local — {local}us of the {ready}us call is ours, and the call itself is \
             slow. Kind lookup, tool selection and the store write are where to look"
        );
    }
    format!("launch: healthy — {ready}us to hand off, {first}us to first token")
}

fn print_hist(label: &str, hist: &HistogramSnapshot) {
    if hist.count == 0 {
        println!("{label}: no samples");
        return;
    }
    println!(
        "{label}: n={} mean={}us p50={}us p99={}us max={}us",
        hist.count,
        hist.mean_micros(),
        hist.quantile_micros(0.50),
        hist.quantile_micros(0.99),
        hist.max_micros
    );
}

fn human_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    match bytes {
        b if b >= MIB => format!("{:.1} MiB", b as f64 / MIB as f64),
        b if b >= 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// Calls per commit. At 1 the grouping is not happening and the store is back to
/// paying a commit per call, which is the number that says whether a deeper batch
/// would buy anything.
fn mean_batch_size(store: &StoreStats) -> String {
    if store.batches == 0 {
        return "n/a".to_string();
    }
    format!("{:.1}", store.completed as f64 / store.batches as f64)
}

/// What each call actually paid for the commit it shared, in microseconds. The
/// commit histogram is per batch, so this is the per-batch total spread over the
/// calls the batch carried.
fn commit_per_call_micros(store: &StoreStats) -> u64 {
    store.commit.total_micros / store.completed.max(1)
}

/// The store half of the report is what settles "is the database the
/// bottleneck". Three costs, and they call for different answers:
///
/// - A `wait` far above `exec` means the writer is the constraint, not the disk:
///   the calls queue up behind each other while each one is cheap. No storage
///   engine helps; only a wider writer or a shallower one does.
/// - A `commit` that is still milliseconds *after* it has been divided among the
///   calls of its batch means the disk is the constraint. That is the case
///   `synchronous=NORMAL` is meant to remove, so when it shows up anyway the
///   thing to look at is the checkpoint, not the durability setting.
/// - Anything else is a store keeping up, and the numbers are there to prove it
///   rather than to be improved.
fn explain_store(store: &StoreStats) -> String {
    if store.exec.count == 0 {
        return "store: idle".to_string();
    }
    let exec_mean = store.exec.mean_micros();
    let wait_mean = store.wait.mean_micros();
    let per_call = commit_per_call_micros(store);
    if wait_mean > exec_mean * 4 && wait_mean > 1_000 {
        format!(
            "store: writer-bound — callers wait {wait_mean}us for {exec_mean}us of work; \
             batch commits or widen the writer rather than touching durability"
        )
    } else if per_call > 1_000 {
        format!(
            "store: commit-bound — {per_call}us of commit per call, over batches of {}; \
             synchronous=NORMAL is already in force, so the lever is batch depth \
             or the WAL checkpoint",
            mean_batch_size(store)
        )
    } else {
        format!(
            "store: healthy — {exec_mean}us of work per call, {wait_mean}us queued, \
             {per_call}us of commit in batches of {}",
            mean_batch_size(store)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_store_says_so_rather_than_guessing() {
        assert_eq!(explain_store(&StoreStats::default()), "store: idle");
    }

    #[test]
    fn a_deep_queue_is_named_as_writer_bound() {
        let mut store = StoreStats::default();
        store.exec.count = 100;
        store.exec.total_micros = 100 * 200; // 200us of work
        store.wait.count = 100;
        store.wait.total_micros = 100 * 50_000; // 50ms queued
        let text = explain_store(&store);
        assert!(text.contains("writer-bound"), "{text}");
    }

    #[test]
    fn a_slow_commit_is_named_as_commit_not_as_durability_to_trade_away() {
        let mut store = StoreStats::default();
        store.exec.count = 100;
        store.exec.total_micros = 100 * 200; // 200us of work per call
        store.wait.count = 100;
        store.wait.total_micros = 100 * 50; // 50us queued
        // Ten commits of 100ms each, shared by a hundred calls: 10ms of commit
        // per call, which is the number that has to move.
        store.batches = 10;
        store.completed = 100;
        store.commit.count = 10;
        store.commit.total_micros = 10 * 100_000;
        let text = explain_store(&store);
        assert!(text.contains("commit-bound"), "{text}");
        assert!(
            text.contains("synchronous=NORMAL is already in force"),
            "the verdict must not offer a lever that is already pulled: {text}"
        );
    }

    #[test]
    fn a_grouped_store_reports_what_the_batching_bought() {
        let mut store = StoreStats::default();
        store.exec.count = 128;
        store.exec.total_micros = 128 * 200;
        store.wait.count = 128;
        store.wait.total_micros = 128 * 900;
        // Two commits for 128 calls, 4ms each.
        store.batches = 2;
        store.completed = 128;
        store.commit.count = 2;
        store.commit.total_micros = 2 * 4_000;
        assert_eq!(mean_batch_size(&store), "64.0");
        assert_eq!(commit_per_call_micros(&store), 62);
        let text = explain_store(&store);
        assert!(text.contains("healthy"), "{text}");
        assert!(text.contains("batches of 64.0"), "{text}");
    }

    #[test]
    fn a_store_with_no_commits_yet_does_not_divide_by_zero() {
        let mut store = StoreStats::default();
        store.exec.count = 1;
        store.exec.total_micros = 10;
        assert_eq!(mean_batch_size(&store), "n/a");
        assert_eq!(commit_per_call_micros(&store), 0);
    }

    #[test]
    fn byte_counts_are_human_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}

#[cfg(test)]
mod launch_tests {
    use super::{AgentLaunchStats, explain_launch};
    use pacode_types::HistogramSnapshot;

    fn hist(count: u64, total_micros: u64) -> HistogramSnapshot {
        HistogramSnapshot {
            count,
            total_micros,
            ..HistogramSnapshot::default()
        }
    }

    fn launch(resolve: u64, session_open: u64, ready: u64, first: u64) -> AgentLaunchStats {
        AgentLaunchStats {
            spawns: 128,
            refused: 0,
            resolve: hist(128, 128 * resolve),
            session_open: hist(128, 128 * session_open),
            ready: hist(128, 128 * ready),
            prompt_to_first: hist(128, 128 * first),
        }
    }

    /// The verdict's whole job is to keep the two causes apart. Local work and
    /// the model round-trip have opposite fixes, so naming the wrong one sends
    /// whoever reads this down the wrong path.
    #[test]
    fn a_slow_model_is_not_reported_as_a_slow_spawn() {
        // A real round trip: seconds, not the milliseconds a local call takes.
        let text = explain_launch(&launch(50, 100, 200, 9_000_000));
        assert!(text.contains("provider-bound"), "{text}");
        assert!(!text.contains("local"), "{text}");
    }

    #[test]
    fn a_slow_local_path_is_reported_as_local() {
        // A second to hand the subagent off, most of it ours, and the model
        // answers quickly — the shape that local tuning actually fixes.
        let text = explain_launch(&launch(400_000, 500_000, 1_000_000, 1_100_000));
        assert!(text.contains("local"), "{text}");
    }

    #[test]
    fn a_fast_launch_says_so() {
        let text = explain_launch(&launch(30, 90, 150, 1_200));
        assert!(text.contains("healthy"), "{text}");
    }

    #[test]
    fn a_daemon_that_never_spawned_is_not_reported_as_broken() {
        let idle = AgentLaunchStats::default();
        assert!(
            explain_launch(&idle).contains("idle"),
            "{}",
            explain_launch(&idle)
        );
    }
}
