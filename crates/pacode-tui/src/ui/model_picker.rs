//! The `/model` picker: a large overlay with a provider sidebar on the left and,
//! on the right, a kind row, a search box, the model list with its price /
//! context columns, a detail line for the highlighted model and a role strip.
//!
//! `render_lines` is a pure function of state and size, so the layout is tested
//! by asserting on the text of each line.

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod model_picker_tests;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::ModelInfo;

use crate::state::AppState;
use crate::state::model_picker::{PickerRole, SIDE_ALL, SIDE_ROLES};
pub use crate::ui::model_picker_fmt::{detail_text, fmt_ctx, fmt_price};
use crate::ui::overlays::{menu_block, selected_row};

const SIDEBAR_MAX: usize = 26;
/// Narrower than this the sidebar is dropped (`↑/↓` still switch provider).
const SIDEBAR_MIN_TOTAL: usize = 64;

pub fn hint(width: u16, ascii: bool) -> String {
    let full = if ascii {
        "enter select . up/down providers . type to search . alt+left/right kind . r role . esc close"
    } else {
        "enter select · ↑/↓ providers · type to search · alt+←/→ kind · r role · esc close"
    };
    if display_width(full) + 4 <= width as usize {
        return full.to_string();
    }
    let short = if ascii {
        "enter select . up/down provider . alt+left/right kind . r role . esc"
    } else {
        "enter select · ↑/↓ provider · alt+←/→ kind · r role · esc"
    };
    short.to_string()
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, opts: &RenderOptions) {
    frame.render_widget(Clear, area);
    let block = menu_block("Models", &hint(area.width, opts.glyphs.ascii), opts);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = render_lines(state, inner.width, inner.height, opts);
    frame.render_widget(Paragraph::new(lines), inner);

    if state.model_picker.side != SIDE_ROLES {
        let (side_w, _) = split_widths(inner.width as usize);
        let sep = usize::from(side_w > 0);
        let x = inner.x as usize + side_w + sep + 2 + display_width(&state.model_picker.query);
        let max_x = (inner.x + inner.width).saturating_sub(1) as usize;
        frame.set_cursor_position((x.min(max_x) as u16, inner.y + 1));
    }
}

/// `(sidebar width, pane width)` for an inner width.
fn split_widths(w: usize) -> (usize, usize) {
    if w < SIDEBAR_MIN_TOTAL {
        return (0, w);
    }
    let side = (w / 4).clamp(18, SIDEBAR_MAX);
    (side, w - side - 1)
}

pub fn render_lines(state: &AppState, w: u16, h: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    let (w, h) = (w as usize, h as usize);
    let (side_w, pane_w) = split_widths(w);
    let models_view = state.model_picker.side != SIDE_ROLES;
    // The rule, detail line and role strip span the full width under both panes.
    let body_h = if models_view {
        h.saturating_sub(BOTTOM_ROWS)
    } else {
        h
    };
    let pane = if models_view {
        models_pane(state, pane_w, body_h, opts)
    } else {
        roles_pane(state, pane_w, body_h, opts)
    };
    let mut out: Vec<Line<'static>> = if side_w == 0 {
        pane
    } else {
        sidebar(state, side_w, body_h, opts)
            .into_iter()
            .zip(pane)
            .map(|(mut l, p)| {
                l.spans
                    .push(Span::styled(opts.glyphs.vline, opts.theme.faint));
                l.spans.extend(p.spans);
                l
            })
            .collect()
    };
    if models_view {
        let highlighted = state.model_picker.highlighted(&state.models);
        out.push(pad(
            vec![Span::styled(opts.glyphs.hline.repeat(w), opts.theme.faint)],
            w,
        ));
        out.push(detail_line(highlighted, w, opts));
        out.push(role_strip(state, highlighted, w, opts));
    }
    finish_rows(out, w, h)
}

/// Rule, detail line and role strip.
const BOTTOM_ROWS: usize = 3;

// ---- shared helpers -------------------------------------------------------

fn fit(text: &str, w: usize) -> String {
    truncate_to_width(text, w, true)
}

fn pad(mut spans: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let used: usize = spans.iter().map(|s| display_width(&s.content)).sum();
    if used < w {
        spans.push(Span::raw(" ".repeat(w - used)));
    }
    Line::from(spans)
}

fn blank(w: usize) -> Line<'static> {
    Line::from(Span::raw(" ".repeat(w)))
}

fn finish_rows(mut rows: Vec<Line<'static>>, w: usize, h: usize) -> Vec<Line<'static>> {
    rows.truncate(h);
    while rows.len() < h {
        rows.push(blank(w));
    }
    rows
}

// ---- sidebar --------------------------------------------------------------

struct SideRow {
    marker: Option<(&'static str, Style)>,
    label: String,
    style: Style,
    right: String,
    index: usize,
}

fn side_line(row: &SideRow, selected: bool, w: usize, opts: &RenderOptions) -> Line<'static> {
    let pointer = if selected { opts.glyphs.pointer } else { " " };
    let mut spans = vec![Span::styled(format!("{pointer} "), opts.theme.accent)];
    let mut used = 2;
    if let Some((glyph, style)) = row.marker {
        spans.push(Span::styled(format!("{glyph} "), style));
        used += 2;
    }
    let right_w = display_width(&row.right);
    let avail = w.saturating_sub(used + right_w + usize::from(right_w > 0));
    let label = fit(&row.label, avail);
    let label_w = display_width(&label);
    let label_style = if selected {
        opts.theme.bold.patch(row.style)
    } else {
        row.style
    };
    spans.push(Span::styled(label, label_style));
    used += label_w;
    if right_w > 0 {
        let gap = w.saturating_sub(used + right_w);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(row.right.clone(), opts.theme.dim));
    }
    let line = pad(spans, w);
    if selected {
        selected_row(line, w as u16, opts)
    } else {
        line
    }
}

fn sidebar(state: &AppState, w: usize, h: usize, opts: &RenderOptions) -> Vec<Line<'static>> {
    let mp = &state.model_picker;
    let mut rows = vec![
        SideRow {
            marker: None,
            label: "Roles".to_string(),
            style: opts.theme.fg,
            right: String::new(),
            index: SIDE_ROLES,
        },
        SideRow {
            marker: None,
            label: format!("All models ({})", mp.entries.len()),
            style: opts.theme.fg,
            right: String::new(),
            index: SIDE_ALL,
        },
    ];
    let mut out: Vec<Line<'static>> = rows
        .drain(..)
        .map(|r| side_line(&r, mp.side == r.index, w, opts))
        .collect();
    out.push(blank(w));

    let list_h = h.saturating_sub(out.len());
    let selected_group = mp.side.checked_sub(2);
    let start = match selected_group {
        Some(g) if list_h > 0 && g >= list_h => g + 1 - list_h,
        _ => 0,
    };
    for (gi, g) in mp.groups.iter().enumerate().skip(start).take(list_h) {
        let (glyph, style) = if g.configured {
            (opts.glyphs.main_dot, opts.theme.fg)
        } else {
            (opts.glyphs.agent_dot, opts.theme.faint)
        };
        let row = SideRow {
            marker: Some((glyph, style)),
            label: g.id.clone(),
            style,
            right: if g.configured {
                g.count.to_string()
            } else {
                String::new()
            },
            index: gi + 2,
        };
        out.push(side_line(&row, mp.side == row.index, w, opts));
    }
    finish_rows(out, w, h)
}

// ---- roles pane -----------------------------------------------------------

fn roles_pane(state: &AppState, w: usize, h: usize, opts: &RenderOptions) -> Vec<Line<'static>> {
    let mp = &state.model_picker;
    let mut rows = vec![
        pad(vec![Span::styled("Roles", opts.theme.bold)], w),
        pad(
            vec![Span::styled(
                fit(
                    "enter picks the role that the next model assignment applies to",
                    w,
                ),
                opts.theme.faint,
            )],
            w,
        ),
        blank(w),
    ];
    for (i, role) in PickerRole::ALL.iter().enumerate() {
        let marker = if *role == mp.role {
            opts.glyphs.main_dot
        } else {
            opts.glyphs.agent_dot
        };
        let target = match state.role_target(*role) {
            Some(r) => {
                let follows = *role != PickerRole::Main && !state.role_is_set(*role);
                if follows {
                    format!("{r} (follows default)")
                } else {
                    r.to_string()
                }
            }
            None => "not set".to_string(),
        };
        let selected = mp.cursor == i;
        let pointer = if selected { opts.glyphs.pointer } else { " " };
        let spans = vec![
            Span::styled(format!("{pointer} "), opts.theme.accent),
            Span::styled(format!("{marker} "), opts.theme.accent),
            Span::styled(format!("{:<12}", role.label()), opts.theme.fg),
            Span::styled(fit(&target, w.saturating_sub(18)), opts.theme.dim),
        ];
        let line = pad(spans, w);
        rows.push(if selected {
            selected_row(line, w as u16, opts)
        } else {
            line
        });
    }
    finish_rows(rows, w, h)
}

// ---- models pane ----------------------------------------------------------

/// Right-aligned columns that fit in `pane_w`.
#[derive(Clone, Copy)]
struct Cols {
    ctx: bool,
    price: bool,
}

const FLAGS_W: usize = 4; // "R V "
const CTX_W: usize = 6; // " 1000k"
const PRICE_W: usize = 13; // " $0.075/0.3"

impl Cols {
    fn for_width(w: usize) -> Cols {
        Cols {
            ctx: w >= 44,
            price: w >= 54,
        }
    }
    fn width(self) -> usize {
        FLAGS_W + if self.ctx { CTX_W } else { 0 } + if self.price { PRICE_W } else { 0 }
    }
}

fn model_row(
    m: &ModelInfo,
    selected: bool,
    w: usize,
    cols: Cols,
    opts: &RenderOptions,
) -> Line<'static> {
    let pointer = if selected { opts.glyphs.pointer } else { " " };
    let name_w = w.saturating_sub(2 + cols.width());
    let name = fit(&m.route.to_string(), name_w);
    let name_used = display_width(&name);
    let mut spans = vec![
        Span::styled(format!("{pointer} "), opts.theme.accent),
        Span::styled(
            name,
            if selected {
                opts.theme.bold
            } else {
                opts.theme.fg
            },
        ),
        Span::raw(" ".repeat(name_w.saturating_sub(name_used))),
    ];
    let flag =
        |on: bool, c: &'static str| Span::styled(if on { c } else { " " }, opts.theme.accent);
    spans.push(flag(m.supports_reasoning, "R"));
    spans.push(Span::raw(" "));
    spans.push(flag(m.vision, "V"));
    spans.push(Span::raw(" "));
    if cols.ctx {
        let ctx = m.context_window.map(fmt_ctx).unwrap_or_default();
        spans.push(Span::styled(format!("{ctx:>CTX_W$}"), opts.theme.dim));
    }
    if cols.price {
        let price = m.pricing.as_ref().map(fmt_price).unwrap_or_default();
        spans.push(Span::styled(format!("{price:>PRICE_W$}"), opts.theme.dim));
    }
    let line = pad(spans, w);
    if selected {
        selected_row(line, w as u16, opts)
    } else {
        line
    }
}

fn kind_row(state: &AppState, w: usize, opts: &RenderOptions) -> Line<'static> {
    let mp = &state.model_picker;
    let mut spans = vec![Span::styled("Kind: ", opts.theme.dim)];
    let item = |label: &str, on: bool| {
        Span::styled(
            label.to_string(),
            if on {
                opts.theme.accent.patch(opts.theme.bold)
            } else {
                opts.theme.dim
            },
        )
    };
    spans.push(item("all", mp.kind.is_none()));
    for k in &mp.kinds {
        spans.push(Span::styled(opts.glyphs.dot_sep, opts.theme.faint));
        spans.push(item(k.as_str(), mp.kind == Some(*k)));
    }
    pad(spans, w)
}

fn search_row(state: &AppState, w: usize, opts: &RenderOptions) -> Line<'static> {
    let q = &state.model_picker.query;
    let mut spans = vec![Span::styled("> ", opts.theme.accent)];
    if q.is_empty() {
        spans.push(Span::styled("type to search", opts.theme.faint));
    } else {
        spans.push(Span::styled(fit(q, w.saturating_sub(2)), opts.theme.fg));
    }
    pad(spans, w)
}

fn models_pane(state: &AppState, w: usize, h: usize, opts: &RenderOptions) -> Vec<Line<'static>> {
    let mp = &state.model_picker;
    let mut rows = vec![kind_row(state, w, opts), search_row(state, w, opts)];
    let list_h = h.saturating_sub(2);
    let cols = Cols::for_width(w);

    if mp.view.is_empty() {
        let msg = if state.models.is_empty() {
            "  loading…"
        } else {
            "  no matching models"
        };
        rows.push(pad(vec![Span::styled(msg, opts.theme.dim)], w));
    } else {
        let rule = mp.recent_rows > 0 && mp.recent_rows < mp.view.len();
        let total = mp.view.len() + usize::from(rule);
        let cursor_vis = mp.cursor + usize::from(rule && mp.cursor >= mp.recent_rows);
        let start = if list_h > 0 && cursor_vis >= list_h {
            cursor_vis + 1 - list_h
        } else {
            0
        };
        for vi in start..total.min(start + list_h) {
            if rule && vi == mp.recent_rows {
                let rule_w = w.saturating_sub(2);
                rows.push(pad(
                    vec![
                        Span::raw("  "),
                        Span::styled(opts.glyphs.hline.repeat(rule_w), opts.theme.faint),
                    ],
                    w,
                ));
                continue;
            }
            let v = if rule && vi > mp.recent_rows {
                vi - 1
            } else {
                vi
            };
            let model = mp
                .view
                .get(v)
                .and_then(|&e| mp.entries.get(e as usize))
                .and_then(|e| state.models.get(e.model));
            if let Some(m) = model {
                rows.push(model_row(m, v == mp.cursor, w, cols, opts));
            }
        }
    }
    finish_rows(rows, w, h)
}

fn detail_line(m: Option<&ModelInfo>, w: usize, opts: &RenderOptions) -> Line<'static> {
    let text = m
        .map(|m| {
            let full = detail_text(m, true);
            if display_width(&full) <= w {
                full
            } else {
                detail_text(m, false)
            }
        })
        .unwrap_or_default();
    pad(vec![Span::styled(fit(&text, w), opts.theme.dim)], w)
}

fn role_strip(
    state: &AppState,
    m: Option<&ModelInfo>,
    w: usize,
    opts: &RenderOptions,
) -> Line<'static> {
    let mut spans = Vec::new();
    for role in PickerRole::ALL {
        // A role that only inherits the main model is not "assigned" to it.
        let assigned = role == PickerRole::Main || state.role_is_set(role);
        let points_here =
            assigned && m.is_some_and(|m| state.role_target(role).as_ref() == Some(&m.route));
        let glyph = if points_here {
            opts.glyphs.main_dot
        } else {
            opts.glyphs.agent_dot
        };
        let active = role == state.model_picker.role;
        let style = if active {
            opts.theme.accent.patch(opts.theme.bold)
        } else {
            opts.theme.dim
        };
        spans.push(Span::styled(format!("{glyph} {}", role.label()), style));
        spans.push(Span::raw("  "));
    }
    spans.pop();
    pad(spans, w)
}
