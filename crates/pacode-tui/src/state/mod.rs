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

#[cfg(test)]
mod picker_tests;

use std::collections::{HashMap, VecDeque};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicU64};
use std::time::Instant;

use pacode_client::{ClientEvent, ClientEventContext};
use pacode_render::{ColorMode, Glyphs, RenderOptions};
use pacode_types::time::now_ms;
use pacode_types::{
    AgentId, Config, Effort, Event, MarketplacePluginInfo, McpServerInfo, Mode, ModelFlags,
    ModelInfo, ModelRoute, PluginInfo, SessionMeta, TaskId, ToastLevel, ToolStatus, TranscriptKind,
    ZdrMode,
};

pub use files::FilesState;
pub use git::GitState;
pub use input::InputState;
pub use questions::{PendingQuestions, QuestionFocus};
pub use rail::RailState;
pub use selection::Selection;
pub use session::{
    PanelDiff, PanelState, PendingAttach, PromptBatch, SessionContext, SessionUiState, UiContext,
    UserMessageInFlight,
};
pub use slots::{NUM_SLOTS, SessionSlot};
pub use transcript::{
    BackgroundKind, BackgroundOutcome, BackgroundResult, Cell, CellKind, PermExplain, Transcript,
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

/// ZDR active = config-forced `always`, or `session` mode with the toggle on.
/// Free function (not `&self`) so picker code can call it while `state.focus`
/// is mutably borrowed.
pub fn zdr_active(config: &Config, meta: Option<&SessionMeta>) -> bool {
    match config.zdr {
        ZdrMode::Always => true,
        ZdrMode::Session => meta.is_some_and(|m| m.zdr),
        ZdrMode::Off => false,
    }
}

/// Nearest supported level below `current`; when nothing lower is supported,
/// falls back to the lowest supported entry (spec §6 downgrade chain —
/// `Xhigh→High→Medium→Low`, then floor). `None` only when `supported` is
/// empty.
pub fn nearest_supported_effort(current: Effort, supported: &[Effort]) -> Option<Effort> {
    let idx = Effort::ALL.iter().position(|e| *e == current)?;
    Effort::ALL[..idx]
        .iter()
        .rev()
        .find(|e| supported.contains(e))
        .or_else(|| Effort::ALL.iter().find(|e| supported.contains(e)))
        .copied()
}

/// Model-picker rows: `zdr_only` restricts to `zdr_capable` routes (ZDR active
/// or the Alt+Z enable flow); favorites sort first, then display name, then
/// route. Field-level args keep this callable while `state.focus` is borrowed.
pub fn picker_models<'a>(
    models: &'a [ModelInfo],
    flags: &HashMap<String, ModelFlags>,
    query: &str,
    zdr_only: bool,
) -> Vec<&'a ModelInfo> {
    let q = query.to_lowercase();
    let mut rows: Vec<&ModelInfo> = models
        .iter()
        .filter(|m| {
            (!zdr_only
                || flags
                    .get(&m.route.to_string())
                    .is_some_and(|f| f.zdr_capable))
                && (q.is_empty()
                    || m.route.to_string().to_lowercase().contains(&q)
                    || m.display_name.to_lowercase().contains(&q))
        })
        .collect();
    rows.sort_by(|a, b| {
        let fa = flags.get(&a.route.to_string()).is_some_and(|f| f.favorite);
        let fb = flags.get(&b.route.to_string()).is_some_and(|f| f.favorite);
        fb.cmp(&fa)
            .then_with(|| a.display_name.cmp(&b.display_name))
            .then_with(|| a.route.provider.cmp(&b.route.provider))
            .then_with(|| a.route.model.cmp(&b.route.model))
    });
    rows
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
    /// Slash pickers and other overlays.
    Overlay(Overlay),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelTarget {
    Agent(AgentId),
    Task(TaskId),
    /// An MCP server's daemon-log tail — the menu's `l` action (§5.3). The
    /// lines are read locally off `paths.daemon_log()`; there is no daemon
    /// request behind this target, so `Action::LoadPanel` skips it.
    McpServer(String),
    /// The Files level's `enter` (§5.5): the worktree diff of one touched
    /// file. The patch is produced by a deferred `git` job
    /// (`files.diff_want` → `BgResponse::FileDiffReady` → [`PanelDiff`]) —
    /// there is no daemon request, so `Action::LoadPanel` skips it.
    FileDiff {
        path: String,
    },
    /// The Git level's `enter` (§5.10): one commit's patch. `Request::GitDiff`
    /// flies the moment the row opens (not via `LoadPanel`); the reply lands
    /// in `state.git.diff`, keyed by hash.
    CommitDiff {
        hash: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Overlay {
    ModelPicker {
        query: String,
        index: usize,
        /// Opened by the Alt+Z enable flow: Enter switches to the picked
        /// ZDR-capable route and turns ZDR on; Esc keeps ZDR off.
        for_zdr: bool,
    },
    LoginPicker {
        query: String,
        index: usize,
    },
    EffortPicker {
        index: usize,
    },
    ModePicker {
        index: usize,
    },
    /// Questions the model asked. The turn is waiting on the answers, so
    /// this overlay owns the keyboard until the queue in `state.questions`
    /// is empty — the questions and the per-question picker state live
    /// there, not on the layer, so several pending asks can be walked.
    QuestionPicker,
    ThemePicker {
        index: usize,
        original_theme: Box<pacode_render::Theme>,
        original_name: String,
        step: ThemePickerStep,
        user_themes: Vec<String>,
    },
    /// Plan + agents on the `Tiny` tier.
    RailOverlay {
        selected: AgentId,
    },
    /// The menu base (§3): the stack of levels plus the UI state of the one on
    /// screen. An empty stack means closed — `keys` restores `Focus::Normal`.
    /// Every central submenu (`MenuId`) lives inside this variant.
    Menu {
        stack: crate::ui::menu::MenuStack,
        ui: crate::ui::menu::MenuUiState,
    },
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
    /// Liveness link to an input token (e.g. `[Image 3]`): once the label is
    /// gone from the input text the toast is dead — a "pasted" line must not
    /// outlive the token it describes.
    pub token_label: Option<String>,
    pub shown_at: Instant,
}

pub const TOAST_TTL_SECS: u64 = 6;
pub const TOAST_MAX: usize = 2;
/// Toast exit window: the strip line fades to `theme.faint` this long before
/// expiry.
pub const TOAST_EXIT_MS: u64 = 800;
/// Overlay/picker unfold duration (the centred grow-in, spec §animation).
pub const OVERLAY_GROW_MS: u64 = 140;
/// Welcome cascade window after attach: the header animates only inside it.
pub const WELCOME_ANIM_MS: u64 = 400;
/// AGENTS ⇄ SESSION switch debounce (spec §5).
pub const IDLE_DEBOUNCE_MS: u64 = 2000;
/// Animation frames in one blink of [`AppState::pulse_lit`], lit for half of
/// them. Tuned for the default `tick_ms` of 66 ms, which puts the blink at
/// ~1 s — the same beat as the `second_parity` fallback, so a pulse does not
/// change speed when the animation clock takes over from the 1 s tick.
pub const PULSE_FRAMES: u64 = 16;
/// Minimum spacing between two `GetSnapshot` resync sends — a burst of
/// dropped daemon events arms one request, not one per detected seq gap.
pub const RESYNC_DEBOUNCE_MS: u64 = 500;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderContext {
    pub theme: pacode_render::Theme,
    pub glyphs: Glyphs,
    pub color: ColorMode,
    pub thinking: bool,
    pub expand_hint: String,
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

fn attach_matches_snapshot(
    attach: &pacode_types::Attach,
    context: &ClientEventContext,
    snapshot: &pacode_types::SessionSnapshot,
) -> bool {
    if context.connection_epoch != 0 && context.session.as_ref() != Some(&snapshot.meta.id) {
        return false;
    }
    match attach {
        pacode_types::Attach::Resume { session } => &snapshot.meta.id == session,
        pacode_types::Attach::New { cwd, .. } | pacode_types::Attach::Latest { cwd } => {
            snapshot.meta.cwd == *cwd
        }
    }
}

fn expand_hint_for(keymap: &crate::binding::Keymap) -> String {
    keymap
        .bindings_for(crate::binding::Action::ExpandOutput)
        .first()
        .map(crate::binding::format_binding)
        .unwrap_or_else(|| "click".to_string())
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

    pub fn agent_is_interesting(&self, agent: &AgentId) -> bool {
        if agent.is_main() {
            return true;
        }
        if matches!(
            self.panel_agent_target(),
            Some(PanelTarget::Agent(ref id)) if id == agent
        ) {
            return true;
        }
        match &self.focus {
            Focus::Panel {
                target: PanelTarget::Agent(id),
                ..
            } => id == agent,
            Focus::SelectAgent { index } => {
                self.selectable_agent_id(*index).as_ref() == Some(agent)
            }
            Focus::Overlay(Overlay::RailOverlay { selected }) => selected == agent,
            _ => false,
        }
    }

    pub fn event_is_interesting(&self, event: &Event) -> bool {
        match event {
            Event::TextDelta { agent, .. } | Event::ReasoningDelta { agent, .. } => {
                self.agent_is_interesting(agent)
            }
            Event::ItemAdded(item) | Event::ItemUpdated(item) => {
                item.agent.is_main()
                    || self.agent_is_interesting(&item.agent)
                    || matches!(&item.kind, TranscriptKind::ToolCall { .. })
            }
            _ => true,
        }
    }

    fn selectable_agent_id(&self, index: usize) -> Option<AgentId> {
        if self.rail.agents.is_empty() {
            return None;
        }
        let has_main = self.rail.agents.iter().any(|agent| agent.id.is_main());
        let sub_index = index.saturating_sub(if has_main { 1 } else { 0 });
        if index == 0 {
            return Some(AgentId::main());
        }
        self.rail
            .agents
            .iter()
            .filter(|agent| !agent.id.is_main())
            .nth(sub_index)
            .map(|agent| agent.id.clone())
    }

    pub fn session_context(&self) -> SessionContext {
        SessionContext {
            generation: self.session.generation,
            deferred_epoch: self.session.deferred_epoch,
            slot: self.active_slot,
            session: self.meta.as_ref().map(|meta| meta.id.clone()),
        }
    }

    pub fn ui_context(&self) -> UiContext {
        UiContext {
            generation: self.session.generation,
            deferred_epoch: self.session.deferred_epoch,
            slot: self.active_slot,
        }
    }

    pub fn ui_context_is_current(&self, context: &UiContext) -> bool {
        context.generation == self.session.generation
            && context.deferred_epoch == self.session.deferred_epoch
            && context.slot == self.active_slot
    }

    pub fn pending_attach_token(&self) -> Option<u64> {
        self.pending_attach.as_ref().map(|pending| pending.token)
    }

    pub fn set_pending_attach_client_epoch(&mut self, epoch: u64) -> Option<u64> {
        let pending = self.pending_attach.as_mut()?;
        pending.set_client_session_epoch(epoch);
        Some(pending.token)
    }

    pub fn pending_attach_epoch_signal(&self) -> Option<Arc<AtomicU64>> {
        self.pending_attach
            .as_ref()
            .map(PendingAttach::epoch_signal)
    }

    pub fn begin_global_refresh(&mut self, count: usize) -> bool {
        if count == 0 || self.global_refresh_in_flight {
            return false;
        }
        self.global_refresh_in_flight = true;
        self.global_refresh_remaining = count;
        true
    }

    pub fn finish_global_refresh_request(&mut self) {
        if !self.global_refresh_in_flight {
            return;
        }
        self.global_refresh_remaining = self.global_refresh_remaining.saturating_sub(1);
        if self.global_refresh_remaining == 0 {
            self.global_refresh_in_flight = false;
        }
    }

    pub fn cancel_global_refresh(&mut self) {
        self.global_refresh_in_flight = false;
        self.global_refresh_remaining = 0;
        self.global_refresh_context = None;
    }

    pub fn global_refresh_in_flight(&self) -> bool {
        self.global_refresh_in_flight
    }

    pub fn global_refresh_context_is_current(&self, context: UiContext) -> bool {
        self.global_refresh_context == Some(context)
    }

    pub fn record_global_refresh_context(&mut self, context: UiContext) {
        self.global_refresh_context = Some(context);
    }

    pub fn take_reconnect_snapshot_boundary(&mut self) -> bool {
        std::mem::take(&mut self.reconnect_snapshot_pending)
    }

    fn cancel_resync_for_attach(&mut self) {
        self.resync_want = false;
        self.resync_from_prompt = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
    }

    pub fn clear_pending_attach(&mut self, reason: String) {
        self.pending_attach = None;
        self.attach_in_flight = false;
        self.event_session_epoch = 0;
        self.awaiting_snapshot = false;
        self.snapshot_rebase_allowed = false;
        self.reconnect_snapshot_pending = false;
        self.resync_want = false;
        self.resync_from_prompt = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
        self.cancel_global_refresh();
        self.session.clear_buffered_events();
        self.connection = Connection::Disconnected { reason };
        self.dirty = true;
    }

    pub fn request_attach_retry(&mut self) -> bool {
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
            return false;
        }
        self.cancel_resync_for_attach();
        if self.attach_in_flight {
            return false;
        }
        let Some(pending) = self.pending_attach.as_mut() else {
            return false;
        };
        if pending.retry_count >= 2 {
            pending.retry_requested = false;
            return false;
        }
        pending.retry_count += 1;
        pending.retry_requested = true;
        true
    }

    pub fn take_pending_attach_retry(&mut self) -> Option<PendingAttach> {
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
            return None;
        }
        self.cancel_resync_for_attach();
        if self.attach_in_flight {
            return None;
        }
        if !self
            .pending_attach
            .as_ref()
            .is_some_and(|pending| pending.retry_requested)
        {
            return None;
        }
        let client_connected = matches!(self.connection, Connection::Connected)
            || (matches!(self.connection, Connection::Connecting)
                && self.event_connection_epoch != 0);
        if !client_connected {
            if !matches!(self.connection, Connection::Connecting)
                && let Some(pending) = self.pending_attach.as_mut()
            {
                pending.retry_requested = false;
            }
            return None;
        }
        self.pending_attach.as_mut()?.retry_requested = false;
        self.attach_in_flight = true;
        self.pending_attach.clone()
    }

    pub fn context_is_current(&self, context: &SessionContext) -> bool {
        if self.meta.is_none() {
            return context.slot == self.active_slot
                && self
                    .pending_attach
                    .as_ref()
                    .is_some_and(|pending| pending.context == *context);
        }
        context.matches(
            self.session.generation,
            self.session.deferred_epoch,
            self.active_slot,
            self.meta.as_ref().map(|meta| &meta.id),
        )
    }

    pub fn observe_connection_context(&mut self, context: &ClientEventContext) -> bool {
        if context.connection_epoch == 0 {
            return true;
        }
        if context.connection_epoch < self.event_connection_epoch {
            return false;
        }
        if context.connection_epoch > self.event_connection_epoch {
            let initial_connection = self.event_connection_epoch == 0;
            self.event_connection_epoch = context.connection_epoch;
            self.session.clear_buffered_events();
            if !initial_connection {
                self.session.advance_deferred_epoch();
                self.session.cancel_deferred_work();
            }
            self.awaiting_snapshot = true;
            self.snapshot_rebase_allowed = false;
            self.resync_want = false;
            self.resync_from_prompt = false;
            self.resync_in_flight = false;
            self.resync_sent_at = None;
            if self.pending_attach.is_none() {
                self.attach_in_flight = false;
            }
            if !initial_connection {
                let attach_token = self
                    .pending_attach
                    .as_ref()
                    .map(|_| self.session.allocate_attach_token());
                if let (Some(token), Some(pending)) = (attach_token, &mut self.pending_attach) {
                    pending.token = token;
                    pending.retry_requested = true;
                }
            }
            self.reconnect_snapshot_pending =
                !initial_connection && self.meta.is_some() && self.pending_attach.is_none();
        }
        true
    }

    pub fn client_context_matches_current_session(&self, context: &ClientEventContext) -> bool {
        context.connection_epoch != 0
            && context.connection_epoch == self.event_connection_epoch
            && context.session_epoch == self.event_session_epoch
            && context.session.as_ref() == self.meta.as_ref().map(|meta| &meta.id)
    }

    pub fn event_context_is_current(&self, context: &ClientEventContext) -> bool {
        (context.connection_epoch == 0 && self.meta.is_none() && self.pending_attach.is_none())
            || (!self.awaiting_snapshot && self.client_context_matches_current_session(context))
    }

    fn event_context_can_buffer(&self, context: &ClientEventContext) -> bool {
        if context.connection_epoch == 0 || context.connection_epoch != self.event_connection_epoch
        {
            return false;
        }
        if let Some(pending) = &self.pending_attach {
            if context.session_epoch != pending.client_session_epoch() {
                return false;
            }
            return match &pending.attach {
                pacode_types::Attach::Resume { session } => {
                    context.session.as_ref() == Some(session)
                }
                pacode_types::Attach::New { .. } | pacode_types::Attach::Latest { .. } => {
                    context.session.is_some()
                }
            };
        }
        self.meta.as_ref().is_some_and(|meta| {
            context.session.as_ref() == Some(&meta.id)
                && context.session_epoch == self.event_session_epoch
        })
    }

    fn apply_tracked_session_event(&mut self, seq: u64, event: Event, now: Instant) -> bool {
        if let Some(last) = self.last_event_seq
            && seq <= last
        {
            return false;
        }
        if self
            .last_event_seq
            .is_some_and(|last| seq != last.wrapping_add(1))
        {
            self.resync_want = true;
        }
        self.last_event_seq = Some(seq);
        self.observe_background_completion(&event);
        self.apply_event(seq, event, now);
        self.dirty = true;
        true
    }

    fn replay_buffered_session_events(&mut self, now: Instant) {
        let expected = ClientEventContext {
            connection_epoch: self.event_connection_epoch,
            session_epoch: self.event_session_epoch,
            session: self.meta.as_ref().map(|meta| meta.id.clone()),
        };
        let (mut buffered, overflowed) = self.session.take_buffered_events();
        buffered.sort_by_key(|buffered| (buffered.first_seq, buffered.seq));
        for buffered in buffered {
            if buffered.context != expected {
                continue;
            }
            if self.last_event_seq.is_some_and(|last| buffered.seq <= last) {
                continue;
            }
            if let Some(event) = buffered.event {
                match self.last_event_seq {
                    None => {
                        self.last_event_seq = Some(buffered.seq);
                        self.observe_background_completion(&event);
                        self.apply_event(buffered.seq, event, now);
                        self.dirty = true;
                    }
                    Some(last) if buffered.first_seq <= last => {
                        self.last_event_seq = Some(buffered.seq);
                        self.resync_want = true;
                    }
                    Some(last) if buffered.first_seq == last.wrapping_add(1) => {
                        self.last_event_seq = Some(buffered.seq);
                        self.observe_background_completion(&event);
                        self.apply_event(buffered.seq, event, now);
                        self.dirty = true;
                    }
                    Some(_) => {
                        self.apply_tracked_session_event(buffered.seq, event, now);
                    }
                }
            } else {
                self.last_event_seq = Some(buffered.seq);
            }
        }
        if overflowed {
            self.resync_want = true;
            self.dirty = true;
        }
    }

    fn invalidate_pending_attach_after_disconnect(&mut self) {
        let Some(token) = self
            .pending_attach
            .as_ref()
            .map(|_| self.session.allocate_attach_token())
        else {
            self.attach_in_flight = false;
            return;
        };
        if let Some(pending) = &mut self.pending_attach {
            pending.token = token;
            pending.retry_requested = true;
        }
        self.attach_in_flight = false;
    }

    pub fn accepts_event_snapshot(
        &mut self,
        context: &ClientEventContext,
        snapshot: &pacode_types::SessionSnapshot,
    ) -> bool {
        if context.connection_epoch == 0 {
            if self.pending_attach.is_some() {
                return false;
            }
            return snapshot.seq >= self.last_event_seq.unwrap_or(0)
                && self
                    .meta
                    .as_ref()
                    .is_none_or(|meta| meta.id == snapshot.meta.id);
        }
        if !self.observe_connection_context(context) {
            return false;
        }
        if !self.awaiting_snapshot && snapshot.seq < self.last_event_seq.unwrap_or(0) {
            return false;
        }
        if let Some(pending) = &self.pending_attach {
            if pending.context.generation != self.session.generation
                || pending.context.slot != self.active_slot
                || context.session_epoch != pending.client_session_epoch()
                || !attach_matches_snapshot(&pending.attach, context, snapshot)
            {
                return false;
            }
            self.cancel_resync_for_attach();
            self.session.fail_in_flight_user_messages();
            self.pending_attach = None;
            self.attach_in_flight = false;

            self.accept_attach_context(context);
            return true;
        }
        let Some(meta) = &self.meta else {
            return false;
        };
        if context.session.as_ref() != Some(&meta.id)
            || context.session_epoch != self.event_session_epoch
        {
            return false;
        }
        self.accept_attach_context(context);
        true
    }

    pub fn accept_attach_context(&mut self, context: &ClientEventContext) {
        if context.connection_epoch != 0 {
            self.event_connection_epoch = context.connection_epoch;
        }
        self.event_session_epoch = context.session_epoch;
        self.awaiting_snapshot = false;
        self.snapshot_rebase_allowed = true;
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
        }
    }

    pub fn begin_attach(&mut self, attach: pacode_types::Attach) -> SessionContext {
        let context = self.session_context();
        let expected_epoch = self.event_session_epoch.wrapping_add(1);
        let token = self.session.allocate_attach_token();
        self.cancel_global_refresh();
        self.reconnect_snapshot_pending = false;
        self.session.clear_buffered_events();
        self.event_session_epoch = 0;
        self.awaiting_snapshot = true;
        self.snapshot_rebase_allowed = false;
        self.resync_want = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
        self.attach_in_flight = false;
        self.pending_attach = Some(PendingAttach {
            context: context.clone(),
            attach,
            token,
            client_session_epoch: Arc::new(AtomicU64::new(expected_epoch)),
            retry_requested: false,
            retry_count: 0,
        });
        context
    }

    pub fn accepts_attach(
        &self,
        context: &SessionContext,
        client_context: &ClientEventContext,
        attach: &pacode_types::Attach,
        snapshot: &pacode_types::SessionSnapshot,
    ) -> bool {
        self.accepts_attach_with_token(
            context,
            self.pending_attach_token().unwrap_or_default(),
            client_context,
            attach,
            snapshot,
        )
    }

    pub fn accepts_attach_with_token(
        &self,
        context: &SessionContext,
        token: u64,
        client_context: &ClientEventContext,
        attach: &pacode_types::Attach,
        snapshot: &pacode_types::SessionSnapshot,
    ) -> bool {
        if !self.context_is_current(context) {
            return false;
        }
        let Some(pending) = &self.pending_attach else {
            return self
                .meta
                .as_ref()
                .is_some_and(|meta| meta.id == snapshot.meta.id)
                && attach_matches_snapshot(attach, client_context, snapshot);
        };
        if pending.token != token
            || pending.context != *context
            || pending.attach != *attach
            || !attach_matches_snapshot(attach, client_context, snapshot)
        {
            return false;
        }
        if client_context.connection_epoch != 0 {
            if client_context.connection_epoch < self.event_connection_epoch {
                return false;
            }
            if client_context.session_epoch != pending.client_session_epoch() {
                return false;
            }
        }
        true
    }

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
            expand_hint: expand_hint_for(&keymap),
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
        }
    }

    pub fn render_options(&self, width: u16) -> RenderOptions {
        RenderOptions::with_theme_color(
            width,
            self.config.ui.ascii_only,
            self.theme.clone(),
            self.color_mode,
        )
        .thinking(self.config.ui.thinking)
        .expand_hint(expand_hint_for(&self.keymap))
        .cache_epoch(self.render_epoch)
    }

    pub fn sync_render_context(&mut self) {
        let next = RenderContext {
            theme: self.theme.clone(),
            glyphs: Glyphs::new(self.config.ui.ascii_only),
            color: self.color_mode,
            thinking: self.config.ui.thinking,
            expand_hint: expand_hint_for(&self.keymap),
        };
        if self.render_context == next {
            return;
        }
        self.render_context = next;
        self.render_epoch = self.render_epoch.wrapping_add(1);
        let truecolor = self.truecolor;
        if let Some(header) = &mut self.transcript.header {
            header.truecolor = truecolor;
        }
        self.transcript.cache.clear();
        self.panel.agent_transcript.cache.clear();
        if let Some(diff) = &mut self.panel.diff {
            diff.rendered = None;
        }
        if let Some(diff) = &mut self.git.diff {
            diff.rendered = None;
        }
    }

    pub fn rebuild_theme(&mut self) {
        self.color_mode = ColorMode::from_setting(&self.config.ui.color, self.detected_color_mode);
        self.truecolor = self.color_mode == ColorMode::Truecolor;
        let (palette, _) = pacode_config::theme::load_theme(&self.paths, &self.config.theme);
        self.theme = pacode_render::Theme::from_palette(&palette, self.truecolor);
    }

    pub fn apply_runtime_config(&mut self, key: &str) {
        if key == "ui.color" || key.starts_with("theme.") {
            self.rebuild_theme();
        }
        if key == "ui.transcript_cells" {
            let max_cells = self.config.ui.transcript_cells;
            self.transcript.set_max_cells(max_cells);
            self.panel.agent_transcript.set_max_cells(max_cells);
        }
        self.sync_render_context();
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

        // The main agent is idle — plain idle, whoever else is still running.
        // Live subagents and background tasks keep their rail entries and card
        // animations; they are just not the activity line's business.
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
        let text = self.input.text.clone();
        self.pasted_images.cleanup_unreferenced(&text);
    }

    /// Records whose `[Image N]` token is still referenced by the current
    /// input. Submitted records are excluded: they stay on disk for the
    /// daemon but the preview strip no longer shows them.
    pub fn live_pasted_images(&self) -> Vec<&crate::clipboard_read::PastedImageRecord> {
        self.pasted_images
            .records()
            .iter()
            .filter(|r| !r.submitted && self.input.text.contains(r.token_label().as_str()))
            .collect()
    }

    /// Preview strip height in terminal rows: 0 when images are disabled or
    /// no live `[Image N]` token remains in the input.
    pub fn image_strip_rows(&self) -> u16 {
        if self.config.ui.images_enabled() && !self.live_pasted_images().is_empty() {
            images::STRIP_ROWS.min(images::STRIP_MAX_ROWS)
        } else {
            0
        }
    }

    fn apply_attach_failed(
        &mut self,
        context: &ClientEventContext,
        attach: &pacode_types::Attach,
        reason: String,
    ) -> bool {
        if !self.observe_connection_context(context) {
            return false;
        }
        let relevant = self.pending_attach.as_ref().map_or_else(
            || {
                self.meta.as_ref().is_some_and(|meta| {
                    matches!(attach, pacode_types::Attach::Resume { session } if session == &meta.id)
                })
            },
            |pending| {
                let target_matches = match (&pending.attach, attach) {
                    (
                        pacode_types::Attach::New { .. } | pacode_types::Attach::Latest { .. },
                        pacode_types::Attach::Resume { .. },
                    ) => true,
                    _ => &pending.attach == attach,
                };
                target_matches
                    && (context.connection_epoch == 0
                        || (context.connection_epoch == self.event_connection_epoch
                            && context.session_epoch == pending.client_session_epoch()))
            },
        );
        if !relevant {
            return false;
        }

        self.clear_pending_attach(reason.clone());
        self.push_notice_with_level(ToastLevel::Error, format!("attach failed: {reason}"));
        self.attach_error = Some(pacode_client::ClientError::Daemon(reason));
        self.quit = true;
        true
    }

    /// Fold a client event into the state (spec §5 state table, §7 follow rules).
    /// Returns true when a redraw is needed (almost always).
    pub fn apply_client_event(&mut self, event: ClientEvent, now: Instant) -> bool {
        match event {
            ClientEvent::Connected { context, .. } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                self.awaiting_snapshot = self.awaiting_snapshot
                    || self.meta.is_none()
                    || self.reconnect_snapshot_pending;
                self.connection = if self.meta.is_some() {
                    Connection::Connected
                } else {
                    Connection::Connecting
                };
                self.dirty = true;
                true
            }
            ClientEvent::Disconnected { context, reason } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                self.cancel_global_refresh();
                self.reconnect_snapshot_pending = false;
                self.session.advance_deferred_epoch();
                self.session.cancel_deferred_work();
                self.session.fail_in_flight_user_messages();
                self.session.drop_pending_detach_turns();
                self.session.clear_buffered_events();
                self.invalidate_pending_attach_after_disconnect();
                self.resync_want = false;
                self.lists.git.in_flight = false;
                self.awaiting_snapshot = true;
                self.snapshot_rebase_allowed = false;
                self.connection = Connection::Disconnected { reason };
                self.dirty = true;
                true
            }
            ClientEvent::Reconnecting { context, attempt } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                self.cancel_global_refresh();
                self.session.fail_in_flight_user_messages();
                self.session.drop_pending_detach_turns();
                self.reconnect_snapshot_pending =
                    self.meta.is_some() && self.pending_attach.is_none();
                self.awaiting_snapshot = true;
                self.snapshot_rebase_allowed = false;
                self.connection = Connection::Reconnecting { attempt };
                self.dirty = true;
                true
            }
            ClientEvent::AttachFailed {
                context,
                attach,
                reason,
            } => self.apply_attach_failed(&context, &attach, reason),
            ClientEvent::Snapshot { context, snapshot } => {
                if !self.accepts_event_snapshot(&context, &snapshot) {
                    return false;
                }
                self.apply_snapshot(snapshot, now)
            }
            ClientEvent::ResyncNeeded { context } => {
                if !self.observe_connection_context(&context)
                    || self.pending_attach.is_some()
                    || !self.client_context_matches_current_session(&context)
                {
                    return false;
                }
                if !self.resync_in_flight
                    && (self.awaiting_snapshot || !matches!(self.connection, Connection::Connected))
                {
                    return false;
                }
                self.resync_want = true;
                self.dirty = true;
                true
            }
            ClientEvent::Event {
                context,
                seq,
                event,
            } => {
                if !self.observe_connection_context(&context) {
                    return false;
                }
                if event.is_global() {
                    self.apply_event(seq, event, now);
                    self.dirty = true;
                    return true;
                }
                if self.awaiting_snapshot {
                    if !self.event_context_can_buffer(&context) {
                        return false;
                    }
                    if self.event_is_interesting(&event) {
                        self.session.buffer_event(context, seq, event);
                    } else {
                        self.session.buffer_filtered_event(context, seq);
                    }
                    if self.resync_in_flight {
                        self.resync_want = true;
                    }
                    return false;
                }
                if !self.event_context_is_current(&context) {
                    return false;
                }
                self.apply_tracked_session_event(seq, event, now)
            }
        }
    }

    pub(crate) fn apply_snapshot(
        &mut self,
        snapshot: pacode_types::SessionSnapshot,
        now: Instant,
    ) -> bool {
        if snapshot.seq < self.last_event_seq.unwrap_or(0) && !self.snapshot_rebase_allowed {
            return false;
        }
        self.snapshot_rebase_allowed = false;
        let retry_resync = self.resync_want && self.resync_in_flight;
        self.connection = Connection::Connected;
        self.last_event_seq = Some(snapshot.seq);
        self.resync_want = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
        if self.pending_attach.is_none() {
            self.attach_in_flight = false;
        }
        if let Some(msg) = pacode_config::take_migration_notice(&self.paths) {
            self.push_toast(ToastLevel::Info, msg, None, now);
        }
        let project_changed = self.cwd() != snapshot.meta.cwd;
        if project_changed {
            self.git = GitState::default();
            self.lists.git = crate::cache::ListHealth::default();
            self.files.clear_preview();
            self.input.at_comp = Default::default();
            self.input.bash_comp = Default::default();
        }
        self.started_at = now;
        self.meta = Some(snapshot.meta.clone());
        self.record_slot(self.active_slot, &snapshot);
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
            if let TranscriptKind::ToolCall {
                name,
                title,
                call_id,
                diff,
                ..
            } = &item.kind
            {
                let arg = crate::state::files::title_path_arg(name, title);
                let input = serde_json::json!({ "path": arg });
                self.files.observe_tool_item(name, &input, item.ts_ms);
                if let Some(diff) = diff {
                    self.files
                        .apply_diff(name, &input, call_id, *diff, item.ts_ms);
                }
            }
        }
        let active_turn = snapshot.active_turn.clone();
        let terminal_turn = snapshot.terminal_turn.clone();
        self.session.reconcile_turn_fence(
            snapshot.turn_active,
            active_turn.as_ref(),
            terminal_turn.as_ref(),
        );
        if snapshot.turn_active {
            if self.turn_started_at.is_none() {
                self.turn_started_at = Some(now);
            }
        } else {
            self.turn_started_at = None;
        }
        self.rail.plan = snapshot.plan;
        self.rail.replace_agents(snapshot.agents);
        self.rail.replace_tasks(snapshot.tasks);
        self.rail.cron_jobs = snapshot.cron_jobs;
        self.rail.monitors = snapshot.monitors;
        self.rail.usage = snapshot.usage;
        let turn_active = self.turn_active;
        self.rail.update_idle(turn_active, now);
        for (i, perm) in snapshot.pending_permissions.into_iter().enumerate() {
            let ts_ms = perm.created_at_ms;
            self.transcript.upsert_permission(
                perm,
                ts_ms,
                snapshot.seq.wrapping_add(1).wrapping_add(i as u64),
            );
        }
        if self.questions.replace(snapshot.pending_questions) {
            self.focus = Focus::Overlay(Overlay::QuestionPicker);
        } else if matches!(self.focus, Focus::Overlay(Overlay::QuestionPicker)) {
            self.focus = Focus::Normal;
        }
        self.awaiting_snapshot = false;
        self.replay_buffered_session_events(now);
        self.resync_want |= retry_resync;
        self.dirty = true;
        true
    }

    /// Fold a daemon event (`ClientEvent::Event`) into transcript/rail/panel/toasts.
    pub fn apply_event(&mut self, seq: u64, event: Event, now: Instant) {
        let main_status = match &event {
            Event::AgentUpdated(info) if info.id.is_main() => Some(info.status),
            _ => None,
        };
        events::apply_event(self, seq, event, now);
        if let Some(status) = main_status {
            self.session.observe_main_agent_status(status, now);
        }
    }

    pub(crate) fn apply_session_status(
        &mut self,
        session: pacode_types::SessionId,
        status: pacode_types::SessionStatusKind,
    ) {
        for slot in self.slots.iter_mut().flatten() {
            if slot.id == session {
                slot.status = status;
                self.dirty = true;
            }
        }
    }

    pub(crate) fn apply_login_progress(
        &mut self,
        provider: String,
        stage: pacode_types::LoginStage,
        now: Instant,
    ) {
        match stage {
            pacode_types::LoginStage::OpenUrl { url, opened } => {
                let detail = if opened {
                    format!("Opened browser to sign in to {provider}: {url}")
                } else {
                    format!("Open this URL in your browser to sign in to {provider}:\n{url}")
                };
                self.push_toast(ToastLevel::Info, "Login".into(), Some(detail), now);
            }
            pacode_types::LoginStage::Waiting => {
                self.push_toast(
                    ToastLevel::Info,
                    "Login".into(),
                    Some(format!("Waiting for authorization from {provider}...")),
                    now,
                );
            }
            pacode_types::LoginStage::Exchanging => {
                self.push_toast(
                    ToastLevel::Info,
                    "Login".into(),
                    Some(format!("Exchanging authorization code for {provider}...")),
                    now,
                );
            }
            pacode_types::LoginStage::Done { label } => {
                self.push_toast(ToastLevel::Success, "Signed in".into(), Some(label), now);
                if matches!(self.focus, Focus::Overlay(Overlay::LoginPicker { .. })) {
                    self.focus = Focus::Normal;
                }
            }
            pacode_types::LoginStage::Failed { message } => {
                self.push_toast(
                    ToastLevel::Error,
                    format!("Login failed: {provider}"),
                    Some(message),
                    now,
                );
                if matches!(self.focus, Focus::Overlay(Overlay::LoginPicker { .. })) {
                    self.focus = Focus::Normal;
                }
            }
        }
        self.dirty = true;
    }

    pub fn push_toast(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        now: Instant,
    ) {
        self.push_toast_inner(level, title, detail, None, now);
    }

    /// `push_toast` plus a liveness link: the toast expires early once
    /// `token_label` leaves the input text (token deleted or submitted).
    pub fn push_toast_linked(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        now: Instant,
        token_label: String,
    ) {
        self.push_toast_inner(level, title, detail, Some(token_label), now);
    }

    fn push_toast_inner(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        token_label: Option<String>,
        now: Instant,
    ) {
        let toast = Toast {
            level,
            title,
            detail,
            token_label,
            shown_at: now,
        };
        self.toasts.push_back(toast);
        while self.toasts.len() > TOAST_MAX {
            self.toasts.pop_front();
        }
        self.dirty = true;
    }

    /// Drop expired toasts and toasts whose linked token left the input;
    /// returns true when something changed.
    pub fn expire_toasts(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        let input_text = self.input.text.clone();
        self.toasts.retain(|t| {
            now.saturating_duration_since(t.shown_at).as_secs() < TOAST_TTL_SECS
                && t.token_label
                    .as_ref()
                    .is_none_or(|label| input_text.contains(label.as_str()))
        });
        let changed = self.toasts.len() != before;
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Whether a toast sits inside its fade-out window, i.e. the fast tick
    /// has visible work. Bounded: the last `TOAST_EXIT_MS` of each toast.
    pub fn toast_needs_anim(&self, now: Instant) -> bool {
        let ttl_ms = TOAST_TTL_SECS * 1000;
        self.toasts.iter().any(|t| {
            let age = now.saturating_duration_since(t.shown_at).as_millis() as u64;
            ttl_ms.saturating_sub(age) < TOAST_EXIT_MS
        })
    }

    /// Whether the welcome cascade is still playing (bounded: first 400 ms
    /// after `started_at`).
    pub fn welcome_playing(&self, now: Instant) -> bool {
        (now.saturating_duration_since(self.started_at).as_millis() as u64) < WELCOME_ANIM_MS
    }

    /// Whether the current overlay/picker is inside its unfold window.
    pub fn overlay_growing(&self, now: Instant) -> bool {
        self.overlay_since.is_some_and(|t| {
            (now.saturating_duration_since(t).as_millis() as u64) < OVERLAY_GROW_MS
        })
    }

    /// Whether a short-lived animation (toast slide/fade, overlay unfold,
    /// welcome cascade) wants the fast tick instead of the ambient 125 ms one.
    pub fn fast_anim_active(&self, now: Instant) -> bool {
        self.toast_needs_anim(now) || self.welcome_playing(now) || self.overlay_growing(now)
    }

    /// The shared two-state blink phase for pulsing dots: driven by
    /// `anim_frame` while the fast anim tick is armed, by `second_parity` when
    /// only the 1 s tick runs. Either way it is a pure function of counters the
    /// existing timers already advance — it never arms a timer of its own.
    pub fn pulse_lit(&self, now: Instant) -> bool {
        if self.turn_active || self.transcript.has_backlog() || self.needs_anim_tick(now) {
            self.anim_frame % PULSE_FRAMES < PULSE_FRAMES / 2
        } else {
            self.second_parity
        }
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

    /// `supported_efforts` of the current route; `None` — route unknown to
    /// the model list or unrestricted — means every [`Effort`] is selectable
    /// (spec §6).
    pub fn supported_efforts(&self) -> Option<&[Effort]> {
        let route = &self.meta.as_ref()?.model;
        self.models
            .iter()
            .find(|m| &m.route == route)?
            .supported_efforts
            .as_deref()
    }

    /// Whether `effort` may be selected on the current route (spec §6).
    pub fn effort_supported(&self, effort: Effort) -> bool {
        self.supported_efforts().is_none_or(|l| l.contains(&effort))
    }

    /// After a route switch: when the session effort is outside `route`'s
    /// supported set, downgrade to the nearest lower supported level, notice
    /// the change, and return the `SetEffort` action to send (spec §6).
    pub fn clamp_effort_actions(&mut self, route: &ModelRoute) -> Vec<crate::keys::Action> {
        let Some(supported) = self
            .models
            .iter()
            .find(|m| &m.route == route)
            .and_then(|m| m.supported_efforts.as_deref())
        else {
            return Vec::new();
        };
        let cur = self.effort();
        if supported.contains(&cur) {
            return Vec::new();
        }
        let Some(next) = nearest_supported_effort(cur, supported) else {
            return Vec::new();
        };
        self.save_pref_effort(next);
        self.push_notice_with_level(
            ToastLevel::Warn,
            format!("effort {cur} → {next}: not supported by {route}"),
        );
        vec![crate::keys::Action::Send(pacode_types::Request::SetEffort(
            next,
        ))]
    }

    pub fn push_notice(&mut self, text: String) {
        let now = now_ms();
        self.transcript.push_cell(Cell {
            id: now,
            kind: CellKind::Item(TranscriptKind::Notice {
                level: ToastLevel::Info,
                text,
            }),
            version: 0,
            ts_ms: now,
            stats: None,
            perm_explain: None,
        });
        self.dirty = true;
    }

    pub fn push_notice_with_level(&mut self, level: ToastLevel, text: String) {
        let now = now_ms();
        self.transcript.push_cell(Cell {
            id: now,
            kind: CellKind::Item(TranscriptKind::Notice { level, text }),
            version: 0,
            ts_ms: now,
            stats: None,
            perm_explain: None,
        });
        self.transcript.scroll_to_bottom();
        self.dirty = true;
    }

    pub fn resync_reply_is_current(
        &self,
        context: &SessionContext,
        client_context: &ClientEventContext,
    ) -> bool {
        self.resync_in_flight
            && self.pending_attach.is_none()
            && matches!(self.connection, Connection::Connected)
            && self.context_is_current(context)
            && self.client_context_matches_current_session(client_context)
    }

    pub fn release_resync_barrier(&mut self, retry: bool, now: Instant) {
        if self.pending_attach.is_some() {
            self.cancel_resync_for_attach();
            self.dirty = true;
            return;
        }
        self.resync_in_flight = false;
        self.awaiting_snapshot = false;
        self.snapshot_rebase_allowed = false;
        self.session.reconcile_turn_boundary(self.turn_active);
        self.replay_buffered_session_events(now);
        self.resync_want |= retry && matches!(self.connection, Connection::Connected);
        if !retry || !matches!(self.connection, Connection::Connected) {
            self.resync_sent_at = None;
        }
        self.dirty = true;
    }

    /// Drain a seq-gap resync want into a `Request::GetSnapshot` — at most
    /// one in flight, and no resend within `RESYNC_DEBOUNCE_MS` of the last
    /// send. While no session is attached or the link is down the want is
    /// dropped instead: the (re)attach snapshot supersedes any resync.
    pub fn take_resync_request(&mut self, now: Instant) -> Option<pacode_types::Request> {
        if !self.resync_want {
            return None;
        }
        if self.pending_attach.is_some() {
            self.cancel_resync_for_attach();
            return None;
        }
        if self.meta.is_none() || !matches!(self.connection, Connection::Connected) {
            self.resync_want = false;
            self.resync_in_flight = false;
            self.resync_sent_at = None;
            return None;
        }
        if self.awaiting_snapshot {
            if self.resync_in_flight || !self.resync_want {
                return None;
            }
            self.awaiting_snapshot = false;
            self.snapshot_rebase_allowed = false;
        }
        if self.resync_in_flight
            || self.resync_sent_at.is_some_and(|t| {
                (now.saturating_duration_since(t).as_millis() as u64) < RESYNC_DEBOUNCE_MS
            })
        {
            return None;
        }
        self.resync_want = false;
        self.resync_from_prompt = false;
        self.session.fail_in_flight_user_messages();
        self.session.drop_pending_detach_turns();
        self.cancel_global_refresh();
        self.session.advance_deferred_epoch();
        self.session.cancel_deferred_work();
        self.awaiting_snapshot = true;
        self.snapshot_rebase_allowed = false;
        self.resync_in_flight = true;
        self.resync_sent_at = Some(now);
        Some(pacode_types::Request::GetSnapshot)
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
                    .task(&info.id)
                    .is_some_and(|task| task.status.is_terminal());
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
                    .agent(&info.id)
                    .is_some_and(|agent| !agent.status.is_live());
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
        self.transcript.push_cell(Cell {
            id: now,
            kind: CellKind::BackgroundResult(result),
            version: 0,
            ts_ms: now,
            stats: None,
            perm_explain: None,
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

    /// Persist the picker `z`/`s` toggles to `prefs.toml`.
    pub fn save_pref_model_flags(&self) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.model_flags = self.model_flags.clone();
        if let Err(e) = pacode_config::save_prefs(&self.paths, &prefs) {
            log::warn!("failed to save prefs (model_flags): {e}");
        }
    }

    /// ZDR badge state: config-forced `always`, or `session` mode with the
    /// toggle on (`meta.zdr` mirrors the daemon via `SessionUpdated`).
    pub fn zdr_active(&self) -> bool {
        zdr_active(&self.config, self.meta.as_ref())
    }

    pub fn is_bottom_picker(&self) -> bool {
        matches!(
            &self.focus,
            Focus::Overlay(Overlay::EffortPicker { .. })
                | Focus::Overlay(Overlay::ModePicker { .. })
                | Focus::Overlay(Overlay::ModelPicker { .. })
                | Focus::Overlay(Overlay::LoginPicker { .. })
                | Focus::Overlay(Overlay::QuestionPicker)
                | Focus::Overlay(Overlay::ThemePicker { .. })
        )
    }

    pub fn bottom_picker_height(&self) -> u16 {
        match &self.focus {
            Focus::Overlay(Overlay::EffortPicker { .. })
            | Focus::Overlay(Overlay::ModePicker { .. }) => 7,
            Focus::Overlay(Overlay::ModelPicker { .. })
            | Focus::Overlay(Overlay::ThemePicker { .. }) => 12,
            Focus::Overlay(Overlay::LoginPicker { .. }) => 13,
            // Header chip + wrapped question + two rows per option + the
            // free-text plaque + hint, capped — the options window scrolls.
            Focus::Overlay(Overlay::QuestionPicker) => self
                .questions
                .current()
                .map(|q| {
                    crate::ui::question_picker::height(q, self.questions.len(), self.picker_width())
                        .min(18) as u16
                })
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// Width the bottom picker will occupy: the dialog column, or the full
    /// screen row when an open panel owns the column (mirrors `ui::draw`).
    fn picker_width(&self) -> usize {
        let tier = crate::layout::WidthTier::for_width(self.cols);
        let rail_w = tier.rail_width();
        let left_w = if rail_w > 0 && self.cols > rail_w + 2 {
            self.cols - rail_w - 2
        } else {
            self.cols
        };
        let panel_open = (self.panel.target.is_some() || matches!(self.focus, Focus::Panel { .. }))
            && !self.agent_replaces_dialog();
        if !panel_open {
            return left_w as usize;
        }
        match tier.dialog_share_with_panel() {
            0 => self.cols as usize,
            share => (left_w.saturating_sub(1) as usize * share as usize / 100).max(1),
        }
    }

    /// Whether a periodic 1 s tick is needed (live agents or tasks: durations
    /// change, and the SESSION-stats debounce has to fire on a quiet screen).
    pub fn needs_second_tick(&self) -> bool {
        self.turn_active
            || self.rail.has_live_agents()
            || self.rail.has_running_tasks()
            || self.rail.idle_debounce_pending()
    }

    /// Whether the animation tick is needed: the turn running, an active panel
    /// agent, a pending permission pulse, a young overlay unfold, the welcome
    /// cascade, or the reconnect dots. Every arm is a visible animation.
    pub fn needs_anim_tick(&self, now: Instant) -> bool {
        if self.turn_active {
            return true;
        }
        if self.welcome_playing(now) || self.overlay_growing(now) {
            return true;
        }
        // A permission request pulses until it is answered; waiting on the
        // reader is activity, so the tick is allowed.
        if self.has_pending_permission() {
            return true;
        }
        // A `NeedsInput` slot badge blinks while it waits — armed only while
        // one exists (an unanswered question is the active slot's own badge).
        if !self.questions.is_empty()
            || self
                .slots
                .iter()
                .flatten()
                .any(|s| s.status == pacode_types::SessionStatusKind::NeedsInput)
        {
            return true;
        }
        if matches!(self.connection, Connection::Reconnecting { .. }) {
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
                    PanelTarget::Task(_)
                    | PanelTarget::McpServer(_)
                    | PanelTarget::FileDiff { .. }
                    | PanelTarget::CommitDiff { .. } => None,
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

    /// Refresh the slot's retained summary from an attach snapshot. The status
    /// is reseeded too: an attach means the session is on screen, so a stale
    /// `Done` clears and live pending/turn state wins.
    pub fn record_slot(&mut self, index: usize, snapshot: &pacode_types::SessionSnapshot) {
        if index < NUM_SLOTS {
            self.slots[index] = Some(SessionSlot {
                id: snapshot.meta.id.clone(),
                title: snapshot.meta.title(),
                turns: snapshot.usage.turns,
                context_tokens: snapshot.usage.context_tokens,
                agents_count: snapshot.agents.len(),
                tasks_count: snapshot.tasks.len(),
                status: crate::state::slots::status_from_snapshot(snapshot),
            });
        }
    }

    /// A pending permission cell still sitting in the transcript — the same
    /// predicate the footer prompt and the anim tick use.
    pub fn has_pending_permission(&self) -> bool {
        self.transcript
            .cells
            .iter()
            .any(|c| matches!(c.kind, CellKind::Item(TranscriptKind::Permission(_))))
    }

    /// The status the ACTIVE slot shows. Derived live: `SessionStatus` events
    /// describe sessions nobody is attached to, while this session's asks and
    /// turn are on screen — `Done` cannot apply to the slot being viewed.
    pub fn active_slot_status(&self) -> pacode_types::SessionStatusKind {
        use pacode_types::SessionStatusKind as Kind;
        if !self.questions.is_empty() || self.has_pending_permission() {
            Kind::NeedsInput
        } else if self.turn_active || self.rail.has_live_agents() {
            Kind::Working
        } else {
            Kind::Idle
        }
    }

    pub fn leave_active_slot(&mut self) {
        let current = self.active_slot;
        // Computed while the leaving session's asks/turn are still live state
        // — and quiet lands on `Idle`, not `Done`: the session was on screen,
        // so there is nothing unseen.
        let status = self.active_slot_status();
        if let Some(meta) = &self.meta {
            self.slots[current] = Some(SessionSlot {
                id: meta.id.clone(),
                title: meta.title(),
                turns: self.rail.usage.turns,
                context_tokens: self.rail.usage.context_tokens,
                agents_count: self.rail.agents.len(),
                tasks_count: self.rail.tasks.len(),
                status,
            });
        }
        self.session.reset(self.config.ui.transcript_cells);
        self.cancel_global_refresh();
        self.reconnect_snapshot_pending = false;
        self.event_session_epoch = 0;
        self.awaiting_snapshot = true;
        self.snapshot_rebase_allowed = false;
        self.pending_image_escapes.clear();
        self.ctrl_c_at = None;
        self.pending_attach = None;
        self.attach_in_flight = false;
        self.lists.git = crate::cache::ListHealth::default();
        self.connection = Connection::Connecting;
        self.dirty = true;
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

    /// The deferred clipboard copy finished — post the toasts draw used to
    /// post inline (§8.1 moved the IO off the frame; the reporting follows it).
    pub fn on_copy_done(&mut self, auto: bool, chars: usize) {
        let detail = auto.then(|| "disable autocopy in /config".to_string());
        self.push_toast(
            ToastLevel::Info,
            format!("copied {chars} chars"),
            detail,
            Instant::now(),
        );
        if crate::clipboard::remote_hint().is_some() && !self.clipboard_warned {
            self.clipboard_warned = true;
            self.push_toast(
                ToastLevel::Warn,
                "remote clipboard".to_string(),
                Some(crate::clipboard::REMOTE_HINT_TEXT.to_string()),
                Instant::now(),
            );
        }
    }
}
