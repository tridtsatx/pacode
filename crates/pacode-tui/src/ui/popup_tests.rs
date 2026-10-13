use std::time::Instant;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use ratatui::layout::Rect;

use pacode_types::{Config, TaskId};

use crate::layout::compute;
use crate::state::{AppState, Focus, Overlay, PanelTarget};
use crate::ui::menu::{MenuStack, MenuUiState};
use crate::ui::popup::{self, PopupId};

fn state(cols: u16, rows: u16) -> AppState {
    AppState::new(Config::default(), "test".to_string(), cols, rows)
}

#[test]
fn full_overlay_wins_over_every_input_completion() {
    let mut app = state(80, 24);
    app.input.insert_str("!echo @src");
    app.focus = Focus::Overlay(Overlay::ModelPalette {
        palette: Default::default(),
    });

    let popup = popup::resolve(&app, &crate::layout::current(&app), Instant::now()).unwrap();
    assert_eq!(popup.id(), PopupId::ModelPalette);
    assert!(popup.is_full());
}

fn palette_models(count: usize) -> Vec<pacode_types::ModelInfo> {
    (0..count)
        .map(|n| pacode_types::ModelInfo {
            route: pacode_types::ModelRoute::new("p", format!("m{n:02}")),
            display_name: format!("m{n:02}"),
            context_window: None,
            supports_reasoning: false,
            supports_vision: false,
            supported_efforts: None,
            pricing: None,
        })
        .collect()
}

fn palette_area_with(app: &AppState) -> (Rect, Rect, Rect) {
    let layout = crate::layout::current(app);
    let popup = popup::resolve(app, &layout, Instant::now()).expect("palette resolves");
    (
        popup.area(),
        popup::overlay_container(&layout),
        layout.preview,
    )
}

/// B25: the palette sits in the shared overlay container (the left column
/// above the prompt rules, panel or not), 44..=84 wide and centred across;
/// its height follows its rows up to the tallest box, whose centred top it
/// keeps.
#[test]
fn the_model_palette_sizes_to_its_rows_in_the_overlay_container() {
    for (cols, rows) in [(130u16, 40u16), (200, 60), (60, 20), (40, 12)] {
        let mut app = state(cols, rows);
        app.panel.target = Some(PanelTarget::Task(TaskId::new("task")));
        app.focus = Focus::Overlay(Overlay::ModelPalette {
            palette: Default::default(),
        });
        for (models, list_rows) in [(3usize, 4u16), (0, 0), (60, 61)] {
            app.models = palette_models(models);
            let (area, container, preview) = palette_area_with(&app);
            let expected_w = container
                .width
                .saturating_sub(4)
                .clamp(44.min(container.width), 84);
            let tallest = container.height.min(popup::PALETTE_MAX_HEIGHT);
            // Border, search + spacer + effort + hint, the rows plus one
            // spacer, and never fewer than four list rows.
            let wanted = 2 + 4 + (list_rows + 1).max(4);
            let at = format!("{cols}x{rows}, {models} models");
            assert_eq!(area.width, expected_w, "{at}");
            assert_eq!(area.height, wanted.min(tallest), "{at}");
            assert_eq!(
                area.x,
                container.x + (container.width - area.width) / 2,
                "{at}: centred across"
            );
            assert_eq!(
                area.y,
                container.y + (container.height - tallest) / 2,
                "{at}: the tallest box's top"
            );
            assert!(area.bottom() <= preview.y, "{at}: clear of the prompt");
        }
    }
}

/// A filter that shrinks the list shortens the box from below: the frame
/// keeps its x, width and top, so the search line never moves.
#[test]
fn filtering_shrinks_the_palette_without_moving_it() {
    let mut app = state(120, 34);
    app.models = palette_models(12);
    app.focus = Focus::Overlay(Overlay::ModelPalette {
        palette: Default::default(),
    });
    let (before, _, _) = palette_area_with(&app);
    if let Focus::Overlay(Overlay::ModelPalette { palette }) = &mut app.session.focus {
        palette.push_str("m07");
    }
    let (after, _, _) = palette_area_with(&app);
    assert!(after.height < before.height, "{before:?} -> {after:?}");
    assert_eq!(
        (after.x, after.y, after.width),
        (before.x, before.y, before.width)
    );
}

#[test]
/// B25: with a panel open, overlays used to centre in the dialog rect — 45–50 %
/// of the column — and came out squeezed beside empty space. The container is
/// the whole left column above the prompt, panel or not.
fn overlays_centre_in_the_whole_left_column_with_or_without_a_panel() {
    for cols in [60u16, 80, 100, 140] {
        for panel in [None, Some(true), Some(false)] {
            let mut app = state(cols, 34);
            app.panel.target = panel.map(|is_task| {
                if is_task {
                    PanelTarget::Task(TaskId::new("task"))
                } else {
                    PanelTarget::CommitDiff {
                        hash: "commit".to_string(),
                    }
                }
            });
            let layout = crate::layout::current(&app);
            let container = popup::overlay_container(&layout);
            assert_eq!(
                container,
                Rect::new(
                    layout.input_top.x,
                    layout.dialog.y,
                    layout.input_top.width,
                    layout.preview.y - layout.dialog.y,
                ),
                "{cols} cols, panel {panel:?}"
            );
            if let Some(panel) = layout.panel {
                assert!(container.width >= layout.dialog.width + panel.width);
            }
        }
    }
}

#[test]
fn at_completion_wins_over_bash_even_before_candidates_land() {
    let mut app = state(80, 24);
    app.input.insert_str("!echo @src");

    let popup = popup::resolve_current(&app).unwrap();
    assert_eq!(popup.id(), PopupId::At);
    assert_eq!(popup.window().total, 0);

    app.input.at_closed = true;
    assert_eq!(popup::resolve_current(&app).unwrap().id(), PopupId::Bash);

    app.input.bash_complete_closed = true;
    app.input.queue_push("queued".to_string());
    assert_eq!(
        popup::resolve_current(&app).unwrap().id(),
        PopupId::PromptQueue
    );
}

#[test]
fn slash_completion_wins_over_the_prompt_queue() {
    let mut app = state(80, 24);
    app.input.insert_str("/mo");
    app.input.queue_push("queued".to_string());

    let popup = popup::resolve_current(&app).unwrap();
    assert_eq!(popup.id(), PopupId::Slash);
    assert!(popup.window().total > 0);
}

#[test]
fn pre_draw_prepare_arms_each_completion_once() {
    let mut app = state(80, 24);
    app.input.insert_str("!echo @sr");
    let now = Instant::now();
    let area = Rect::new(0, 0, 80, 24);

    crate::ui::prepare_frame(area, &mut app, now);
    let at_key = app.input.at_comp.want.clone().expect("at completion armed");
    crate::ui::prepare_frame(area, &mut app, now);
    assert_eq!(app.input.at_comp.want.as_ref(), Some(&at_key));
    assert!(app.input.bash_comp.want.is_none());
    assert_eq!(app.input.at_comp.claim_want(), Some(at_key));
    assert_eq!(app.input.at_comp.claim_want(), None);

    app.input.at_closed = true;
    crate::ui::prepare_frame(area, &mut app, now);
    let bash_key = app
        .input
        .bash_comp
        .want
        .clone()
        .expect("bash completion armed");
    assert!(app.input.at_comp.want.is_none());
    assert_eq!(app.input.bash_comp.claim_want(), Some(bash_key));
    assert_eq!(app.input.bash_comp.claim_want(), None);
}

#[test]
fn completion_geometry_shrinks_before_it_positions() {
    let layout = compute(Rect::new(0, 0, 30, 6), 1, false, 0, 0);
    let area = popup::above_input(&layout, 50, 6);

    assert!(area.width <= layout.input.width);
    assert!(area.height < 6);
    assert!(area.bottom() <= layout.input_top.y);
    assert!(area.bottom() <= layout.input.y);
    assert!(area.bottom() <= layout.footer.y);
}

#[test]
fn common_clamps_never_position_an_oversized_rect_outside_its_container() {
    let container = Rect::new(10, 20, 10, 4);
    let top = popup::top_area(container, 30, 20, 2);
    let bottom = popup::bottom_area(container, 30, 20);
    let center = popup::center_area(container, 30, 20);

    assert_eq!(top, container);
    assert_eq!(bottom, container);
    assert_eq!(center, container);
}

#[test]
fn query_cursor_uses_display_columns_and_stays_inside() {
    let area = Rect::new(5, 7, 10, 2);
    assert_eq!(popup::query_cursor(area, "模型", 2, 1), Some((11, 8)));
    assert_eq!(popup::query_cursor(area, "abcdefghij", 2, 1), Some((14, 8)));
    assert_eq!(popup::query_cursor(area, "x", 2, 2), None);
    assert_eq!(
        popup::query_cursor(Rect::new(0, 0, 3, 2), "x", 2, 1),
        Some((2, 1))
    );
    assert_eq!(popup::query_cursor(Rect::new(0, 0, 2, 2), "x", 2, 1), None);
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn mouse_cannot_start_a_selection_through_an_input_popup() {
    let mut app = state(80, 24);
    app.input.insert_str("/mo");
    let layout = crate::layout::current(&app);
    let popup = popup::resolve_current(&app).unwrap();
    let area = popup.area();

    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), area.x, area.y),
        &layout,
    );

    assert!(!app.selection.is_active());
}

#[test]
fn mouse_hit_test_uses_resized_geometry_instead_of_the_last_frame() {
    let mut app = state(80, 24);
    let stale = crate::layout::current(&app);
    app.rows = 6;
    app.input.insert_str("/mo");
    let popup = popup::resolve_current(&app).unwrap();
    let area = popup.area();

    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), area.x, area.y),
        &stale,
    );

    assert!(!app.selection.is_active());
}

#[test]
fn full_overlay_blocks_mouse_outside_its_visible_rect() {
    let mut app = state(80, 24);
    app.focus = Focus::Overlay(Overlay::Menu {
        stack: MenuStack::root(crate::ui::menu::MenuId::Root),
        ui: MenuUiState::default(),
    });
    let popup = popup::resolve_current(&app).unwrap();
    let outside = (popup.area().x, popup.area().bottom());
    let layout = crate::layout::current(&app);

    crate::mouse::handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            outside.0,
            outside.1,
        ),
        &layout,
    );

    assert!(!app.selection.is_active());
}

#[test]
fn the_prepared_slash_popup_keeps_only_the_visible_commands() {
    let mut app = state(80, 24);
    app.input.insert_str("/");
    let layout = crate::layout::current(&app);
    let completion = popup::InputCompletion::detect(&app);

    let popup::PreparedInlinePopup {
        area: _,
        window,
        content,
    } = popup::resolve_inline_prepared(&app, &layout, completion.as_ref())
        .expect("slash popup prepared");
    let popup::PreparedInlineContent::Slash(commands) = content else {
        panic!("expected the slash popup");
    };

    assert_eq!(commands.len(), window.len);
    assert!(window.total > window.len);
    assert!(window.len <= popup::MAX_VISIBLE as usize);
}
