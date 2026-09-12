//! Overlays drawn over the dialog: the menu base (§3) and the rail overlay
//! (Tiny tier). Width 78% of the dialog column, top offset 2, rounded border,
//! bold title on the top edge, dim key hints on the bottom edge.
//!
//! Shared chrome lives here: `menu_block` draws the border, `selected_row`
//! paints the highlighted row as a full-width bar, and `window_groups`
//! scrolls variable-height rows so the selected one stays fully visible.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear};

use pacode_render::RenderOptions;

use crate::state::{AppState, Focus, Overlay};

pub fn draw(frame: &mut Frame, dialog_area: Rect, state: &mut AppState, opts: &RenderOptions) {
    if dialog_area.width < 20 || dialog_area.height < 6 {
        return;
    }

    // Bottom pickers (effort/mode/model/login/question/theme) are drawn by
    // `picker`, not here.
    if !matches!(state.focus, Focus::Overlay(_)) || state.is_bottom_picker() {
        return;
    }

    // The menu base owns its geometry (§3.3): 80 % wide, height from the
    // `MenuView`.
    if let Focus::Overlay(Overlay::Menu { stack, ui }) = &state.focus {
        if !stack.is_empty() {
            let view = crate::ui::menu::view_for(stack, state, ui);
            // §8.1: the Files detail card's file read runs on the deferred
            // preview job — the draw only records what it wants. Any other
            // level on top arms nothing.
            state.files.preview_want = if view.id == crate::ui::menu::MenuId::Files {
                crate::ui::menu::files::preview_want(&view, ui)
            } else {
                None
            };
            // Same split for the Background card's output tail (§5.9): the
            // draw records the selected `(task, output_bytes)`; the blocking
            // spool read runs in `dispatch_deferred`.
            state.rail.task_tail_want = if view.id == crate::ui::menu::MenuId::Background {
                crate::ui::menu::background::tail_want(state, &view, ui)
            } else {
                None
            };
            // Git level (§5.10): the selected commit's `--stat` want (only
            // where the detail pane is wide enough to show it) and the
            // end-of-list page trigger. `dispatch_deferred` turns them into
            // `Action::GitShow` / `Action::GitLog`.
            let (show_want, more_want) = if view.id == crate::ui::menu::MenuId::Git {
                let inner_w = crate::ui::menu::overlay_area(dialog_area, &view, ui)
                    .width
                    .saturating_sub(2);
                let detail_wide = crate::ui::menu::detail_panel_width(inner_w).is_some();
                crate::ui::menu::git::wants(state, &view, ui, detail_wide)
            } else {
                (None, None)
            };
            state.git.show_want = show_want;
            state.git.more_want = more_want;
            crate::ui::menu::draw(frame, dialog_area, &view, ui, opts);
        }
        return;
    }

    // The rail overlay is the only centred overlay left — every other
    // `Focus::Overlay` variant is a bottom picker or a menu.
    if matches!(state.focus, Focus::Overlay(Overlay::RailOverlay)) {
        let w = ((dialog_area.width as u32 * 78) / 100).max(20) as u16;
        let h = dialog_area.height.saturating_sub(4).max(6);
        let x = dialog_area.x + (dialog_area.width.saturating_sub(w)) / 2;
        let y = dialog_area.y + 2;
        let area = Rect::new(x, y, w, h);

        frame.render_widget(Clear, area);
        draw_rail_overlay(frame, area, state, opts);
    }
}

/// The shared overlay frame: rounded border, bold title on the top edge, dim
/// `key action · key action` hints on the bottom edge.
pub(crate) fn menu_block(title: &str, hint: &str, opts: &RenderOptions) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(format!(" {title} "), opts.theme.bold))
        .title_bottom(Span::styled(hint.to_string(), opts.theme.dim))
        .border_style(opts.theme.dim)
}

/// The selected row: every span keeps its colour over `selected_bg`, padded to
/// the row's full width so the highlight is a bar, not a word. The pointer span
/// stays accent on top of the bar.
pub(crate) fn selected_row(line: Line<'static>, width: u16, opts: &RenderOptions) -> Line<'static> {
    let width = width as usize;
    let used: usize = line
        .spans
        .iter()
        .map(|s| pacode_render::display_width(&s.content))
        .sum();
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|s| {
            Span::styled(
                s.content.into_owned(),
                opts.theme.selected_bg.patch(s.style),
            )
        })
        .collect();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            opts.theme.selected_bg,
        ));
    }
    Line::from(spans)
}

/// First index of a window over variable-height row groups such that the
/// selected group is the last fully visible one and the whole window fits the
/// line budget — the same rule `window` uses for fixed-height rows.
pub(crate) fn window_groups(heights: &[usize], sel: usize, budget: usize) -> usize {
    if heights.is_empty() || budget == 0 {
        return 0;
    }
    let sel = sel.min(heights.len() - 1);
    let mut start = sel;
    let mut used = heights[sel];
    while start > 0 {
        let next = heights[start - 1];
        if used + next > budget {
            break;
        }
        used += next;
        start -= 1;
    }
    start
}

/// `2h ago`-style age of a timestamp; `just now` under a minute.
pub(crate) fn age_ago(ts_ms: u64, now: u64) -> String {
    let mins = now.saturating_sub(ts_ms) / 60_000;
    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 60 * 24 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", mins / (60 * 24))
    }
}

/// `~/rel/path` when under HOME, else the path as written.
/// `pub(crate)`: the Sessions menu's detail card shows the same shape.
pub(crate) fn short_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::Path::new(&home);
        if let Ok(rel) = path.strip_prefix(home_path) {
            return format!("~/{}", rel.display());
        }
    }
    raw.into_owned()
}

/// The `… — loading…` / `… — offline copy` title suffix a picker gets from
/// its list's cache health (§8.2).
pub(crate) fn title_with_badge(title: &str, badge: Option<crate::cache::Badge>) -> String {
    match badge {
        Some(crate::cache::Badge::Loading) => format!("{title} — loading…"),
        Some(crate::cache::Badge::OfflineCopy) => format!("{title} — offline copy"),
        None => title.to_string(),
    }
}

fn draw_rail_overlay(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions) {
    let block = menu_block("Plan & Agents", " esc close ", opts);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    crate::ui::rail::draw(frame, inner, state, opts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_groups_keeps_the_selected_group_fully_visible() {
        // Two-line groups, budget for three groups.
        let heights = vec![2, 2, 2, 2, 2];
        assert_eq!(window_groups(&heights, 0, 6), 0);
        assert_eq!(window_groups(&heights, 2, 6), 0);
        assert_eq!(window_groups(&heights, 3, 6), 1);
        // A taller selected group pulls the window further down.
        let heights = vec![2, 3, 2, 4, 2];
        assert_eq!(window_groups(&heights, 3, 6), 2);
        // A group that alone fills the budget starts at itself.
        let heights = vec![2, 2, 2, 5, 2];
        assert_eq!(window_groups(&heights, 3, 5), 3);
        // Degenerate inputs do not panic.
        assert_eq!(window_groups(&[], 0, 5), 0);
        assert_eq!(window_groups(&heights, 0, 0), 0);
        assert_eq!(window_groups(&heights, 99, 6), 4);
    }

    #[test]
    fn selected_row_pads_to_the_full_width() {
        let opts = RenderOptions::new(40, false);
        let line = Line::from(vec![
            Span::styled("▸ ".to_string(), opts.theme.accent),
            Span::styled("name".to_string(), opts.theme.bold),
        ]);
        let row = selected_row(line, 20, &opts);
        let width: usize = row
            .spans
            .iter()
            .map(|s| pacode_render::display_width(&s.content))
            .sum();
        assert_eq!(width, 20);
        // The pointer keeps the accent colour under the selection bar.
        assert_eq!(row.spans[0].style.fg, opts.theme.accent.fg);
    }

    #[test]
    fn age_ago_reads_like_a_clock() {
        let now = 1_000_000_000_000u64;
        assert_eq!(age_ago(now, now), "just now");
        assert_eq!(age_ago(now - 5 * 60_000, now), "5m ago");
        assert_eq!(age_ago(now - 3 * 3_600_000, now), "3h ago");
        assert_eq!(age_ago(now - 2 * 86_400_000, now), "2d ago");
    }
}
