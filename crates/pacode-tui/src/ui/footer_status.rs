//! Footer row 1 — the session's status line, powerline style:
//!
//! ```text
//!  NOR  Gemini 3.8 Flash  high /effort  plugin              ctx 12% · /model
//! ```
//!
//! Each segment is padded by one space and sits on a background alternating
//! `status_bg` / `status_bg_alt`; a VIM or ZDR badge brings its own plaque.
//! With a font known to carry them (`Glyphs::seg_sep_*`) a powerline arrow
//! joins the segments and closes each group; without, the segments abut and
//! the background change is the separator. Where the theme has no status
//! shades (ANSI-16, mono) the row stays flat: segments joined by ` · `.
//! The permission mode is row 2's business, not repeated here.
//! With no session attached nothing is known, and the row says so rather
//! than showing defaults (B10).
//! Narrow widths drop, in order: the `/effort` hint, the `/model` hint, the
//! plugin status, the context fill — then cut the model name.

use ratatui::style::Style;
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

/// One status-line segment: its text, unpadded, and — for a badge — the
/// plaque it already is (padded, with its own background).
struct Segment {
    spans: Vec<Span<'static>>,
    plaque: Option<Style>,
}

/// Which way a group's arrows point: the left group's at the gap, the right
/// group's at the row's end.
#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

pub(super) fn render(width: usize, state: &AppState, opts: &RenderOptions) -> Line<'static> {
    let Some(meta) = state.meta.as_ref() else {
        return Line::from(Span::styled(
            truncate_to_width(NO_SESSION, width, true),
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
    // Nothing optional left: the model name yields. Its segment costs more
    // than the name — padding, the ` · ` or arrow that joins it — so that
    // glue is measured (with the name minus without) and paid for first.
    let keep = Keep {
        effort_hint: false,
        model_hint: false,
        plugin: false,
        ctx: false,
    };
    let without = spans_width(&pieces(meta, state, opts, keep, Some(0)).0);
    let with = spans_width(&pieces(meta, state, opts, keep, None).0);
    let glue = with.saturating_sub(without + display_width(&meta.model.display_name()));
    let budget = width.saturating_sub(without + glue);
    let (left, _) = pieces(meta, state, opts, keep, Some(budget));
    let left = pacode_render::truncate_spans(&left, width, opts.glyphs.ellipsis);
    Line::from(left)
}

/// The row's left and right spans for one rung of the ladder. `model_budget`
/// cuts the model name to that many cells (`Some(0)` leaves its segment out,
/// to measure everything else).
fn pieces(
    meta: &SessionMeta,
    state: &AppState,
    opts: &RenderOptions,
    keep: Keep,
    model_budget: Option<usize>,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let theme = &opts.theme;
    let mut left = Vec::new();
    let mut badge = |label: &str, tone: Style| {
        let span = chrome::badge(label, tone, opts);
        let plaque = Style {
            bg: span.style.bg,
            ..Style::default()
        };
        left.push(Segment {
            spans: vec![span],
            plaque: Some(plaque),
        });
    };
    if state.config.ui.vim {
        let (label, tone) = match state.vim.mode {
            crate::state::vim::VimMode::Normal => ("NOR", theme.cyan),
            crate::state::vim::VimMode::Insert => ("INS", theme.green),
            crate::state::vim::VimMode::Visual => ("VIS", theme.yellow),
        };
        badge(label, tone);
    }
    if state.zdr_active() {
        badge("ZDR", theme.green);
    }
    let model = meta.model.display_name();
    let model = match model_budget {
        Some(budget) => truncate_to_width(&model, budget, true),
        None => model,
    };
    if !model.is_empty() {
        left.push(text(vec![Span::styled(model, theme.bold)]));
    }
    let mut effort = vec![Span::styled(meta.effort.to_string(), theme.accent)];
    if keep.effort_hint && state.config.ui.hints.effort {
        effort.push(Span::styled(" /effort", theme.cyan));
    }
    left.push(text(effort));
    if keep.plugin
        && let Some((_, status)) = &state.plugin_status
    {
        left.push(text(vec![Span::styled(status.clone(), theme.dim)]));
    }

    let mut ctx = Vec::new();
    if keep.ctx
        && let Some(pct) = context_percent(state)
    {
        ctx.push(Span::styled(format!("ctx {pct}%"), theme.dim));
    }
    if keep.model_hint && state.config.ui.hints.model {
        if !ctx.is_empty() {
            ctx.push(Span::styled(opts.glyphs.dot_sep, theme.faint));
        }
        ctx.push(Span::styled("/model", theme.cyan));
    }
    let right = if ctx.is_empty() {
        Vec::new()
    } else {
        vec![text(ctx)]
    };
    (
        assemble(&left, Side::Left, opts),
        assemble(&right, Side::Right, opts),
    )
}

fn text(spans: Vec<Span<'static>>) -> Segment {
    Segment {
        spans,
        plaque: None,
    }
}

/// One group's spans. Flat under a theme without status shades; otherwise
/// padded segments on alternating shades, joined (and the group closed
/// towards the gap) by the side's powerline arrow when the font has one.
fn assemble(segments: &[Segment], side: Side, opts: &RenderOptions) -> Vec<Span<'static>> {
    let theme = &opts.theme;
    let mut out = Vec::new();
    if theme.status_bg.bg.is_none() {
        for (i, seg) in segments.iter().enumerate() {
            if i > 0 {
                // A plaque is a block of its own: a space beside it, the dot
                // between plain texts.
                let glue = if seg.plaque.is_some() || segments[i - 1].plaque.is_some() {
                    " "
                } else {
                    opts.glyphs.dot_sep
                };
                out.push(Span::styled(glue, theme.faint));
            }
            out.extend(seg.spans.iter().cloned());
        }
        return out;
    }
    let shades = [theme.status_bg, theme.status_bg_alt];
    let mut shade = 0;
    let bgs: Vec<Style> = segments
        .iter()
        .map(|seg| {
            seg.plaque.unwrap_or_else(|| {
                shade += 1;
                shades[(shade - 1) % 2]
            })
        })
        .collect();
    // An arrow drawn from one background into the next: its foreground is
    // the background it points away from.
    let arrow = |glyph: &'static str, from: Style, into: Style| {
        let style = Style {
            fg: from.bg,
            bg: into.bg,
            ..Style::default()
        };
        Span::styled(glyph, style)
    };
    let glyph = match side {
        Side::Left => opts.glyphs.seg_sep_left,
        Side::Right => opts.glyphs.seg_sep_right,
    };
    for (i, (seg, &bg)) in segments.iter().zip(&bgs).enumerate() {
        if !glyph.is_empty() {
            match side {
                Side::Left if i > 0 => out.push(arrow(glyph, bgs[i - 1], bg)),
                Side::Left => {}
                Side::Right => {
                    let prev = if i > 0 { bgs[i - 1] } else { Style::default() };
                    out.push(arrow(glyph, bg, prev));
                }
            }
        }
        if seg.plaque.is_some() {
            out.extend(seg.spans.iter().cloned());
        } else {
            out.push(Span::styled(" ", bg));
            out.extend(seg.spans.iter().map(|s| s.clone().patch_style(bg)));
            out.push(Span::styled(" ", bg));
        }
    }
    if let (Side::Left, Some(&last)) = (side, bgs.last())
        && !glyph.is_empty()
    {
        out.push(arrow(glyph, last, Style::default()));
    }
    out
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
