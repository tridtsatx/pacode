//! Two-column model picker: providers on the left with live counts, models on
//! the right with context window, price and capability tags, a `Kind:` filter
//! row, a search line, a detail line for the highlighted model and a hint
//! footer.
//!
//! ```text
//!  Switch Model
//!  > sonnet
//!  Kind: all · reasoning · vision
//!    all        12
//!  > anthropic  Claude Opus 5    200k ctx · $5.00/$25.00 per M · R V E
//!    openai     GPT-5.1 Codex    400k ctx · — · R
//!  detail anthropic/claude-opus-5 · efforts low/medium/high
//!  effort  >low      medium  high    xhigh   max
//!  ↑/↓ model · ←/→ provider · tab kind · h/l effort · enter switch · esc cancel
//! ```
//!
//! [`model_picker_view`](crate::ui::model_picker_view) draws this; the state
//! and the effort rules live here.
//!
//! # Efforts
//!
//! The effort row lists the whole [`Effort`] ladder, including the levels the
//! highlighted model does not accept. Those are drawn in the theme's `faint`
//! role and cannot be selected — not by Enter, not by a click on the chip —
//! and the cycle skips them: from `low` with `medium` and `xhigh` unsupported
//! the next step is `high`, the next *available* rung, not the rung right of
//! `low`. From the last available level the cycle wraps to the first
//! *available* one, never to the first rung of the ladder.
//!
//! [`ModelInfo::supported_efforts`] of `None` means the catalog said nothing
//! about the model, which is not the same as "no effort works": such a model
//! keeps every level selectable and cycles through the full ladder. Only a
//! catalog that enumerated levels — opencode's `reasoning_options`, the ChatGPT
//! backend's `supported_reasoning_levels`, a providers.toml `efforts` list —
//! narrows the row, because an invented "unsupported" list would grey out a
//! level that works.

use std::collections::{BTreeMap, HashMap};

use ratatui::layout::Rect;

use pacode_types::{Effort, ModelFlags, ModelInfo, Pricing};

/// Per-model flags, keyed by `provider/model`.
pub type Flags = HashMap<String, ModelFlags>;

/// The effort row's label, and the width of one chip. Chips keep their slot
/// whatever the model supports, so a click coordinate and the muscle memory
/// behind it survive a move to a model with a narrower ladder.
pub(crate) const CHIP_LABEL: &str = "effort  ";
pub(crate) const CHIP_WIDTH: usize = 9;

/// Shown wherever the catalog carries no value: an unknown price is never
/// rendered as a number, and an unknown window is never guessed.
const UNKNOWN: &str = "—";

/// The `Kind:` filter row. `All` keeps every model; the others keep the ones
/// whose catalog entry claims the capability.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    All,
    Reasoning,
    Vision,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::All, Kind::Reasoning, Kind::Vision];

    pub fn label(self) -> &'static str {
        match self {
            Kind::All => "all",
            Kind::Reasoning => "reasoning",
            Kind::Vision => "vision",
        }
    }

    /// The next filter in the row, wrapping at the end.
    pub fn next(self) -> Kind {
        let idx = Kind::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Kind::ALL[(idx + 1) % Kind::ALL.len()]
    }

    fn keeps(self, model: &ModelInfo) -> bool {
        match self {
            Kind::All => true,
            Kind::Reasoning => model.supports_reasoning,
            Kind::Vision => model.supports_vision,
        }
    }
}

/// One row of the left column: a provider, or the `all` pseudo-provider that
/// keeps every provider in one list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRow {
    /// `None` is the `all` row, always first.
    pub key: Option<String>,
    /// Models of this provider that survive the search, the `Kind:` filter
    /// and the ZDR gate — the count is live, not the catalog's total.
    pub count: usize,
}

/// The picker's own state. Owned by the app once the overlay is wired; every
/// query here is a pure function of the catalog plus these fields, so the same
/// code answers a keypress, a click and a repaint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelPicker {
    /// Free-text search over route and display name.
    pub query: String,
    /// `None` is the `all` pseudo-provider.
    pub provider: Option<String>,
    pub kind: Kind,
    /// Cursor inside [`ModelPicker::models`].
    pub row: usize,
    /// The effort the user is on. An available level of the highlighted model,
    /// unless the model accepts none at all.
    pub effort: Effort,
    /// Restrict to `zdr_capable` routes: ZDR active, or the Alt+Z enable flow.
    pub zdr_only: bool,
}

impl ModelPicker {
    pub fn new(effort: Effort) -> Self {
        Self {
            effort,
            ..Default::default()
        }
    }

    /// The left column: `all` first, then one row per provider in key order,
    /// each carrying the number of models the current filters leave it.
    pub fn providers(&self, models: &[ModelInfo], flags: &Flags) -> Vec<ProviderRow> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for model in crate::state::picker_models(models, flags, &self.query, self.zdr_only) {
            if self.kind.keeps(model) {
                *counts.entry(model.route.provider.as_str()).or_default() += 1;
            }
        }
        let all = counts.values().sum();
        let mut rows = vec![ProviderRow {
            key: None,
            count: all,
        }];
        rows.extend(counts.into_iter().map(|(key, count)| ProviderRow {
            key: Some(key.to_string()),
            count,
        }));
        rows
    }

    /// The right column: the models of the selected provider that survive the
    /// search, the `Kind:` filter and the ZDR gate, favorites first.
    pub fn models<'a>(&self, models: &'a [ModelInfo], flags: &Flags) -> Vec<&'a ModelInfo> {
        crate::state::picker_models(models, flags, &self.query, self.zdr_only)
            .into_iter()
            .filter(|model| {
                self.provider
                    .as_deref()
                    .is_none_or(|provider| model.route.provider == provider)
            })
            .filter(|model| self.kind.keeps(model))
            .collect()
    }

    /// The model the detail line and the effort row describe.
    pub fn highlighted<'a>(&self, models: &'a [ModelInfo], flags: &Flags) -> Option<&'a ModelInfo> {
        if models.is_empty() {
            return None;
        }
        self.models(models, flags).get(self.row).copied()
    }

    /// Move the model cursor, clamped to the list. Out of range counts (a
    /// catalog that shrank under the cursor) pull the cursor back inside.
    pub fn move_row(&mut self, delta: isize, models: &[ModelInfo], flags: &Flags) {
        let count = self.models(models, flags).len();
        self.row = if count == 0 {
            0
        } else {
            (self.row as isize + delta).clamp(0, count as isize - 1) as usize
        };
        self.resync_effort(models, flags);
    }

    /// Move the provider cursor, wrapping, and rescope the right column.
    pub fn move_provider(&mut self, delta: isize, models: &[ModelInfo], flags: &Flags) {
        let rows = self.providers(models, flags);
        if rows.len() <= 1 {
            return;
        }
        let current = rows
            .iter()
            .position(|row| row.key == self.provider)
            .unwrap_or(0);
        let len = rows.len() as isize;
        let next = (current as isize + delta).rem_euclid(len) as usize;
        self.provider = rows[next].key.clone();
        self.row = 0;
        self.resync_effort(models, flags);
    }

    /// Type into the search. Search text is always text: `z`, `s` and the
    /// mark chords' letters reach the query, as everywhere else in the pickers.
    pub fn push_char(&mut self, ch: char) {
        if self.query.chars().count() >= crate::paste::FILTER_MAX_CHARS {
            return;
        }
        self.query.push(ch);
        self.row = 0;
    }

    /// Backspace: the last character of the search, or the provider scope once
    /// the search is empty — so one key climbs back out of a provider.
    pub fn backspace(&mut self) {
        if self.query.pop().is_none() {
            self.provider = None;
        }
        self.row = 0;
    }

    /// Step the `Kind:` filter. The provider cursor stays put: the filter
    /// narrows the counts beside it, it does not move the selection.
    pub fn cycle_kind(&mut self) {
        self.kind = self.kind.next();
        self.row = 0;
    }

    /// Step the effort to the next level the highlighted model accepts,
    /// skipping the ones it does not and wrapping to the first *accepted*
    /// level. Returns the level landed on, or `None` when the model accepts
    /// none — the caller sends that as the session's new effort.
    pub fn cycle_effort(
        &mut self,
        forward: bool,
        models: &[ModelInfo],
        flags: &Flags,
    ) -> Option<Effort> {
        let model = self.highlighted(models, flags)?;
        let next = if forward {
            next_effort(model, self.effort)
        } else {
            prev_effort(model, self.effort)
        };
        if let Some(next) = next {
            self.effort = next;
        }
        next
    }

    /// Apply an effort picked with Enter or by a click on the effort row. An
    /// unavailable level is refused — `false`, cursor unchanged — so neither
    /// key can land the session on a value the model rejects.
    pub fn select_effort(&mut self, effort: Effort, models: &[ModelInfo], flags: &Flags) -> bool {
        let Some(model) = self.highlighted(models, flags) else {
            return false;
        };
        if !is_effort_available(model, effort) {
            return false;
        }
        self.effort = effort;
        true
    }

    /// Keep the effort on a level the (possibly just changed) highlighted
    /// model accepts, so the row never points at a chip the user cannot pick.
    fn resync_effort(&mut self, models: &[ModelInfo], flags: &Flags) {
        if let Some(model) = self.highlighted(models, flags)
            && let Some(next) = snap_effort(model, self.effort)
        {
            self.effort = next;
        }
    }
}

/// Whether `effort` may be selected on `model`. `None` in the catalog is
/// "unknown", never "nothing works": every level stays selectable.
pub fn is_effort_available(model: &ModelInfo, effort: Effort) -> bool {
    model
        .supported_efforts
        .as_ref()
        .is_none_or(|list| list.contains(&effort))
}

/// The level above `current` in the ladder that `model` accepts, skipping
/// unsupported rungs and wrapping to the first *accepted* one. `None` only
/// when the model accepts nothing at all.
pub fn next_effort(model: &ModelInfo, current: Effort) -> Option<Effort> {
    step_effort(model, current, 1)
}

/// The level below `current`; see [`next_effort`].
pub fn prev_effort(model: &ModelInfo, current: Effort) -> Option<Effort> {
    step_effort(model, current, -1)
}

/// One rung along the ladder in `step` direction, taken only if the model
/// accepts it. The walk is bounded by the ladder, not by the data, so a model
/// that accepts nothing terminates on the first lap instead of spinning.
fn step_effort(model: &ModelInfo, current: Effort, step: isize) -> Option<Effort> {
    let len = Effort::ALL.len() as isize;
    let start = Effort::ALL.iter().position(|e| *e == current)? as isize;
    let mut idx = (start + step).rem_euclid(len);
    for _ in 0..len {
        if is_effort_available(model, Effort::ALL[idx as usize]) {
            return Some(Effort::ALL[idx as usize]);
        }
        idx = (idx + step).rem_euclid(len);
    }
    None
}

/// The level to show for `model` when the user arrives with `current`:
/// `current` itself when the model accepts it, else the nearest accepted level
/// below it, else the lowest accepted one. The same rule the session's
/// post-switch downgrade uses, so a route change lands on the level the picker
/// is already showing. `None` only when the model accepts nothing.
pub fn snap_effort(model: &ModelInfo, current: Effort) -> Option<Effort> {
    if is_effort_available(model, current) {
        return Some(current);
    }
    let start = Effort::ALL.iter().position(|e| *e == current)?;
    Effort::ALL[..start]
        .iter()
        .rev()
        .copied()
        .find(|effort| is_effort_available(model, *effort))
        .or_else(|| {
            Effort::ALL
                .iter()
                .copied()
                .find(|effort| is_effort_available(model, *effort))
        })
}

/// The effort chip under a click at column `x` of the effort row. Chips are
/// fixed width in ladder order, so the mapping holds for any model. `None`
/// left of the chips or past the last one — the row's label and its padding
/// are not clickable, so a stray click cannot clear the effort.
pub fn effort_under(x: u16) -> Option<Effort> {
    let start = CHIP_LABEL.chars().count() as u16;
    if x < start {
        return None;
    }
    Effort::ALL
        .get(usize::from((x - start) / CHIP_WIDTH as u16))
        .copied()
}

/// The effort a click at `(x, y)` inside `area` selects. The effort row is the
/// second line from the bottom of the area, so its hit box keeps the same rows
/// and columns whatever the model list does — the caller only has to hand over
/// the click coordinates.
pub fn effort_at(area: Rect, x: u16, y: u16) -> Option<Effort> {
    let row = area.y + area.height.saturating_sub(2);
    if y != row {
        return None;
    }
    effort_under(x.saturating_sub(area.x))
}

/// `200k ctx` for a known window, [`UNKNOWN`] when the catalog said nothing.
pub fn ctx_label(window: Option<u32>) -> String {
    match window {
        Some(tokens) if tokens >= 1000 => format!("{}k ctx", tokens / 1000),
        Some(tokens) => format!("{tokens} ctx"),
        None => UNKNOWN.to_string(),
    }
}

/// `$5.00/$25.00 per M` for input/output, `free` when both are zero,
/// [`UNKNOWN`] when the catalog carries no price at all.
pub fn price_label(pricing: Option<Pricing>) -> String {
    let Some(pricing) = pricing else {
        return UNKNOWN.to_string();
    };
    if pricing.input_per_m == 0.0 && pricing.output_per_m == 0.0 {
        return "free".to_string();
    }
    format!(
        "{}/{} per M",
        usd(pricing.input_per_m),
        usd(pricing.output_per_m)
    )
}

/// Two decimals under ten dollars, none above: the cents matter where the
/// price is small, and the column has no room where it is not.
fn usd(amount: f64) -> String {
    if amount >= 10.0 {
        format!("${amount:.0}")
    } else {
        format!("${amount:.2}")
    }
}

/// Capability tags after a row's price: `R` reasoning, `V` vision, `E` a
/// catalog that enumerated the model's effort levels. Letters rather than
/// pictographs, so a row is the same width under `ui.ascii_only`.
pub fn capability_tags(model: &ModelInfo) -> String {
    let mut tags = String::new();
    if model.supports_reasoning {
        tags.push('R');
    }
    if model.supports_vision {
        tags.push('V');
    }
    if model.supported_efforts.is_some() {
        tags.push('E');
    }
    tags
}

/// The accepted ladder for the detail line: the levels themselves, or that
/// nothing is known — which is not the same as "no effort works".
pub(crate) fn ladder_label(model: &ModelInfo) -> String {
    match &model.supported_efforts {
        None => "all efforts".to_string(),
        Some(list) => {
            // Ladder order, not catalog order: two catalogs spelling the same
            // set differently have to read the same on screen.
            let names: Vec<&str> = Effort::ALL
                .iter()
                .filter(|effort| list.contains(effort))
                .map(|effort| effort.as_str())
                .collect();
            if names.is_empty() {
                "no efforts".to_string()
            } else {
                format!("efforts {}", names.join("/"))
            }
        }
    }
}

pub use crate::ui::model_picker_view::{draw, lines};

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod model_picker_tests;
