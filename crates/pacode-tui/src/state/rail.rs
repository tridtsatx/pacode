//! Rail state: plan, agents, background tasks, session stats (spec §4, §5).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use pacode_types::{AgentId, AgentInfo, Plan, TaskId, TaskInfo, TurnId, UsageTotals};

use super::session::TurnEndMatch;

/// What the Background level's detail card needs for its selected row —
/// recorded by the draw path, read by `dispatch_deferred`. `output_bytes`
/// doubles as the version: a `TaskUpdated` carrying more output re-keys the
/// want and re-arms the read by itself — the daemon's task events drive the
/// refresh, so no event-side invalidation or timer is needed (§5.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskTailKey {
    pub task: TaskId,
    /// The daemon-reported output size the want was recorded at.
    pub output_bytes: u64,
}

/// The tail of one task's spool output — the last ≤
/// `ui::menu::background::TAIL_CAP_BYTES`, ANSI-stripped — produced by a
/// deferred blocking read (`BgResponse::TaskTailReady`), never in the draw
/// path (§8.1).
#[derive(Clone, Debug)]
pub struct TaskTail {
    pub task: TaskId,
    /// The `output_bytes` the tail was read at — part of the cache key.
    pub output_bytes: u64,
    /// `None` when the spool file was missing or unreadable; `Some("")` when
    /// it simply had nothing yet.
    pub text: Option<String>,
    /// The file continues before what `text` shows — the card's cut marker.
    pub truncated: bool,
}

impl TaskTail {
    /// The want this tail answers — `dispatch_deferred` compares it against
    /// `task_tail_want` to decide whether another read is owed.
    pub fn key(&self) -> TaskTailKey {
        TaskTailKey {
            task: self.task.clone(),
            output_bytes: self.output_bytes,
        }
    }
}

#[derive(Default)]
struct AgentTurnFence {
    active: Option<TurnId>,
    terminal: VecDeque<TurnId>,
}

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
    agent_index: HashMap<AgentId, usize>,
    /// Which revision of `agents` / `tasks` the index above mirrors.
    agent_index_version: u64,
    agents_version: u64,
    task_index: HashMap<TaskId, usize>,
    task_index_version: u64,
    tasks_version: u64,
    agent_turn_fences: HashMap<AgentId, AgentTurnFence>,
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
    /// The Background level's deferred output-tail read (§5.9): the selected
    /// task's key recorded by the draw path (`overlays`), the key of the read
    /// currently on a blocking thread, and the landed tail — the only copy,
    /// at most one task's ≤ `ui::menu::background::TAIL_CAP_BYTES` of text.
    /// `clear_task_tail` drops all three when the level leaves the screen.
    pub task_tail_want: Option<TaskTailKey>,
    pub task_tail_in_flight: Option<TaskTailKey>,
    pub task_tail: Option<TaskTail>,
}

impl RailState {
    fn rebuild_agent_index(&mut self) {
        self.agent_index.clear();
        self.agent_index.reserve(self.agents.len());
        for (index, agent) in self.agents.iter().enumerate() {
            self.agent_index.insert(agent.id.clone(), index);
        }
        self.agent_index_version = self.agents_version;
    }

    fn rebuild_task_index(&mut self) {
        self.task_index.clear();
        self.task_index.reserve(self.tasks.len());
        for (index, task) in self.tasks.iter().enumerate() {
            self.task_index.insert(task.id.clone(), index);
        }
        self.task_index_version = self.tasks_version;
    }

    /// Where a row sits, read-only. The index is a mirror of the row list, so
    /// it is only trusted while it is the current revision; a miss falls back
    /// to a scan instead of rebuilding, which a lookup for a row the rail does
    /// not list would otherwise do on every event.
    fn agent_row(&self, id: &AgentId) -> Option<usize> {
        if self.agent_index_version == self.agents_version
            && let Some(index) = self.agent_index.get(id).copied()
            && self.agents.get(index).is_some_and(|agent| &agent.id == id)
        {
            return Some(index);
        }
        self.agents.iter().position(|agent| &agent.id == id)
    }

    fn task_row(&self, id: &TaskId) -> Option<usize> {
        if self.task_index_version == self.tasks_version
            && let Some(index) = self.task_index.get(id).copied()
            && self.tasks.get(index).is_some_and(|task| &task.id == id)
        {
            return Some(index);
        }
        self.tasks.iter().position(|task| &task.id == id)
    }

    /// The same lookup, allowed to refresh the mirror — the row is about to be
    /// written, which is where the index is brought back up to date.
    fn agent_position(&mut self, id: &AgentId) -> Option<usize> {
        if self.agent_index_version != self.agents_version {
            self.rebuild_agent_index();
        }
        self.agent_row(id)
    }

    fn task_position(&mut self, id: &TaskId) -> Option<usize> {
        if self.task_index_version != self.tasks_version {
            self.rebuild_task_index();
        }
        self.task_row(id)
    }

    fn agents_changed(&mut self) {
        self.agents_version = self.agents_version.wrapping_add(1);
    }

    fn tasks_changed(&mut self) {
        self.tasks_version = self.tasks_version.wrapping_add(1);
    }

    pub fn replace_agents(&mut self, mut agents: Vec<AgentInfo>) {
        agents.sort_by_key(|agent| agent.started_at_ms);
        let ids: HashSet<_> = agents.iter().map(|agent| agent.id.clone()).collect();
        self.agent_turn_fences.retain(|id, _| ids.contains(id));
        self.agents = agents;
        self.agents_changed();
        self.rebuild_agent_index();
    }

    pub fn replace_tasks(&mut self, mut tasks: Vec<TaskInfo>) {
        tasks.sort_by_key(|task| task.started_at_ms);
        self.tasks = tasks;
        self.tasks_changed();
        self.rebuild_task_index();
    }

    pub fn upsert_agent(&mut self, mut info: AgentInfo) {
        if let Some(position) = self.agent_position(&info.id) {
            if !self.agents[position].status.is_live()
                && !info.status.is_live()
                && info.finished_at_ms.is_none()
            {
                info.finished_at_ms = self.agents[position].finished_at_ms;
            }
            self.agents[position] = info;
            return;
        }
        let started_at = info.started_at_ms;
        if self
            .agents
            .last()
            .is_none_or(|agent| agent.started_at_ms <= started_at)
        {
            self.agents.push(info);
        } else {
            let index = self
                .agents
                .partition_point(|agent| agent.started_at_ms < started_at);
            self.agents.insert(index, info);
        }
        self.agents_changed();
        self.rebuild_agent_index();
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
        if let Some(position) = self.task_position(&info.id) {
            self.tasks[position] = info;
            return;
        }
        let started_at = info.started_at_ms;
        if self
            .tasks
            .last()
            .is_none_or(|task| task.started_at_ms <= started_at)
        {
            self.tasks.push(info);
        } else {
            let index = self
                .tasks
                .partition_point(|task| task.started_at_ms < started_at);
            self.tasks.insert(index, info);
        }
        self.tasks_changed();
        self.rebuild_task_index();
    }

    pub fn agent(&self, id: &AgentId) -> Option<&AgentInfo> {
        self.agent_row(id).and_then(|index| self.agents.get(index))
    }

    pub fn agent_mut(&mut self, id: &AgentId) -> Option<&mut AgentInfo> {
        let position = self.agent_position(id)?;
        self.agents.get_mut(position)
    }

    pub fn task(&self, id: &TaskId) -> Option<&TaskInfo> {
        self.task_row(id).and_then(|index| self.tasks.get(index))
    }

    pub(crate) fn begin_agent_turn(&mut self, agent: &AgentId, turn: &TurnId) {
        let fence = self.agent_turn_fences.entry(agent.clone()).or_default();
        if fence.terminal.iter().any(|known| known == turn) {
            return;
        }
        fence.active = Some(turn.clone());
    }

    pub(crate) fn mark_agent_terminal(&mut self, agent: &AgentId, now_ms: u64) {
        if !self.agent(agent).is_some_and(|info| info.status.is_live()) {
            return;
        }
        if let Some(info) = self.agent_mut(agent) {
            info.finished_at_ms.get_or_insert(now_ms);
        }
        let fence = self.agent_turn_fences.entry(agent.clone()).or_default();
        if let Some(turn) = fence.active.take() {
            fence.terminal.push_back(turn);
            while fence.terminal.len() > 16 {
                fence.terminal.pop_front();
            }
        }
    }

    pub(crate) fn finish_agent_turn(
        &mut self,
        agent: &AgentId,
        turn: &TurnId,
        stop: &pacode_types::TurnStop,
        now_ms: u64,
    ) -> TurnEndMatch {
        let Some(position) = self.agent_position(agent) else {
            return TurnEndMatch::Stale;
        };
        let live = self.agents[position].status.is_live();
        let known_terminal = self
            .agent_turn_fences
            .get(agent)
            .is_some_and(|fence| fence.terminal.iter().any(|known| known == turn));
        let (has_active, active_matches) = self
            .agent_turn_fences
            .get(agent)
            .map(|fence| (fence.active.is_some(), fence.active.as_ref() == Some(turn)))
            .unwrap_or((false, false));
        let disposition = if active_matches {
            TurnEndMatch::Current
        } else if has_active {
            TurnEndMatch::Stale
        } else if known_terminal {
            TurnEndMatch::Duplicate
        } else if live {
            TurnEndMatch::Current
        } else {
            TurnEndMatch::Duplicate
        };
        if disposition == TurnEndMatch::Current {
            let fence = self.agent_turn_fences.entry(agent.clone()).or_default();
            fence.active = None;
            fence.terminal.push_back(turn.clone());
            while fence.terminal.len() > 16 {
                fence.terminal.pop_front();
            }
            if let Some(info) = self.agents.get_mut(position)
                && info.status.is_live()
            {
                info.status = match stop {
                    pacode_types::TurnStop::Completed => pacode_types::AgentStatus::Finished,
                    pacode_types::TurnStop::Interrupted => pacode_types::AgentStatus::Stopped,
                    pacode_types::TurnStop::Failed { .. } => pacode_types::AgentStatus::Failed,
                };
                info.finished_at_ms.get_or_insert(now_ms);
            }
        }
        disposition
    }

    /// Drop the output-tail cache and its outstanding wants — the Background
    /// level left the screen; reopening re-arms from scratch (§5.9).
    pub fn clear_task_tail(&mut self) {
        self.task_tail_want = None;
        self.task_tail_in_flight = None;
        self.task_tail = None;
    }

    pub fn live_agents(&self) -> impl Iterator<Item = &AgentInfo> {
        self.agents
            .iter()
            .filter(|a| a.status.is_live() && !a.id.is_main())
    }

    /// Live non-main agents (same set `live_agents` yields): a subagent is
    /// live from spawn — `Idle` included, it sits there until `TurnStarted` —
    /// so its rail duration keeps ticking. The main agent's own liveness is
    /// gated by `turn_active` at the call sites, never by this predicate.
    pub fn has_live_agents(&self) -> bool {
        self.live_agents().next().is_some()
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
