//! All mutable UI state. Widgets read it; `app` and `keys` mutate it; `apply_event`
//! folds daemon events in. Nothing here touches the terminal.

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
    SessionMeta, TaskId, ToastLevel, TranscriptKind,
};

pub use files::FilesState;
pub use input::InputState;
pub use rail::RailState;
pub use selection::Selection;
pub use slots::{NUM_SLOTS, SessionSlot};
pub use transcript::{Cell, CellKind, Transcript};
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
    ConfigPicker {
        index: usize,
        editing_number: Option<String>,
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
            pending_chord: None,
            slots: Default::default(),
            active_slot: 0,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            running_bash: None,
        }
    }

    pub fn is_running_bash(&self) -> bool {
        self.running_bash.is_some()
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
                    mascot: self.mascot,
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
                | Focus::Overlay(Overlay::ThemePicker { .. })
        )
    }

    pub fn bottom_picker_height(&self) -> u16 {
        match &self.focus {
            Focus::Overlay(Overlay::EffortPicker { .. })
            | Focus::Overlay(Overlay::ModePicker { .. }) => 7,
            Focus::Overlay(Overlay::ModelPicker { .. })
            | Focus::Overlay(Overlay::ConfigPicker { .. })
            | Focus::Overlay(Overlay::ThemePicker { .. }) => 12,
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
