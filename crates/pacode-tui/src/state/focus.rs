//! Interaction modes and overlays (spec §5): the `Focus` layer stack's types,
//! the panel/agent targeting queries built on them, and the `/analyze` page
//! opener.

use pacode_types::{AgentId, Event, TaskId, TranscriptKind};

use super::AppState;

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
    /// The model picker: providers on the left with live counts, models on the
    /// right with context, price and capabilities, and an effort ladder that
    /// greys out the levels a model does not accept.
    ModelPickerPage {
        picker: crate::ui::model_picker::ModelPicker,
    },
    /// The settings page: sections across the top, `setting → value` rows, a
    /// description line for the highlighted row, and a search that filters
    /// across every section.
    Settings {
        ui: crate::ui::settings::SettingsState,
    },
    /// The `/analyze` page. Reachable only when the client was started with
    /// `--analyze-mode`: the report is a reading of work the daemon did, and
    /// offering it unconditionally would put a scorecard in front of someone
    /// who never asked for one.
    Analyze {
        view: crate::ui::analyze::AnalyzeView,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePickerStep {
    SelectTheme,
    SelectBase,
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

    /// How long the `/analyze` page is and how much of it fits, for the scroll
    /// keys. Built here rather than in the key handler because the row count
    /// needs the report *and* the width, and both live on the state.
    pub fn analyze_geometry(&self) -> (usize, usize) {
        let Some(report) = &self.analyze_report else {
            return (0, 0);
        };
        // The page is a centred overlay; approximating it with the full screen
        // is close enough for a scroll bound and avoids threading a `Rect`
        // through the key handler.
        let area = ratatui::layout::Rect::new(0, 0, self.cols, self.rows);
        let width = usize::from(area.width.saturating_sub(2));
        let rows =
            crate::ui::analyze::rows::build(report, width, &self.render_options(area.width)).len();
        (rows, crate::ui::analyze::visible_rows(area))
    }

    /// Open the `/analyze` page. Gated on `--analyze-mode`, and saying so when
    /// it is off is the whole point: a command that silently does nothing is
    /// worse than one that explains itself.
    pub fn open_analyze(&mut self) {
        if !self.analyze_mode {
            self.push_notice("/analyze needs --analyze-mode at startup".to_string());
            return;
        }
        self.focus = Focus::Overlay(Overlay::Analyze {
            view: crate::ui::analyze::AnalyzeView::default(),
        });
        self.dirty = true;
    }
}
