//! Footer row 1 — the session's status line:
//!
//! ```text
//!  BYPASS  Gemini 3.8 Flash · high /effort              ctx 12% · /model
//! ```
//!
//! The permission mode is a badge in its tone, the model bold, the effort in
//! the accent, the context fill flush right. With no session attached nothing
//! is known, and the row says so rather than showing defaults (B10).
//! Narrow widths drop, in order: the `/effort` hint, the `/model` hint, the
//! plugin status, the context fill — then cut the model name.

use ratatui::text::{Line, Span};

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::SessionMeta;

use crate::state::AppState;
use crate::ui::chrome;

/// What the row shows while no session is attached.
pub const NO_SESSION: &str = "no session";

/// Which optional pieces a candidate row keeps.
#[derive(Clone, Copy)]
struct Keep {
    effort_hint: bool,
    model_hint: bool,
    plugin: bool,
    ctx: bool,
}

pub(super) fn render(width: usize, state: &AppState, opts: &RenderOptions) -> Line<'static> {
    let Some(meta) = state.meta.as_ref() else {
        return Line::from(Span::styled(
            truncate_to_width(NO_SESSION, width, opts.glyphs.ascii),
            opts.theme.faint,
        ));
    };
    let ladder = [
        Keep {
            effort_hint: true,
            model_hint: true,
            plugin: true,
            ctx: true,
        },
        Keep {
            effort_hint: false,
            model_hint: true,
            plugin: true,
            ctx: true,
        },
        Keep {
            effort_hint: false,
            model_hint: false,
            plugin: true,
            ctx: true,
        },
        Keep {
            effort_hint: false,
            model_hint: false,
            plugin: false,
            ctx: true,
        },
    ];
    for keep in ladder {
        let (left, right) = pieces(meta, state, opts, keep, None);
        if spans_width(&left) + 1 + spans_width(&right) <= width {
            return justify(left, right, width);
        }
    }
    // Nothing optional left: the model name yields.
    let keep = Keep {
        effort_hint: false,
        model_hint: false,
        plugin: false,
        ctx: false,
    };
    let (left, _) = pieces(meta, state, opts, keep, Some(0));
    let budget = width.saturating_sub(spans_width(&left));
    let (left, _) = pieces(meta, state, opts, keep, Some(budget));
    let left = pacode_render::truncate_spans(&left, width, opts.glyphs.ellipsis);
    Line::from(left)
}

/// The row's left and right spans for one rung of the ladder. `model_budget`
/// cuts the model name to that many cells (`Some(0)` leaves it out, to
/// measure everything else).
fn pieces(
    meta: &SessionMeta,
    state: &AppState,
    opts: &RenderOptions,
    keep: Keep,
    model_budget: Option<usize>,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let theme = &opts.theme;
    let sep = || Span::styled(opts.glyphs.dot_sep, theme.faint);
    let mut left = vec![chrome::badge(
        &meta.mode.label().to_uppercase(),
        chrome::mode_tone(meta.mode, theme),
        opts,
    )];
    if state.config.ui.vim {
        let (label, tone) = match state.vim.mode {
            crate::state::vim::VimMode::Normal => ("NOR", theme.cyan),
            crate::state::vim::VimMode::Insert => ("INS", theme.green),
            crate::state::vim::VimMode::Visual => ("VIS", theme.yellow),
        };
        left.push(Span::raw(" "));
        left.push(chrome::badge(label, tone, opts));
    }
    if state.zdr_active() {
        left.push(Span::raw(" "));
        left.push(chrome::badge("ZDR", theme.green, opts));
    }
    if keep.plugin
        && let Some((_, text)) = &state.plugin_status
    {
        left.push(Span::styled(format!(" {text}"), theme.dim));
    }
    left.push(Span::raw(" "));
    let model = meta.model.display_name();
    let model = match model_budget {
        Some(budget) => truncate_to_width(&model, budget, opts.glyphs.ascii),
        None => model,
    };
    if !model.is_empty() {
        left.push(Span::styled(model, theme.bold));
        left.push(sep());
    }
    left.push(Span::styled(meta.effort.to_string(), theme.accent));
    if keep.effort_hint && state.config.ui.hints.effort {
        left.push(Span::styled(" /effort", theme.cyan));
    }

    let mut right = Vec::new();
    if keep.ctx
        && let Some(pct) = context_percent(state)
    {
        right.push(Span::styled(format!("ctx {pct}%"), theme.dim));
    }
    if keep.model_hint && state.config.ui.hints.model {
        if !right.is_empty() {
            right.push(sep());
        }
        right.push(Span::styled("/model", theme.cyan));
    }
    (left, right)
}

/// How full the context window is, when the window is known.
fn context_percent(state: &AppState) -> Option<u64> {
    let usage = &state.rail.usage;
    let window = u64::from(usage.context_window?);
    (window > 0).then(|| (u64::from(usage.context_tokens) * 100 / window).min(100))
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

fn justify(mut left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let pad = width.saturating_sub(spans_width(&left) + spans_width(&right));
    if !right.is_empty() {
        left.push(Span::raw(" ".repeat(pad)));
        left.extend(right);
    }
    Line::from(left)
}
