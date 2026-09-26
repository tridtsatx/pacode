//! Drawing for the model picker: the two columns, the detail line, the effort
//! row and the hint footer.
//!
//! The state and the rules live in [`crate::ui::model_picker`]; this module
//! only turns a picker plus a catalog into lines, so the same output answers a
//! repaint and a test.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::{Effort, ModelInfo};

use crate::ui::model_picker::{
    CHIP_LABEL, CHIP_WIDTH, Flags, Kind, ModelPicker, ProviderRow, capability_tags, ctx_label,
    is_effort_available, ladder_label, price_label,
};
use crate::ui::overlays::selected_row;

/// Title, search, `Kind:`, detail, effort row, hint — everything around the
/// two columns. The list gets whatever is left.
const FIXED_ROWS: usize = 6;

/// Rows the picker wants for `list_rows` model rows: the columns plus
/// [`FIXED_ROWS`]. `AppState::bottom_picker_height` reserves this much before
/// the overlay is drawn, so the number has to come from here rather than from
/// a second count of the lines `lines` produces.
pub fn height(list_rows: usize) -> usize {
    list_rows + FIXED_ROWS
}

/// The whole overlay as lines, so a test can read what the user reads.
pub fn lines(
    picker: &ModelPicker,
    models: &[ModelInfo],
    flags: &Flags,
    area: Rect,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::from(Span::styled(
            "Switch Model",
            opts.theme.bold.patch(opts.theme.accent),
        )),
        Line::from(vec![
            Span::styled("> ", opts.theme.accent),
            Span::styled(picker.query.clone(), opts.theme.fg),
        ]),
        kind_line(picker, opts),
    ];

    let providers = picker.providers(models, flags);
    let rows = picker.models(models, flags);
    let list_height = usize::from(area.height).saturating_sub(FIXED_ROWS);
    let highlighted = rows.get(picker.row).copied();
    let provider_sel = providers
        .iter()
        .position(|row| row.key == picker.provider)
        .unwrap_or(0);
    let providers_start = window_start(providers.len(), provider_sel, list_height);
    let models_start = window_start(rows.len(), picker.row, list_height);

    if models.is_empty() {
        out.push(Line::from(Span::styled("  loading…", opts.theme.faint)));
    } else if rows.is_empty() {
        out.push(Line::from(Span::styled(
            "  no matching models",
            opts.theme.dim,
        )));
        out.push(Line::from(Span::styled(
            "  backspace to widen the search",
            opts.theme.faint,
        )));
    } else {
        let provider_width = (usize::from(area.width) / 3).clamp(10, 26);
        for offset in 0..list_height {
            let provider = providers.get(providers_start + offset);
            let model = rows.get(models_start + offset).copied();
            if provider.is_none() && model.is_none() {
                out.push(Line::default());
                continue;
            }
            out.push(row_line(
                model,
                provider,
                models_start + offset == picker.row,
                providers_start + offset == provider_sel,
                provider_width,
                flags,
                area,
                opts,
            ));
        }
    }

    out.push(detail_line(highlighted, opts));
    out.push(effort_line(picker, highlighted, opts));
    out.push(hint_line(opts));
    out
}

fn kind_line(picker: &ModelPicker, opts: &RenderOptions) -> Line<'static> {
    let sep = opts.glyphs.dot_sep;
    let mut spans = vec![Span::styled("Kind: ", opts.theme.dim)];
    for (index, kind) in Kind::ALL.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(format!(" {sep} "), opts.theme.dim));
        }
        let selected = *kind == picker.kind;
        spans.push(Span::styled(
            kind.label().to_string(),
            if selected {
                opts.theme.bold.patch(opts.theme.accent)
            } else {
                opts.theme.dim
            },
        ));
    }
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn row_line(
    model: Option<&ModelInfo>,
    provider: Option<&ProviderRow>,
    model_selected: bool,
    provider_selected: bool,
    provider_width: usize,
    flags: &Flags,
    area: Rect,
    opts: &RenderOptions,
) -> Line<'static> {
    let sep = opts.glyphs.dot_sep;
    let mut spans = vec![Span::styled(
        format!(
            "{} ",
            if model_selected {
                opts.glyphs.pointer
            } else {
                " "
            }
        ),
        opts.theme.accent,
    )];

    // Left cell: provider name padded to the column, then its live count. The
    // name is truncated, never the count — a count clipped to `12…` would be a
    // different number.
    let name = provider.map_or("", |row| row.key.as_deref().unwrap_or("all"));
    spans.push(Span::styled(
        cell(name, provider_width.saturating_sub(4)),
        if provider_selected {
            opts.theme.bold.patch(opts.theme.accent)
        } else {
            opts.theme.fg
        },
    ));
    // The count belongs to a *provider*. On a model row it is not zero, it is
    // absent, and printing `0` there put a column of zeros down the model list.
    // The cell is still padded, so the two columns stay aligned.
    spans.push(Span::styled(
        match provider {
            Some(row) => format!(" {:>2} ", row.count),
            None => "    ".to_string(),
        },
        if provider_selected {
            opts.theme.bold
        } else {
            opts.theme.dim
        },
    ));

    if let Some(model) = model {
        let route = model.route.to_string();
        spans.push(Span::styled(
            model.display_name.clone(),
            if model_selected {
                opts.theme.bold
            } else {
                opts.theme.fg
            },
        ));
        spans.push(Span::styled(
            format!("{sep}{}", ctx_label(model.context_window)),
            opts.theme.dim,
        ));
        spans.push(Span::styled(
            format!("{sep}{}", price_label(model.pricing)),
            opts.theme.dim,
        ));
        let tags = capability_tags(model);
        if !tags.is_empty() {
            spans.push(Span::styled(format!("{sep}{tags}"), opts.theme.violet));
        }
        let marks = flags.get(&route).copied().unwrap_or_default();
        if marks.zdr_capable {
            spans.push(Span::styled(format!("{sep}ZDR"), opts.theme.green));
        }
        if marks.favorite {
            let star = if opts.glyphs.ascii { "*" } else { "★" };
            spans.push(Span::styled(format!("{sep}{star}"), opts.theme.yellow));
        }
    }

    let line = Line::from(spans);
    if model_selected {
        selected_row(line, area.width, opts)
    } else {
        line
    }
}

/// What the highlighted route is, in full, plus everything its row cannot fit:
/// the ladder it accepts, or that the catalog said nothing about it.
fn detail_line(model: Option<&ModelInfo>, opts: &RenderOptions) -> Line<'static> {
    let Some(model) = model else {
        return Line::default();
    };
    let sep = opts.glyphs.dot_sep;
    let mut spans = vec![
        Span::styled("detail ", opts.theme.dim),
        Span::styled(model.route.to_string(), opts.theme.bold),
    ];
    if !model.display_name.is_empty() && model.display_name != model.route.model {
        spans.push(Span::styled(
            format!("{sep}{}", model.display_name),
            opts.theme.dim,
        ));
    }
    spans.push(Span::styled(
        format!("{sep}{}", ctx_label(model.context_window)),
        opts.theme.dim,
    ));
    spans.push(Span::styled(
        format!("{sep}{}", price_label(model.pricing)),
        opts.theme.dim,
    ));
    let tags = capability_tags(model);
    if !tags.is_empty() {
        spans.push(Span::styled(format!("{sep}{tags}"), opts.theme.violet));
    }
    spans.push(Span::styled(
        format!("{sep}{}", ladder_label(model)),
        opts.theme.dim,
    ));
    Line::from(spans)
}

/// The effort row: one fixed-width chip per ladder rung, in ladder order. An
/// unsupported chip keeps its slot — the row does not reflow as the cursor
/// moves or as the highlighted model changes — and is drawn faint.
fn effort_line(
    picker: &ModelPicker,
    model: Option<&ModelInfo>,
    opts: &RenderOptions,
) -> Line<'static> {
    let mut spans = vec![Span::styled(CHIP_LABEL.to_string(), opts.theme.dim)];
    for effort in Effort::ALL {
        let available = model.is_none_or(|model| is_effort_available(model, effort));
        let selected = model.is_some() && available && picker.effort == effort;
        let marker = if selected { opts.glyphs.pointer } else { " " };
        let style = if !available {
            opts.theme.faint
        } else if selected {
            opts.theme.bold.patch(opts.theme.accent)
        } else {
            opts.theme.fg
        };
        spans.push(Span::styled(
            format!("{marker}{:<width$}", effort, width = CHIP_WIDTH - 1),
            style,
        ));
    }
    Line::from(spans)
}

fn hint_line(opts: &RenderOptions) -> Line<'static> {
    let sep = opts.glyphs.dot_sep;
    let up_down = if opts.glyphs.ascii { "^/v" } else { "↑/↓" };
    let left_right = if opts.glyphs.ascii { "<>" } else { "←/→" };
    Line::from(Span::styled(
        [
            format!("{up_down} model"),
            format!("{left_right} provider"),
            "tab kind".to_string(),
            "h/l effort".to_string(),
            "enter switch".to_string(),
            "esc cancel".to_string(),
        ]
        .join(&format!(" {sep} ")),
        opts.theme.dim,
    ))
}

/// First visible index of a `budget`-row window over `count` rows that keeps
/// `selected` inside and scrolls only once the cursor passes the bottom.
fn window_start(count: usize, selected: usize, budget: usize) -> usize {
    if count == 0 || budget == 0 {
        return 0;
    }
    if selected >= budget {
        selected + 1 - budget
    } else {
        0
    }
}

/// `text` fitted to `width` cells and padded with spaces to exactly `width`,
/// so the two columns stay aligned whatever the model names look like.
fn cell(text: &str, width: usize) -> String {
    let fit = truncate_to_width(text, width, true);
    let pad = width.saturating_sub(display_width(&fit));
    format!("{fit}{}", " ".repeat(pad))
}

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    picker: &ModelPicker,
    models: &[ModelInfo],
    flags: &Flags,
    opts: &RenderOptions,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    let lines = lines(picker, models, flags, area, opts);
    frame.render_widget(Paragraph::new(lines), area);
}
