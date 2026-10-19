use super::AnalyzeView;

#[test]
fn a_page_that_fits_never_scrolls() {
    let mut view = AnalyzeView::new();
    view.scroll_by(5, 10, 22);
    assert_eq!(view.scroll, 0, "no content below the fold, no scroll");
    view.clamp(10, 22);
    assert_eq!(view.scroll, 0);
}

#[test]
fn scrolling_stops_at_the_last_full_screen() {
    let mut view = AnalyzeView::new();
    // 100 rows, 22 visible: the last screen starts at 78.
    assert_eq!(AnalyzeView::max_scroll(100, 22), 78);
    view.scroll_by(1_000, 100, 22);
    assert_eq!(
        view.scroll, 78,
        "scrolling past the end parks on the last row"
    );
    view.scroll_by(-1_000, 100, 22);
    assert_eq!(view.scroll, 0, "and back up stops at the top");
}

#[test]
fn a_page_that_shrinks_pulls_the_offset_back_into_view() {
    // The session restarted and the report is now shorter than the offset the
    // user left it at; without the clamp the page would show blank rows.
    let mut view = AnalyzeView { scroll: 500 };
    view.clamp(30, 22);
    assert_eq!(view.scroll, 8);
}

#[test]
fn a_held_key_at_the_end_does_not_wrap_the_offset_around() {
    let mut view = AnalyzeView { scroll: 78 };
    view.scroll_by(1, 100, 22);
    assert_eq!(view.scroll, 78);
    view.scroll_to_top();
    assert_eq!(view.scroll, 0);
}
