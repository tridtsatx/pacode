use super::*;
use ratatui::layout::Rect;

#[test]
fn test_selection_normalization_anchor_after_head() {
    // Same row, anchor after head (dragged left)
    let sel = Selection {
        anchor: (25, 5),
        head: (10, 5),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    let (start, end) = sel.normalized();
    assert_eq!(start, (10, 5));
    assert_eq!(end, (25, 5));

    // Anchor below head (dragged upward)
    let sel = Selection {
        anchor: (30, 10),
        head: (5, 4),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    let (start, end) = sel.normalized();
    assert_eq!(start, (5, 4));
    assert_eq!(end, (30, 10));

    // Anchor above head (dragged downward - normal order)
    let sel = Selection {
        anchor: (5, 4),
        head: (30, 10),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    let (start, end) = sel.normalized();
    assert_eq!(start, (5, 4));
    assert_eq!(end, (30, 10));
}

#[test]
fn test_selection_row_range_computation() {
    let dialog = Rect::new(5, 2, 50, 20); // y: 2..22 (rows 2..=21)

    // Selection entirely inside dialog
    let sel = Selection {
        anchor: (10, 4),
        head: (20, 8),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    assert_eq!(sel.row_range(dialog), Some(4..=8));

    // Reverse selection (anchor after head)
    let sel = Selection {
        anchor: (20, 8),
        head: (10, 4),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    assert_eq!(sel.row_range(dialog), Some(4..=8));

    // Single-line selection
    let sel = Selection {
        anchor: (10, 6),
        head: (30, 6),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    assert_eq!(sel.row_range(dialog), Some(6..=6));

    // Clamping to dialog top and bottom
    let sel = Selection {
        anchor: (10, 0),
        head: (30, 30),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    assert_eq!(sel.row_range(dialog), Some(2..=21));
}

#[test]
fn test_selection_col_range_computation() {
    let dialog = Rect::new(10, 5, 40, 10); // x: 10..50, y: 5..15

    // Single line
    let sel = Selection {
        anchor: (15, 6),
        head: (35, 6),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    assert_eq!(sel.col_range_for_row(6, dialog), Some(15..=35));
    assert_eq!(sel.col_range_for_row(5, dialog), None);

    // Multi-line selection: rows 6..=8
    let sel = Selection {
        anchor: (20, 6),
        head: (30, 8),
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    };
    // First row: from start_col to dialog right (49)
    assert_eq!(sel.col_range_for_row(6, dialog), Some(20..=49));
    // Middle row: full width (10..=49)
    assert_eq!(sel.col_range_for_row(7, dialog), Some(10..=49));
    // Last row: from dialog left (10) to end_col (30)
    assert_eq!(sel.col_range_for_row(8, dialog), Some(10..=30));
    // Outside rows
    assert_eq!(sel.col_range_for_row(5, dialog), None);
    assert_eq!(sel.col_range_for_row(9, dialog), None);
}

#[test]
fn test_selection_clamping_to_dialog() {
    let dialog = Rect::new(10, 5, 20, 10); // cols 10..=29, rows 5..=14

    assert_eq!(clamp_to_rect(0, 0, dialog), (10, 5));
    assert_eq!(clamp_to_rect(50, 50, dialog), (29, 14));
    assert_eq!(clamp_to_rect(15, 8, dialog), (15, 8));
}

#[test]
fn test_selection_start_drag_finish_lifecycle() {
    let dialog = Rect::new(0, 0, 80, 24);
    let mut sel = Selection::default();
    assert!(!sel.is_active());

    // Mouse Down
    sel.start(10, 5, dialog);
    assert!(sel.is_active());
    assert!(sel.dragging);
    assert!(sel.is_empty());
    assert_eq!(sel.copy_request, CopyRequest::None);

    // Mouse Drag
    sel.drag(25, 7, dialog);
    assert!(sel.is_active());
    assert!(sel.dragging);
    assert!(!sel.is_empty());

    // Mouse Up
    sel.finish();
    assert!(sel.is_active());
    assert!(!sel.dragging);
    assert_eq!(sel.copy_request, CopyRequest::Auto);

    // Explicit copy
    sel.request_explicit_copy();
    assert_eq!(sel.copy_request, CopyRequest::Explicit);

    // Clear
    sel.clear();
    assert!(!sel.is_active());
    assert_eq!(sel.copy_request, CopyRequest::None);
}

#[test]
fn test_mouse_selection_keys_integration() {
    let mut state =
        crate::state::AppState::new(pacode_types::Config::default(), "0.1.0".into(), 100, 30);
    let layout = crate::layout::compute(Rect::new(0, 0, 100, 30), 1, false);

    // Mouse Down inside dialog
    let down_col = layout.dialog.x + 2;
    let down_row = layout.dialog.y + 2;
    let ev_down = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: down_col,
        row: down_row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    crate::keys::handle_mouse(&mut state, ev_down, &layout);
    assert!(state.selection.is_active());
    assert!(state.selection.dragging);
    assert_eq!(state.selection.anchor, (down_col, down_row));

    // Mouse Drag inside dialog
    let drag_col = layout.dialog.x + 10;
    let drag_row = layout.dialog.y + 3;
    let ev_drag = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: drag_col,
        row: drag_row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    crate::keys::handle_mouse(&mut state, ev_drag, &layout);
    assert_eq!(state.selection.head, (drag_col, drag_row));

    // Mouse Up
    let ev_up = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: drag_col,
        row: drag_row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    crate::keys::handle_mouse(&mut state, ev_up, &layout);
    assert!(!state.selection.dragging);
    assert!(state.selection.is_active());
    assert_eq!(
        state.selection.copy_request,
        crate::state::selection::CopyRequest::Auto
    );

    // Explicit copy via ctrl+shift+c
    state.selection.copy_request = crate::state::selection::CopyRequest::None;
    let ctrl_shift_c = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('C'),
        crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
    );
    crate::keys::handle_key(&mut state, ctrl_shift_c, std::time::Instant::now());
    assert_eq!(
        state.selection.copy_request,
        crate::state::selection::CopyRequest::Explicit
    );

    // Esc clears selection
    let esc = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    );
    crate::keys::handle_key(&mut state, esc, std::time::Instant::now());
    assert!(!state.selection.is_active());
}

#[test]
fn test_selection_buffer_extraction() {
    let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 40, 10));
    buffer.set_string(5, 2, "hello world", ratatui::style::Style::default());
    buffer.set_string(5, 3, "second line", ratatui::style::Style::default());

    let dialog = Rect::new(5, 0, 35, 10);

    // 1. Single-line extraction of "hello"
    let mut sel = Selection::default();
    sel.start(5, 2, dialog);
    sel.drag(9, 2, dialog);
    sel.finish();

    let text = crate::ui::extract_selection_text(&buffer, &sel, dialog);
    assert_eq!(text, "hello");

    // 2. Multi-line extraction
    let mut sel_multi = Selection::default();
    sel_multi.start(5, 2, dialog);
    sel_multi.drag(10, 3, dialog);
    sel_multi.finish();

    let text_multi = crate::ui::extract_selection_text(&buffer, &sel_multi, dialog);
    assert_eq!(text_multi, "hello world\nsecond");
}
