//! Pickers (effort / mode / question / theme at the bottom, the model palette
//! in the centre): effort support and clamping, ZDR, the bottom picker's
//! height, recent routes, and the `prefs.toml` writes the pickers make.

use pacode_types::{Config, Effort, ModelRoute, SessionMeta, ToastLevel, ZdrMode};

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
        self.push_notice_with_level(
            ToastLevel::Warn,
            format!("effort {cur} → {next}: not supported by {route}"),
        );
        Some(next)
    }

    /// Remember the marketplace `/plugins <source>` pointed at.
    pub fn save_pref_marketplace(&self, source: &str) {
        let mut prefs = pacode_config::load_prefs(&self.paths);
        prefs.marketplace = Some(source.to_string());
        if let Err(e) = pacode_config::save_prefs(&self.paths, &prefs) {
            log::warn!("failed to save prefs (marketplace): {e}");
        }
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
