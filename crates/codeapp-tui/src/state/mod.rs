//! All mutable UI state. Widgets read it; `app` and `keys` mutate it; `apply_event`
//! folds daemon events in. Nothing here touches the terminal.

pub mod input;
pub mod rail;
pub mod transcript;

use std::collections::VecDeque;
use std::time::Instant;

use codeapp_client::ClientEvent;
use codeapp_types::{
    AgentId, Config, Effort, Event, Mode, ModelInfo, ModelRoute, SessionMeta, TaskId, ToastLevel,
};

pub use input::InputState;
pub use rail::RailState;
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
    SessionPicker {
        query: String,
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
    pub app_version: String,
    pub meta: Option<SessionMeta>,
    pub connection: Connection,
    pub turn_active: bool,
    /// Main agent transcript.
    pub transcript: Transcript,
    /// Panel transcript (agent) or task output lines.
    pub panel: PanelState,
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
    /// `ctrl+c` pressed once (exit on second within 2 s).
    pub ctrl_c_at: Option<Instant>,
    pub quit: bool,
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
            rail: RailState::default(),
            input: InputState::default(),
            focus: Focus::Normal,
            toasts: VecDeque::new(),
            models: Vec::new(),
            sessions: Vec::new(),
            dirty: true,
            cols,
            rows,
            ctrl_c_at: None,
            quit: false,
        }
    }

    /// Fold a client event into the state (spec §5 state table, §7 follow rules).
    /// Returns true when a redraw is needed (almost always).
    pub fn apply_client_event(&mut self, event: ClientEvent, now: Instant) -> bool {
        let _ = (event, now);
        todo!("AppState::apply_client_event")
    }

    /// Fold a daemon event (`ClientEvent::Event`) into transcript/rail/panel/toasts.
    pub fn apply_event(&mut self, seq: u64, event: Event, now: Instant) {
        let _ = (seq, event, now);
        todo!("AppState::apply_event")
    }

    pub fn push_toast(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        now: Instant,
    ) {
        let _ = (level, title, detail, now);
        todo!("AppState::push_toast")
    }

    /// Drop expired toasts; returns true when something changed.
    pub fn expire_toasts(&mut self, now: Instant) -> bool {
        let _ = now;
        todo!("AppState::expire_toasts")
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

    /// Whether a periodic 1 s tick is needed (live agents or tasks: durations change).
    pub fn needs_second_tick(&self) -> bool {
        self.turn_active || self.rail.has_live_agents() || self.rail.has_running_tasks()
    }

    /// Whether the paced stream needs its 33 ms tick.
    pub fn needs_stream_tick(&self) -> bool {
        self.transcript.has_backlog() || self.panel.agent_transcript.has_backlog()
    }
}
