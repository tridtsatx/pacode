//! Rail state: plan, agents, background tasks, session stats (spec §4, §5).

use std::time::Instant;

use codeapp_types::{AgentId, AgentInfo, Plan, TaskId, TaskInfo, UsageTotals};

#[derive(Default)]
pub struct RailState {
    pub plan: Plan,
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
}

#[cfg(test)]
#[path = "rail_tests.rs"]
mod rail_tests;
