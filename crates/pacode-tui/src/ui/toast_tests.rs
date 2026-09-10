use std::time::Instant;

use pacode_render::RenderOptions;
use pacode_types::Config;
use pacode_types::TranscriptKind;
use pacode_types::state::ToastLevel;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use super::*;
use crate::state::AppState;
use crate::state::selection::CopyRequest;
use crate::state::transcript::{Cell, CellKind};

#[test]
fn enter_offset_eases_from_four_cells_to_zero() {
    assert_eq!(enter_offset(0), 4);
    // Ease-out: the first half covers most of the travel.
    assert_eq!(enter_offset(crate::state::TOAST_ENTER_MS / 2), 1);
    assert_eq!(enter_offset(crate::state::TOAST_ENTER_MS - 1), 0);
    assert_eq!(enter_offset(crate::state::TOAST_ENTER_MS), 0);
    assert_eq!(enter_offset(crate::state::TOAST_ENTER_MS * 10), 0);
    // Monotonically non-increasing.
    let mut prev = enter_offset(0);
    for age in 1..=crate::state::TOAST_ENTER_MS {
        let cur = enter_offset(age);
        assert!(cur <= prev, "offset grew at {age}ms");
        prev = cur;
    }
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
fn toast_needs_anim_only_while_a_toast_is_young_or_expiring() {
    let now = Instant::now();
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    assert!(!state.toast_needs_anim(now));

    state.push_toast(ToastLevel::Info, "fresh".into(), None, now);
    assert!(state.toast_needs_anim(now));

    let middle = now + std::time::Duration::from_millis(3000);
    assert!(!state.toast_needs_anim(middle));

    let late = now + std::time::Duration::from_millis(crate::state::TOAST_TTL_SECS * 1000 - 300);
    assert!(state.toast_needs_anim(late));
}

#[test]
fn test_toast_without_detail_no_dot_to_open_and_has_borders() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(
        ToastLevel::Info,
        "copied 42 chars".to_string(),
        None,
        Instant::now(),
    );

    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(80, false);
    let area = Rect::new(5, 2, 40, 4);

    terminal
        .draw(|f| {
            draw(f, area, &state, &opts);
        })
        .unwrap();

    let backend = terminal.backend();
    let view = format!("{backend}");

    assert!(view.contains("copied 42 chars"));
    // Assert no ". to open" text appears anywhere
    assert!(
        !view.contains(". to open"),
        "must not contain '. to open' fallback"
    );
    // Assert border characters are present
    assert!(view.contains('╭'), "top-left border '╭' expected");
    assert!(view.contains('╮'), "top-right border '╮' expected");
    assert!(view.contains('╰'), "bottom-left border '╰' expected");
    assert!(view.contains('╯'), "bottom-right border '╯' expected");
    assert!(view.contains('─'), "horizontal border '─' expected");
    assert!(view.contains('│'), "vertical border '│' expected");
}

#[test]
fn test_toast_with_detail_renders_both_lines() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(
        ToastLevel::Success,
        "build succeeded".to_string(),
        Some("2 warnings".to_string()),
        Instant::now(),
    );

    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(80, false);
    let area = Rect::new(5, 2, 40, 4);

    terminal
        .draw(|f| {
            draw(f, area, &state, &opts);
        })
        .unwrap();

    let backend = terminal.backend();
    let view = format!("{backend}");

    // Assert a toast with a detail renders both lines
    assert!(view.contains("build succeeded"), "title line missing");
    assert!(view.contains("2 warnings"), "detail line missing");
    assert!(
        !view.contains(". to open"),
        "must not contain '. to open' fallback"
    );
    // Assert border characters are present
    assert!(view.contains('╭'));
    assert!(view.contains('╮'));
    assert!(view.contains('╰'));
    assert!(view.contains('╯'));
}

#[test]
fn test_toast_detail_not_rendered_when_height_insufficient() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.push_toast(
        ToastLevel::Warn,
        "warning alert".to_string(),
        Some("should not fit".to_string()),
        Instant::now(),
    );

    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let opts = RenderOptions::new(80, false);
    // Height 3: border consumes 2 rows, inner height is 1 -> detail requires inner.height > 1
    let area = Rect::new(5, 2, 40, 3);

    terminal
        .draw(|f| {
            draw(f, area, &state, &opts);
        })
        .unwrap();

    let backend = terminal.backend();
    let view = format!("{backend}");

    assert!(view.contains("warning alert"));
    assert!(
        !view.contains("should not fit"),
        "detail line should not render when inner height is 1"
    );
    assert!(!view.contains(". to open"));
}

#[test]
fn test_all_toast_levels_render_with_borders() {
    let levels = [
        (ToastLevel::Success, "success toast"),
        (ToastLevel::Error, "error toast"),
        (ToastLevel::Warn, "warn toast"),
        (ToastLevel::Info, "info toast"),
    ];

    for (level, title) in levels {
        let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
        state.push_toast(level, title.to_string(), None, Instant::now());

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let opts = RenderOptions::new(80, false);
        let area = Rect::new(0, 0, 40, 4);

        terminal
            .draw(|f| {
                draw(f, area, &state, &opts);
            })
            .unwrap();

        let backend = terminal.backend();
        let view = format!("{backend}");

        assert!(view.contains(title));
        assert!(view.contains('╭'));
        assert!(view.contains('╯'));
        assert!(!view.contains(". to open"));
    }
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
    });

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    // Render once to populate the buffer
    terminal
        .draw(|f| {
            crate::ui::draw(f, &mut state);
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
            crate::ui::draw(f, &mut state);
        })
        .unwrap();

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

    // Render again to draw the pushed toast
    terminal
        .draw(|f| {
            crate::ui::draw(f, &mut state);
        })
        .unwrap();

    let backend = terminal.backend();
    let view = format!("{backend}");
    assert!(view.contains("copied "));
    assert!(view.contains("disable autocopy in /config"));
    assert!(!view.contains(". to open"));
    assert!(view.contains('╭'));
    assert!(view.contains('╯'));

    // 2. Selection with CopyRequest::Explicit
    state.selection.start(2, 0, dialog_area, 0);
    state.selection.drag(15, 0, dialog_area, 0);
    state.selection.finish();
    state.selection.request_explicit_copy(); // Sets copy_request = CopyRequest::Explicit
    assert_eq!(state.selection.copy_request, CopyRequest::Explicit);

    terminal
        .draw(|f| {
            crate::ui::draw(f, &mut state);
        })
        .unwrap();

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
