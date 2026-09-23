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
pub mod slots;
pub mod stats;
pub mod transcript;
pub mod vim;

#[cfg(test)]
mod picker_tests;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use pacode_client::ClientEvent;
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
    RailOverlay,
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

/// A selection copy the draw path prepared for the event loop. `clipboard::copy`
/// writes OSC 52 to stdout and may touch `arboard` — that IO must not run inside
/// `ui::draw` (§8.1), so draw stashes the text here and the loop performs it.
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
    pub meta: Option<SessionMeta>,
    pub connection: Connection,
    pub turn_active: bool,
    /// Main agent transcript.
    pub transcript: Transcript,
    /// Panel transcript (agent) or task output lines.
    pub panel: PanelState,
    pub files: FilesState,
    /// Git history for the VCS submenu (filled by `Reply::GitLog`; T17 renders).
    pub git: GitState,
    /// The scan behind the Import submenu (§5.7): what `pacode-import`
    /// discovered plus each entry's conflict status and the apply target.
    /// Filled by `menu::import::on_open`, rebuilt on every open, cleared by a
    /// successful apply; `None` while the menu has never been opened.
    pub import: Option<crate::ui::menu::import::ImportScan>,
    pub rail: RailState,
    pub input: InputState,
    pub vim: VimState,
    pub focus: Focus,
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
    /// Mouse selection in dialog area.
    pub selection: Selection,
    /// Whether the OSC 52 remote clipboard hint toast has already been shown this session.
    pub clipboard_warned: bool,
    /// `ctrl+c` pressed once (exit on second within 2 s).
    pub ctrl_c_at: Option<Instant>,
    pub quit: bool,
    /// When the main agent turn started (for calculating thinking duration and animations).
    pub turn_started_at: Option<Instant>,
    /// Frame counter for activity animations and spinners; advanced by the
    /// animation tick (paced by `ui.animation`, `tick_ms`/`fast_tick_ms`).
    pub anim_frame: u64,
    /// Mascot shown in the header banner; random per run, fixed for the session.
    pub mascot: crate::ui::mascot::MascotKind,
    /// Whether the terminal takes RGB; decided once from `[ui] color` and the
    /// environment, and reused by the theme and the mascot.
    pub truecolor: bool,
    /// Pending readline key chord (e.g. `ctrl+x` waiting for `ctrl+e`).
    pub pending_chord: Option<crossterm::event::KeyEvent>,
    /// Double-Shift detector state for the menu hub (§4.2).
    pub double_shift: crate::double_shift::DoubleShift,
    /// Session slot table (1..=9).
    pub slots: [Option<SessionSlot>; NUM_SLOTS],
    /// Currently active slot index (0..8).
    pub active_slot: usize,
    /// Questions the model asked and still waits on — several can be open at
    /// once (other agents, replays). The `QuestionPicker` overlay renders the
    /// one under the queue cursor.
    pub questions: PendingQuestions,
    /// Working directory for the session.
    pub cwd: PathBuf,
    /// Cancellation sender for currently running local bash command.
    pub running_bash: Option<tokio::sync::oneshot::Sender<()>>,
    /// Temporary files created for pasted images.
    pub pasted_images: crate::clipboard_read::PastedImages,
    /// Thumbnail cache for the image preview strip above the input.
    pub image_strip: images::ImageStripState,
    /// Escape sequences stashed by `ui::images::draw` and flushed to the
    /// terminal post-draw: (terminal area, escape payload).
    pub pending_image_escapes: Vec<(ratatui::layout::Rect, String)>,
    /// Current activity phase and when it started. The activity line ages from
    /// this instant, so the thinking wording and its colour reset after every
    /// tool call instead of drifting with the whole turn.
    pub phase: Option<(crate::state::activity::Phase, Instant)>,
    /// Age of `phase`, refreshed by the event loop before each draw so the draw
    /// path stays a pure function of state.
    pub phase_elapsed_ms: u64,
    /// When this session's UI came up; drives the welcome cascade in the
    /// header. Re-armed on every snapshot so a re-attach replays the cascade.
    pub started_at: Instant,
    /// Toggles once per second tick; the 1 s blink phase for pulses that run
    /// while only the second tick is armed (idle main, background work alive).
    pub second_parity: bool,
    /// Focus layer seen by the last draw, as a small key (`ui::focus_key`), so
    /// a new overlay can restart its unfold without inspecting the enum again.
    pub last_focus_key: u8,
    /// When the current overlay/picker focus layer opened (render-side clock
    /// for the ~140 ms unfold); `None` while the focus layer is not a popup.
    pub overlay_since: Option<Instant>,
    /// A copy the draw path prepared for the event loop to perform (§8.1 —
    /// `clipboard::copy` does IO and must not run inside `ui::draw`).
    pub pending_copy: Option<PendingCopy>,
    /// Session-bound requests sent while no session was attached — the
    /// startup attach runs as a background request, so the event loop is
    /// already live before the first snapshot lands. Flushed in order when
    /// it does; `dispatch_action` caps the queue and drops the oldest.
    pub pending_requests: VecDeque<pacode_types::Request>,
    /// Seq of the last applied daemon event (`ClientEvent::Event`). A jump
    /// over `last + 1` means the lossy daemon→client pipe dropped events in
    /// between (broadcast lag, a full client queue) and the folded state is
    /// stale until a `GetSnapshot` resync lands — a lost `AgentUpdated`
    /// leaves a subagent "live" in the rail forever. Rebased to
    /// `snapshot.seq` whenever a snapshot is applied.
    pub last_event_seq: Option<u64>,
    /// A seq gap asked for a snapshot resync. The event loop drains this
    /// into ONE debounced `Request::GetSnapshot` via
    /// [`Self::take_resync_request`]; a landing snapshot clears it — any
    /// gap it was armed by is covered by the fresh state.
    pub resync_want: bool,
    /// A `GetSnapshot` resync was sent and no reply landed yet — blocks a
    /// second send until the reply arrives or the request fails.
    pub resync_in_flight: bool,
    /// When the last resync request was sent — rate-limits resends to
    /// `RESYNC_DEBOUNCE_MS`.
    pub resync_sent_at: Option<Instant>,
    /// The `-p`/`--prompt` argv text, carried until the startup attach lands
    /// — `BgResponse::AttachFinished` sends it then.
    pub initial_prompt: Option<String>,
    /// Why the startup attach failed — `run` returns it once the loop exits
    /// (the error also sets `quit`, so a failed attach still exits fast).
    pub attach_error: Option<pacode_client::ClientError>,
}

/// What the panel shows.
pub struct PanelState {
    pub target: Option<PanelTarget>,
    pub agent_transcript: Transcript,
    pub task_lines: Vec<String>,
    pub task_total_lines: u64,
    pub scroll: usize,
    /// The `PanelTarget::FileDiff` payload — written by
    /// `BgResponse::FileDiffReady`, `None` while the probe runs.
    pub diff: Option<PanelDiff>,
}

/// The file-diff payload the panel renders for `PanelTarget::FileDiff`.
/// Owner: `PanelState`.
/// Cap: one patch ≤ `pacode_vcs::DIFF_CAP_BYTES` plus its rendered lines.
/// Invalidation: replaced when the target re-arms or the diff job lands.
pub struct PanelDiff {
    /// The path this diff is for — a response applies only while it is still
    /// the panel's target.
    pub path: String,
    /// `git diff` output — `None` for a clean worktree, a missing file, or a
    /// repo-less path (`note` says which).
    pub patch: Option<String>,
    /// What the probe's `git status` said — feeds the header label.
    pub status: Option<crate::state::files::FileStatus>,
    /// The patch hit the capture cap.
    pub truncated: bool,
    /// Why the patch is missing, or where a synthetic one came from.
    pub note: Option<String>,
    /// `render_diff_ex` output at the width it was rendered for — rebuilt on
    /// resize, never per frame (§8.1: no repeat renders).
    pub rendered: Option<(u16, Vec<ratatui::text::Line<'static>>)>,
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
        let model_flags = pacode_config::load_prefs(&paths).model_flags;
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
                diff: None,
            },
            files: FilesState::default(),
            git: GitState::default(),
            import: None,
            rail: RailState::default(),
            input: InputState::default(),
            vim: VimState::default(),
            focus: Focus::Normal,
            questions: PendingQuestions::default(),
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
            selection: Selection::default(),
            clipboard_warned: false,
            ctrl_c_at: None,
            quit: false,
            turn_started_at: None,
            anim_frame: 0,
            mascot: crate::ui::mascot::MascotKind::random(),
            truecolor,
            pending_chord: None,
            double_shift: crate::double_shift::DoubleShift::default(),
            slots: Default::default(),
            active_slot: 0,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            running_bash: None,
            pasted_images: crate::clipboard_read::PastedImages::new(),
            image_strip: images::ImageStripState::default(),
            pending_image_escapes: Vec::new(),
            phase: None,
            phase_elapsed_ms: 0,
            started_at: Instant::now(),
            second_parity: false,
            last_focus_key: 0,
            overlay_since: None,
            pending_copy: None,
            pending_requests: VecDeque::new(),
            last_event_seq: None,
            resync_want: false,
            resync_in_flight: false,
            resync_sent_at: None,
            initial_prompt: None,
            attach_error: None,
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

    /// Fold a client event into the state (spec §5 state table, §7 follow rules).
    /// Returns true when a redraw is needed (almost always).
    pub fn apply_client_event(&mut self, event: ClientEvent, now: Instant) -> bool {
        match event {
            ClientEvent::Connected { .. } => {
                // While no session is attached — the startup attach is still
                // in flight, or a slot switch is between Detach and Attach —
                // the footer keeps saying "connecting…"; the incoming
                // snapshot is what flips it to Connected.
                self.connection = if self.meta.is_some() {
                    Connection::Connected
                } else {
                    Connection::Connecting
                };
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
                // `snapshot.seq` is the last event folded into the snapshot —
                // the tracker must rebase on it, or the first post-snapshot
                // event looks like a fresh gap and resyncs forever. The landing
                // snapshot also ends any in-flight resync and settles a want
                // armed by a pre-snapshot gap (that gap is now covered).
                self.last_event_seq = Some(snapshot.seq);
                self.resync_want = false;
                self.resync_in_flight = false;
                // One-time config-split migration notice (spec §5): the
                // daemon leaves a marker when it split `config.toml`; the
                // first attach to see it toasts the message.
                if let Some(msg) = pacode_config::take_migration_notice(&self.paths) {
                    self.push_toast(ToastLevel::Info, msg, None, now);
                }
                // Project switch (§8.2): git history, file previews and path
                // completions are all keyed to the session cwd — a different
                // one invalidates them. `self.cwd()` already falls back to the
                // startup cwd before `meta` exists, so this also catches
                // attaching straight into a session living elsewhere.
                // The cache file side needs nothing: the git key carries a
                // cwd fingerprint, so a new project is simply a different file.
                let project_changed = self.cwd() != snapshot.meta.cwd;
                if project_changed {
                    self.git = GitState::default();
                    self.lists.git = crate::cache::ListHealth::default();
                    self.files.clear_preview();
                    self.input.at_comp = Default::default();
                    self.input.bash_comp = Default::default();
                }
                // The header banner is (re)installed below, so the welcome
                // cascade re-arms here: a re-attach replays it.
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
                        // Snapshot items arrive finished — the `DiffStat` is
                        // already on the item, no `ItemUpdated` follows.
                        if let Some(d) = diff {
                            self.files.apply_diff(name, &input, call_id, *d, item.ts_ms);
                        }
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
                    // A pending request also sits in the transcript tail —
                    // keying on the permission id keeps one prompt, not two.
                    self.transcript.upsert_permission(
                        perm,
                        ts_ms,
                        snapshot.seq.wrapping_add(1).wrapping_add(i as u64),
                    );
                }
                // The snapshot is authoritative for the question queue:
                // anything still waiting lands (re-attach must not orphan an
                // ask), everything else is gone. Focus state survives for
                // ids that are still queued.
                if self.questions.replace(snapshot.pending_questions) {
                    self.focus = Focus::Overlay(Overlay::QuestionPicker);
                } else if matches!(self.focus, Focus::Overlay(Overlay::QuestionPicker)) {
                    self.focus = Focus::Normal;
                }
                self.dirty = true;
                true
            }
            ClientEvent::Event { seq, event } => {
                // The daemon→client pipe drops events silently under load
                // (broadcast lag, a full client queue): a jump in `seq` means
                // the folded state is stale — a lost `AgentUpdated` leaves a
                // subagent "live" in the rail forever. Arm ONE debounced
                // `GetSnapshot` resync; skipped pre-attach, where the incoming
                // snapshot is authoritative anyway.
                //
                // `SessionStatus` and `LoginProgress` ride the daemon-wide
                // stream instead — their `seq` is the GLOBAL seq space, so
                // folding them into the per-session tracker would forge a gap
                // and resync on every other session's turn boundary.
                let session_stream = !matches!(
                    event,
                    Event::SessionStatus { .. } | Event::LoginProgress { .. }
                );
                if session_stream {
                    if let Some(last) = self.last_event_seq
                        && seq != last.wrapping_add(1)
                        && self.meta.is_some()
                    {
                        self.resync_want = true;
                    }
                    self.last_event_seq = Some(seq);
                }
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
        self.toasts.retain(|t| {
            now.saturating_duration_since(t.shown_at).as_secs() < TOAST_TTL_SECS
                && t.token_label
                    .as_ref()
                    .is_none_or(|label| self.input.text.contains(label.as_str()))
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

    /// Drain a seq-gap resync want into a `Request::GetSnapshot` — at most
    /// one in flight, and no resend within `RESYNC_DEBOUNCE_MS` of the last
    /// send. While no session is attached or the link is down the want is
    /// dropped instead: the (re)attach snapshot supersedes any resync.
    pub fn take_resync_request(&mut self, now: Instant) -> Option<pacode_types::Request> {
        if !self.resync_want {
            return None;
        }
        if self.meta.is_none() || !matches!(self.connection, Connection::Connected) {
            self.resync_want = false;
            return None;
        }
        if self.resync_in_flight
            || self.resync_sent_at.is_some_and(|t| {
                (now.saturating_duration_since(t).as_millis() as u64) < RESYNC_DEBOUNCE_MS
            })
        {
            return None;
        }
        self.resync_want = false;
        self.resync_in_flight = true;
        self.resync_sent_at = Some(now);
        Some(pacode_types::Request::GetSnapshot)
    }

    /// Drain the first prompt from the queue if the main agent is idle.
    pub fn drain_prompt_queue(&mut self) -> Option<pacode_types::Request> {
        if !self.turn_active && !self.input.prompt_queue.is_empty() {
            let text = self.input.prompt_queue.pop_front()?;
            self.turn_active = true;
            self.dirty = true;
            Some(pacode_types::Request::UserMessage {
                text,
                images: Vec::new(),
            })
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

    /// Whether a periodic 1 s tick is needed (live agents or tasks: durations change).
    pub fn needs_second_tick(&self) -> bool {
        self.turn_active || self.rail.has_live_agents() || self.rail.has_running_tasks()
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
        // The git list belongs to the session's project — leaving the slot
        // must not show it under another session (§8.2).
        self.git = GitState::default();
        self.lists.git = crate::cache::ListHealth::default();
        self.files.clear_preview();
        self.rail.agents.clear();
        self.rail.tasks.clear();
        self.rail.plan = pacode_types::state::Plan::default();
        self.rail.usage = pacode_types::state::UsageTotals::default();
        // The next attach starts a fresh seq space and lands its own
        // snapshot — no gap tracking or resync state carries over.
        self.last_event_seq = None;
        self.resync_want = false;
        self.resync_in_flight = false;
        self.resync_sent_at = None;
    }

    /// Drop a cache file (§8.2 invalidation points: plugin install/remove,
    /// MCP config change). Best-effort on a blocking thread — the next list
    /// reply rewrites the file anyway.
    pub fn invalidate_cache(&self, kind: crate::cache::ListKind) {
        let cache = self.cache.clone();
        let stem = kind.stem(&self.cwd()).into_owned();
        let schema = kind.schema();
        tokio::task::spawn_blocking(move || {
            cache.invalidate(&stem, schema);
        });
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
