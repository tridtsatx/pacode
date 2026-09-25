use std::time::{Duration, Instant};

use pacode_render::RenderOptions;
use pacode_types::Config;
use pacode_types::TranscriptKind;
use pacode_types::state::ToastLevel;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};

use super::*;
use crate::state::AppState;
use crate::state::selection::CopyRequest;
use crate::state::transcript::{Cell, CellKind};

fn prepare_and_draw_frame(frame: &mut Frame<'_>, state: &mut AppState) {
    let prepared = crate::ui::prepare_frame(frame.area(), state, Instant::now());
    crate::ui::draw(frame, state, &prepared);
}

/// Render the strip into `area` on a `cols`×`rows` backend; return one string
/// per terminal row, read straight from the buffer (no Display quoting).
fn render_rows(state: &AppState, area: Rect, cols: u16, rows: u16) -> Vec<String> {
    let backend = TestBackend::new(cols, rows);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(cols, false);
    terminal
        .draw(|f| {
            draw(f, area, state, &opts);
        })
        .unwrap();
    let buf = terminal.backend().buffer();
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(""))
                .collect::<String>()
        })
        .collect()
}

#[test]
fn a_toast_fades_only_inside_the_exit_window() {
    let ttl_ms = crate::state::TOAST_TTL_SECS * 1000;
    assert!(!is_fading(0));
    assert!(!is_fading(ttl_ms - crate::state::TOAST_EXIT_MS));
    assert!(is_fading(ttl_ms - crate::state::TOAST_EXIT_MS + 1));
    assert!(is_fading(ttl_ms - 1));
    // Past TTL the toast is gone anyway, but the predicate stays monotone.
    assert!(is_fading(ttl_ms + 1));
}

#[test]
fn toast_needs_anim_only_inside_the_fade_window() {
    let now = Instant::now();
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    assert!(!state.toast_needs_anim(now));

    state.push_toast(ToastLevel::Info, "fresh".into(), None, now);
    // The strip has no enter animation — a young toast needs no fast tick.
    assert!(!state.toast_needs_anim(now));

    let middle = now + Duration::from_millis(3000);
    assert!(!state.toast_needs_anim(middle));

    let late = now + Duration::from_millis(crate::state::TOAST_TTL_SECS * 1000 - 300);
    assert!(state.toast_needs_anim(late));
}

#[test]
fn a_token_linked_toast_dies_with_its_token() {
    let now = Instant::now();
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.input.insert_str("[Image 1] ");
    state.push_toast_linked(
        ToastLevel::Info,
        "Image pasted as [Image 1]".into(),
        None,
        now,
        "[Image 1]".into(),
    );
    assert_eq!(live_toasts(&state, now).len(), 1);
    // Deleting the token kills the toast instantly — no 6 s ghost row.
    state.input.text = String::new();
    assert!(live_toasts(&state, now).is_empty());
    assert!(state.expire_toasts(now));
    assert!(state.toasts.is_empty());
}

#[test]
fn live_toasts_picks_two_most_recent_unexpired_oldest_first() {
    let now = Instant::now();
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(ToastLevel::Info, "first".into(), None, now);
    state.push_toast(ToastLevel::Info, "second".into(), None, now);
    // The queue is capped at TOAST_MAX, so pushing a third drops "first".
    state.push_toast(ToastLevel::Info, "third".into(), None, now);

    let live: Vec<&str> = live_toasts(&state, now)
        .iter()
        .map(|t| t.title.as_str())
        .collect();
    assert_eq!(live, ["second", "third"], "newest must come last");

    // An expired toast is skipped even while still queued.
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    let expired_at = now - Duration::from_secs(crate::state::TOAST_TTL_SECS + 1);
    state.push_toast(ToastLevel::Info, "stale".into(), None, expired_at);
    state.push_toast(ToastLevel::Info, "fresh".into(), None, now);
    let live: Vec<&str> = live_toasts(&state, now)
        .iter()
        .map(|t| t.title.as_str())
        .collect();
    assert_eq!(live, ["fresh"]);
}

#[test]
fn strip_renders_plain_line_at_left_edge_without_border() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(
        ToastLevel::Info,
        "copied 42 chars".to_string(),
        None,
        Instant::now(),
    );

    let rows = render_rows(&state, Rect::new(5, 2, 40, 2), 80, 10);
    let view = rows.join("\n");

    assert!(view.contains("copied 42 chars"));
    for border in ['╭', '╮', '╰', '╯', '─', '│'] {
        assert!(
            !view.contains(border),
            "border char {border} must not render"
        );
    }
    // One toast anchors to the strip's bottom row (y = 3), glyph at its left
    // edge (x = 5), cells to the left untouched.
    let row: Vec<char> = rows[3].chars().collect();
    assert!(
        row[..5].iter().all(|c| *c == ' '),
        "cells left of the strip must stay blank: {:?}",
        rows[3]
    );
    assert!(
        !row[5].is_whitespace(),
        "glyph must start at the strip's left edge: {:?}",
        rows[3]
    );
    assert!(rows[2].trim().is_empty(), "top strip row stays empty");
}

#[test]
fn strip_renders_title_and_detail_on_one_line() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(
        ToastLevel::Success,
        "build succeeded".to_string(),
        Some("2 warnings".to_string()),
        Instant::now(),
    );

    let rows = render_rows(&state, Rect::new(0, 0, 60, 2), 80, 10);

    let row = rows
        .iter()
        .find(|l| l.contains("build succeeded"))
        .expect("title line missing");
    assert!(
        row.contains("· 2 warnings"),
        "detail must follow the title on the same line: {row:?}"
    );
}

#[test]
fn strip_truncates_title_and_detail_to_width() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(
        ToastLevel::Warn,
        "a very long warning title that will not fit".to_string(),
        Some("detail does not fit either".to_string()),
        Instant::now(),
    );

    // 20 cells: glyph + space + 18 text cells — the detail is cut entirely.
    let rows = render_rows(&state, Rect::new(0, 0, 20, 1), 80, 10);
    let row: Vec<char> = rows[0].chars().collect();
    assert_eq!(row[0], '!', "warn glyph at the strip's left edge");
    assert_eq!(row[19], '…', "text truncated to the strip width");
    assert!(
        !row[..20].iter().collect::<String>().contains("detail"),
        "detail must be truncated away: {:?}",
        rows[0]
    );
    assert!(
        row[20..].iter().all(|c| *c == ' '),
        "nothing may be drawn past the strip width: {:?}",
        rows[0]
    );
}

#[test]
fn strip_renders_two_toasts_newest_on_the_bottom_row() {
    let now = Instant::now();
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(ToastLevel::Info, "older toast".into(), None, now);
    state.push_toast(
        ToastLevel::Error,
        "newer toast".into(),
        None,
        now + Duration::from_millis(100),
    );

    let rows = render_rows(&state, Rect::new(0, 5, 60, 2), 80, 10);

    assert!(
        rows[5].contains("older toast"),
        "older on top: {:?}",
        rows[5]
    );
    assert!(
        rows[6].contains("newer toast"),
        "newest sits closest to the input: {:?}",
        rows[6]
    );
}

#[test]
fn strip_renders_nothing_when_all_toasts_expired() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    let expired_at = Instant::now() - Duration::from_secs(crate::state::TOAST_TTL_SECS + 1);
    state.push_toast(ToastLevel::Info, "gone".into(), None, expired_at);

    let rows = render_rows(&state, Rect::new(0, 0, 60, 2), 80, 10);
    assert!(rows.iter().all(|r| r.trim().is_empty()));
}

#[test]
fn fading_toast_flattens_to_faint_style() {
    let now = Instant::now();
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    // Inside the fade window: TTL − 300 ms.
    let shown = now - Duration::from_millis(crate::state::TOAST_TTL_SECS * 1000 - 300);
    state.push_toast(ToastLevel::Error, "fading away".into(), None, shown);

    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(80, false);
    terminal
        .draw(|f| {
            draw(f, Rect::new(0, 0, 60, 1), &state, &opts);
        })
        .unwrap();

    let cell = terminal
        .backend()
        .buffer()
        .cell((2, 0))
        .expect("title cell exists");
    assert_eq!(
        Some(cell.fg),
        opts.theme.faint.fg,
        "a toast in its exit window renders fully faint"
    );
}

#[test]
fn test_autocopy_toast_detail_auto_vs_explicit() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 100, 30);
    state.config.ui.auto_copy = true;

    let now = pacode_types::time::now_ms();
    state.transcript.cells.push_back(Cell {
        id: now,
        kind: CellKind::Item(TranscriptKind::User {
            text: "autocopy test string".to_string(),
        }),
        version: 0,
        ts_ms: now,
        stats: None,
        perm_explain: None,
    });

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    // Render once to populate the buffer
    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();

    let dialog_area = Rect::new(0, 0, 60, 20);

    // 1. Selection with CopyRequest::Auto
    state.selection.start(2, 0, dialog_area, 0);
    state.selection.drag(15, 0, dialog_area, 0);
    state.selection.finish(); // Sets copy_request = CopyRequest::Auto
    assert_eq!(state.selection.copy_request, CopyRequest::Auto);

    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();

    // §8.1: draw stashes the copy in `pending_copy`; the event loop performs
    // it and `on_copy_done` posts the toast. Simulate that reply here.
    let copy = state
        .pending_copy
        .take()
        .expect("draw must stash the autocopy");
    assert!(copy.auto, "CopyRequest::Auto marks the pending copy auto");
    state.on_copy_done(copy.auto, copy.text.chars().count());

    let auto_toast = state
        .toasts
        .back()
        .expect("toast should be pushed on autocopy");
    assert!(auto_toast.title.starts_with("copied "));
    assert_eq!(
        auto_toast.detail.as_deref(),
        Some("disable autocopy in /config"),
        "CopyRequest::Auto must set detail 'disable autocopy in /config'"
    );

    // Render again to draw the pushed toast into the strip
    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();

    let backend = terminal.backend();
    let view = format!("{backend}");
    // Title and detail share one strip line above the input separator.
    let row = view
        .lines()
        .find(|l| l.contains("copied "))
        .expect("toast strip line missing");
    assert!(
        row.contains("disable autocopy in /config"),
        "detail must be inline on the strip line: {row:?}"
    );
    for border in ['╭', '╮', '╰', '╯'] {
        assert!(
            !view.contains(border),
            "strip has no box: border char {border} must not render"
        );
    }

    // 2. Selection with CopyRequest::Explicit
    state.selection.start(2, 0, dialog_area, 0);
    state.selection.drag(15, 0, dialog_area, 0);
    state.selection.finish();
    state.selection.request_explicit_copy(); // Sets copy_request = CopyRequest::Explicit
    assert_eq!(state.selection.copy_request, CopyRequest::Explicit);

    terminal
        .draw(|f| {
            prepare_and_draw_frame(f, &mut state);
        })
        .unwrap();

    let copy = state
        .pending_copy
        .take()
        .expect("draw must stash the explicit copy");
    assert!(!copy.auto, "CopyRequest::Explicit must not mark it auto");
    state.on_copy_done(copy.auto, copy.text.chars().count());

    let explicit_toast = state
        .toasts
        .back()
        .expect("toast should be pushed on explicit copy");
    assert!(explicit_toast.title.starts_with("copied "));
    assert_eq!(
        explicit_toast.detail.as_deref(),
        None,
        "CopyRequest::Explicit must have None detail"
    );
}
