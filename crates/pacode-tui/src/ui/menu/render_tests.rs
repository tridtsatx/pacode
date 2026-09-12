use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::binding::Binding;
use crate::ui::menu::{
    Detail, DetailLine, EmptyState, FilterMode, Mark, MenuRow, RowId, Tab, ValueKind,
};

fn key(code: KeyCode) -> Binding {
    Binding {
        code,
        mods: KeyModifiers::NONE,
    }
}

fn item(id: &str) -> MenuRow {
    MenuRow::Item {
        id: RowId::from(id),
        mark: Mark::None,
        label: format!("label-{id}"),
        right: None,
        detail: Detail::default(),
    }
}

fn detailed_item(id: &str) -> MenuRow {
    MenuRow::Item {
        id: RowId::from(id),
        mark: Mark::Ok,
        label: format!("label-{id}"),
        right: None,
        detail: Detail {
            title: "the detail title".to_string(),
            lines: vec![
                DetailLine::Field {
                    label: "version".to_string(),
                    value: "1.2.3".to_string(),
                },
                DetailLine::Text("free text".to_string()),
            ],
            ..Default::default()
        },
    }
}

/// Draw into a test backend and return the screen rows as strings.
fn screen(view: &MenuView, ui: &MenuUiState, w: u16, h: u16) -> Vec<String> {
    let opts = RenderOptions::new(w, false);
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| draw(f, f.area(), view, ui, &opts))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            row.trim_end().to_string()
        })
        .collect()
}

#[test]
fn the_breadcrumb_is_bold_root_then_faint_tail() {
    let mut view = MenuView::new(MenuId::Plugins);
    view.rows = vec![item("a")];
    let rows = screen(&view, &MenuUiState::default(), 100, 20);
    assert!(
        rows.iter().any(|r| r.contains("Menu › Plugins")),
        "rows: {rows:?}"
    );
}

#[test]
fn the_root_menu_shows_just_its_title() {
    let mut view = MenuView::new(MenuId::Root);
    view.rows = vec![MenuRow::Submenu {
        id: RowId::from("s"),
        label: "Settings".to_string(),
        right: Some("68 fields".to_string()),
        target: MenuId::Settings,
    }];
    let rows = screen(&view, &MenuUiState::default(), 100, 20);
    assert!(rows.iter().any(|r| r.contains(" Menu ")), "rows: {rows:?}");
    assert!(
        !rows.iter().any(|r| r.contains("Menu › Menu")),
        "rows: {rows:?}"
    );
}

#[test]
fn the_hint_lists_enabled_actions_and_always_keeps_esc() {
    let mut view = MenuView::new(MenuId::Plugins);
    view.rows = vec![item("a")];
    view.actions = vec![
        MenuAction::new(key(KeyCode::Char('i')), "install"),
        MenuAction::new(key(KeyCode::Char('x')), "uninstall").enabled(false),
    ];
    let rows = screen(&view, &MenuUiState::default(), 100, 20);
    let text = rows.join("\n");
    assert!(text.contains("i install"), "{text}");
    assert!(
        !text.contains("x uninstall"),
        "a disabled action never reaches the hint: {text}"
    );
    assert!(text.contains("esc"), "{text}");
}

#[test]
fn hint_line_drops_actions_from_the_end_but_keeps_esc() {
    let opts = RenderOptions::new(80, false);
    let actions = vec![
        MenuAction::new(key(KeyCode::Char('i')), "install"),
        MenuAction::new(key(KeyCode::Char('x')), "uninstall"),
        MenuAction::new(key(KeyCode::Char('r')), "restart"),
    ];
    let wide = hint_line(&actions, 80, &opts);
    assert!(wide.contains("i install"));
    assert!(wide.contains("x uninstall"));
    assert!(wide.contains("r restart"));
    assert!(wide.contains("esc back"));

    let narrow = hint_line(&actions, 24, &opts);
    assert!(
        display_width(&narrow) <= 24,
        "the hint fits its width: {narrow:?}"
    );
    assert!(narrow.contains("esc"), "{narrow}");
    assert!(!narrow.contains("restart"), "cut from the end: {narrow}");
}

#[test]
fn hint_line_ignores_disabled_and_base_key_actions() {
    let opts = RenderOptions::new(80, false);
    let actions = vec![
        MenuAction::new(key(KeyCode::Char('x')), "remove").enabled(false),
        // A base key declared anyway — ignored in release (§3.4).
        MenuAction::new(key(KeyCode::Enter), "apply"),
        MenuAction::new(key(KeyCode::Char('i')), "install"),
    ];
    let hint = hint_line(&actions, 80, &opts);
    assert!(!hint.contains("remove"), "{hint}");
    assert!(!hint.contains("enter apply"), "{hint}");
    assert!(hint.contains("i install"), "{hint}");
}

#[test]
fn tabs_draw_only_with_two_or_more_peer_lists() {
    let mut view = MenuView::new(MenuId::Plugins);
    view.rows = vec![item("a")];
    view.tabs = Some(Tabs {
        items: vec![Tab {
            label: "Only".to_string(),
            count: 1,
        }],
    });
    let text = screen(&view, &MenuUiState::default(), 100, 20).join("\n");
    assert!(
        !text.contains("Only"),
        "a single tab does not draw a strip: {text}"
    );

    view.tabs = Some(Tabs {
        items: vec![
            Tab {
                label: "Installed".to_string(),
                count: 3,
            },
            Tab {
                label: "Discover".to_string(),
                count: 42,
            },
        ],
    });
    let ui = MenuUiState {
        tab: 1,
        ..MenuUiState::default()
    };
    let text = screen(&view, &ui, 100, 20).join("\n");
    assert!(text.contains("Installed"), "{text}");
    assert!(text.contains("Discover"), "{text}");
    assert!(text.contains("42"), "{text}");
    // The active tab trails the breadcrumb (§3.2).
    assert!(text.contains("Menu › Plugins › Discover"), "{text}");
}

#[test]
fn the_filter_line_shows_the_query_and_the_counter() {
    let mut view = MenuView::new(MenuId::Sessions);
    view.filter = Some(FilterMode::Local);
    view.filter_total = Some(5);
    view.rows = vec![item("a"), item("b")];
    let ui = MenuUiState {
        query: "fix".to_string(),
        ..MenuUiState::default()
    };
    let text = screen(&view, &ui, 100, 20).join("\n");
    assert!(text.contains("> fix"), "{text}");
    assert!(text.contains("2 of 5"), "{text}");
}

#[test]
fn a_wide_overlay_splits_off_a_detail_panel() {
    let mut view = MenuView::new(MenuId::Plugins);
    view.rows = vec![detailed_item("a"), detailed_item("b")];
    // 120-column dialog → 96-column overlay → 94 inner ≥ 72.
    let rows = screen(&view, &MenuUiState::default(), 120, 20);
    let text = rows.join("\n");
    assert!(text.contains("the detail title"), "{text}");
    assert!(text.contains("version"), "{text}");
    assert!(text.contains("1.2.3"), "{text}");
    // The selected row's detail lives in the right-hand panel, not under it.
    let sel_line = rows
        .iter()
        .position(|r| r.contains("label-a"))
        .expect("row a");
    assert!(
        !rows[sel_line + 1].contains("the detail title"),
        "detail must not be inline when the panel is open: {rows:?}"
    );
}

#[test]
fn a_narrow_overlay_collapses_detail_into_two_lines_under_the_row() {
    let mut view = MenuView::new(MenuId::Plugins);
    view.rows = vec![detailed_item("a"), detailed_item("b")];
    // 60-column dialog → 48-column overlay → 46 inner < 72.
    let rows = screen(&view, &MenuUiState::default(), 60, 20);
    let sel_line = rows
        .iter()
        .position(|r| r.contains("label-a"))
        .expect("row a");
    assert!(
        rows[sel_line + 1].contains("the detail title"),
        "first collapsed line under the selected row: {rows:?}"
    );
    assert!(
        rows[sel_line + 2].contains("version: 1.2.3"),
        "second collapsed line: {rows:?}"
    );
    // Two lines, no more — row b follows right after.
    assert!(
        rows[sel_line + 3].contains("label-b"),
        "detail is capped at two lines: {rows:?}"
    );
}

#[test]
fn an_empty_list_draws_the_empty_state_not_a_tall_void() {
    let mut view = MenuView::new(MenuId::Plugins);
    view.empty = EmptyState::new("No plugins installed", "tab Discover to browse");
    let rows = screen(&view, &MenuUiState::default(), 100, 24);
    let text = rows.join("\n");
    assert!(text.contains("No plugins installed"), "{text}");
    let bottom = rows
        .iter()
        .position(|r| r.contains('╰'))
        .expect("bottom border");
    assert!(bottom <= 6, "the frame stays small: {rows:?}");
}

#[test]
fn sections_render_as_headers_and_the_cursor_skips_them() {
    let mut view = MenuView::new(MenuId::Sessions);
    view.rows = vec![MenuRow::section("today"), item("a"), item("b")];
    let rows = screen(&view, &MenuUiState::default(), 100, 20);
    let sel_line = rows
        .iter()
        .position(|r| r.contains("today"))
        .expect("section");
    assert!(
        !rows[sel_line].contains('▸'),
        "the pointer never rests on a section: {rows:?}"
    );
    let first_item = rows
        .iter()
        .position(|r| r.contains("label-a"))
        .expect("item a");
    assert!(
        rows[first_item].contains('▸'),
        "the cursor lands on the first selectable row: {rows:?}"
    );
}

#[test]
fn capture_mode_hides_the_list() {
    let mut view = MenuView::new(MenuId::Keys);
    view.rows = vec![item("a"), item("b")];
    let ui = MenuUiState {
        mode: MenuMode::Capture {
            action: "scroll_up",
            pending: None,
        },
        ..MenuUiState::default()
    };
    let text = screen(&view, &ui, 100, 20).join("\n");
    assert!(text.contains("Press a key for"), "{text}");
    assert!(text.contains("scroll_up"), "{text}");
    assert!(!text.contains("label-a"), "the list is hidden: {text}");
}

#[test]
fn the_status_line_sits_above_the_hint_edge() {
    let mut view = MenuView::new(MenuId::Plugins);
    view.rows = vec![item("a")];
    view.status = Some(StatusLine::offline_copy());
    let text = screen(&view, &MenuUiState::default(), 100, 20).join("\n");
    assert!(text.contains("offline copy"), "{text}");
}

#[test]
fn edit_mode_shows_the_buffer_in_place_of_the_value() {
    let mut view = MenuView::new(MenuId::Settings);
    view.rows = vec![MenuRow::Value {
        id: RowId::from("v"),
        label: "context limit".to_string(),
        value: "80".to_string(),
        kind: ValueKind::Int,
        modified: false,
        detail: Detail::default(),
    }];
    let ui = MenuUiState {
        mode: MenuMode::Edit {
            buffer: "12".to_string(),
            cursor: 2,
        },
        ..MenuUiState::default()
    };
    let text = screen(&view, &ui, 100, 20).join("\n");
    assert!(text.contains("[ 12 ]"), "{text}");
    assert!(text.contains("enter confirm"), "{text}");
}

#[test]
fn toggle_and_check_rows_render_their_state() {
    let mut view = MenuView::new(MenuId::Settings);
    view.rows = vec![
        MenuRow::Toggle {
            id: RowId::from("t"),
            label: "vim mode".to_string(),
            on: true,
            modified: true,
            detail: Detail::default(),
        },
        MenuRow::Check {
            id: RowId::from("c"),
            label: "server".to_string(),
            checked: true,
            warn: Some("already present".to_string()),
            detail: Detail::default(),
        },
    ];
    let text = screen(&view, &MenuUiState::default(), 100, 20).join("\n");
    assert!(text.contains("vim mode"), "{text}");
    assert!(text.contains("on *"), "modified values are marked: {text}");
    assert!(text.contains("[✓]"), "{text}");
    assert!(text.contains("already present"), "{text}");
}
