//! All mutable UI state. Widgets read it; `app` and `keys` mutate it; `apply_event`
//! folds daemon events in. Nothing here touches the terminal.

pub mod files;
pub mod input;
pub mod rail;
pub mod selection;
pub mod stats;
pub mod transcript;

use std::collections::VecDeque;
use std::time::Instant;

use pacode_client::ClientEvent;
use pacode_types::time::now_ms;
use pacode_types::{
    AgentId, Config, Effort, Event, Mode, ModelInfo, ModelRoute, PermissionDecision, SessionMeta,
    TaskId, TaskStatus, ToastLevel, TranscriptKind,
};

pub use files::FilesState;
pub use input::InputState;
pub use rail::RailState;
pub use selection::Selection;
pub use transcript::{Cell, CellKind, Transcript};

/// Interaction modes (spec §5). Layers are removed one at a time by `esc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Focus {
    Normal,
    /// `alt+↓` pressed: the rail lists agents, one is highlighted.
    SelectAgent {
        index: usize,
    },
    /// Panel open on an agent or task.
    Panel {
        target: PanelTarget,
        follow: bool,
        follow_paused: bool,
    },
    /// `.` in an empty prompt: background task list overlay.
    BgList {
        index: usize,
    },
    /// Slash pickers and other overlays.
    Overlay(Overlay),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelTarget {
    Agent(AgentId),
    Task(TaskId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    ModelPicker {
        query: String,
        index: usize,
    },
    EffortPicker {
        index: usize,
    },
    ModePicker {
        index: usize,
    },
    ConfigPicker {
        index: usize,
        editing_number: Option<String>,
    },
    SessionPicker {
        query: String,
        index: usize,
    },
    Files {
        index: usize,
    },
    /// Plan + agents on the `Tiny` tier.
    RailOverlay,
    Help,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub level: ToastLevel,
    pub title: String,
    pub detail: Option<String>,
    pub shown_at: Instant,
}

pub const TOAST_TTL_SECS: u64 = 6;
pub const TOAST_MAX: usize = 2;
/// AGENTS ⇄ SESSION switch debounce (spec §5).
pub const IDLE_DEBOUNCE_MS: u64 = 2000;

/// Connection state shown in the footer / as a notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Connected,
    Reconnecting { attempt: u32 },
    Disconnected { reason: String },
}

pub struct AppState {
    pub config: Config,
    /// Footer status text set by a plugin via `pacode.status` (plugin name, text).
    pub plugin_status: Option<(String, String)>,
    pub paths: pacode_config::Paths,
    pub app_version: String,
    pub meta: Option<SessionMeta>,
    pub connection: Connection,
    pub turn_active: bool,
    /// Main agent transcript.
    pub transcript: Transcript,
    /// Panel transcript (agent) or task output lines.
    pub panel: PanelState,
    pub files: FilesState,
    pub rail: RailState,
    pub input: InputState,
    pub focus: Focus,
    pub toasts: VecDeque<Toast>,
    /// Models for the picker (filled by `ListModels`).
    pub models: Vec<ModelInfo>,
    pub sessions: Vec<SessionMeta>,
    /// Set by any mutation; cleared after a frame is drawn.
    pub dirty: bool,
    /// Terminal size as last seen.
    pub cols: u16,
    pub rows: u16,
    /// Mouse selection in dialog area.
    pub selection: Selection,
    /// Whether the OSC 52 remote clipboard hint popup has already been shown this session.
    pub clipboard_warned: bool,
    /// `ctrl+c` pressed once (exit on second within 2 s).
    pub ctrl_c_at: Option<Instant>,
    pub quit: bool,
    /// When the main agent turn started (for calculating thinking duration and animations).
    pub turn_started_at: Option<Instant>,
    /// Frame counter for activity animations and spinners (8 fps).
    pub anim_frame: u64,
}

/// What the panel shows.
pub struct PanelState {
    pub target: Option<PanelTarget>,
    pub agent_transcript: Transcript,
    pub task_lines: Vec<String>,
    pub task_total_lines: u64,
    pub scroll: usize,
}

impl AppState {
    pub fn new(config: Config, app_version: String, cols: u16, rows: u16) -> Self {
        let cells = config.ui.transcript_cells;
        Self {
            config,
            plugin_status: None,
            paths: pacode_config::Paths::discover(),
            app_version,
            meta: None,
            connection: Connection::Connected,
            turn_active: false,
            transcript: Transcript::new(cells),
            panel: PanelState {
                target: None,
                agent_transcript: Transcript::new(300),
                task_lines: Vec::new(),
                task_total_lines: 0,
                scroll: 0,
            },
            files: FilesState::default(),
            rail: RailState::default(),
            input: InputState::default(),
            focus: Focus::Normal,
            toasts: VecDeque::new(),
            models: Vec::new(),
            sessions: Vec::new(),
            dirty: true,
            cols,
            rows,
            selection: Selection::default(),
            clipboard_warned: false,
            ctrl_c_at: None,
            quit: false,
            turn_started_at: None,
            anim_frame: 0,
        }
    }

    /// Fold a client event into the state (spec §5 state table, §7 follow rules).
    /// Returns true when a redraw is needed (almost always).
    pub fn apply_client_event(&mut self, event: ClientEvent, now: Instant) -> bool {
        match event {
            ClientEvent::Connected { .. } => {
                self.connection = Connection::Connected;
                self.dirty = true;
                true
            }
            ClientEvent::Disconnected { reason } => {
                self.connection = Connection::Disconnected { reason };
                self.dirty = true;
                true
            }
            ClientEvent::Reconnecting { attempt } => {
                self.connection = Connection::Reconnecting { attempt };
                self.dirty = true;
                true
            }
            ClientEvent::Snapshot(snapshot) => {
                self.connection = Connection::Connected;
                self.meta = Some(snapshot.meta.clone());
                self.transcript
                    .reset(snapshot.transcript.clone(), snapshot.has_more_history);
                let header = crate::state::transcript::HeaderInfo {
                    model: snapshot.meta.model.model.clone(),
                    effort: snapshot.meta.effort.as_str().to_string(),
                    provider: snapshot.meta.model.provider.clone(),
                    cwd: snapshot.meta.cwd.display().to_string(),
                    config_path: self.paths.config_file.display().to_string(),
                    version: self.app_version.clone(),
                };
                self.transcript.insert_header(header);
                for item in &snapshot.transcript {
                    if let TranscriptKind::ToolCall { name, title, .. } = &item.kind {
                        let arg = title
                            .strip_prefix(name.as_str())
                            .map(str::trim_start)
                            .unwrap_or_else(|| {
                                title
                                    .split_once(' ')
                                    .map(|(_, r)| r.trim())
                                    .unwrap_or(title.as_str())
                            });
                        let input = serde_json::json!({ "path": arg });
                        self.files.observe_tool_item(name, &input, item.ts_ms);
                    }
                }
                self.turn_active = snapshot.turn_active;
                if snapshot.turn_active {
                    if self.turn_started_at.is_none() {
                        self.turn_started_at = Some(now);
                    }
                } else {
                    self.turn_started_at = None;
                }
                self.rail.plan = snapshot.plan;
                self.rail.agents = snapshot.agents;
                self.rail.agents.sort_by_key(|a| a.started_at_ms);
                self.rail.tasks = snapshot.tasks;
                self.rail.tasks.sort_by_key(|t| t.started_at_ms);
                self.rail.usage = snapshot.usage;
                self.rail.update_idle(self.turn_active, now);
                for (i, perm) in snapshot.pending_permissions.into_iter().enumerate() {
                    let ts_ms = perm.created_at_ms;
                    self.transcript.cells.push_back(Cell {
                        id: snapshot.seq.wrapping_add(1).wrapping_add(i as u64),
                        kind: CellKind::Item(TranscriptKind::Permission(perm)),
                        version: 0,
                        ts_ms,
                        stats: None,
                    });
                }
                self.dirty = true;
                true
            }
            ClientEvent::Event { seq, event } => {
                self.apply_event(seq, event, now);
                self.dirty = true;
                true
            }
        }
    }

    /// Fold a daemon event (`ClientEvent::Event`) into transcript/rail/panel/toasts.
    pub fn apply_event(&mut self, seq: u64, event: Event, now: Instant) {
        match event {
            Event::SessionUpdated(meta) => {
                let mut prefs = pacode_config::load_prefs(&self.paths);
                prefs.model = Some(meta.model.to_string());
                prefs.effort = Some(meta.effort);
                prefs.mode = Some(meta.mode);
                let _ = pacode_config::save_prefs(&self.paths, &prefs);
                let header = crate::state::transcript::HeaderInfo {
                    model: meta.model.model.clone(),
                    effort: meta.effort.as_str().to_string(),
                    provider: meta.model.provider.clone(),
                    cwd: meta.cwd.display().to_string(),
                    config_path: self.paths.config_file.display().to_string(),
                    version: self.app_version.clone(),
                };
                self.transcript.insert_header(header);
                self.meta = Some(meta);
            }
            Event::TurnStarted { agent, turn: _ } => {
                if agent.is_main() {
                    self.turn_active = true;
                    self.turn_started_at = Some(now);
                    self.rail.update_idle(true, now);
                }
            }
            Event::TurnEnded {
                agent,
                turn: _,
                usage,
                stop,
            } => {
                if let pacode_types::TurnStop::Failed { message } = &stop {
                    self.push_toast(
                        ToastLevel::Error,
                        "turn failed".to_string(),
                        Some(message.clone()),
                        now,
                    );
                }
                if agent.is_main() {
                    self.transcript.flush_stream();
                    if let Some(ref u) = usage
                        && u.output_tokens > 0
                    {
                        let duration_ms = self
                            .turn_started_at
                            .map(|t| now.saturating_duration_since(t).as_millis() as u64)
                            .unwrap_or_else(|| {
                                let last_ts = self
                                    .transcript
                                    .cells
                                    .iter()
                                    .rev()
                                    .find_map(|c| {
                                        if matches!(
                                            c.kind,
                                            CellKind::Item(TranscriptKind::Assistant { .. })
                                        ) {
                                            Some(c.ts_ms)
                                        } else {
                                            None
                                        }
                                    })
                                    .unwrap_or(0);
                                pacode_types::time::now_ms().saturating_sub(last_ts)
                            });
                        if let Some(stats) = compute_turn_stats(duration_ms, u) {
                            attach_turn_stats(&mut self.transcript, stats);
                        }
                    }
                    self.turn_active = false;
                    self.turn_started_at = None;
                    self.rail.update_idle(false, now);
                } else {
                    let is_panel_target = self.panel.target.as_ref().is_some_and(|t| match t {
                        PanelTarget::Agent(id) => *id == agent,
                        _ => false,
                    });
                    if is_panel_target {
                        self.panel.agent_transcript.flush_stream();
                        if let Some(ref u) = usage
                            && u.output_tokens > 0
                        {
                            let last_ts = self
                                .panel
                                .agent_transcript
                                .cells
                                .iter()
                                .rev()
                                .find_map(|c| {
                                    if matches!(
                                        c.kind,
                                        CellKind::Item(TranscriptKind::Assistant { .. })
                                    ) {
                                        Some(c.ts_ms)
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(0);
                            let duration_ms = pacode_types::time::now_ms().saturating_sub(last_ts);
                            if let Some(stats) = compute_turn_stats(duration_ms, u) {
                                attach_turn_stats(&mut self.panel.agent_transcript, stats);
                            }
                        }
                    }
                }
            }
            Event::ItemAdded(item) => {
                if let TranscriptKind::ToolCall { name, title, .. } = &item.kind {
                    let arg = title
                        .strip_prefix(name.as_str())
                        .map(str::trim_start)
                        .unwrap_or_else(|| {
                            title
                                .split_once(' ')
                                .map(|(_, r)| r.trim())
                                .unwrap_or(title.as_str())
                        });
                    let input = serde_json::json!({ "path": arg });
                    self.files.observe_tool_item(name, &input, item.ts_ms);
                }
                if item.agent.is_main() {
                    self.transcript.upsert(item, now);
                } else if let Focus::Panel {
                    target: PanelTarget::Agent(ref id),
                    ..
                } = self.focus
                    && *id == item.agent
                {
                    self.panel.agent_transcript.upsert(item, now);
                }
            }
            Event::ItemUpdated(item) => {
                if item.agent.is_main() {
                    self.transcript.upsert(item, now);
                } else if let Focus::Panel {
                    target: PanelTarget::Agent(ref id),
                    ..
                } = self.focus
                    && *id == item.agent
                {
                    self.panel.agent_transcript.upsert(item, now);
                }
            }
            Event::TextDelta {
                agent,
                item_seq,
                text,
            } => {
                if agent.is_main() {
                    self.transcript.push_delta(item_seq, &text, false, now);
                } else if let Focus::Panel {
                    target: PanelTarget::Agent(ref id),
                    ..
                } = self.focus
                    && *id == agent
                {
                    self.panel
                        .agent_transcript
                        .push_delta(item_seq, &text, false, now);
                }
            }
            Event::ReasoningDelta {
                agent,
                item_seq,
                text,
            } => {
                if agent.is_main() {
                    self.transcript.push_delta(item_seq, &text, true, now);
                } else if let Focus::Panel {
                    target: PanelTarget::Agent(ref id),
                    ..
                } = self.focus
                    && *id == agent
                {
                    self.panel
                        .agent_transcript
                        .push_delta(item_seq, &text, true, now);
                }
            }
            Event::PermissionRequested(req) => {
                let ts_ms = req.created_at_ms;
                self.transcript.cells.push_back(Cell {
                    id: seq,
                    kind: CellKind::Item(TranscriptKind::Permission(req)),
                    version: 0,
                    ts_ms,
                    stats: None,
                });
            }
            Event::PermissionResolved {
                permission,
                decision,
            } => {
                for cell in &mut self.transcript.cells {
                    if let CellKind::Item(TranscriptKind::Permission(ref req)) = cell.kind
                        && req.id == permission
                    {
                        let (level, text) = match decision {
                            PermissionDecision::AllowOnce => {
                                (ToastLevel::Success, format!("Allowed: {}", req.title))
                            }
                            PermissionDecision::AllowSession => (
                                ToastLevel::Success,
                                format!("Allowed for session: {}", req.title),
                            ),
                            PermissionDecision::Deny => {
                                (ToastLevel::Warn, format!("Denied: {}", req.title))
                            }
                        };
                        cell.kind = CellKind::Item(TranscriptKind::Notice { level, text });
                        cell.version = cell.version.wrapping_add(1);
                        break;
                    }
                }
            }
            Event::PlanUpdated(plan) => {
                self.rail.plan = plan;
            }
            Event::AgentAdded(info) => {
                self.rail.upsert_agent(info);
            }
            Event::AgentUpdated(info) => {
                if !info.status.is_live()
                    && let Focus::Panel {
                        target: PanelTarget::Agent(ref id),
                        ref mut follow,
                        ..
                    } = self.focus
                {
                    if *id == info.id {
                        *follow = false;
                    } else if *follow {
                        let title = format!("{} finished", info.name);
                        self.push_toast(ToastLevel::Info, title, info.summary.clone(), now);
                    }
                }
                self.rail.upsert_agent(info);
            }
            Event::TaskAdded(info) => {
                self.rail.upsert_task(info);
            }
            Event::TaskUpdated(info) => {
                if info.status.is_terminal()
                    && let Focus::Panel { follow: true, .. } = self.focus
                {
                    let level = match info.status {
                        TaskStatus::Failed => ToastLevel::Error,
                        _ => ToastLevel::Success,
                    };
                    let duration =
                        pacode_types::time::format_duration_ms(info.duration_ms(now_ms()));
                    let title = format!(
                        "{} {}",
                        info.label,
                        if info.status == TaskStatus::Failed {
                            "failed"
                        } else {
                            "completed"
                        }
                    );
                    self.push_toast(level, title, Some(duration), now);
                }
                self.rail.upsert_task(info);
            }
            Event::UsageUpdated(usage) => {
                self.rail.usage = usage;
            }
            Event::Toast {
                level,
                title,
                detail,
            } => {
                self.push_toast(level, title, detail, now);
            }
            Event::DaemonShuttingDown => {
                self.connection = Connection::Disconnected {
                    reason: "Daemon shutting down".into(),
                };
            }
            Event::PluginToast { plugin, text } => {
                self.push_toast(pacode_types::ToastLevel::Info, text, Some(plugin), now);
            }
            Event::PluginStatus { plugin, text } => {
                if text.is_empty() {
                    self.plugin_status = None;
                } else {
                    self.plugin_status = Some((plugin, text));
                }
            }
        }
        self.rail.update_idle(self.turn_active, now);
    }

    pub fn push_toast(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        now: Instant,
    ) {
        let toast = Toast {
            level,
            title,
            detail,
            shown_at: now,
        };
        self.toasts.push_back(toast);
        while self.toasts.len() > TOAST_MAX {
            self.toasts.pop_front();
        }
        self.dirty = true;
    }

    /// Drop expired toasts; returns true when something changed.
    pub fn expire_toasts(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts
            .retain(|t| now.saturating_duration_since(t.shown_at).as_secs() < TOAST_TTL_SECS);
        let changed = self.toasts.len() != before;
        if changed {
            self.dirty = true;
        }
        changed
    }

    pub fn mode(&self) -> Mode {
        self.meta.as_ref().map(|m| m.mode).unwrap_or_default()
    }

    pub fn effort(&self) -> Effort {
        self.meta.as_ref().map(|m| m.effort).unwrap_or_default()
    }

    pub fn model(&self) -> Option<&ModelRoute> {
        self.meta.as_ref().map(|m| &m.model)
    }

    pub fn push_notice(&mut self, text: String) {
        let now = now_ms();
        self.transcript.cells.push_back(Cell {
            id: now,
            kind: CellKind::Item(TranscriptKind::Notice {
                level: ToastLevel::Info,
                text,
            }),
            version: 0,
            ts_ms: now,
            stats: None,
        });
        self.dirty = true;
    }

    pub fn save_pref_model(&self, model: &str) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.model = Some(model.to_string());
        let _ = pacode_config::save_prefs(&self.paths, &prefs);
    }

    pub fn save_pref_effort(&self, effort: Effort) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.effort = Some(effort);
        let _ = pacode_config::save_prefs(&self.paths, &prefs);
    }

    pub fn save_pref_mode(&self, mode: Mode) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.mode = Some(mode);
        let _ = pacode_config::save_prefs(&self.paths, &prefs);
    }

    pub fn is_bottom_picker(&self) -> bool {
        matches!(
            &self.focus,
            Focus::Overlay(Overlay::EffortPicker { .. })
                | Focus::Overlay(Overlay::ModePicker { .. })
                | Focus::Overlay(Overlay::ModelPicker { .. })
                | Focus::Overlay(Overlay::ConfigPicker { .. })
        )
    }

    pub fn bottom_picker_height(&self) -> u16 {
        match &self.focus {
            Focus::Overlay(Overlay::EffortPicker { .. })
            | Focus::Overlay(Overlay::ModePicker { .. }) => 7,
            Focus::Overlay(Overlay::ModelPicker { .. })
            | Focus::Overlay(Overlay::ConfigPicker { .. }) => 12,
            _ => 0,
        }
    }

    /// Whether a periodic 1 s tick is needed (live agents or tasks: durations change).
    pub fn needs_second_tick(&self) -> bool {
        self.turn_active || self.rail.has_live_agents() || self.rail.has_running_tasks()
    }

    /// Whether the 8 fps (125 ms) animation tick is needed (turn active or active panel agent).
    pub fn needs_anim_tick(&self) -> bool {
        if self.turn_active {
            return true;
        }
        match &self.focus {
            Focus::Panel {
                target: PanelTarget::Agent(id),
                ..
            } => self.rail.agent(id).is_some_and(|a| a.status.is_active()),
            _ => self
                .panel
                .target
                .as_ref()
                .and_then(|t| match t {
                    PanelTarget::Agent(id) => self.rail.agent(id),
                    _ => None,
                })
                .is_some_and(|a| a.status.is_active()),
        }
    }

    /// Whether the paced stream needs its 33 ms tick.
    pub fn needs_stream_tick(&self) -> bool {
        self.transcript.has_backlog() || self.panel.agent_transcript.has_backlog()
    }

    pub fn tick_stream(&mut self, now: Instant) -> bool {
        let r1 = self.transcript.tick_stream(now);
        let r2 = self.panel.agent_transcript.tick_stream(now);
        if r1 || r2 {
            self.dirty = true;
            true
        } else {
            false
        }
    }
}

fn compute_turn_stats(duration_ms: u64, usage: &pacode_types::stream::Usage) -> Option<String> {
    if usage.output_tokens == 0 {
        return None;
    }
    Some(stats::stats_line(duration_ms))
}

fn attach_turn_stats(transcript: &mut Transcript, stats_str: String) {
    if let Some(cell) = transcript
        .cells
        .iter_mut()
        .rev()
        .find(|c| matches!(c.kind, CellKind::Item(TranscriptKind::Assistant { .. })))
    {
        cell.stats = Some(stats_str);
        cell.version = cell.version.wrapping_add(1);
    }
}
