//! Rules over the model catalog that the model palette reads: the `Kind`
//! filter, the effort ladder a model accepts, the fuzzy match the search runs,
//! and the short labels a row prints.
//!
//! # Efforts
//!
//! [`ModelInfo::supported_efforts`] of `None` means the catalog said nothing
//! about the model, which is not the same as "no effort works": such a model
//! keeps every level selectable and cycles through the full ladder. Only a
//! catalog that enumerated levels — opencode's `reasoning_options`, the ChatGPT
//! backend's `supported_reasoning_levels`, a providers.toml `efforts` list —
//! narrows the ladder, because an invented "unsupported" list would hide a
//! level that works.
//!
//! The cycle skips the levels a model does not accept: from `low` with
//! `medium` and `xhigh` unsupported the next step is `high`. From the last
//! accepted level it wraps to the first *accepted* one, never to the first
//! rung of the ladder.

use std::collections::HashMap;

use pacode_types::{Effort, ModelFlags, ModelInfo, Pricing};

/// Per-model flags, keyed by `provider/model`.
pub type Flags = HashMap<String, ModelFlags>;

/// Shown wherever the catalog carries no value: an unknown price is never
/// rendered as a number, and an unknown window is never guessed.
const UNKNOWN: &str = "—";

/// The capability filter. `All` keeps every model; the others keep the ones
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

    /// The next filter, wrapping at the end.
    pub fn next(self) -> Kind {
        let idx = Kind::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Kind::ALL[(idx + 1) % Kind::ALL.len()]
    }

    pub fn keeps(self, model: &ModelInfo) -> bool {
        match self {
            Kind::All => true,
            Kind::Reasoning => model.supports_reasoning,
            Kind::Vision => model.supports_vision,
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

/// The levels `model` accepts, in ladder order.
pub fn accepted_efforts(model: &ModelInfo) -> Vec<Effort> {
    Effort::ALL
        .into_iter()
        .filter(|effort| is_effort_available(model, *effort))
        .collect()
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
/// below it, else the lowest accepted one — the same chain the client's
/// post-switch downgrade uses. `None` only when the model accepts nothing.
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

/// Subsequence match of `query` in `text`, ignoring case and the query's
/// whitespace. `None` when a query character is missing; otherwise a score
/// where higher is better: consecutive runs and word starts earn, gaps cost.
///
/// ponytail: greedy leftmost alignment, not the optimal one — a later
/// word-start alignment can score higher than the one found. Fine for a list
/// of a few hundred names; switch to a DP matcher if ranking complaints come in.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0i32;
    let mut from = 0usize;
    let mut prev: Option<usize> = None;
    for qc in query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
    {
        let pos = from + text.get(from..)?.iter().position(|c| *c == qc)?;
        score += 1;
        if prev.is_some_and(|p| p + 1 == pos) {
            score += 5;
        }
        if pos == 0 || !text[pos - 1].is_alphanumeric() {
            score += 3;
        }
        let gap = pos - prev.map_or(0, |p| p + 1);
        score -= gap.min(3) as i32;
        prev = Some(pos);
        from = pos + 1;
    }
    Some(score)
}

/// `200k` for a known window, `1M` from a million up, [`UNKNOWN`] when the
/// catalog said nothing.
pub fn ctx_label(window: Option<u32>) -> String {
    match window {
        Some(tokens) if tokens >= 1_000_000 => format!("{}M", (tokens + 500_000) / 1_000_000),
        Some(tokens) if tokens >= 1000 => format!("{}k", tokens / 1000),
        Some(tokens) => tokens.to_string(),
        None => UNKNOWN.to_string(),
    }
}

/// `$3/$15` per million input/output tokens, `free` when both are zero,
/// [`UNKNOWN`] when the catalog carries no price at all.
pub fn price_label(pricing: Option<Pricing>) -> String {
    let Some(pricing) = pricing else {
        return UNKNOWN.to_string();
    };
    if pricing.input_per_m == 0.0 && pricing.output_per_m == 0.0 {
        return "free".to_string();
    }
    format!("{}/{}", usd(pricing.input_per_m), usd(pricing.output_per_m))
}

/// Cents only where they matter: `$0.25`, `$1.5`, `$3`, `$15`.
fn usd(amount: f64) -> String {
    if amount >= 10.0 {
        return format!("${amount:.0}");
    }
    let cents = format!("{amount:.2}");
    let trimmed = cents.trim_end_matches('0').trim_end_matches('.');
    format!("${trimmed}")
}

/// Capability tags: `R` reasoning, `V` vision, `E` a catalog that enumerated
/// the model's effort levels. Letters rather than pictographs, so a row is the
/// same width under `ui.ascii_only`.
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

#[cfg(test)]
#[path = "model_catalog_tests.rs"]
mod model_catalog_tests;
