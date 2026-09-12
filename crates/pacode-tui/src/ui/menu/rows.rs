//! The list zone internals (§3.2): rows of one display line plus the detail
//! card — split off into a right-hand panel when the width allows, or two
//! collapsed lines under the selected row when it does not. Split out of
//! `render.rs` to keep every file under the 500-line module rule.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, display_width, truncate_to_width};

use crate::binding::format_binding;
use crate::ui::overlays::selected_row;

use super::layout::{COLLAPSED_DETAIL_LINES, detail_panel_width, resolve_window, row_height};
use super::model::{Detail, DetailLine, Mark, MenuRow, resolve_cursor};
use super::view::{MenuAction, MenuMode, MenuUiState, MenuView, is_base_key};

/// The list zone — with the detail panel split off to the right once the inner
/// width allows (§3.2) — and the collapsed inline detail when it does not.
/// Returns the edit-box cursor position while a `Value` row is being edited.
pub(super) fn draw_list(
    frame: &mut Frame,
    area: Rect,
    view: &MenuView,
    ui: &MenuUiState,
    opts: &RenderOptions,
) -> Option<(u16, u16)> {
    if view.rows.is_empty() {
        let mut lines = Vec::new();
        if !view.empty.primary.is_empty() {
            lines.push(Line::from(Span::styled(
                view.empty.primary.clone(),
                opts.theme.dim,
            )));
        }
        if !view.empty.hint.is_empty() {
            lines.push(Line::from(Span::styled(
                view.empty.hint.clone(),
                opts.theme.faint,
            )));
        }
        frame.render_widget(Paragraph::new(lines), area);
        return None;
    }

    let sel = resolve_cursor(&view.rows, &ui.cursor);
    // The panel opens only when the menu has detail to show at all — a menu of
    // detail-less rows keeps the full width instead of an empty gutter.
    let any_detail = view
        .rows
        .iter()
        .any(|r| r.detail().is_some_and(|d| !d.is_empty()));
    let panel_w = if any_detail {
        detail_panel_width(area.width)
    } else {
        None
    };
    let (list_w, panel_x) = match panel_w {
        Some(w) => (area.width - w, Some(area.x + area.width - w)),
        None => (area.width, None),
    };
    let detail_inline = panel_w.is_none();
    let heights: Vec<usize> = view
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| row_height(r, Some(i) == sel, detail_inline))
        .collect();
    let budget = area.height as usize;
    let start = resolve_window(&heights, sel.unwrap_or(0), budget, ui.scroll);

    let editing = match &ui.mode {
        MenuMode::Edit { buffer, .. } => Some(buffer.as_str()),
        MenuMode::List | MenuMode::Capture { .. } => None,
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut used = 0usize;
    let mut sel_y = None;
    for (i, row) in view.rows.iter().enumerate().skip(start) {
        if used >= budget {
            break;
        }
        let h = heights[i];
        let is_sel = Some(i) == sel;
        // The selected row always renders — clipped by the paragraph when it
        // alone exceeds the budget — because the cursor never leaves the screen.
        if used + h > budget && !is_sel {
            break;
        }
        if is_sel {
            sel_y = Some(area.y + used as u16);
        }
        let edit = editing.filter(|_| is_sel && matches!(row, MenuRow::Value { .. }));
        let line = main_line(row, is_sel, list_w, edit, opts);
        lines.push(if is_sel {
            selected_row(line, list_w, opts)
        } else {
            line
        });
        if is_sel && detail_inline {
            lines.extend(
                collapsed_lines(row, list_w, opts)
                    .into_iter()
                    .take(COLLAPSED_DETAIL_LINES),
            );
        }
        used += h;
    }
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(area.x, area.y, list_w, area.height),
    );

    if let (Some(pw), Some(px)) = (panel_w, panel_x) {
        let separator: Vec<Line<'static>> = (0..area.height)
            .map(|_| Line::from(Span::styled(opts.glyphs.vline, opts.theme.dim)))
            .collect();
        frame.render_widget(
            Paragraph::new(separator),
            Rect::new(px, area.y, 1, area.height),
        );
        let panel = Rect::new(px + 2, area.y, pw.saturating_sub(2), area.height);
        let lines = sel
            .and_then(|i| view.rows.get(i))
            .and_then(MenuRow::detail)
            .map_or_else(Vec::new, |d| {
                detail_lines(d, panel.width as usize, area.height as usize, opts)
            });
        frame.render_widget(Paragraph::new(lines), panel);
    }

    // The edit box sits where the value was: right-aligned in the list column.
    match (&ui.mode, sel, sel_y) {
        (MenuMode::Edit { buffer, cursor }, Some(sel_i), Some(y))
            if matches!(view.rows.get(sel_i), Some(MenuRow::Value { .. })) =>
        {
            let buf_w = display_width(buffer) as u16;
            let box_w = buf_w + 4; // "[ buf ]"
            let right_start = area.x + list_w.saturating_sub(box_w);
            let before: String = buffer.chars().take(*cursor).collect();
            let x = right_start + 2 + (display_width(&before) as u16).min(buf_w);
            Some((x.min(area.x + list_w.saturating_sub(1)), y))
        }
        _ => None,
    }
}

/// The single display line of a row (§3.1): pointer column, optional mark or
/// checkbox, the label, then the right-hand tail flush right. `edit` is the
/// live buffer shown in place of the value on the row being edited.
fn main_line(
    row: &MenuRow,
    is_sel: bool,
    width: u16,
    edit: Option<&str>,
    opts: &RenderOptions,
) -> Line<'static> {
    let w = width as usize;
    let label_style = if is_sel {
        opts.theme.bold
    } else {
        opts.theme.fg
    };
    let pointer = if is_sel { opts.glyphs.pointer } else { " " };
    let pointer_span = || Span::styled(format!("{pointer} "), opts.theme.accent);

    let mut prefix: Vec<Span<'static>> = Vec::new();
    let label: (String, Style);
    let right: Option<(String, Style)>;
    match row {
        MenuRow::Section {
            label: text,
            right: tail,
        } => {
            // No pointer column: a section is a header, not a row.
            prefix.push(Span::raw("  "));
            label = (text.clone(), opts.theme.accent.patch(opts.theme.bold));
            right = tail.clone().map(|t| (t, opts.theme.faint));
        }
        MenuRow::Item {
            mark,
            label: text,
            right: tail,
            ..
        } => {
            prefix.push(pointer_span());
            if let Some((glyph, style)) = mark_glyph(*mark, opts) {
                prefix.push(Span::styled(format!("{glyph} "), style));
            }
            label = (text.clone(), label_style);
            right = tail.clone().map(|t| (t, opts.theme.dim));
        }
        MenuRow::Toggle {
            label: text,
            on,
            modified,
            ..
        } => {
            prefix.push(pointer_span());
            label = (text.clone(), label_style);
            let word = if *on { "on" } else { "off" };
            let text = if *modified {
                format!("{word} *")
            } else {
                word.to_string()
            };
            let style = if *modified {
                opts.theme.cyan
            } else {
                opts.theme.dim
            };
            right = Some((text, style));
        }
        MenuRow::Value {
            label: text,
            value,
            modified,
            ..
        } => {
            prefix.push(pointer_span());
            label = (text.clone(), label_style);
            right = Some(match edit {
                Some(buf) => (
                    format!("[ {buf} ]"),
                    opts.theme.accent.patch(opts.theme.bold),
                ),
                None => {
                    let text = if *modified {
                        format!("{value} *")
                    } else {
                        value.clone()
                    };
                    let style = if *modified {
                        opts.theme.cyan
                    } else {
                        opts.theme.dim
                    };
                    (text, style)
                }
            });
        }
        MenuRow::Submenu {
            label: text,
            right: tail,
            ..
        } => {
            prefix.push(pointer_span());
            label = (text.clone(), label_style);
            // The `›` tail marks a row that descends into another menu.
            let marker = if opts.glyphs.ascii { ">" } else { "›" };
            let text = match tail {
                Some(t) => format!("{t} {marker}"),
                None => marker.to_string(),
            };
            right = Some((text, opts.theme.faint));
        }
        MenuRow::Check {
            label: text,
            checked,
            warn,
            ..
        } => {
            prefix.push(pointer_span());
            let (glyph, style) = if *checked {
                (opts.glyphs.check_done, opts.theme.green)
            } else {
                (opts.glyphs.check_pending, opts.theme.dim)
            };
            prefix.push(Span::styled(format!("{glyph} "), style));
            label = (text.clone(), label_style);
            right = warn.clone().map(|t| {
                let sign = if opts.glyphs.ascii { "!" } else { "⚠" };
                (format!("{sign} {t}"), opts.theme.yellow)
            });
        }
    }

    // Fit: truncate the label so the right tail keeps its flush-right edge.
    let prefix_w: usize = prefix.iter().map(|s| display_width(&s.content)).sum();
    let right_w = right.as_ref().map_or(0, |(t, _)| display_width(t));
    let gap = usize::from(right.is_some());
    let label_budget = w.saturating_sub(prefix_w + right_w + gap);
    let label_shown = truncate_to_width(&label.0, label_budget, opts.glyphs.ascii);
    let label_w = display_width(&label_shown);

    let mut spans = prefix;
    spans.push(Span::styled(label_shown, label.1));
    if let Some((text, style)) = right {
        let pad = w.saturating_sub(prefix_w + label_w + right_w);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// Glyph + style for a row mark; `Mark::None` renders nothing at all.
/// `pub(crate)`: the MCP log panel header draws the same status glyph.
pub(crate) fn mark_glyph(mark: Mark, opts: &RenderOptions) -> Option<(&'static str, Style)> {
    let pair = match mark {
        Mark::None => return None,
        Mark::Active => (opts.glyphs.main_dot, opts.theme.green),
        Mark::Ok => (opts.glyphs.ok, opts.theme.green),
        Mark::Fail => (opts.glyphs.fail, opts.theme.red),
        Mark::Idle => (opts.glyphs.agent_dot, opts.theme.dim),
        Mark::Disabled => (opts.glyphs.disabled, opts.theme.dim),
        Mark::Running => (opts.glyphs.running, opts.theme.violet),
        Mark::Pending => (opts.glyphs.running, opts.theme.yellow),
        Mark::Stopped => (opts.glyphs.stopped, opts.theme.dim),
    };
    Some(pair)
}

/// Up to [`COLLAPSED_DETAIL_LINES`] lines under the selected row while the
/// detail panel is collapsed (§3.2): the title takes the first line.
fn collapsed_lines(row: &MenuRow, width: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    let Some(detail) = row.detail() else {
        return Vec::new();
    };
    let w = (width as usize).saturating_sub(4);
    let ascii = opts.glyphs.ascii;
    let mut out: Vec<Line<'static>> = Vec::new();
    if !detail.title.is_empty() {
        out.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                truncate_to_width(&detail.title, w, ascii),
                opts.theme.dim.patch(opts.theme.bold),
            ),
        ]));
    }
    for line in &detail.lines {
        if out.len() >= COLLAPSED_DETAIL_LINES {
            break;
        }
        out.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                truncate_to_width(&detail_text(line), w, ascii),
                opts.theme.faint,
            ),
        ]));
    }
    out
}

/// A `DetailLine` as plain text: `label: value`, the free line, or the code
/// line stripped of its tokens.
fn detail_text(line: &DetailLine) -> String {
    match line {
        DetailLine::Field { label, value } => format!("{label}: {value}"),
        DetailLine::Text(t) | DetailLine::Code { text: t, .. } => t.clone(),
    }
}

/// The detail card rendered into the right-hand panel (§3.2): title, lines,
/// then the row's action chips pinned to the panel's last row.
fn detail_lines(
    detail: &Detail,
    width: usize,
    height: usize,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    let ascii = opts.glyphs.ascii;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if !detail.title.is_empty() {
        lines.push(Line::from(Span::styled(
            truncate_to_width(&detail.title, width, ascii),
            opts.theme.bold,
        )));
    }
    for line in &detail.lines {
        match line {
            DetailLine::Field { label, value } => {
                let label_shown = truncate_to_width(label, width.saturating_sub(2), ascii);
                let value_budget = width.saturating_sub(display_width(&label_shown) + 2);
                lines.push(Line::from(vec![
                    Span::styled(format!("{label_shown}:"), opts.theme.dim),
                    Span::styled(
                        format!(" {}", truncate_to_width(value, value_budget, ascii)),
                        opts.theme.fg,
                    ),
                ]));
            }
            DetailLine::Text(t) => {
                lines.push(Line::from(Span::styled(
                    truncate_to_width(t, width, ascii),
                    opts.theme.fg,
                )));
            }
            DetailLine::Code { text, toks } => {
                lines.push(Line::from(code_spans(text, toks, width, ascii, opts)));
            }
        }
    }
    let chips = chip_spans(&detail.actions, opts);
    if !chips.is_empty() {
        while lines.len() + 1 < height {
            lines.push(Line::default());
        }
        lines.push(Line::from(chips));
    }
    lines
}

/// A lexed code line as spans: one `Span` per token, colours from
/// `token_style`, cut at `width` on a char boundary (`…` marks the cut).
fn code_spans(
    text: &str,
    toks: &[pacode_render::syntax::Token],
    width: usize,
    ascii: bool,
    opts: &RenderOptions,
) -> Vec<Span<'static>> {
    if toks.is_empty() || opts.color == pacode_render::ColorMode::Mono {
        return vec![Span::styled(
            truncate_to_width(text, width, ascii),
            opts.theme.fg,
        )];
    }
    let mut spans = Vec::with_capacity(toks.len());
    let mut used = 0usize;
    for tok in toks {
        // Lexer ranges index `text` byte-wise; `get` keeps a malformed one
        // from panicking mid-draw instead of trusting it blindly.
        let Some(piece) = text.get(tok.range.clone()) else {
            continue;
        };
        let w = display_width(piece);
        if used + w > width {
            // Cut inside the token: keep what fits minus the ellipsis cell —
            // `truncate_to_width`'s own mark stays off so the tail below is
            // the only one.
            let budget = width.saturating_sub(used).saturating_sub(1);
            if budget > 0 {
                let head = truncate_to_width(piece, budget, false);
                spans.push(Span::styled(head, token_style_or_fg(tok.kind, opts)));
            }
            if used < width {
                spans.push(Span::styled(
                    if ascii { "..." } else { "…" },
                    opts.theme.faint,
                ));
            }
            return spans;
        }
        spans.push(Span::styled(
            piece.to_string(),
            token_style_or_fg(tok.kind, opts),
        ));
        used += w;
    }
    spans
}

fn token_style_or_fg(
    kind: Option<pacode_render::syntax::TokenKind>,
    opts: &RenderOptions,
) -> Style {
    kind.map(|k| pacode_render::syntax::token_style(&opts.theme, k))
        .unwrap_or(opts.theme.fg)
}

/// `key label · key label` spans for a row's action chips. Disabled chips and
/// base-key collisions never render (§3.2, §3.4).
fn chip_spans(actions: &[MenuAction], opts: &RenderOptions) -> Vec<Span<'static>> {
    let sep = opts.glyphs.dot_sep;
    let mut spans = Vec::new();
    for a in actions.iter().filter(|a| a.enabled && !is_base_key(&a.key)) {
        if !spans.is_empty() {
            spans.push(Span::styled(sep, opts.theme.faint));
        }
        spans.push(Span::styled(format_binding(&a.key), opts.theme.cyan));
        spans.push(Span::styled(format!(" {}", a.label), opts.theme.dim));
    }
    spans
}
