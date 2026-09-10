//! All mutable UI state. Widgets read it; `app` and `keys` mutate it; `apply_event`
//! folds daemon events in. Nothing here touches the terminal.

pub mod activity;
pub mod events;
pub mod files;
pub mod input;
pub mod rail;
pub mod selection;
pub mod slots;
pub mod stats;
pub mod transcript;
pub mod vim;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use pacode_client::ClientEvent;
use pacode_types::time::now_ms;
use pacode_types::{
    AgentId, Config, Effort, Event, McpServerInfo, Mode, ModelInfo, ModelRoute, PluginInfo,
    SessionMeta, TaskId, ToastLevel, ToolStatus, TranscriptKind,
};

pub use files::FilesState;
pub use input::InputState;
pub use rail::RailState;
pub use selection::Selection;
pub use slots::{NUM_SLOTS, SessionSlot};
pub use transcript::{
    BackgroundKind, BackgroundOutcome, BackgroundResult, Cell, CellKind, Transcript,
};

/// A background job shorter than this is not reported in the transcript: it is
/// over before the reader could act on it, and the rail already carried it.
pub const BACKGROUND_NOTICE_MIN_MS: u64 = 30_000;

/// Whether a finished background job earns a transcript line. Failures always do,
/// however brief: a command that died immediately is exactly what must be seen.
pub fn background_worth_reporting(outcome: BackgroundOutcome, duration_ms: u64) -> bool {
    match outcome {
        BackgroundOutcome::Completed => duration_ms >= BACKGROUND_NOTICE_MIN_MS,
        BackgroundOutcome::Failed | BackgroundOutcome::Killed => true,
    }
}
pub use vim::{VimEffect, VimMode, VimState};

/// Interaction modes (spec §5). Layers are removed one at a time by `esc`.
#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginsTab {
    /// Plugins loaded in this session.
    Installed,
    /// Plugins the configured marketplace offers.
    Discover,
}

impl PluginsTab {
    pub fn next(self) -> Self {
        match self {
            Self::Installed => Self::Discover,
            Self::Discover => Self::Installed,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Installed => "Installed",
            Self::Discover => "Discover",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
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
    /// The settings view. Its state lives in `AppState::config_view`; the
    /// variant only says which overlay is open.
    ConfigPicker,
    /// A question the model asked. The turn is waiting on the answer, so this
    /// overlay owns the keyboard until it is answered or dismissed.
    QuestionPicker {
        question: Box<pacode_types::Question>,
        index: usize,
        /// Chosen options, in the order they were picked (multi-select only).
        selected: Vec<usize>,
        /// Free text typed instead of picking.
        typed: String,
        /// Whether the reader is typing rather than choosing.
        typing: bool,
    },
    ThemePicker {
        index: usize,
        original_theme: Box<pacode_render::Theme>,
        original_name: String,
        step: ThemePickerStep,
        user_themes: Vec<String>,
    },
    SessionPicker {
        query: String,
        index: usize,
    },
    Files {
        index: usize,
    },
    McpPicker {
        index: usize,
        servers: Vec<McpServerInfo>,
        loading: bool,
    },
    PluginsPicker {
        index: usize,
        plugins: Vec<PluginInfo>,
        /// Which list is on screen.
        tab: PluginsTab,
        /// What the configured marketplace offers, once it has answered.
        market: Vec<pacode_types::MarketplacePluginInfo>,
        /// Filter typed in the Discover tab.
        query: String,
        /// A marketplace request is in flight.
        loading: bool,
        /// The listing came from a cached copy past its TTL.
        stale: bool,
    },
    KeysPicker {
        index: usize,
        capturing: bool,
    },
    Import(crate::ui::import::ImportOverlayState),
    /// Plan + agents on the `Tiny` tier.
    RailOverlay,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePickerStep {
    SelectTheme,
    SelectBase,
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
    pub theme: pacode_render::Theme,
    pub keymap: crate::binding::Keymap,
    pub keymap_warnings: Vec<String>,
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
    pub vim: VimState,
    pub focus: Focus,
    pub config_view: crate::ui::config_view::ConfigViewState,
    pub toasts: VecDeque<Toast>,
    /// Models for the picker (filled by `ListModels`).
    pub models: Vec<ModelInfo>,
    pub sessions: Vec<SessionMeta>,
    pub plugins: Vec<PluginInfo>,
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
    /// Mascot shown in the header banner; random per run, fixed for the session.
    pub mascot: crate::ui::mascot::MascotKind,
    /// Whether the terminal takes RGB; decided once from `[ui] color` and the
    /// environment, and reused by the theme and the mascot.
    pub truecolor: bool,
    /// Pending readline key chord (e.g. `ctrl+x` waiting for `ctrl+e`).
    pub pending_chord: Option<crossterm::event::KeyEvent>,
    /// Session slot table (1..=9).
    pub slots: [Option<SessionSlot>; NUM_SLOTS],
    /// Currently active slot index (0..8).
    pub active_slot: usize,
    /// Working directory for the session.
    pub cwd: PathBuf,
    /// Cancellation sender for currently running local bash command.
    pub running_bash: Option<tokio::sync::oneshot::Sender<()>>,
    /// Temporary files created for pasted images.
    pub pasted_images: crate::clipboard_read::PastedImages,
    /// Current activity phase and when it started. The activity line ages from
    /// this instant, so the thinking wording and its colour reset after every
    /// tool call instead of drifting with the whole turn.
    pub phase: Option<(crate::state::activity::Phase, Instant)>,
    /// Age of `phase`, refreshed by the event loop before each draw so the draw
    /// path stays a pure function of state.
    pub phase_elapsed_ms: u64,
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
    /// Whether a subagent (or task) takes over the conversation column instead of
    /// splitting it. Task output always splits: it is read against the chat.
    pub fn agent_replaces_dialog(&self) -> bool {
        self.config.ui.agent_view == pacode_types::config::AgentView::Replace
            && matches!(self.panel_agent_target(), Some(PanelTarget::Agent(_)))
    }

    /// The panel's current target, whether it came from focus or from a previous
    /// selection that is still open.
    pub fn panel_agent_target(&self) -> Option<PanelTarget> {
        match &self.focus {
            Focus::Panel { target, .. } => Some(target.clone()),
            _ => self.panel.target.clone(),
        }
    }

    pub fn new(config: Config, app_version: String, cols: u16, rows: u16) -> Self {
        let cells = config.ui.transcript_cells;
        let paths = pacode_config::Paths::discover();
        let truecolor = match config.ui.color.as_str() {
            "ansi" => false,
            "truecolor" | "24bit" => true,
            _ => pacode_render::detect_truecolor(),
        };
        let (palette, _) = pacode_config::theme::load_theme(&paths, &config.theme);
        let theme = pacode_render::Theme::from_palette(&palette, truecolor);
        let (keymap, keymap_warnings) = crate::binding::Keymap::from_config(&config.keys);
        Self {
            config,
            theme,
            keymap,
            keymap_warnings,
            plugin_status: None,
            paths,
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
            vim: VimState::default(),
            focus: Focus::Normal,
            config_view: crate::ui::config_view::ConfigViewState::default(),
            toasts: VecDeque::new(),
            models: Vec::new(),
            sessions: Vec::new(),
            plugins: Vec::new(),
            dirty: true,
            cols,
            rows,
            selection: Selection::default(),
            clipboard_warned: false,
            ctrl_c_at: None,
            quit: false,
            turn_started_at: None,
            anim_frame: 0,
            mascot: crate::ui::mascot::MascotKind::random(),
            truecolor,
            pending_chord: None,
            slots: Default::default(),
            active_slot: 0,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            running_bash: None,
            pasted_images: crate::clipboard_read::PastedImages::new(),
            phase: None,
            phase_elapsed_ms: 0,
        }
    }

    /// The phase the session is in right now, from what the transcript and the rail
    /// show. Pure: it reads state only, never the clock.
    pub fn current_phase(&self) -> Option<crate::state::activity::Phase> {
        use crate::state::activity::Phase;

        if self.turn_active {
            // A tool that is executing wins: that is what the session is doing,
            // whatever the model streamed before starting it.
            if let Some(title) = self
                .transcript
                .cells
                .iter()
                .rev()
                .find_map(|c| match &c.kind {
                    CellKind::Item(TranscriptKind::ToolCall {
                        status: ToolStatus::Running,
                        title,
                        ..
                    }) => Some(title.clone()),
                    _ => None,
                })
            {
                return Some(Phase::Tool(title));
            }
            // Answer text already arriving (or still draining) is responding, not thinking.
            let responding = self.transcript.has_backlog()
                || self
                    .transcript
                    .cells
                    .iter()
                    .rev()
                    .find_map(|c| match &c.kind {
                        CellKind::Item(TranscriptKind::Assistant { complete, .. }) => {
                            Some(!complete)
                        }
                        CellKind::Item(TranscriptKind::Reasoning { .. }) => Some(false),
                        CellKind::Item(TranscriptKind::ToolCall { .. }) => Some(false),
                        _ => None,
                    })
                    == Some(true);
            return Some(if responding {
                Phase::Responding
            } else {
                Phase::Thinking
            });
        }

        // The main agent is idle: the session may still be waiting on someone else.
        if let Some(agent) = self.rail.live_agents().next() {
            return Some(Phase::WaitingAgent(agent.name.clone()));
        }
        let running = self.rail.running_task_count();
        if running > 0 {
            return Some(Phase::WaitingTask(running));
        }
        None
    }

    /// Recompute the phase and keep its start instant across redraws. Returns the
    /// phase with the milliseconds it has been running.
    pub fn tick_phase(&mut self, now: Instant) -> Option<(crate::state::activity::Phase, u64)> {
        let current = self.current_phase();
        let out = match (&current, &self.phase) {
            (Some(new), Some((old, since))) if new == old => {
                let elapsed = now.saturating_duration_since(*since).as_millis() as u64;
                Some((old.clone(), elapsed))
            }
            (Some(new), _) => {
                self.phase = Some((new.clone(), now));
                Some((new.clone(), 0))
            }
            (None, _) => {
                self.phase = None;
                None
            }
        };
        let elapsed = out.as_ref().map(|(_, ms)| *ms).unwrap_or(0);
        // A redraw between ticks must not change the reading, so only whole
        // seconds are kept; the sub-second remainder only paces the next tick.
        if crate::state::activity::displayed_secs(elapsed)
            != crate::state::activity::displayed_secs(self.phase_elapsed_ms)
        {
            self.dirty = true;
        }
        self.phase_elapsed_ms = elapsed;
        out
    }

    pub fn is_running_bash(&self) -> bool {
        self.running_bash.is_some()
    }

    pub fn cleanup_pasted_images(&mut self) {
        self.pasted_images.cleanup_unreferenced(&self.input.text);
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
                self.record_slot(
                    self.active_slot,
                    &snapshot.meta,
                    &snapshot.usage,
                    snapshot.agents.len(),
                    snapshot.tasks.len(),
                );
                self.transcript
                    .reset(snapshot.transcript.clone(), snapshot.has_more_history);
                let header = crate::state::transcript::HeaderInfo {
                    version: self.app_version.clone(),
                    day: crate::ui::phrases::day_index(pacode_types::time::now_ms()),
                    mascot: self.mascot,
                    truecolor: self.truecolor,
                };
                self.transcript.set_header(header);
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
                self.rail.cron_jobs = snapshot.cron_jobs;
                self.rail.monitors = snapshot.monitors;
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
                self.observe_background_completion(&event);
                self.apply_event(seq, event, now);
                self.dirty = true;
                true
            }
        }
    }

    /// Fold a daemon event (`ClientEvent::Event`) into transcript/rail/panel/toasts.
    pub fn apply_event(&mut self, seq: u64, event: Event, now: Instant) {
        events::apply_event(self, seq, event, now);
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

    pub fn push_notice_with_level(&mut self, level: ToastLevel, text: String) {
        let now = now_ms();
        self.transcript.cells.push_back(Cell {
            id: now,
            kind: CellKind::Item(TranscriptKind::Notice { level, text }),
            version: 0,
            ts_ms: now,
            stats: None,
        });
        self.transcript.scroll_to_bottom();
        self.dirty = true;
    }

    /// Drain the first prompt from the queue if the main agent is idle.
    pub fn drain_prompt_queue(&mut self) -> Option<pacode_types::Request> {
        if !self.turn_active && !self.input.prompt_queue.is_empty() {
            let text = self.input.prompt_queue.pop_front()?;
            self.turn_active = true;
            self.dirty = true;
            Some(pacode_types::Request::UserMessage { text })
        } else {
            None
        }
    }

    /// Observe background tasks and subagents ending and record a line in the transcript.
    ///
    /// Only jobs that actually ran for a while are worth interrupting the reader
    /// for: anything shorter than `BACKGROUND_NOTICE_MIN_MS` finished about as
    /// fast as reading about it would take, and the rail already showed it. The
    /// line is the only report — the toast that used to accompany it said the
    /// same thing twice.
    pub fn observe_background_completion(&mut self, event: &Event) {
        match event {
            Event::TaskUpdated(info) if info.status.is_terminal() => {
                let was_terminal = self
                    .rail
                    .tasks
                    .iter()
                    .find(|t| t.id == info.id)
                    .is_some_and(|t| t.status.is_terminal());
                if was_terminal {
                    return;
                }
                let outcome = match info.status {
                    pacode_types::TaskStatus::Completed => {
                        if info.exit_code.unwrap_or(0) == 0 {
                            BackgroundOutcome::Completed
                        } else {
                            BackgroundOutcome::Failed
                        }
                    }
                    pacode_types::TaskStatus::Failed => BackgroundOutcome::Failed,
                    pacode_types::TaskStatus::Killed => BackgroundOutcome::Killed,
                    pacode_types::TaskStatus::Running => return,
                };
                let duration_ms = info.duration_ms(now_ms());
                if !background_worth_reporting(outcome, duration_ms) {
                    return;
                }
                let label = if info.command.is_empty() {
                    info.label.clone()
                } else {
                    info.command.clone()
                };
                self.push_background_result(BackgroundResult {
                    kind: BackgroundKind::Task,
                    label,
                    outcome,
                    exit_code: info.exit_code,
                    duration_ms,
                });
            }
            Event::AgentUpdated(info) if !info.id.is_main() && !info.status.is_live() => {
                let was_finished = self
                    .rail
                    .agents
                    .iter()
                    .find(|a| a.id == info.id)
                    .is_some_and(|a| !a.status.is_live());
                if was_finished {
                    return;
                }
                let outcome = match info.status {
                    pacode_types::AgentStatus::Finished => BackgroundOutcome::Completed,
                    pacode_types::AgentStatus::Stopped => BackgroundOutcome::Killed,
                    pacode_types::AgentStatus::Failed => BackgroundOutcome::Failed,
                    pacode_types::AgentStatus::Idle
                    | pacode_types::AgentStatus::Thinking
                    | pacode_types::AgentStatus::RunningTool
                    | pacode_types::AgentStatus::WaitingApproval => return,
                };
                let duration_ms = info.duration_ms(now_ms());
                if !background_worth_reporting(outcome, duration_ms) {
                    return;
                }
                self.push_background_result(BackgroundResult {
                    kind: BackgroundKind::Agent,
                    label: info.name.clone(),
                    outcome,
                    exit_code: None,
                    duration_ms,
                });
            }
            _ => {}
        }
    }

    fn push_background_result(&mut self, result: BackgroundResult) {
        let now = now_ms();
        self.transcript.cells.push_back(Cell {
            id: now,
            kind: CellKind::BackgroundResult(result),
            version: 0,
            ts_ms: now,
            stats: None,
        });
        self.transcript.scroll_to_bottom();
        self.dirty = true;
    }

    /// Remember the marketplace `/plugins <source>` pointed at.
    pub fn save_pref_marketplace(&self, source: &str) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.marketplace = Some(source.to_string());
        if let Err(e) = pacode_config::save_prefs(&self.paths, &prefs) {
            log::warn!("failed to save prefs (marketplace): {e}");
        }
    }

    pub fn save_pref_model(&self, model: &str) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.model = Some(model.to_string());
        if let Err(e) = pacode_config::save_prefs(&self.paths, &prefs) {
            log::warn!("failed to save prefs (model): {e}");
        }
    }

    pub fn save_pref_effort(&self, effort: Effort) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.effort = Some(effort);
        if let Err(e) = pacode_config::save_prefs(&self.paths, &prefs) {
            log::warn!("failed to save prefs (effort): {e}");
        }
    }

    pub fn save_pref_mode(&self, mode: Mode) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.mode = Some(mode);
        if let Err(e) = pacode_config::save_prefs(&self.paths, &prefs) {
            log::warn!("failed to save prefs (mode): {e}");
        }
    }

    pub fn is_bottom_picker(&self) -> bool {
        matches!(
            &self.focus,
            Focus::Overlay(Overlay::EffortPicker { .. })
                | Focus::Overlay(Overlay::ModePicker { .. })
                | Focus::Overlay(Overlay::ModelPicker { .. })
                | Focus::Overlay(Overlay::QuestionPicker { .. })
                | Focus::Overlay(Overlay::ThemePicker { .. })
        )
    }

    pub fn bottom_picker_height(&self) -> u16 {
        match &self.focus {
            Focus::Overlay(Overlay::EffortPicker { .. })
            | Focus::Overlay(Overlay::ModePicker { .. }) => 7,
            Focus::Overlay(Overlay::ModelPicker { .. })
            | Focus::Overlay(Overlay::ThemePicker { .. }) => 12,
            // Header, question, one row per option, and the hint line.
            Focus::Overlay(Overlay::QuestionPicker { question, .. }) => {
                (question.options.len() as u16).saturating_add(5).min(16)
            }
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

    pub fn cwd(&self) -> PathBuf {
        self.meta
            .as_ref()
            .map(|m| m.cwd.clone())
            .unwrap_or_else(|| self.cwd.clone())
    }

    pub fn can_load_history(&self) -> bool {
        self.transcript.has_more_history && !self.transcript.loading_history
    }

    pub fn record_slot(
        &mut self,
        index: usize,
        meta: &SessionMeta,
        usage: &pacode_types::state::UsageTotals,
        agents_count: usize,
        tasks_count: usize,
    ) {
        if index < NUM_SLOTS {
            self.slots[index] = Some(SessionSlot {
                id: meta.id.clone(),
                title: meta.title(),
                turns: usage.turns,
                context_tokens: usage.context_tokens,
                agents_count,
                tasks_count,
            });
        }
    }

    pub fn leave_active_slot(&mut self) {
        let current = self.active_slot;
        if let Some(meta) = &self.meta {
            self.slots[current] = Some(SessionSlot {
                id: meta.id.clone(),
                title: meta.title(),
                turns: self.rail.usage.turns,
                context_tokens: self.rail.usage.context_tokens,
                agents_count: self.rail.agents.len(),
                tasks_count: self.rail.tasks.len(),
            });
        }
        self.transcript.cells.clear();
        self.transcript.cache.clear();
        self.transcript.stream = None;
        self.transcript.live_cell = None;
        self.transcript.pending_final = None;
        self.transcript.scroll_from_bottom = 0;
        self.transcript.has_more_history = false;
        self.transcript.loading_history = false;

        self.panel.agent_transcript.cells.clear();
        self.panel.agent_transcript.cache.clear();
        self.panel.task_lines.clear();
        self.panel.task_total_lines = 0;
        self.panel.target = None;

        self.focus = Focus::Normal;
        self.meta = None;
        self.rail.agents.clear();
        self.rail.tasks.clear();
        self.rail.plan = pacode_types::state::Plan::default();
        self.rail.usage = pacode_types::state::UsageTotals::default();
    }
}
