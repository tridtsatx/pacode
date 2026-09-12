//! Rendering of the menu overlay (§3.2–§3.3): the border with the breadcrumb
//! on the top edge and the action hints on the bottom edge, then the zones —
//! tabs, filter, list, detail, status. The list-zone internals live in
//! `rows.rs`. Everything a frame needs is built here and dies with the frame
//! (§3.6).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use pacode_render::{RenderOptions, display_width, truncate_to_width};

use crate::binding::format_binding;

use super::layout::{overlay_area, shows_filter, shows_tabs};
use super::model::selectable;
use super::rows::draw_list;
use super::view::{
    MenuAction, MenuId, MenuMode, MenuUiState, MenuView, StatusKind, StatusLine, Tabs, is_base_key,
};

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;

/// The hint edge always ends with this action; `esc` peels one level (§3.4).
const ESC_HINT: &str = "esc back";

/// Draw one menu overlay inside `area` — the dialog column rect; the overlay
/// rect itself is derived (80 % wide, centred, content height, §3.3).
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &MenuView,
    ui: &MenuUiState,
    opts: &RenderOptions,
) {
    let rect = overlay_area(area, view, ui);
    if rect.is_empty() {
        return;
    }
    report_conflicts(view);

    let hint = bottom_hint(view, ui, rect.width.saturating_sub(2), opts);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Line::from(breadcrumb_spans(view, ui, opts)))
        .title_bottom(Span::styled(hint, opts.theme.dim))
        .border_style(opts.theme.dim);

    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    if inner.is_empty() {
        return;
    }

    // Zones top to bottom: tabs, filter, list (+ detail), status pinned to the
    // last inner row (§3.2).
    let mut top = inner.y;
    let mut filter_y = None;
    if shows_tabs(view)
        && let Some(tabs) = &view.tabs
    {
        let line = tab_line(tabs, ui.tab, opts);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(inner.x, top, inner.width, 1),
        );
        top += 1;
    }
    if shows_filter(view) {
        filter_y = Some(top);
        let line = filter_line(view, ui, inner.width, opts);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(inner.x, top, inner.width, 1),
        );
        top += 1;
    }
    let status_h = u16::from(view.status.is_some());
    let list_h = inner.bottom().saturating_sub(top + status_h);
    let list_area = Rect::new(inner.x, top, inner.width, list_h);
    let mut edit_cursor = None;
    if !list_area.is_empty() {
        match &ui.mode {
            MenuMode::Capture { action } => {
                frame.render_widget(Paragraph::new(capture_lines(action, opts)), list_area);
            }
            MenuMode::List | MenuMode::Edit { .. } => {
                edit_cursor = draw_list(frame, list_area, view, ui, opts);
            }
        }
    }
    if let Some(status) = &view.status {
        let line = status_line(status, opts);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }

    if let Some(pos) = edit_cursor {
        frame.set_cursor_position(pos);
    } else if let (Some(y), MenuMode::List) = (filter_y, &ui.mode) {
        // The text cursor rests at the end of the query while browsing.
        let qx = inner.x + 2 + (display_width(&ui.query) as u16).min(inner.width.saturating_sub(3));
        frame.set_cursor_position((qx, y));
    }
}

/// The bottom-edge hint: `key action · key action` from the enabled declared
/// actions, then `esc back`. Actions drop from the end when the width runs
/// out; `esc` always stays (§3.2).
pub fn hint_line(actions: &[MenuAction], width: u16, opts: &RenderOptions) -> String {
    let sep = opts.glyphs.dot_sep;
    // One space of margin on each edge of the hint text.
    let budget = (width as usize).saturating_sub(2);
    if budget <= display_width(ESC_HINT) {
        return truncate_to_width(&format!(" {ESC_HINT} "), width as usize, opts.glyphs.ascii);
    }
    // Room the declared actions may take; the separator plus `esc` is reserved.
    let limit = budget - display_width(sep) - display_width(ESC_HINT);
    let mut items: Vec<String> = Vec::new();
    let mut used = 0usize;
    for action in actions.iter().filter(|a| a.enabled && !is_base_key(&a.key)) {
        let item = format!("{} {}", format_binding(&action.key), action.label);
        let need = display_width(&item)
            + if items.is_empty() {
                0
            } else {
                display_width(sep)
            };
        if used + need > limit {
            break;
        }
        used += need;
        items.push(item);
    }
    let mut out = String::with_capacity(width as usize + 2);
    out.push(' ');
    out.push_str(&items.join(sep));
    if !items.is_empty() {
        out.push_str(sep);
    }
    out.push_str(ESC_HINT);
    out.push(' ');
    out
}

/// `Menu › Plugins › Discover`: the root segment bold, the tail faint (§3.2).
/// The active tab label trails the title when the tab strip is drawn.
fn breadcrumb_spans(view: &MenuView, ui: &MenuUiState, opts: &RenderOptions) -> Vec<Span<'static>> {
    let sep = if opts.glyphs.ascii { " > " } else { " › " };
    let mut segs: Vec<String> = Vec::new();
    if view.id == MenuId::Root {
        segs.push(view.title.to_string());
    } else {
        segs.push(MenuId::Root.title().to_string());
        segs.extend(view.breadcrumb.iter().cloned());
        segs.push(view.title.to_string());
        if shows_tabs(view)
            && let Some(tabs) = &view.tabs
            && !tabs.items.is_empty()
        {
            let t = ui.tab.min(tabs.items.len() - 1);
            segs.push(tabs.items[t].label.clone());
        }
    }
    let mut spans = vec![Span::raw(" ")];
    for (i, seg) in segs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(sep, opts.theme.faint));
        }
        spans.push(Span::styled(
            seg.clone(),
            if i == 0 {
                opts.theme.bold
            } else {
                opts.theme.faint
            },
        ));
    }
    spans.push(Span::raw(" "));
    spans
}

/// The tab strip: ` Label count` per tab, the active one on the selection
/// background (§3.2).
fn tab_line(tabs: &Tabs, current: usize, opts: &RenderOptions) -> Line<'static> {
    let cur = current.min(tabs.items.len().saturating_sub(1));
    let mut spans = Vec::new();
    for (i, tab) in tabs.items.iter().enumerate() {
        let style = if i == cur {
            opts.theme.selected_bg
        } else {
            opts.theme.faint
        };
        spans.push(Span::styled(format!(" {} ", tab.label), style));
        spans.push(Span::styled(format!("{} ", tab.count), opts.theme.dim));
    }
    Line::from(spans)
}

/// `> query` plus the `N of M` counter flush right (§3.2). `N` counts the
/// selectable rows on screen — sections are not items (§3.3).
fn filter_line(
    view: &MenuView,
    ui: &MenuUiState,
    width: u16,
    opts: &RenderOptions,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled("> ", opts.theme.accent),
        Span::styled(ui.query.clone(), opts.theme.fg),
    ];
    if let Some(total) = view.filter_total {
        let shown = selectable(&view.rows).count();
        let counter = format!("{shown} of {total}");
        let used = 2 + display_width(&ui.query) + display_width(&counter);
        if used < width as usize {
            spans.push(Span::raw(" ".repeat(width as usize - used)));
            spans.push(Span::styled(counter, opts.theme.faint));
        }
    }
    Line::from(spans)
}

/// The one-line status; `Loading` appends the ellipsis glyph so the ASCII
/// fallback applies (§3.2).
fn status_line(status: &StatusLine, opts: &RenderOptions) -> Line<'static> {
    let (style, tail) = match status.kind {
        StatusKind::Info => (opts.theme.dim, ""),
        StatusKind::Loading => (opts.theme.faint, opts.glyphs.ellipsis),
        StatusKind::Warn => (opts.theme.yellow, ""),
        StatusKind::Error => (opts.theme.red, ""),
    };
    Line::from(Span::styled(format!("{}{tail}", status.text), style))
}

/// The capture prompt that replaces the whole list in `MenuMode::Capture`
/// (§5.4).
fn capture_lines(action: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled("Press a key for ", opts.theme.fg),
            Span::styled(action.to_string(), opts.theme.accent.patch(opts.theme.bold)),
        ]),
        Line::from(Span::styled("esc cancels", opts.theme.faint)),
    ]
}

/// The hint edge for the mode the menu is in; `List` builds it from the
/// declared actions.
fn bottom_hint(view: &MenuView, ui: &MenuUiState, width: u16, opts: &RenderOptions) -> String {
    match &ui.mode {
        MenuMode::List => hint_line(&view.actions, width, opts),
        MenuMode::Edit { .. } => format!(" enter confirm{}esc cancel ", opts.glyphs.dot_sep),
        MenuMode::Capture { .. } => format!(" press a key{}esc cancel ", opts.glyphs.dot_sep),
    }
}

/// §3.4: a declared action colliding with a base key is a bug — debug builds
/// assert, release ignores the action (it never reaches the hint line or the
/// dispatch) and warns once per distinct conflict.
fn report_conflicts(view: &MenuView) {
    thread_local! {
        static WARNED: std::cell::RefCell<std::collections::BTreeSet<String>> =
            const { std::cell::RefCell::new(std::collections::BTreeSet::new()) };
    }
    let conflicts = view.base_key_conflicts();
    debug_assert!(
        conflicts.is_empty(),
        "menu '{}' declares base keys: {conflicts:?}",
        view.id.slug()
    );
    for c in conflicts {
        WARNED.with(|warned| {
            if warned.borrow_mut().insert(c.clone()) {
                log::warn!("menu '{}': {c} — ignored", view.id.slug());
            }
        });
    }
}
