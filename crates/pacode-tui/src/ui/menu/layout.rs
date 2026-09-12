//! Geometry of the menu overlay (§3.3): width and height derived from the
//! dialog column and the rows themselves, the variable-height scroll window,
//! and the detail-panel threshold.

use ratatui::layout::Rect;

use super::model::{MenuRow, resolve_cursor};
use super::view::{FilterMode, MenuMode, MenuUiState, MenuView};

#[cfg(test)]
#[path = "layout_tests.rs"]
mod layout_tests;

/// The overlay takes this share of the dialog column (§3.3).
pub const WIDTH_PERCENT: u32 = 80;
/// … and never less than this.
pub const MIN_WIDTH: u16 = 20;
/// Rows above the overlay — the same top offset the legacy overlays use.
pub const TOP_OFFSET: u16 = 2;
/// The detail panel needs at least this much inner width to open (§3.2, §12.4).
pub const DETAIL_MIN_INNER: u16 = 72;
/// … and the panel itself must get at least this many columns.
pub const DETAIL_MIN_PANEL: u16 = 26;
/// The share of the inner width the panel gets once the gate opens (2/5).
const DETAIL_SHARE_NUM: u16 = 2;
const DETAIL_SHARE_DEN: u16 = 5;
/// Lines the collapsed detail may take under the selected row (§3.2).
pub const COLLAPSED_DETAIL_LINES: usize = 2;
/// Lines the capture prompt takes while `MenuMode::Capture` hides the list.
const CAPTURE_LINES: usize = 2;

/// Overlay width inside a `dialog_width` column: 80 %, min 20 (§3.3). Callers
/// clamp to the column itself; [`overlay_area`] already does.
pub fn overlay_width(dialog_width: u16) -> u16 {
    ((dialog_width as u32 * WIDTH_PERCENT) / 100).max(MIN_WIDTH as u32) as u16
}

/// Width the detail panel gets, or `None` when it collapses into two lines
/// under the selected row (§3.2). Both spec gates are checked — 72+ inner
/// columns and 26+ for the panel; at the current 2/5 share the first implies
/// the second (28 ≥ 26), so the floor is a guard against share changes.
pub fn detail_panel_width(inner_width: u16) -> Option<u16> {
    if inner_width < DETAIL_MIN_INNER {
        return None;
    }
    let panel = inner_width * DETAIL_SHARE_NUM / DETAIL_SHARE_DEN;
    (panel >= DETAIL_MIN_PANEL).then_some(panel)
}

/// Display lines one row occupies: one for the row itself, plus up to
/// [`COLLAPSED_DETAIL_LINES`] of inline detail on the selected row while the
/// detail panel is collapsed (§3.2).
pub fn row_height(row: &MenuRow, selected: bool, detail_inline: bool) -> usize {
    let extra = if selected && detail_inline {
        row.detail()
            .map_or(0, |d| d.collapsed_lines().min(COLLAPSED_DETAIL_LINES))
    } else {
        0
    };
    1 + extra
}

/// Whether the tab strip occupies a line: only for two or more peer lists
/// (§3.2).
pub fn shows_tabs(view: &MenuView) -> bool {
    view.tabs.as_ref().is_some_and(|t| t.items.len() >= 2)
}

/// Whether the filter zone occupies a line — `Local` and `Remote` do, `Off`
/// and `None` do not.
pub fn shows_filter(view: &MenuView) -> bool {
    matches!(
        view.filter,
        Some(FilterMode::Local | FilterMode::Remote { .. })
    )
}

/// Lines the empty state contributes when there are no rows.
fn empty_lines_count(view: &MenuView) -> usize {
    usize::from(!view.empty.primary.is_empty()) + usize::from(!view.empty.hint.is_empty())
}

/// Inner height the view asks for: tabs + filter + list + status (§3.2). The
/// list part is the sum of the row heights — the height is computed from
/// `rows`, not from a per-menu match (§3.3).
pub fn inner_height(view: &MenuView, ui: &MenuUiState, detail_inline: bool) -> usize {
    let mut h = usize::from(shows_tabs(view)) + usize::from(shows_filter(view));
    h += match ui.mode {
        MenuMode::Capture { .. } => CAPTURE_LINES,
        MenuMode::List | MenuMode::Edit { .. } => {
            if view.rows.is_empty() {
                empty_lines_count(view)
            } else {
                let sel = resolve_cursor(&view.rows, &ui.cursor);
                view.rows
                    .iter()
                    .enumerate()
                    .map(|(i, r)| row_height(r, Some(i) == sel, detail_inline))
                    .sum()
            }
        }
    };
    h + usize::from(view.status.is_some())
}

/// The rect `draw` renders into: centred horizontally, [`TOP_OFFSET`] rows
/// below the dialog top, height by content capped at `dialog.height - 4`
/// (§3.3). A zero-size rect when the dialog cannot host an overlay.
pub fn overlay_area(dialog: Rect, view: &MenuView, ui: &MenuUiState) -> Rect {
    if dialog.width < MIN_WIDTH || dialog.height < 6 {
        return Rect::new(dialog.x, dialog.y, 0, 0);
    }
    let w = overlay_width(dialog.width).min(dialog.width);
    let inner_w = w.saturating_sub(2);
    let detail_inline = detail_panel_width(inner_w).is_none();
    let max_h = dialog.height.saturating_sub(4).max(6);
    let h = (inner_height(view, ui, detail_inline) as u16 + 2).min(max_h);
    let x = dialog.x + dialog.width.saturating_sub(w) / 2;
    Rect::new(x, dialog.y + TOP_OFFSET, w, h)
}

/// First row of a window over variable-height rows such that the selected row
/// is the last fully visible one and the window fits `budget` lines — the
/// `window_groups` rule moved here from `ui::overlays` (§3.3). The legacy copy
/// stays: the old overlays still use it until the scaffold comes down.
pub fn window_rows(heights: &[usize], sel: usize, budget: usize) -> usize {
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

/// The first visible row. `stored` (the `scroll` a level remembers) holds while
/// it still keeps the selected row fully visible — every start between the
/// [`window_rows`] anchor and `sel` does. Outside that range the anchor wins.
pub fn resolve_window(heights: &[usize], sel: usize, budget: usize, stored: usize) -> usize {
    let earliest = window_rows(heights, sel, budget);
    if stored >= earliest && stored <= sel {
        stored
    } else {
        earliest
    }
}
