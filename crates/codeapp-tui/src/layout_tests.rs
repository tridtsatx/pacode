use ratatui::layout::Rect;

use crate::layout::{RailDemand, WidthTier, compute, compute_rail};

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
    let layout = compute(area, 1, false);

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
    let layout = compute(area, 1, true);

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
    let layout = compute(area, 1, false);

    assert_eq!(layout.tier, WidthTier::Tiny);
    assert_eq!(layout.rail.width, 0);
    assert_eq!(layout.rail_separator.width, 0);
    assert_eq!(layout.dialog.width, 60);
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
fn test_toast_rect_inside_dialog() {
    // 1. Without panel
    let area = Rect::new(0, 0, 140, 40);
    let layout = compute(area, 1, false);
    // Dialog column: x = 0, width = 106, dialog_y = 0, input_top = y: 35
    assert_eq!(layout.dialog.width, 106);
    // Toast width = min(dialog.width - 2, 60) = 60
    assert_eq!(layout.toast.width, 60);
    assert_eq!(layout.toast.height, 2);
    // Toast must be strictly inside dialog: x >= dialog.x and toast.right() <= dialog.right() - 1
    assert!(layout.toast.x >= layout.dialog.x);
    assert_eq!(
        layout.toast.x + layout.toast.width,
        layout.dialog.x + layout.dialog.width - 1
    );
    // Toast must be above input separator: toast.bottom() == layout.input_top.y
    assert_eq!(layout.toast.y + layout.toast.height, layout.input_top.y);

    // 2. With panel (dialog is narrower)
    let layout_panel = compute(area, 1, true);
    assert_eq!(layout_panel.dialog.width, 52);
    // Toast width = min(52 - 2, 60) = 50
    assert_eq!(layout_panel.toast.width, 50);
    assert!(layout_panel.toast.x >= layout_panel.dialog.x);
    assert_eq!(
        layout_panel.toast.x + layout_panel.toast.width,
        layout_panel.dialog.x + layout_panel.dialog.width - 1
    );
    assert_eq!(
        layout_panel.toast.y + layout_panel.toast.height,
        layout_panel.input_top.y
    );
}

#[test]
fn test_anchor_rows() {
    let area = Rect::new(0, 0, 120, 34);
    let layout = compute(area, 1, false);
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
