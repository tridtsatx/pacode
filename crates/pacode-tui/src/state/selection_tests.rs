use super::*;
use ratatui::layout::Rect;

fn p(line: usize, col: u16) -> Point {
    Point { line, col }
}

fn sel(anchor: Point, head: Point) -> Selection {
    Selection {
        anchor,
        head,
        active: true,
        dragging: false,
        copy_request: CopyRequest::None,
    }
}

#[test]
fn normalization_puts_the_ends_in_reading_order() {
    // Same line, dragged leftward.
    let s = sel(p(5, 25), p(5, 10));
    assert_eq!(s.normalized(), (p(5, 10), p(5, 25)));

    // Dragged upward across lines.
    let s = sel(p(10, 30), p(4, 5));
    assert_eq!(s.normalized(), (p(4, 5), p(10, 30)));

    // Already in order.
    let s = sel(p(4, 5), p(10, 30));
    assert_eq!(s.normalized(), (p(4, 5), p(10, 30)));
}

#[test]
fn an_empty_or_inactive_selection_covers_nothing() {
    let mut s = sel(p(3, 2), p(3, 2));
    assert!(s.is_empty());
    assert_eq!(s.line_range(), None);

    s.head = p(6, 2);
    assert_eq!(s.line_range(), Some(3..=6));

    s.active = false;
    assert_eq!(s.line_range(), None);
}

#[test]
fn column_ranges_span_full_lines_between_the_ends() {
    let s = sel(p(4, 10), p(7, 20));
    let width = 50;

    assert_eq!(s.col_range_for_line(4, width), Some(10..=49));
    assert_eq!(s.col_range_for_line(5, width), Some(0..=49));
    assert_eq!(s.col_range_for_line(6, width), Some(0..=49));
    assert_eq!(s.col_range_for_line(7, width), Some(0..=20));
    assert_eq!(s.col_range_for_line(8, width), None);
    assert_eq!(s.col_range_for_line(3, width), None);

    // A selection inside one line stays inside it.
    let s = sel(p(4, 10), p(4, 20));
    assert_eq!(s.col_range_for_line(4, width), Some(10..=20));

    // A zero-width area selects nothing.
    assert_eq!(s.col_range_for_line(4, 0), None);
}

#[test]
fn contains_cell_follows_the_column_ranges() {
    let s = sel(p(2, 4), p(3, 6));
    assert!(s.contains_cell(2, 4, 40));
    assert!(s.contains_cell(2, 39, 40));
    assert!(!s.contains_cell(2, 3, 40));
    assert!(s.contains_cell(3, 0, 40));
    assert!(!s.contains_cell(3, 7, 40));
    assert!(!s.contains_cell(4, 0, 40));
}

#[test]
fn a_screen_position_maps_to_the_content_line_under_it() {
    let dialog = Rect::new(5, 2, 50, 20);
    let mut s = Selection::new();

    // Top row of the viewport while scrolled 100 lines into the transcript.
    s.start(10, 2, dialog, 100);
    assert_eq!(s.anchor, p(100, 5));

    // Three rows down is three content lines further.
    s.drag(20, 5, dialog, 100);
    assert_eq!(s.head, p(103, 15));
}

#[test]
fn a_position_outside_the_dialog_is_clamped_into_it() {
    let dialog = Rect::new(5, 2, 50, 20);
    let mut s = Selection::new();

    // Above and left of the area.
    s.start(0, 0, dialog, 40);
    assert_eq!(s.anchor, p(40, 0));

    // Below and right of the area: last row, last column.
    s.drag(200, 200, dialog, 40);
    assert_eq!(s.head, p(59, 49));
}

#[test]
fn scrolling_keeps_the_selection_on_its_text() {
    let dialog = Rect::new(0, 0, 40, 10);
    let mut s = Selection::new();

    // Select the line drawn at the top while the viewport starts at line 50.
    s.start(0, 0, dialog, 50);
    s.drag(10, 0, dialog, 50);
    assert_eq!(s.normalized(), (p(50, 0), p(50, 10)));

    // Scroll up by five lines: the same content line is now five rows lower,
    // and the selection still names line 50 rather than the top row.
    assert_eq!(s.line_range(), Some(50..=50));
    assert!(s.contains_cell(50, 3, 40));
    assert!(!s.contains_cell(45, 3, 40));
}

#[test]
fn dragging_past_the_viewport_keeps_extending_the_selection() {
    let dialog = Rect::new(0, 0, 40, 10);
    let mut s = Selection::new();
    s.start(0, 0, dialog, 100);

    // The reader scrolls while holding the button: the pointer is still on the
    // last row, but the content under it has moved on.
    s.drag(39, 9, dialog, 100);
    assert_eq!(s.head, p(109, 39));
    s.drag(39, 9, dialog, 130);
    assert_eq!(s.head, p(139, 39));
    assert_eq!(s.line_range(), Some(100..=139));

    // A selection can also be extended to a content line directly.
    s.drag_to_line(200, 5);
    assert_eq!(s.line_range(), Some(100..=200));
}

#[test]
fn finishing_an_empty_drag_deactivates_and_requests_no_copy() {
    let dialog = Rect::new(0, 0, 40, 10);
    let mut s = Selection::new();

    s.start(3, 3, dialog, 0);
    s.finish();
    assert!(!s.is_active());
    assert_eq!(s.copy_request, CopyRequest::None);

    s.start(3, 3, dialog, 0);
    s.drag(9, 3, dialog, 0);
    s.finish();
    assert!(s.is_active());
    assert_eq!(s.copy_request, CopyRequest::Auto);

    s.request_explicit_copy();
    assert_eq!(s.copy_request, CopyRequest::Explicit);
}

#[test]
fn extraction_takes_the_selected_text_from_content_not_the_screen() {
    let lines = vec![
        "first line of the transcript".to_string(),
        "second line".to_string(),
        "third line here".to_string(),
    ];
    let s = sel(p(0, 6), p(2, 4));
    let text = crate::ui::extract_selection_text(&lines, &s, 40);
    assert_eq!(text, "line of the transcript\nsecond line\nthird");

    // Single line, single word.
    let s = sel(p(1, 0), p(1, 5));
    assert_eq!(crate::ui::extract_selection_text(&lines, &s, 40), "second");

    // A selection reaching past the last rendered line simply stops there.
    let s = sel(p(2, 0), p(9, 3));
    assert_eq!(
        crate::ui::extract_selection_text(&lines, &s, 40),
        "third line here"
    );
}

#[test]
fn extraction_is_capped_so_a_huge_selection_cannot_run_away() {
    let long = "x".repeat(1000);
    let lines: Vec<String> = std::iter::repeat_n(long, 8000).collect();
    let s = sel(p(0, 0), p(7999, 999));
    let text = crate::ui::extract_selection_text(&lines, &s, 1000);
    assert!(text.chars().count() <= crate::ui::COPY_MAX_CHARS + 8000);
    assert!(text.chars().count() >= crate::ui::COPY_MAX_CHARS);
}
