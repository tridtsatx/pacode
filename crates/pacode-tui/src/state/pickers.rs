//! Bottom pickers (model / effort / mode / login / question / theme): row
//! filtering, effort support and clamping, ZDR, the picker's height, and the
//! `prefs.toml` writes the pickers make.

use std::collections::HashMap;

use pacode_types::{
    Config, Effort, Mode, ModelFlags, ModelInfo, ModelRoute, SessionMeta, ToastLevel, ZdrMode,
};

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

/// Serializes every read-modify-write of `prefs.toml` in this process, so a
/// write on a blocking thread and one on the UI thread cannot drop each
/// other's field.
static PREFS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Load `prefs.toml`, apply `edit`, save it back — under [`PREFS_LOCK`].
fn update_prefs(paths: &pacode_config::Paths, edit: impl FnOnce(&mut pacode_config::Prefs)) {
    // A poisoned lock only means another writer panicked mid-edit; the file
    // on disk is still whole, so the guard is taken over.
    let _guard = PREFS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut prefs = pacode_config::load_prefs(paths);
    edit(&mut prefs);
    if let Err(e) = pacode_config::save_prefs(paths, &prefs) {
        log::warn!("failed to save prefs: {e}");
    }
}

/// The global defaults (`prefs.toml`) a session change moves: only the fields
/// that session actually changed, so one session's untouched effort does not
/// overwrite the default another session set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrefsDelta {
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub mode: Option<Mode>,
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
        }
    }

    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none() && self.mode.is_none()
    }

    /// Fold a later change in: its fields win.
    pub fn merge(&mut self, later: Self) {
        self.model = later.model.or(self.model.take());
        self.effort = later.effort.or(self.effort);
        self.mode = later.mode.or(self.mode);
    }

    /// Blocking disk IO — the event loop runs it on a blocking thread.
    pub fn save(self, paths: &pacode_config::Paths) {
        update_prefs(paths, |prefs| {
            if let Some(model) = self.model {
                prefs.model = Some(model);
            }
            if let Some(effort) = self.effort {
                prefs.effort = Some(effort);
            }
            if let Some(mode) = self.mode {
                prefs.mode = Some(mode);
            }
        });
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

    /// Switch the session to `route`, as one ordered sequence: `SetModel`,
    /// then — when the session effort is outside `route`'s supported set — the
    /// downgrade to the nearest lower supported level (spec §6), then, for the
    /// Alt+Z enable flow, `SetZdr(true)`. Each later step is checked by the
    /// daemon against the route the first one sets, so they must not race.
    pub fn model_switch_action(
        &mut self,
        route: ModelRoute,
        enable_zdr: bool,
    ) -> crate::keys::Action {
        use pacode_types::Request;
        let mut requests = vec![Request::SetModel(route.clone())];
        if let Some(effort) = self.clamp_effort(&route) {
            requests.push(Request::SetEffort(effort));
        }
        if enable_zdr {
            requests.push(Request::SetZdr(true));
        }
        crate::keys::Action::SendSequence { requests }
    }

    /// The effort to downgrade to when the session effort is outside
    /// `route`'s supported set; notices the change.
    fn clamp_effort(&mut self, route: &ModelRoute) -> Option<Effort> {
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
        self.save_pref_effort(next);
        self.push_notice_with_level(
            ToastLevel::Warn,
            format!("effort {cur} → {next}: not supported by {route}"),
        );
        Some(next)
    }

    /// Remember the marketplace `/plugins <source>` pointed at.
    pub fn save_pref_marketplace(&self, source: &str) {
        update_prefs(&self.paths, |prefs| {
            prefs.marketplace = Some(source.to_string())
        });
    }

    pub fn save_pref_model(&self, model: &str) {
        update_prefs(&self.paths, |prefs| prefs.model = Some(model.to_string()));
    }

    pub fn save_pref_effort(&self, effort: Effort) {
        update_prefs(&self.paths, |prefs| prefs.effort = Some(effort));
    }

    pub fn save_pref_mode(&self, mode: Mode) {
        update_prefs(&self.paths, |prefs| prefs.mode = Some(mode));
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
}
