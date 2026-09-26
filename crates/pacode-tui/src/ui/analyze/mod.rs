//! The `/analyze` page: where the tokens went, and whether the context is any
//! good.
//!
//! One overlay, five sections, one rule: **every number says what it is**. The
//! four estimated rows carry the chars/4 caveat on the row above them, the
//! reply row and the provider totals are the only measured ones, the latency
//! quantiles come from the same [`pacode_types::LatencyHist`] `pacode stats`
//! prints, and anything the caps dropped is stated in the Coverage section
//! rather than quietly missing.
//!
//! The page is a pure function of an [`AnalyzeReport`]: no state of its own
//! beyond the scroll offset ([`AnalyzeView`]), no IO, and no assumption about
//! which session it is looking at.

pub mod rows;
pub mod view;

pub use view::AnalyzeView;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;
use pacode_types::AnalyzeReport;

/// Title on the top border.
pub const TITLE: &str = "analyze";
/// Key hints on the bottom border.
pub const HINT: &str = "↑/↓ scroll · esc close";

/// Smallest area that can hold a border plus one row of content. Below it the
/// page draws nothing: a 3-cell-wide "overlay" is a smudge, not information.
const MIN_WIDTH: u16 = 12;
const MIN_HEIGHT: u16 = 4;

/// Number of content rows inside `area`'s border.
pub fn visible_rows(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(2))
}

/// Draw the page into `area`.
///
/// The scroll offset is re-clamped here as well as in [`AnalyzeView::clamp`],
/// so a caller that forgets to clamp on a shrinking report gets a blank tail
/// for one frame at worst, never a panic and never a half-drawn page.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    report: &AnalyzeReport,
    view: &AnalyzeView,
    opts: &RenderOptions,
) {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return;
    }
    let width = usize::from(area.width.saturating_sub(2));
    let lines = rows::build(report, width, opts);
    let max_scroll = AnalyzeView::max_scroll(lines.len(), visible_rows(area));
    let scroll = view.scroll.min(max_scroll);
    let block = crate::ui::overlays::menu_block(TITLE, HINT, opts);
    // No `wrap()`: the rows are already clipped to the inner width, and
    // wrapping a bar row turns one measurement into two ragged ones.
    let paragraph = Paragraph::new(lines).block(block).scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
#[path = "analyze_tests.rs"]
mod analyze_tests;
