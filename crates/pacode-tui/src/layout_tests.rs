use ratatui::layout::Rect;

use crate::layout::{
    PROMPT_FRAME_MIN_WIDTH, PROMPT_INSET, RailDemand, WidthTier, compute, compute_rail,
};

#[test]
fn test_width_tiers() {
    assert_eq!(WidthTier::for_width(60), WidthTier::Tiny);
    assert_eq!(WidthTier::for_width(79), WidthTier::Tiny);
    assert_eq!(WidthTier::for_width(80), WidthTier::Narrow);
    assert_eq!(WidthTier::for_width(99), WidthTier::Narrow);
    assert_eq!(WidthTier::for_width(100), WidthTier::Normal);
    assert_eq!(WidthTier::for_width(129), WidthTier::Normal);
    assert_eq!(WidthTier::for_width(130), WidthTier::Wide);
    assert_eq!(WidthTier::for_width(200), WidthTier::Wide);

    assert_eq!(WidthTier::Tiny.rail_width(), 0);
    assert_eq!(WidthTier::Narrow.rail_width(), 28);
    assert_eq!(WidthTier::Normal.rail_width(), 32);
    assert_eq!(WidthTier::Wide.rail_width(), 32);
}

#[test]
fn test_layout_wide_no_panel() {
    let area = Rect::new(0, 0, 140, 40);
    let layout = compute(area, 1, false, 0, 0);

    assert_eq!(layout.tier, WidthTier::Wide);
    assert_eq!(layout.rail.width, 32);
    assert_eq!(layout.rail_separator.width, 1);
    // left column = 140 - 32 - 1 = 107
    assert_eq!(layout.dialog.width, 106);
    assert!(layout.panel.is_none());
    assert!(layout.panel_separator.is_none());
    assert_eq!(layout.footer.height, 2);
    assert_eq!(layout.input_bottom.height, 1);
    assert_eq!(layout.input.height, 1);
    assert_eq!(layout.input_top.height, 1);
    assert_eq!(layout.dialog.height, 40 - 2 - 1 - 1 - 1);
}

#[test]
fn test_layout_wide_with_panel() {
    let area = Rect::new(0, 0, 140, 40);
    let layout = compute(area, 1, true, 0, 0);

    assert_eq!(layout.tier, WidthTier::Wide);
    assert!(layout.panel.is_some());
    assert!(layout.panel_separator.is_some());
    let left_w = 140 - 32 - 2; // 106 (one blank column before the separator)
    let avail_w = left_w - 1; // 105
    let dialog_w = avail_w * 50 / 100; // 52
    let panel_w = avail_w - dialog_w; // 53
    assert_eq!(layout.dialog.width, dialog_w);
    assert_eq!(layout.panel.unwrap().width, panel_w);
    assert_eq!(layout.panel_separator.unwrap().width, 1);
}

#[test]
fn test_layout_tiny() {
    let area = Rect::new(0, 0, 60, 20);
    let layout = compute(area, 1, false, 0, 0);

    assert_eq!(layout.tier, WidthTier::Tiny);
    assert_eq!(layout.rail.width, 0);
    assert_eq!(layout.rail_separator.width, 0);
    assert_eq!(layout.dialog.width, 60);
}

#[test]
fn test_preview_strip_sits_directly_above_the_prompt_frame() {
    let area = Rect::new(0, 0, 140, 40);
    let layout = compute(area, 1, false, 6, 0);

    // Strip borrows rows from the dialog; input/footer geometry is unchanged.
    assert_eq!(layout.preview.height, 6);
    assert_eq!(layout.preview.bottom(), layout.input_top.y);
    assert_eq!(layout.input_top.bottom(), layout.input.y);
    assert_eq!(layout.preview.width, layout.input_top.width);
    assert_eq!(layout.input.height, 1);
    assert_eq!(layout.footer.height, 2);
    assert_eq!(layout.dialog.height, 40 - 2 - 1 - 1 - 1 - 6);

    // Zero lines -> zero-height strip, layout identical to no-strip.
    let layout_zero = compute(area, 1, false, 0, 0);
    assert_eq!(layout_zero.preview.height, 0);
    assert_eq!(layout_zero.dialog.height, 40 - 2 - 1 - 1 - 1);

    // A strip taller than the space above the input is clamped; the input
    // itself never shrinks.
    let tiny = Rect::new(0, 0, 140, 10);
    let layout_tiny = compute(tiny, 1, false, 6, 0);
    assert_eq!(layout_tiny.preview.bottom(), layout_tiny.input_top.y);
    assert!(layout_tiny.preview.height < 10 - 2 - 1);
    assert_eq!(layout_tiny.input.height, 1);
}

#[test]
fn test_compute_rail_normal() {
    let rail = Rect::new(100, 0, 32, 34);
    let demand = RailDemand {
        plan_lines: 5,
        background_lines: 3,
        session_lines: 0,
        agent_select_mode: false,
        idle: false,
        anchor_y: None,
    };
    let layout = compute_rail(rail, demand);

    assert_eq!(layout.header.height, 3);
    assert_eq!(layout.anchor.height, 2);
    assert_eq!(layout.plan.height, 5);
    assert_eq!(layout.background.height, 3);
    // avail = 31 - 3 = 28
    // after plan (5) = 23
    // remainder = 23 - 3 = 20 >= 4
    assert_eq!(layout.agents.height, 20);
    assert_eq!(layout.agents.y, layout.plan.bottom());
    assert_eq!(layout.background.y, layout.agents.bottom());
    assert_eq!(layout.anchor.y, layout.background.bottom());
}

#[test]
fn test_compute_rail_select_mode() {
    let rail = Rect::new(100, 0, 32, 34);
    let demand = RailDemand {
        plan_lines: 5,
        background_lines: 0,
        session_lines: 0,
        agent_select_mode: true,
        idle: false,
        anchor_y: None,
    };
    let layout = compute_rail(rail, demand);

    assert_eq!(layout.plan.height, 1);
    // avail = 31 - 3 = 28; after plan (1) = 27
    assert_eq!(layout.agents.height, 27);
}

#[test]
fn test_compute_rail_tight_space() {
    let rail = Rect::new(100, 0, 32, 10);
    let demand = RailDemand {
        plan_lines: 4,
        background_lines: 2,
        session_lines: 0,
        agent_select_mode: false,
        idle: false,
        anchor_y: None,
    };
    let layout = compute_rail(rail, demand);

    assert_eq!(layout.header.height, 3);
    assert_eq!(layout.plan.height, 4);
    assert_eq!(layout.background.height, 0);
    assert_eq!(layout.agents.height, 0);
    assert_eq!(layout.anchor.height, 2);
}

#[test]
fn test_toast_strip_borrows_rows_above_input_separator() {
    let area = Rect::new(0, 0, 140, 40);

    // No toasts: zero-height strip, the dialog keeps every row.
    let layout = compute(area, 1, false, 0, 0);
    assert_eq!(layout.toast.height, 0);
    assert_eq!(layout.dialog.height, 40 - 2 - 1 - 1 - 1);

    // Two toasts: a 2-row strip directly above the input separator,
    // spanning the full left column, its rows borrowed from the dialog.
    let layout = compute(area, 1, false, 0, 2);
    assert_eq!(layout.toast.height, 2);
    assert_eq!(layout.toast.bottom(), layout.input_top.y);
    assert_eq!(layout.toast.x, layout.dialog.x);
    assert_eq!(layout.toast.width, 106);
    assert_eq!(layout.dialog.bottom(), layout.toast.y);
    assert_eq!(layout.dialog.height, 40 - 2 - 1 - 1 - 1 - 2);

    // The strip is capped at TOAST_STRIP_MAX rows no matter what is asked.
    let capped = compute(area, 1, false, 0, 9);
    assert_eq!(capped.toast.height, crate::layout::TOAST_STRIP_MAX);

    // Panel open: the strip still spans the full column — the panel bottom
    // shrinks with the dialog, so the strip's rows are free across it.
    let layout_panel = compute(area, 1, true, 0, 1);
    assert_eq!(layout_panel.toast.height, 1);
    assert_eq!(layout_panel.toast.bottom(), layout_panel.input_top.y);
    assert_eq!(layout_panel.dialog.bottom(), layout_panel.toast.y);
    assert_eq!(layout_panel.panel.unwrap().bottom(), layout_panel.toast.y);
}

#[test]
fn test_anchor_rows() {
    let area = Rect::new(0, 0, 120, 34);
    let layout = compute(area, 1, false, 0, 0);
    // input_bottom.y is row 31
    assert_eq!(layout.input_bottom.y, 31);
    let demand = RailDemand {
        plan_lines: 3,
        background_lines: 1,
        session_lines: 0,
        agent_select_mode: false,
        idle: false,
        anchor_y: Some(layout.input_bottom.y),
    };
    let rail_layout = compute_rail(layout.rail, demand);
    // Anchor pinned statically at input_bottom.y and + 1
    assert_eq!(rail_layout.anchor.y, layout.input_bottom.y);
    assert_eq!(rail_layout.anchor.height, 2);
    assert_eq!(rail_layout.anchor.bottom(), layout.input_bottom.y + 2);
    // The rail zones above end one row above the anchor
    assert_eq!(rail_layout.background.bottom(), rail_layout.anchor.y);
}

/// The prompt frame takes its side borders from the input's width, never a
/// row from anything: at every size the frame, the input and the footer
/// stack without overlap and nothing runs past the screen.
#[test]
fn the_prompt_frame_fits_at_every_size() {
    for (w, h) in [(60, 20), (80, 24), (120, 34), (200, 60)] {
        for lines in [1, 3, 6] {
            for (preview, toast) in [(0, 0), (4, 2)] {
                let l = compute(Rect::new(0, 0, w, h), lines, false, preview, toast);
                let ctx = format!("{w}x{h} lines={lines} preview={preview}");
                assert!(l.prompt_framed(), "{ctx}");
                assert_eq!(l.input.x, l.input_top.x + PROMPT_INSET, "{ctx}");
                assert_eq!(l.input.right() + PROMPT_INSET, l.input_top.right(), "{ctx}");
                assert_eq!(l.input_top.bottom(), l.input.y, "{ctx}");
                assert_eq!(l.input.bottom(), l.input_bottom.y, "{ctx}");
                assert_eq!(l.input_bottom.bottom(), l.footer.y, "{ctx}");
                assert_eq!(l.footer.bottom(), h, "{ctx}");
                assert_eq!(l.preview.bottom(), l.input_top.y, "{ctx}");
                assert_eq!(l.toast.bottom(), l.preview.y, "{ctx}");
                assert_eq!(l.dialog.bottom(), l.toast.y, "{ctx}");
                assert!(l.input.height >= 1, "{ctx}");
                assert_eq!(l.prompt_frame().height, l.input.height + 2, "{ctx}");
            }
        }
    }
    // Too narrow for side borders: the input keeps the whole column.
    let narrow = compute(
        Rect::new(0, 0, PROMPT_FRAME_MIN_WIDTH - 1, 10),
        1,
        false,
        0,
        0,
    );
    assert!(!narrow.prompt_framed());
    assert_eq!(narrow.input.width, PROMPT_FRAME_MIN_WIDTH - 1);
}
