//! Pickers (effort / mode / question / theme at the bottom, the model palette
//! in the centre): effort support and clamping, ZDR, the bottom picker's
//! height, recent routes, and the `prefs.toml` writes the pickers make.

use pacode_types::{Config, Effort, Mode, ModelRoute, SessionMeta, ToastLevel, ZdrMode};

use super::{AppState, Focus, Overlay};

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

/// Recent routes as loaded from `prefs.toml`: the file is user-editable, so
/// duplicates and length are not trusted.
pub(crate) fn capped_recents(loaded: Vec<String>) -> Vec<String> {
    let mut recents: Vec<String> = Vec::new();
    for route in loaded {
        if !recents.contains(&route) {
            recents.push(route);
        }
    }
    recents.truncate(crate::ui::model_palette::RECENT_CAP);
    recents
}

/// Serializes every read-modify-write of `prefs.toml` in this process, so a
/// write on a blocking thread and one on the UI thread cannot drop each
/// other's field.
static PREFS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Load `prefs.toml`, apply `edit`, save it back — under [`PREFS_LOCK`].
fn update_prefs(paths: &pacode_config::Paths, edit: impl FnOnce(&mut pacode_config::Prefs)) {
    let _guard = prefs_lock();
    update_prefs_locked(paths, edit);
}

/// A poisoned lock only means another writer panicked mid-edit; the file on
/// disk is still whole, so the guard is taken over.
fn prefs_lock() -> std::sync::MutexGuard<'static, ()> {
    PREFS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// [`update_prefs`] for a caller already holding [`PREFS_LOCK`].
fn update_prefs_locked(paths: &pacode_config::Paths, edit: impl FnOnce(&mut pacode_config::Prefs)) {
    let mut prefs = pacode_config::load_prefs(paths);
    edit(&mut prefs);
    if let Err(e) = pacode_config::save_prefs(paths, &prefs) {
        log::warn!("failed to save prefs: {e}");
    }
}

/// The global defaults (`prefs.toml`) a session change moves: only the fields
/// that session actually changed, so one session's untouched effort does not
/// overwrite the default another session set. `recent_models` is the whole
/// capped list, set when the daemon accepted a route switch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrefsDelta {
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub mode: Option<Mode>,
    pub recent_models: Option<Vec<String>>,
}

impl PrefsDelta {
    /// The fields `new` changed relative to `old` — empty unless both are the
    /// same session.
    pub fn between(old: &SessionMeta, new: &SessionMeta) -> Self {
        if old.id != new.id {
            return Self::default();
        }
        Self {
            model: (old.model != new.model).then(|| new.model.to_string()),
            effort: (old.effort != new.effort).then_some(new.effort),
            mode: (old.mode != new.mode).then_some(new.mode),
            recent_models: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.effort.is_none()
            && self.mode.is_none()
            && self.recent_models.is_none()
    }

    /// Fold a later change in: its fields win.
    pub fn merge(&mut self, later: Self) {
        self.model = later.model.or(self.model.take());
        self.effort = later.effort.or(self.effort);
        self.mode = later.mode.or(self.mode);
        self.recent_models = later.recent_models.or(self.recent_models.take());
    }

    fn apply_to(self, prefs: &mut pacode_config::Prefs) {
        if let Some(model) = self.model {
            prefs.model = Some(model);
        }
        if let Some(effort) = self.effort {
            prefs.effort = Some(effort);
        }
        if let Some(mode) = self.mode {
            prefs.mode = Some(mode);
        }
        if let Some(recent_models) = self.recent_models {
            prefs.recent_models = recent_models;
        }
    }
}

/// The one pending default change, shared between the event loop (which
/// posts changes in the order they happened) and the blocking tasks that
/// write it. A task takes the delta and writes it under [`PREFS_LOCK`], so
/// whichever task runs first writes the latest merged value and later ones
/// find nothing: a later change can never be overwritten by an earlier one,
/// however the blocking pool orders its tasks.
#[derive(Clone, Default)]
pub struct PrefsOutbox(std::sync::Arc<std::sync::Mutex<PrefsDelta>>);

impl PrefsOutbox {
    fn delta(&self) -> std::sync::MutexGuard<'_, PrefsDelta> {
        // Poisoned only if a merge panicked; the delta is plain data.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Fold a change in, on the event loop, in event order.
    pub fn post(&self, delta: PrefsDelta) {
        self.delta().merge(delta);
    }

    /// Blocking disk IO — the event loop runs it on a blocking thread.
    pub fn flush(&self, paths: &pacode_config::Paths) {
        let _guard = prefs_lock();
        let delta = std::mem::take(&mut *self.delta());
        if delta.is_empty() {
            return;
        }
        update_prefs_locked(paths, |prefs| delta.apply_to(prefs));
    }
}

impl AppState {
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

    /// Switch the session to `route` as one ordered sequence that stops at
    /// the first refusal: `SetModel` when the route changes, `SetEffort` when
    /// `effort` differs from the session's, then `SetZdr(true)` for the Alt+Z
    /// enable flow. Each later step is checked by the daemon against the route
    /// the first one sets, so they must not race. Prefs and recents follow the
    /// daemon's `SessionUpdated`, never this request. Nothing when nothing
    /// would change. The one builder behind `/model <route>` and the palette.
    pub fn model_switch_actions(
        &self,
        route: ModelRoute,
        effort: Option<Effort>,
        enable_zdr: bool,
    ) -> Vec<crate::keys::Action> {
        use pacode_types::Request;
        let meta = self.meta.as_ref();
        let mut requests = Vec::new();
        if meta.is_none_or(|meta| meta.model != route) {
            requests.push(Request::SetModel(route));
        }
        if let Some(effort) = effort.filter(|e| meta.is_none_or(|meta| meta.effort != *e)) {
            requests.push(Request::SetEffort(effort));
        }
        if enable_zdr {
            requests.push(Request::SetZdr(true));
        }
        if requests.is_empty() {
            return vec![];
        }
        vec![crate::keys::prompt_batch_action(super::PromptBatch {
            requests,
        })]
    }

    /// After a route switch: when the session effort is outside `route`'s
    /// supported set, downgrade to the nearest lower supported level, notice
    /// the change, and return the level to send (spec §6).
    pub fn clamp_effort(&mut self, route: &ModelRoute) -> Option<Effort> {
        let supported = self
            .models
            .iter()
            .find(|m| &m.route == route)
            .and_then(|m| m.supported_efforts.as_deref())?;
        let cur = self.effort();
        if supported.contains(&cur) {
            return None;
        }
        let next = nearest_supported_effort(cur, supported)?;
        // Said as a request, not a fact: the daemon may still refuse the
        // switch, and then neither level changes.
        self.push_notice_with_level(
            ToastLevel::Warn,
            format!("{route} does not accept effort {cur}; asking for {next} with the switch"),
        );
        Some(next)
    }

    /// Remember the marketplace `/plugins <source>` pointed at.
    pub fn save_pref_marketplace(&self, source: &str) {
        update_prefs(&self.paths, |prefs| {
            prefs.marketplace = Some(source.to_string())
        });
    }

    /// Move `route` to the front of the recent routes, without duplicates and
    /// capped at [`RECENT_CAP`](crate::ui::model_palette::RECENT_CAP). In
    /// memory only: the `SessionUpdated` prefs write persists it.
    pub fn push_recent_model(&mut self, route: &str) {
        self.recent_models.retain(|known| known != route);
        self.recent_models.insert(0, route.to_string());
        self.recent_models
            .truncate(crate::ui::model_palette::RECENT_CAP);
    }

    /// Persist the picker `z`/`s` toggles to `prefs.toml`.
    pub fn save_pref_model_flags(&self) {
        let flags = self.model_flags.clone();
        update_prefs(&self.paths, |prefs| prefs.model_flags = flags);
    }

    /// The defaults change a `SessionUpdated` asked for, handed to the event
    /// loop to write off the UI thread. `None` when nothing changed.
    pub fn take_prefs_write(&mut self) -> Option<PrefsDelta> {
        let delta = std::mem::take(&mut self.pending_prefs);
        (!delta.is_empty()).then_some(delta)
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
                | Focus::Overlay(Overlay::QuestionPicker)
                | Focus::Overlay(Overlay::ThemePicker { .. })
        )
    }

    pub fn bottom_picker_height(&self) -> u16 {
        match &self.focus {
            Focus::Overlay(Overlay::EffortPicker { .. })
            | Focus::Overlay(Overlay::ModePicker { .. }) => 7,
            Focus::Overlay(Overlay::ThemePicker { .. }) => 12,
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
}
