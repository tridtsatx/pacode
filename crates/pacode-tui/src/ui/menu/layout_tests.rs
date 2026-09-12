use ratatui::layout::Rect;

use super::*;
use crate::ui::menu::model::{Detail, DetailLine, Mark, RowId};
use crate::ui::menu::{EmptyState, MenuId, StatusLine};

fn item(id: &str) -> MenuRow {
    MenuRow::Item {
        id: RowId::from(id),
        mark: Mark::None,
        label: id.to_string(),
        right: None,
        detail: Detail::default(),
    }
}

fn detailed_item(id: &str, lines: usize) -> MenuRow {
    MenuRow::Item {
        id: RowId::from(id),
        mark: Mark::Ok,
        label: id.to_string(),
        right: None,
        detail: Detail {
            title: "info".to_string(),
            lines: (0..lines)
                .map(|i| DetailLine::Text(format!("line {i}")))
                .collect(),
            actions: Vec::new(),
        },
    }
}

#[test]
fn overlay_width_is_eighty_percent_with_a_floor() {
    assert_eq!(overlay_width(100), 80);
    assert_eq!(overlay_width(60), 48);
    assert_eq!(overlay_width(25), 20, "the 20-column floor applies");
    assert_eq!(overlay_width(24), 20);
}

#[test]
fn height_comes_from_the_rows_not_a_per_menu_match() {
    let dialog = Rect::new(0, 0, 100, 30);
    let ui = MenuUiState::default();

    // An empty list draws a small frame, not a tall bordered void.
    let mut view = MenuView::new(MenuId::Plugins);
    view.empty = EmptyState::new("No plugins", "browse");
    let area = overlay_area(dialog, &view, &ui);
    assert_eq!(area.height, 4, "border + two empty-state lines: {area:?}");

    // Three one-line rows: border + 3.
    view.rows = vec![item("a"), item("b"), item("c")];
    let area = overlay_area(dialog, &view, &ui);
    assert_eq!(area.height, 5, "{area:?}");

    // A section row counts as one line and adds to the height.
    view.rows.insert(0, MenuRow::section("today"));
    let area = overlay_area(dialog, &view, &ui);
    assert_eq!(area.height, 6, "{area:?}");

    // Status and filter each add a line.
    view.status = Some(StatusLine::loading());
    view.filter = Some(FilterMode::Local);
    let area = overlay_area(dialog, &view, &ui);
    assert_eq!(area.height, 8, "{area:?}");
}

#[test]
fn height_caps_at_dialog_height_minus_four() {
    let dialog = Rect::new(0, 0, 100, 20);
    let mut view = MenuView::new(MenuId::Sessions);
    view.rows = (0..50).map(|i| item(&format!("s{i}"))).collect();
    let area = overlay_area(dialog, &view, &MenuUiState::default());
    assert_eq!(area.height, 16, "dialog.height - 4: {area:?}");
}

#[test]
fn collapsed_detail_adds_at_most_two_lines_to_the_selected_row() {
    // detail_inline = narrow overlay.
    assert_eq!(row_height(&item("a"), false, true), 1);
    assert_eq!(row_height(&detailed_item("a", 5), true, true), 3);
    assert_eq!(row_height(&detailed_item("a", 5), false, true), 1);
    // With the panel open the row stays one line.
    assert_eq!(row_height(&detailed_item("a", 5), true, false), 1);
    // A detail-less selected row gains nothing.
    assert_eq!(row_height(&item("a"), true, true), 1);
}

#[test]
fn the_detail_panel_needs_72_inner_columns_and_26_for_itself() {
    assert_eq!(detail_panel_width(71), None);
    assert_eq!(detail_panel_width(50), None);
    assert_eq!(detail_panel_width(72), Some(28), "72 * 2/5 = 28 >= 26");
    assert_eq!(detail_panel_width(100), Some(40));
}

#[test]
fn the_window_keeps_the_selected_group_fully_visible() {
    // Same contract as `window_groups` in the legacy overlays.
    let heights = vec![2, 2, 2, 2, 2];
    assert_eq!(window_rows(&heights, 0, 6), 0);
    assert_eq!(window_rows(&heights, 2, 6), 0);
    assert_eq!(window_rows(&heights, 3, 6), 1);
    // A taller selected group pulls the window further down.
    let heights = vec![2, 3, 2, 4, 2];
    assert_eq!(window_rows(&heights, 3, 6), 2);
    // A group that alone fills the budget starts at itself.
    let heights = vec![2, 2, 2, 5, 2];
    assert_eq!(window_rows(&heights, 3, 5), 3);
    // Degenerate inputs do not panic.
    assert_eq!(window_rows(&[], 0, 5), 0);
    assert_eq!(window_rows(&heights, 0, 0), 0);
    assert_eq!(window_rows(&heights, 99, 6), 4);
}

#[test]
fn resolve_window_holds_a_stored_scroll_while_the_selection_stays_visible() {
    let heights = vec![1; 20];
    // The anchor maximizes the context above the selection.
    assert_eq!(window_rows(&heights, 10, 10), 1);
    // A stored start inside [anchor, sel] is honoured — the window stays put.
    assert_eq!(resolve_window(&heights, 10, 10, 1), 1);
    assert_eq!(resolve_window(&heights, 10, 10, 5), 5);
    assert_eq!(resolve_window(&heights, 10, 10, 10), 10);
    // Outside the range the anchor wins — the selection stays fully visible.
    assert_eq!(resolve_window(&heights, 10, 10, 0), 1);
    assert_eq!(resolve_window(&heights, 10, 10, 11), 1);
    // Stored scroll past the rows clamps back.
    assert_eq!(resolve_window(&heights, 2, 10, 99), 0);
}

#[test]
fn inner_height_counts_capture_as_two_lines() {
    let mut view = MenuView::new(MenuId::Keys);
    view.rows = vec![item("a"), item("b"), item("c")];
    let ui = MenuUiState {
        mode: MenuMode::Capture {
            action: "scroll_up",
        },
        ..MenuUiState::default()
    };
    assert_eq!(inner_height(&view, &ui, true), 2, "the list hides");
}
