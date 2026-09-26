//! Painting the settings overlay.
//!
//! Every row is placed by hand rather than by the layout solver: the overlay
//! has to survive a 10-row terminal, where a solver asked for four bands hands
//! back whatever it can. Rows that do not fit are dropped from the bottom, the
//! footer hint lives on the bottom border so it can never reflow the body, and
//! nothing here asks the terminal for a different size.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use pacode_config::registry::{self, SettingEntry, SettingKind};
use pacode_render::RenderOptions;
use pacode_render::display_width;
use pacode_render::wrap::{truncate_to_width, wrap_text};
use pacode_types::Config;

use super::model::{self, SettingsState};
use super::tabs;
use crate::ui::overlays::selected_row;

#[cfg(test)]
#[path = "draw_tests.rs"]
mod draw_tests;

/// The hint row. One row, always the same text: a footer that grew with the
/// error under it would move the body every time a value was refused.
const HINT_GLYPHS: &str = "enter/space change · tab sections · ←/→ tabs · type search · esc close";
const HINT_ASCII: &str = "enter/space change . tab sections . <-/-> tabs . type search . esc close";

/// Description rows the overlay will spend under the body, however long the
/// text is — past that the sentence is cut mid-word, which beats a settings
/// window that no longer shows any settings.
const MAX_DESC_ROWS: usize = 2;

/// Below this the section column is dropped rather than squeezed: a truncated
/// section name beside a truncated value helps nobody.
const MIN_LEFT_COLUMN: u16 = 18;

/// Cells the value column may not squeeze the label below, so a long value
/// truncates itself instead of pushing the label off the frame.
const MIN_LABEL_CELLS: usize = 8;

/// What the description line says when a search matched nothing.
const NO_MATCH: &str = "no settings match — type more, or esc to clear the search";

/// The overlay: tabs, sections, rows, description, scrollbar.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    config: &Config,
    ui: &SettingsState,
    opts: &RenderOptions,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let order = model::section_order();
    // A search spans sections, so the section column follows the highlight
    // rather than the tab that was last pressed.
    let shown = match ui.row(ui.cursor()) {
        Some(entry) if !ui.query().is_empty() => order
            .iter()
            .position(|&name| name == entry.section)
            .unwrap_or(ui.section()),
        _ => ui.section(),
    };

    let description = describe(ui);
    let text_width = area.width.saturating_sub(2).max(1) as usize;
    let wrapped = wrap_text(&description, text_width);
    let inner = area.inner(Margin::new(1, 1));
    let (tabs_area, body, desc_area) = slots(inner, wrapped.len().min(MAX_DESC_ROWS) as u16);

    let title = if ui.query().is_empty() {
        "Settings".to_string()
    } else {
        format!("Settings /{}", ui.query())
    };
    let hint = if opts.glyphs.ascii {
        HINT_ASCII
    } else {
        HINT_GLYPHS
    };
    frame.render_widget(frame_block(title, hint, opts), area);

    draw_tabs(frame, tabs_area, &order, shown, opts);
    draw_body(frame, body, config, ui, &order, shown, opts);
    if desc_area.height > 0 {
        let style = if ui.error().is_some() {
            opts.theme.red.patch(opts.theme.bold)
        } else {
            opts.theme.dim
        };
        let lines: Vec<Line> = wrapped
            .iter()
            .take(desc_area.height as usize)
            .map(|line| Line::from(Span::styled(line.clone(), style)))
            .collect();
        frame.render_widget(Paragraph::new(lines), desc_area);
    }
}

/// The overlay's border. `ui.ascii_only` takes the box drawing with it, not
/// just the icons: a rounded border on a terminal that cannot draw one is the
/// same tofu a missing icon would be.
fn frame_block(title: String, hint: &'static str, opts: &RenderOptions) -> Block<'static> {
    let border = if opts.glyphs.ascii {
        BorderType::Plain
    } else {
        BorderType::Rounded
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(border)
        .border_style(opts.theme.dim)
        .title(Span::styled(title, opts.theme.bold.patch(opts.theme.accent)))
        .title_bottom(Span::styled(hint, opts.theme.dim))
}

/// The description line's text: why the last value was refused, else what the
/// highlighted setting is for.
fn describe(ui: &SettingsState) -> String {
    if let Some(err) = ui.error() {
        return err.to_string();
    }
    match ui.row(ui.cursor()) {
        Some(entry) => entry.description.to_string(),
        None if !ui.query().is_empty() => NO_MATCH.to_string(),
        None => String::new(),
    }
}

/// Tabs, body and description rows inside the border. A body of zero rows is a
/// legitimate answer for a short terminal — the tabs and the hint still draw.
fn slots(inner: Rect, desc_rows: u16) -> (Rect, Rect, Rect) {
    let none = Rect::new(inner.x, inner.y, 0, 0);
    if inner.width == 0 || inner.height == 0 {
        return (none, none, none);
    }
    let tabs = Rect::new(inner.x, inner.y, inner.width, 1);
    let rest = inner.height - 1;
    let desc_h = desc_rows.min(rest.saturating_sub(1));
    let body_h = rest - desc_h;
    let body = Rect::new(inner.x, inner.y + 1, inner.width, body_h);
    let desc = Rect::new(inner.x, body.y + body_h, inner.width, desc_h);
    (tabs, body, desc)
}

fn draw_tabs(
    frame: &mut Frame,
    area: Rect,
    order: &[&'static str],
    shown: usize,
    opts: &RenderOptions,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let bar = tabs::tabs(order, &opts.glyphs, shown);
    let window = tabs::window(&bar, area.width as usize, opts.glyphs.ellipsis);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(window.range.len() + 2);
    if window.clipped_left {
        spans.push(Span::styled(opts.glyphs.ellipsis, opts.theme.dim));
    }
    for tab in &bar[window.range.clone()] {
        let style = if tab.active {
            opts.theme.bold.patch(opts.theme.accent)
        } else {
            opts.theme.faint
        };
        spans.push(Span::styled(format!(" {}", tab.icon), style));
        if tab.active {
            spans.push(Span::styled(format!(" {}", tab.name), style));
        }
    }
    if window.clipped_right {
        spans.push(Span::styled(format!(" {}", opts.glyphs.ellipsis), opts.theme.dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(
    frame: &mut Frame,
    area: Rect,
    config: &Config,
    ui: &SettingsState,
    order: &[&'static str],
    shown: usize,
    opts: &RenderOptions,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // The scrollbar owns the last column; the two columns share what is left.
    let columns = area.width.saturating_sub(1);
    let left_w = if columns < MIN_LEFT_COLUMN {
        0
    } else {
        (columns / 4).clamp(9, 22)
    };
    let gap = u16::from(left_w > 0);
    let right_x = area.x + left_w + gap;
    let right_w = columns.saturating_sub(left_w + gap);

    if left_w > 0 {
        let lines: Vec<Line> = ui
            .section_window(order.len(), area.height as usize)
            .map(|index| {
                let name = order.get(index).copied().unwrap_or("");
                let line = Line::from(Span::styled(
                    truncate_to_width(name, left_w as usize, !opts.glyphs.ascii),
                    opts.theme.faint,
                ));
                if index == shown {
                    selected_row(line, left_w, opts)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(area.x, area.y, left_w, area.height),
        );
    }

    let total = ui.row_count();
    let window = ui.window(total, area.height as usize);
    let editing = ui.editing();
    let lines: Vec<Line> = window
        .filter_map(|index| ui.row(index).map(|entry| (index, entry)))
        .map(|(index, entry)| {
            row_line(
                entry,
                config,
                editing.filter(|&(key, _)| key == entry.dotted_key),
                index == ui.cursor(),
                right_w,
                opts,
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(right_x, area.y, right_w, area.height),
    );

    let mut scrollbar = ScrollbarState::new(total.max(1))
        .position(window.start)
        .viewport_content_length(area.height as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol(opts.glyphs.bar_full)
            .track_symbol(Some(opts.glyphs.bar_empty))
            .style(opts.theme.dim),
        Rect::new(area.x + columns, area.y, 1, area.height),
        &mut scrollbar,
    );
}

/// One `Setting  value` row. The label takes what the value leaves, so a long
/// label loses its tail rather than pushing the value off the frame.
fn row_line(
    entry: &'static SettingEntry,
    config: &Config,
    editing: Option<(&str, usize)>,
    selected: bool,
    width: u16,
    opts: &RenderOptions,
) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::default();
    }
    let (value, value_style) = value_of(entry, config, editing, opts);
    let value_w = display_width(&value).min(width.saturating_sub(MIN_LABEL_CELLS));
    let value = truncate_to_width(&value, value_w, !opts.glyphs.ascii);
    let mut label = entry.label.to_string();
    if is_modified(config, entry) {
        label.insert_str(0, "* ");
    }
    let label = truncate_to_width(&label, width.saturating_sub(value_w + 1), !opts.glyphs.ascii);
    let label_style = if selected {
        opts.theme.bold.patch(opts.theme.fg)
    } else {
        opts.theme.faint
    };
    let mut spans = vec![
        Span::styled(
            if selected { opts.glyphs.pointer } else { " " },
            if selected { opts.theme.accent } else { opts.theme.dim },
        ),
        Span::styled(format!(" {label}"), label_style),
    ];
    let pad = width.saturating_sub(2 + display_width(&label) + display_width(&value));
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.push(Span::styled(value, value_style));
    let line = Line::from(spans);
    if selected {
        selected_row(line, width as u16, opts)
    } else {
        line
    }
}

/// The value cell: the editor's buffer while a value is being edited, `on`/
/// `off` for a bool, the live value otherwise.
fn value_of(
    entry: &SettingEntry,
    config: &Config,
    editing: Option<(&str, usize)>,
    opts: &RenderOptions,
) -> (String, Style) {
    if let Some((buffer, cursor)) = editing {
        let mut text: String = buffer.chars().take(cursor).collect();
        text.push_str(opts.glyphs.vline);
        text.extend(buffer.chars().skip(cursor));
        return (text, opts.theme.accent.patch(opts.theme.bold));
    }
    let current = registry::read_value(config, entry.dotted_key).unwrap_or_default();
    match entry.kind {
        SettingKind::Bool if current == "true" => ("on".to_string(), opts.theme.green),
        SettingKind::Bool => ("off".to_string(), opts.theme.dim),
        _ if current.is_empty() => ("(empty)".to_string(), opts.theme.faint),
        _ => (current, opts.theme.fg),
    }
}

/// `Action` rows hold a command, not a value, so their read-back never equals
/// the default and would count as permanently changed.
fn is_modified(config: &Config, entry: &SettingEntry) -> bool {
    !matches!(entry.kind, SettingKind::Action { .. }) && registry::is_modified(config, entry)
}
