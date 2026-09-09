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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthTier {
    /// ≥ 130: rail 32, panel 50/50.
    Wide,
    /// 100–129: rail 32, panel 45/55.
    Normal,
    /// 80–99: rail 28, panel takes the whole dialog column.
    Narrow,
    /// < 80: no rail; plan/agents live in an overlay; footer compresses.
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

impl Default for WidthTier {
    fn default() -> Self {
        WidthTier::Tiny
    }
}

/// Compute the layout for a terminal of `area` with `input_lines` lines of input and
/// `panel_open`.
pub fn compute(area: Rect, input_lines: u16, panel_open: bool) -> ScreenLayout {
    let _ = (area, input_lines, panel_open);
    todo!("layout::compute")
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

/// Split the rail: header fixed, anchor fixed, plan as demanded (or 1 line), background
/// as demanded (max 5), agents = remainder with a 4-line minimum; when the remainder
/// is under 4 the agents zone collapses to 1 line (`5 agents ▾`).
pub fn compute_rail(rail: Rect, demand: RailDemand) -> RailLayout {
    let _ = (rail, demand);
    todo!("layout::compute_rail")
}
