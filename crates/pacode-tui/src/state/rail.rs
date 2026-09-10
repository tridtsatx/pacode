//! Rail state: plan, agents, background tasks, session stats (spec §4, §5).

use std::time::Instant;

use pacode_types::{AgentId, AgentInfo, Plan, TaskId, TaskInfo, UsageTotals};

#[derive(Default)]
pub struct RailState {
    pub plan: Plan,
    /// Scheduled prompts of this session, newest first in creation order.
    pub cron_jobs: Vec<pacode_types::CronJob>,
    /// Condition watchers of this session; terminal ones are kept until the next
    /// turn so the reader sees what fired.
    pub monitors: Vec<pacode_types::MonitorInfo>,
    /// Sorted by start time, order never changes while displayed (spec §4.3).
    pub agents: Vec<AgentInfo>,
    pub tasks: Vec<TaskInfo>,
    pub usage: UsageTotals,
    /// When the session became idle (no live agents, no running tasks, turn over);
    /// the SESSION block replaces AGENTS after `IDLE_DEBOUNCE_MS`.
    pub idle_since: Option<Instant>,
    /// True once the debounce elapsed and the stats block is shown.
    pub show_session_stats: bool,
    /// Agents collapsed to one line or hidden behind `and N more` are computed at
    /// draw time from the available height; this remembers the first visible index in
    /// select mode so the highlighted agent stays on screen.
    pub agents_scroll: usize,
}

impl RailState {
    pub fn upsert_agent(&mut self, info: AgentInfo) {
        match self.agents.iter_mut().find(|a| a.id == info.id) {
            Some(existing) => *existing = info,
            None => {
                self.agents.push(info);
                self.agents.sort_by_key(|a| a.started_at_ms);
            }
        }
    }

    pub fn upsert_cron_job(&mut self, job: pacode_types::CronJob) {
        match self.cron_jobs.iter_mut().find(|j| j.id == job.id) {
            Some(existing) => *existing = job,
            None => {
                self.cron_jobs.push(job);
                self.cron_jobs.sort_by_key(|j| j.created_at_ms);
            }
        }
    }

    pub fn remove_cron_job(&mut self, id: &pacode_types::CronJobId) {
        self.cron_jobs.retain(|j| &j.id != id);
    }

    pub fn upsert_monitor(&mut self, info: pacode_types::MonitorInfo) {
        match self.monitors.iter_mut().find(|m| m.id == info.id) {
            Some(existing) => *existing = info,
            None => {
                self.monitors.push(info);
                self.monitors.sort_by_key(|m| m.started_at_ms);
            }
        }
    }

    /// How long until a countdown on screen would change, in milliseconds.
    ///
    /// The rail shows seconds under a minute and whole minutes above it, so a
    /// distant job needs one redraw a minute rather than sixty. `None` means
    /// nothing on screen counts down and no tick is needed at all.
    pub fn schedule_tick_ms(&self, now_ms: u64) -> Option<u64> {
        let next = self
            .cron_jobs
            .iter()
            .filter(|j| j.enabled)
            .filter_map(|j| j.due_in_ms(now_ms))
            .min()?;
        if next <= 60_000 {
            // Second resolution: wake on the next whole second of the countdown.
            Some(next % 1000 + 1)
        } else {
            // Minute resolution: wake when the displayed minute changes.
            Some(next % 60_000 + 1)
        }
    }

    /// Cron jobs and monitors worth a row: enabled jobs, and monitors still
    /// watching or freshly fired.
    pub fn has_schedule_rows(&self) -> bool {
        self.cron_jobs.iter().any(|j| j.enabled) || self.monitors.iter().any(|m| m.status.is_live())
    }

    pub fn upsert_task(&mut self, info: TaskInfo) {
        match self.tasks.iter_mut().find(|t| t.id == info.id) {
            Some(existing) => *existing = info,
            None => {
                self.tasks.push(info);
                self.tasks.sort_by_key(|t| t.started_at_ms);
            }
        }
    }

    pub fn agent(&self, id: &AgentId) -> Option<&AgentInfo> {
        self.agents.iter().find(|a| &a.id == id)
    }

    pub fn task(&self, id: &TaskId) -> Option<&TaskInfo> {
        self.tasks.iter().find(|t| &t.id == id)
    }

    pub fn live_agents(&self) -> impl Iterator<Item = &AgentInfo> {
        self.agents
            .iter()
            .filter(|a| a.status.is_live() && !a.id.is_main())
    }

    pub fn has_live_agents(&self) -> bool {
        self.agents.iter().any(|a| a.status.is_active())
    }

    /// Tasks shown in the BACKGROUND zone: backgrounded ones, running or finished and
    /// not yet acknowledged... (finished successful tasks stay until the next turn).
    pub fn background_tasks(&self) -> impl Iterator<Item = &TaskInfo> {
        self.tasks.iter().filter(|t| t.backgrounded)
    }

    pub fn has_running_tasks(&self) -> bool {
        self.tasks.iter().any(|t| !t.status.is_terminal())
    }

    pub fn running_task_count(&self) -> usize {
        self.background_tasks()
            .filter(|t| !t.status.is_terminal())
            .count()
    }

    pub fn failed_unacked_count(&self) -> usize {
        self.background_tasks()
            .filter(|t| t.is_failed_unacked())
            .count()
    }

    /// Update `idle_since` / `show_session_stats` (spec §5 debounce). Returns true when
    /// the displayed block changed.
    pub fn update_idle(&mut self, turn_active: bool, now: Instant) -> bool {
        let is_idle = !turn_active && !self.has_live_agents() && !self.has_running_tasks();
        if !is_idle {
            self.idle_since = None;
            if self.show_session_stats {
                self.show_session_stats = false;
                return true;
            }
            return false;
        }

        if self.show_session_stats {
            return false;
        }

        match self.idle_since {
            Some(since) => {
                if now.saturating_duration_since(since).as_millis() as u64
                    >= crate::state::IDLE_DEBOUNCE_MS
                {
                    self.show_session_stats = true;
                    true
                } else {
                    false
                }
            }
            None => {
                self.idle_since = Some(now);
                false
            }
        }
    }

    /// Whether the SESSION block is still waiting out its debounce.
    ///
    /// The debounce fires only when everything has gone quiet, which is exactly
    /// when the second tick used to be disarmed — so the block never appeared.
    /// The event loop keeps the tick armed while this is true.
    pub fn idle_debounce_pending(&self) -> bool {
        self.idle_since.is_some() && !self.show_session_stats
    }

    /// Milliseconds left of the debounce, if one is running.
    pub fn idle_debounce_remaining_ms(&self, now: Instant) -> Option<u64> {
        let since = self.idle_since?;
        if self.show_session_stats {
            return None;
        }
        let elapsed = now.saturating_duration_since(since).as_millis() as u64;
        Some(crate::state::IDLE_DEBOUNCE_MS.saturating_sub(elapsed))
    }
}

#[cfg(test)]
#[path = "rail_tests.rs"]
mod rail_tests;
