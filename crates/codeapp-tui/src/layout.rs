//! Screen geometry (TUI spec §2, §8).
//!
//! ```text
//! ┌──────────────────────────────┬────────────┐
//! │ dialog  /  agent panel       │ rail       │
//! ├──────────────────────────────┤            │
//! │ ❯ input                      │            │
//! ├──────────────────────────────┤            │
//! │ footer row 1                 │            │
//! │ footer row 2                 │ anchor     │
//! └──────────────────────────────┴────────────┘
//! ```

use ratatui::layout::Rect;

/// Width tiers from spec §8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WidthTier {
    /// ≥ 130: rail 32, panel 50/50.
    Wide,
    /// 100–129: rail 32, panel 45/55.
    Normal,
    /// 80–99: rail 28, panel takes the whole dialog column.
    Narrow,
    /// < 80: no rail; plan/agents live in an overlay; footer compresses.
    #[default]
    Tiny,
}

impl WidthTier {
    pub fn for_width(cols: u16) -> WidthTier {
        match cols {
            0..=79 => WidthTier::Tiny,
            80..=99 => WidthTier::Narrow,
            100..=129 => WidthTier::Normal,
            _ => WidthTier::Wide,
        }
    }

    pub fn rail_width(self) -> u16 {
        match self {
            WidthTier::Wide | WidthTier::Normal => 32,
            WidthTier::Narrow => 28,
            WidthTier::Tiny => 0,
        }
    }

    /// Dialog share of the dialog column when the panel is open (percent).
    pub fn dialog_share_with_panel(self) -> u16 {
        match self {
            WidthTier::Wide => 50,
            WidthTier::Normal => 45,
            WidthTier::Narrow | WidthTier::Tiny => 0,
        }
    }
}

/// Number of lines the input area takes (1 + wrapped extra lines, max 6) plus its two
/// separator lines.
pub const INPUT_MIN_LINES: u16 = 1;
pub const INPUT_MAX_LINES: u16 = 6;
pub const FOOTER_LINES: u16 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenLayout {
    pub tier: WidthTier,
    /// Dialog transcript area (left column, above the input).
    pub dialog: Rect,
    /// Agent/task panel inside the dialog column; `None` when closed.
    pub panel: Option<Rect>,
    /// The vertical separator between dialog and panel (1 col), if a panel is open.
    pub panel_separator: Option<Rect>,
    /// Separator line above the input.
    pub input_top: Rect,
    pub input: Rect,
    /// Separator line below the input.
    pub input_bottom: Rect,
    pub footer: Rect,
    /// Rail column (right), full height; zero-sized on `Tiny`.
    pub rail: Rect,
    /// Vertical line between dialog column and rail.
    pub rail_separator: Rect,
    /// Toast area: 2 lines above `input_top`, right-aligned inside the dialog column.
    pub toast: Rect,
}

/// Compute the layout for a terminal of `area` with `input_lines` lines of input and
/// `panel_open`.
pub fn compute(area: Rect, input_lines: u16, panel_open: bool) -> ScreenLayout {
    let tier = WidthTier::for_width(area.width);
    let rail_w = tier.rail_width();

    // One blank column between the dialog column and the separator line.
    let (left_area, rail, rail_separator) = if rail_w > 0 && area.width > rail_w + 2 {
        let left_w = area.width - rail_w - 2;
        let rail_sep_x = area.x + left_w + 1;
        let rail_x = rail_sep_x + 1;
        (
            Rect::new(area.x, area.y, left_w, area.height),
            Rect::new(rail_x, area.y, rail_w, area.height),
            Rect::new(rail_sep_x, area.y, 1, area.height),
        )
    } else {
        (
            area,
            Rect::new(area.x + area.width, area.y, 0, area.height),
            Rect::new(area.x + area.width, area.y, 0, area.height),
        )
    };

    let footer_h = FOOTER_LINES.min(left_area.height);
    let footer_y = left_area.y + left_area.height.saturating_sub(footer_h);
    let footer = Rect::new(left_area.x, footer_y, left_area.width, footer_h);

    let input_bottom_h = if left_area.height > footer_h { 1 } else { 0 };
    let input_bottom_y = footer_y.saturating_sub(input_bottom_h);
    let input_bottom = Rect::new(left_area.x, input_bottom_y, left_area.width, input_bottom_h);

    let rem_for_input = input_bottom_y.saturating_sub(left_area.y);
    let clamped_input = input_lines.clamp(INPUT_MIN_LINES, INPUT_MAX_LINES);
    let input_h = clamped_input.min(rem_for_input);
    let input_y = input_bottom_y.saturating_sub(input_h);
    let input = Rect::new(left_area.x, input_y, left_area.width, input_h);

    let input_top_h = if input_y > left_area.y { 1 } else { 0 };
    let input_top_y = input_y.saturating_sub(input_top_h);
    let input_top = Rect::new(left_area.x, input_top_y, left_area.width, input_top_h);

    let dialog_h = input_top_y.saturating_sub(left_area.y);
    let dialog_y = left_area.y;

    let toast_h = 2.min(dialog_h);
    let toast_y = input_top_y.saturating_sub(toast_h);
    let toast_w = left_area.width.min(48);
    let toast_x = left_area.x + left_area.width.saturating_sub(toast_w);
    let toast = Rect::new(toast_x, toast_y, toast_w, toast_h);

    let (dialog, panel, panel_separator) = if panel_open {
        let share = tier.dialog_share_with_panel();
        if share == 0 {
            (
                Rect::new(left_area.x, dialog_y, 0, dialog_h),
                Some(Rect::new(left_area.x, dialog_y, left_area.width, dialog_h)),
                None,
            )
        } else {
            let sep_w = 1;
            let avail_w = left_area.width.saturating_sub(sep_w);
            let dialog_w = (avail_w as u32 * share as u32 / 100) as u16;
            let panel_w = avail_w.saturating_sub(dialog_w);
            (
                Rect::new(left_area.x, dialog_y, dialog_w, dialog_h),
                Some(Rect::new(
                    left_area.x + dialog_w + sep_w,
                    dialog_y,
                    panel_w,
                    dialog_h,
                )),
                Some(Rect::new(left_area.x + dialog_w, dialog_y, sep_w, dialog_h)),
            )
        }
    } else {
        (
            Rect::new(left_area.x, dialog_y, left_area.width, dialog_h),
            None,
            None,
        )
    };

    ScreenLayout {
        tier,
        dialog,
        panel,
        panel_separator,
        input_top,
        input,
        input_bottom,
        footer,
        rail,
        rail_separator,
        toast,
    }
}

/// Rail zones (spec §4): header (2 lines + blank), PLAN (1 + items, active item may
/// take 2 lines for its bar), AGENTS (1 + cards, min 4 lines, takes the rest), BACKGROUND
/// (1 + tasks, max 4 lines + `and N more`), anchor (2 lines at the bottom).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RailLayout {
    pub header: Rect,
    pub plan: Rect,
    pub agents: Rect,
    pub background: Rect,
    pub anchor: Rect,
}

/// Inputs that decide how much each zone gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RailDemand {
    /// Lines the plan wants (header + items + bar); never compressed except in
    /// agent-select mode where it becomes 1.
    pub plan_lines: u16,
    /// Lines the background zone wants (header + tasks), 0 when there are none.
    pub background_lines: u16,
    /// Lines the session stats block wants when idle (replaces agents).
    pub session_lines: u16,
    pub agent_select_mode: bool,
    pub idle: bool,
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod layout_tests;

/// Split the rail: header fixed, anchor fixed, plan as demanded (or 1 line), background
/// as demanded (max 5), agents = remainder with a 4-line minimum; when the remainder
/// is under 4 the agents zone collapses to 1 line (`5 agents ▾`).
pub fn compute_rail(rail: Rect, demand: RailDemand) -> RailLayout {
    if rail.width == 0 || rail.height == 0 {
        return RailLayout::default();
    }

    let x = rail.x;
    let w = rail.width;

    let header_h = 3.min(rail.height);
    let header = Rect::new(x, rail.y, w, header_h);

    let rem_after_header = rail.height.saturating_sub(header_h);
    let anchor_h = 2.min(rem_after_header);
    let anchor_y = rail.y + rail.height - anchor_h;
    let anchor = Rect::new(x, anchor_y, w, anchor_h);

    let avail = rail.height.saturating_sub(header_h + anchor_h);

    let plan_h = if demand.agent_select_mode {
        1.min(avail)
    } else {
        demand.plan_lines.min(avail)
    };
    let plan_y = header.bottom();
    let plan = Rect::new(x, plan_y, w, plan_h);

    let avail_after_plan = avail.saturating_sub(plan_h);
    let bg_lines = if demand.background_lines > 0 {
        demand.background_lines.min(5).min(avail_after_plan)
    } else {
        0
    };

    let remainder = avail_after_plan.saturating_sub(bg_lines);
    let agents_h = if remainder >= 4 {
        remainder
    } else if remainder >= 1 {
        1
    } else {
        0
    };
    let agents_y = plan.bottom();
    let agents = Rect::new(x, agents_y, w, agents_h);

    let bg_y = anchor.y.saturating_sub(bg_lines);
    let background = Rect::new(x, bg_y, w, bg_lines);

    RailLayout {
        header,
        plan,
        agents,
        background,
        anchor,
    }
}
