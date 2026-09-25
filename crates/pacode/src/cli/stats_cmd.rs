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
use pacode_types::{HistogramSnapshot, ProviderStats, Reply, Request, RuntimeStats, StoreStats};

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

    println!("store (single writer)");
    println!(
        "  calls {} done, {} refused, {} faults   queue {} now, peak {}",
        core.store.completed,
        core.store.refused,
        core.store.faults,
        core.store.queue_depth,
        core.store.queue_high_water
    );
    print_hist("  wait (queued behind other calls)", &core.store.wait);
    print_hist("  exec (the work itself)", &core.store.exec);
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

/// The store half of the report is what settles "is the database the
/// bottleneck": `exec` is dominated by the fsync that `synchronous=FULL` asks
/// for, so a mean exec in the milliseconds with a flat wait means durability is
/// the cost, and dropping to `synchronous=NORMAL` is the lever. A wait growing
/// with a small exec instead means the writer thread, not the disk — and that
/// one is not a disk problem at all, so no storage engine would help.
fn explain_store(store: &StoreStats) -> String {
    if store.exec.count == 0 {
        return "store: idle".to_string();
    }
    let exec_mean = store.exec.mean_micros();
    let wait_mean = store.wait.mean_micros();
    if wait_mean > exec_mean * 4 && wait_mean > 1_000 {
        format!(
            "store: writer-bound — callers wait {wait_mean}us for {exec_mean}us of work; \
             batch commits or widen the writer rather than touching durability"
        )
    } else if exec_mean > 1_000 {
        format!(
            "store: durability-bound — {exec_mean}us per call is the fsync that \
             synchronous=FULL asks for; synchronous=NORMAL trades power-loss \
             durability for it"
        )
    } else {
        format!("store: healthy — {exec_mean}us per call, {wait_mean}us queued")
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
    fn a_slow_exec_is_named_as_durability_not_queueing() {
        let mut store = StoreStats::default();
        store.exec.count = 100;
        store.exec.total_micros = 100 * 4_000; // 4ms of work per call
        store.wait.count = 100;
        store.wait.total_micros = 100 * 50; // 50us queued
        let text = explain_store(&store);
        assert!(text.contains("durability-bound"), "{text}");
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
    fn byte_counts_are_human_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}
