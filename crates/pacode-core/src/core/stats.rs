//! `Request::DebugStats`: a JSON snapshot of the daemon's counters for benchmarks.
//!
//! Computed on request only (the bench driver polls about once per second); there is no
//! timer and nothing is cached or retained here. Every call walks the live sessions once,
//! so cost is O(sessions + agents + tasks).
//!
//! Keys: `sessions`, `agents{live,finished,streaming}`, `tasks{running,terminal}`,
//! `store_queue_depth`, `provider_gate{queued,in_flight}` (`in_flight` is 0 until the
//! provider gate reports it), `broadcast[{session,receivers}]`,
//! `process{rss_kb,threads}` (both 0 where `/proc` is unavailable).

use serde_json::{Value, json};

use super::Core;

impl Core {
    pub fn debug_stats(&self) -> Value {
        let sessions: Vec<_> = match self.sessions.read() {
            Ok(s) => s.values().cloned().collect(),
            Err(p) => p.into_inner().values().cloned().collect(),
        };

        let (mut live, mut finished, mut streaming) = (0u64, 0u64, 0u64);
        let mut broadcast = Vec::with_capacity(sessions.len());
        for session in &sessions {
            let agents: Vec<_> = session
                .agents
                .read()
                .map(|a| a.values().cloned().collect())
                .unwrap_or_default();
            for agent in agents {
                if agent.status().is_live() {
                    live += 1;
                } else {
                    finished += 1;
                }
                if agent.is_running() {
                    streaming += 1;
                }
            }
            broadcast.push(json!({
                "session": session.id.to_string(),
                "receivers": session.events.receiver_count(),
            }));
        }

        let tasks = self.deps.tasks.list(None);
        let running = tasks
            .iter()
            .filter(|t| t.status == pacode_types::TaskStatus::Running)
            .count();

        let registry = self.providers();
        let queued: usize = registry
            .ids()
            .iter()
            .filter_map(|id| registry.get(id))
            .map(|p| p.queue_depth())
            .sum();

        json!({
            "sessions": sessions.len(),
            "agents": { "live": live, "finished": finished, "streaming": streaming },
            "tasks": { "running": running, "terminal": tasks.len() - running },
            "store_queue_depth": self.deps.store.queue_depth(),
            "provider_gate": { "queued": queued, "in_flight": 0 },
            "broadcast": broadcast,
            "process": { "rss_kb": rss_kb(), "threads": thread_count() },
        })
    }
}

/// Resident set size in kB from a `/proc/self/status` text (`VmRSS:  1234 kB`).
pub(crate) fn parse_vm_rss_kb(status: &str) -> u64 {
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .map(|s| parse_vm_rss_kb(&s))
        .unwrap_or(0)
}

/// OS threads of this process (tokio workers plus blocking pool).
fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|d| d.count())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod stats_tests;
