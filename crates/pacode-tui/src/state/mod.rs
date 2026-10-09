//! All mutable UI state. Widgets read it; `app` and `keys` mutate it; `apply_event`
//! folds daemon events in. Nothing here touches the terminal.

pub mod activity;
pub mod events;
pub mod files;
pub mod git;
pub mod images;
pub mod input;
pub mod questions;
pub mod rail;
pub mod selection;
pub mod session;
pub mod slots;
pub mod stats;
pub mod transcript;
pub mod vim;

mod attach;
mod client_events;
mod focus;
mod notices;
mod pickers;
mod render;
mod ticks;

#[cfg(test)]
mod picker_tests;

use std::collections::{HashMap, VecDeque};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::time::Instant;

use pacode_render::{ColorMode, Glyphs};
use pacode_types::time::now_ms;
use pacode_types::{
    Config, Effort, MarketplacePluginInfo, McpServerInfo, Mode, ModelFlags, ModelInfo, ModelRoute,
    PluginInfo, SessionMeta, TranscriptKind,
};

pub use client_events::RESYNC_DEBOUNCE_MS;
pub use files::FilesState;
pub use focus::{Focus, Overlay, PanelTarget, ThemePickerStep};
pub use git::GitState;
pub use input::InputState;
pub use notices::{
    BACKGROUND_NOTICE_MIN_MS, TOAST_EXIT_MS, TOAST_MAX, TOAST_TTL_SECS, Toast,
    background_worth_reporting,
};
pub use pickers::{PrefsDelta, nearest_supported_effort, picker_models, zdr_active};
pub use questions::{PendingQuestions, QuestionFocus};
pub use rail::RailState;
pub use render::RenderContext;
pub use selection::Selection;
pub use session::{
    PanelDiff, PanelState, PendingAttach, PromptBatch, SessionContext, SessionUiState, UiContext,
    UserMessageInFlight,
};
pub use slots::{NUM_SLOTS, SessionSlot};
pub use ticks::{OVERLAY_GROW_MS, PULSE_FRAMES, WELCOME_ANIM_MS};
pub use transcript::{
    BackgroundKind, BackgroundOutcome, BackgroundResult, Cell, CellKind, PermExplain, Transcript,
};
pub use vim::{VimEffect, VimMode, VimState};

/// AGENTS ⇄ SESSION switch debounce (spec §5).
pub const IDLE_DEBOUNCE_MS: u64 = 2000;

/// A selection copy the pre-draw coordinator prepared for the event loop.
/// `clipboard::copy` writes OSC 52 to stdout and may touch `arboard` — that IO
/// must stay outside the frame, so preparation stashes the text here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCopy {
    pub text: String,
    /// Auto-copied selections report how to turn the feature off.
    pub auto: bool,
}

/// Connection state shown in the footer / as a notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    /// First frame is already painted; socket connect / daemon spawn / attach
    /// are still in flight.
    Connecting,
    Connected,
    Reconnecting {
        attempt: u32,
    },
    Disconnected {
        reason: String,
    },
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
    pub connection: Connection,
    pub event_connection_epoch: u64,
    pub event_session_epoch: u64,
    pub awaiting_snapshot: bool,
    snapshot_rebase_allowed: bool,
    reconnect_snapshot_pending: bool,
    global_refresh_in_flight: bool,
    global_refresh_remaining: usize,
    global_refresh_context: Option<UiContext>,
    pub session: SessionUiState,
    /// The scan behind the Import submenu (§5.7): what `pacode-import`
    /// discovered plus each entry's conflict status and the apply target.
    /// Filled by `menu::import::on_open`, rebuilt on every open, cleared by a
    /// successful apply; `None` while the menu has never been opened.
    pub import: Option<crate::ui::menu::import::ImportScan>,
    pub toasts: VecDeque<Toast>,
    /// Models for the picker (filled by `ListModels`).
    pub models: Vec<ModelInfo>,
    /// MCP servers for the picker (filled by `ListMcpServers`, seeded from the
    /// disk cache at startup).
    pub mcp_servers: Vec<McpServerInfo>,
    /// The configured marketplace's listing (filled by `BrowseMarketplace`,
    /// seeded from the disk cache at startup).
    pub marketplace: Vec<MarketplacePluginInfo>,
    /// Per-list cache health — which sections came from disk, which refreshed,
    /// which are still in flight. Drives `loading…`/`offline copy` (§8.2).
    pub lists: crate::cache::Lists,
    /// The on-disk list cache. `run` points it at `paths.cache_dir` before the
    /// first frame; until then it is a benign no-op directory.
    pub cache: crate::cache::DiskCache,
    /// When this client run started — the TTL baseline for session-scoped
    /// cache entries (anything written before this is `offline copy` material).
    pub session_start_ms: u64,
    /// Per-route flags (`zdr_capable`, `favorite`) mirroring `prefs.toml`;
    /// written through `save_pref_model_flags` and pushed to the daemon via
    /// `Request::SetModelFlag`.
    pub model_flags: HashMap<String, ModelFlags>,
    /// Providers with auth status for the login picker (filled by `ListAuth` or `AuthUpdated`).
    pub auth_providers: Vec<pacode_types::ProviderAuthInfo>,
    pub sessions: Vec<SessionMeta>,
    pub plugins: Vec<PluginInfo>,
    /// Set by any mutation; cleared after a frame is drawn.
    pub dirty: bool,
    /// Set by the animation tick; cleared after a frame is drawn. Paced by
    /// the animation clock (`ui.animation`), not the content clock (`ui.ups`).
    pub dirty_anim: bool,
    /// Terminal size as last seen.
    pub cols: u16,
    pub rows: u16,
    /// Whether the OSC 52 remote clipboard hint toast has already been shown this session.
    pub clipboard_warned: bool,
    /// `ctrl+c` pressed once (exit on second within 2 s).
    pub ctrl_c_at: Option<Instant>,
    pub quit: bool,
    /// Mascot shown in the header banner; random per run, fixed for the session.
    pub mascot: crate::ui::mascot::MascotKind,
    /// Whether the terminal takes RGB; decided once from `[ui] color` and the
    /// environment, and reused by the theme and the mascot.
    pub truecolor: bool,
    /// The last notice-lock state the daemon reported for this session. A view,
    /// not a switch: the lock itself is the daemon's, and this exists so the
    /// footer can say how much is parked.
    pub notice_lock: crate::lock::LockView,
    pub(crate) color_mode: ColorMode,
    pub(crate) detected_color_mode: ColorMode,
    pub(crate) render_context: RenderContext,
    pub(crate) render_epoch: u64,
    /// Double-Shift detector state for the menu hub (§4.2).
    pub double_shift: crate::double_shift::DoubleShift,
    /// Session slot table (1..=9).
    pub slots: [Option<SessionSlot>; NUM_SLOTS],
    /// Currently active slot index (0..8).
    pub active_slot: usize,
    /// Working directory for the session.
    pub cwd: PathBuf,
    /// Escape sequences stashed by `ui::images::draw` and flushed to the
    /// terminal post-draw: (terminal area, escape payload).
    pub pending_image_escapes: Vec<(ratatui::layout::Rect, String)>,
    /// The `-p`/`--prompt` argv text, carried until the startup attach lands
    /// — `BgResponse::AttachFinished` sends it then.
    pub initial_prompt: Option<String>,
    /// Why the startup attach failed — `run` returns it once the loop exits
    /// (the error also sets `quit`, so a failed attach still exits fast).
    pub attach_error: Option<pacode_client::ClientError>,
    pub pending_attach: Option<PendingAttach>,
    pub attach_in_flight: bool,
    /// Whether this client was started with `--analyze-mode`. Per process, so
    /// it outlives session switches. The page is gated on it: the reading is
    /// a scorecard, and a scorecard nobody asked for is noise on a screen
    /// someone is trying to work in.
    pub analyze_mode: bool,
    /// Prompts queued behind a running turn of a session the user switched
    /// away from, keyed by that session; handed back when it is attached
    /// again. Capped at `NUM_SLOTS` sessions (each queue at
    /// `PROMPT_QUEUE_CAP`); the oldest stash is dropped with a toast.
    pub(crate) stashed_queues: VecDeque<(pacode_types::SessionId, VecDeque<String>)>,
    /// Default changes (`prefs.toml`) waiting for the event loop to write
    /// them on a blocking thread; coalesced, so it is at most one record.
    pub(crate) pending_prefs: PrefsDelta,
}

impl Deref for AppState {
    type Target = SessionUiState;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl DerefMut for AppState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

impl AppState {
    pub fn new(config: Config, app_version: String, cols: u16, rows: u16) -> Self {
        let cells = config.ui.transcript_cells;
        let paths = pacode_config::Paths::discover();
        let detected_color_mode = ColorMode::detect();
        let color_mode = ColorMode::from_setting(&config.ui.color, detected_color_mode);
        let truecolor = color_mode == ColorMode::Truecolor;
        let (palette, _) = pacode_config::theme::load_theme(&paths, &config.theme);
        let theme = pacode_render::Theme::from_palette(&palette, truecolor);
        let (keymap, keymap_warnings) = crate::binding::Keymap::from_config(&config.keys);
        let render_context = RenderContext {
            theme: theme.clone(),
            glyphs: Glyphs::new(config.ui.ascii_only),
            color: color_mode,
            thinking: config.ui.thinking,
            expand_hint: render::expand_hint_for(&keymap),
        };
        let model_flags = pacode_config::load_prefs(&paths).model_flags;
        Self {
            config,
            theme,
            keymap,
            keymap_warnings,
            plugin_status: None,
            paths,
            app_version,
            connection: Connection::Connected,
            event_connection_epoch: 0,
            event_session_epoch: 0,
            awaiting_snapshot: true,
            snapshot_rebase_allowed: false,
            reconnect_snapshot_pending: false,
            global_refresh_in_flight: false,
            global_refresh_remaining: 0,
            global_refresh_context: None,
            session: SessionUiState::new(cells),
            import: None,
            toasts: VecDeque::new(),
            models: Vec::new(),
            model_flags,
            auth_providers: Vec::new(),
            sessions: Vec::new(),
            plugins: Vec::new(),
            mcp_servers: Vec::new(),
            marketplace: Vec::new(),
            lists: crate::cache::Lists::default(),
            cache: crate::cache::DiskCache::default(),
            session_start_ms: now_ms(),
            dirty: true,
            dirty_anim: false,
            cols,
            rows,
            clipboard_warned: false,
            ctrl_c_at: None,
            quit: false,
            mascot: crate::ui::mascot::MascotKind::random(),
            truecolor,
            notice_lock: crate::lock::LockView::default(),
            color_mode,
            detected_color_mode,
            render_context,
            render_epoch: 0,
            double_shift: crate::double_shift::DoubleShift::default(),
            slots: Default::default(),
            active_slot: 0,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            pending_image_escapes: Vec::new(),
            initial_prompt: None,
            attach_error: None,
            pending_attach: None,
            attach_in_flight: false,
            analyze_mode: false,
            stashed_queues: VecDeque::new(),
            pending_prefs: PrefsDelta::default(),
        }
    }

    pub fn is_running_bash(&self) -> bool {
        self.running_bash.is_some()
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

    pub fn take_retry_batch(&mut self) -> Option<PromptBatch> {
        if self.meta.is_none()
            || !matches!(self.connection, Connection::Connected)
            || self.awaiting_snapshot
            || self.resync_want
            || self.turn_active
            || self.user_msgs_in_flight > 0
            || self.turn_boundary_pending
        {
            return None;
        }
        self.session.take_retry_batch()
    }

    pub fn drain_pending_sequence(&mut self) -> Option<PromptBatch> {
        if self.meta.is_none()
            || !matches!(self.connection, Connection::Connected)
            || self.awaiting_snapshot
            || self.resync_want
            || self.user_msgs_in_flight > 0
            || self.turn_boundary_pending
            || self.initial_prompt.is_some()
            || self.has_retry_request()
        {
            return None;
        }
        let sequence = self.session.take_pending_sequence()?;
        self.dirty = true;
        Some(sequence)
    }

    pub fn drain_prompt_queue(&mut self) -> Option<pacode_types::Request> {
        if self.meta.is_none()
            || !matches!(self.connection, Connection::Connected)
            || self.awaiting_snapshot
            || self.resync_want
            || self.turn_active
            || self.user_msgs_in_flight > 0
            || self.turn_boundary_pending
            || self.has_retry_request()
            || self.has_pending_sequence()
        {
            return None;
        }
        if let Some(text) = self.initial_prompt.take() {
            self.dirty = true;
            return Some(pacode_types::Request::UserMessage {
                text,
                images: Vec::new(),
                message_id: None,
            });
        }
        let text = self.input.prompt_queue.pop_front()?;
        self.dirty = true;
        Some(pacode_types::Request::UserMessage {
            text,
            images: Vec::new(),
            message_id: None,
        })
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

    /// A pending permission cell still sitting in the transcript — the same
    /// predicate the footer prompt and the anim tick use.
    pub fn has_pending_permission(&self) -> bool {
        self.transcript
            .cells
            .iter()
            .any(|c| matches!(c.kind, CellKind::Item(TranscriptKind::Permission(_))))
    }

    /// Drop a cache file (§8.2 invalidation points: plugin install/remove,
    /// MCP config change). Best-effort on a blocking thread — the next list
    /// reply rewrites the file anyway.
    pub fn invalidate_cache(&self, kind: crate::cache::ListKind) {
        let cache = self.cache.clone();
        let stem = kind.stem(&self.cwd()).into_owned();
        let schema = kind.schema();
        let invalidate = move || {
            cache.invalidate(&stem, schema);
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(invalidate);
        } else {
            std::thread::spawn(invalidate);
        }
    }
}
