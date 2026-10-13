//! Screen geometry (TUI spec §2, §8).
//!
//! ```text
//! ┌──────────────────────────────┬────────────┐
//! │ dialog  /  agent panel       │ rail       │
//! │ toast strip · image strip    │            │
//! │ ──────────────────────────── │            │
//! │ ❯ input                      │            │
//! │ ──────────────────────────── │            │
//! │ footer row 1                 │            │
//! │ footer row 2                 │ anchor     │
//! └──────────────────────────────┴────────────┘
//! ```
//!
//! The prompt is two plain full-width rules — the `input_top` and
//! `input_bottom` rows — with the text rows between them. No side borders:
//! the text gets the whole left column.

use std::time::Instant;

use ratatui::layout::Rect;

use crate::state::AppState;

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
/// Toast strip capacity: up to this many plain text lines directly above the
/// input separator (spec §7).
pub const TOAST_STRIP_MAX: u16 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenLayout {
    pub screen: Rect,
    pub tier: WidthTier,
    /// Dialog transcript area (left column, above the input).
    pub dialog: Rect,
    /// Agent/task panel inside the dialog column; `None` when closed.
    pub panel: Option<Rect>,
    /// The vertical separator between dialog and panel (1 col), if a panel is open.
    pub panel_separator: Option<Rect>,
    /// The rule row above the prompt text (full left-column width).
    pub input_top: Rect,
    /// Pasted-image preview strip directly above the prompt's top rule;
    /// zero-height when no `[Image N]` tokens are live (spec §1).
    pub preview: Rect,
    /// The prompt's text rows between the rules: prompt glyph + text, full
    /// left-column width.
    pub input: Rect,
    /// The rule row below the prompt text.
    pub input_bottom: Rect,
    pub footer: Rect,
    /// Rail column (right), full height; zero-sized on `Tiny`.
    pub rail: Rect,
    /// Vertical line between dialog column and rail.
    pub rail_separator: Rect,
    /// Toast strip: up to `TOAST_STRIP_MAX` plain lines above the preview
    /// strip and the prompt's top rule, borrowing rows from the dialog like the
    /// preview strip. Zero-height when no toast is live.
    pub toast: Rect,
}

pub fn current(state: &AppState) -> ScreenLayout {
    compute_for_state(
        Rect::new(0, 0, state.cols, state.rows),
        state,
        Instant::now(),
    )
}

pub fn compute_for_state(area: Rect, state: &AppState, now: Instant) -> ScreenLayout {
    let panel_open = (state.panel.target.is_some()
        || matches!(state.focus, crate::state::Focus::Panel { .. }))
        && !state.agent_replaces_dialog();
    let temporary = compute(area, 1, panel_open, 0, 0);
    let input_lines = state.input.wrapped_lines(temporary.input.width);
    let preview_lines = if state.is_bottom_picker() {
        0
    } else {
        state.image_strip_rows()
    };
    let toast_lines = crate::ui::toast::live_toasts(state, now).len() as u16;
    compute(area, input_lines, panel_open, preview_lines, toast_lines)
}

/// Compute the layout for a terminal of `area` with `input_lines` lines of input,
/// `panel_open`, `preview_lines` rows of image preview strip (0 = none), and
/// `toast_lines` rows of toast strip (0 = none).
pub fn compute(
    area: Rect,
    input_lines: u16,
    panel_open: bool,
    preview_lines: u16,
    toast_lines: u16,
) -> ScreenLayout {
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
    // Above the input: the top rule, then the preview strip above it. The
    // strip takes space from the dialog (which can scroll), never from the
    // input.
    let input_top_h = if input_y > left_area.y { 1 } else { 0 };
    let input = Rect::new(left_area.x, input_y, left_area.width, input_h);

    let input_top_y = input_y.saturating_sub(input_top_h);
    let input_top = Rect::new(left_area.x, input_top_y, left_area.width, input_top_h);
    let preview_h = preview_lines.min(input_top_y.saturating_sub(left_area.y));
    let preview_y = input_top_y.saturating_sub(preview_h);
    let preview = Rect::new(left_area.x, preview_y, left_area.width, preview_h);

    // The toast strip borrows rows from the bottom of the dialog, directly
    // above the preview strip and the top rule — the same mechanism as the
    // preview strip, so the strip's lines never overdraw transcript text.
    let dialog_y = left_area.y;
    let toast_h = toast_lines
        .min(TOAST_STRIP_MAX)
        .min(preview_y.saturating_sub(dialog_y));
    let toast_y = preview_y.saturating_sub(toast_h);
    let toast = Rect::new(left_area.x, toast_y, left_area.width, toast_h);

    let dialog_h = toast_y.saturating_sub(dialog_y);

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
        screen: area,
        tier,
        dialog,
        panel,
        panel_separator,
        input_top,
        preview,
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
    /// Static row where the anchor starts (defaults to `rail.y + rail.height - 3`).
    pub anchor_y: Option<u16>,
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod layout_tests;

/// Split the rail: header fixed, anchor pinned statically at the input's bottom separator
/// row (`anchor_y` and `anchor_y + 1`), zones above end one row above the anchor.
pub fn compute_rail(rail: Rect, demand: RailDemand) -> RailLayout {
    if rail.width == 0 || rail.height == 0 {
        return RailLayout::default();
    }

    let x = rail.x;
    let w = rail.width;

    let anchor_y = demand
        .anchor_y
        .unwrap_or_else(|| rail.y + rail.height.saturating_sub(3));
    let anchor_h = 2.min(rail.bottom().saturating_sub(anchor_y));
    let anchor = Rect::new(x, anchor_y, w, anchor_h);

    let avail_end = anchor_y;
    let header_h = 3.min(avail_end.saturating_sub(rail.y));
    let header = Rect::new(x, rail.y, w, header_h);

    let avail = avail_end.saturating_sub(header.bottom());

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
