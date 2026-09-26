//! Scroll state for the `/analyze` page.
//!
//! The page is taller than any terminal, so the overlay owns a scroll offset
//! and nothing else. Keeping it in its own type (rather than a bare `usize` in
//! the focus enum) is what lets the clamp live next to the rule it enforces:
//! the offset can never point past the last full screen of rows.

/// How far down the page is scrolled. Owned by the overlay layer, for as long
/// as the overlay is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnalyzeView {
    pub scroll: u16,
}

impl AnalyzeView {
    pub const fn new() -> Self {
        Self { scroll: 0 }
    }

    /// The furthest the page can scroll: the last row, or zero when the
    /// content already fits.
    pub fn max_scroll(row_count: usize, visible: usize) -> u16 {
        u16::try_from(row_count.saturating_sub(visible)).unwrap_or(u16::MAX)
    }

    /// Pull the offset back inside the content. Called before a draw and after
    /// every key, so a page that just got shorter (a fresh session) cannot
    /// leave the user staring at blank rows below the last line.
    pub fn clamp(&mut self, row_count: usize, visible: usize) {
        self.scroll = self.scroll.min(Self::max_scroll(row_count, visible));
    }

    /// Scroll by `delta` rows and re-clamp, so a key held at the end stops
    /// there instead of running the offset to `u16::MAX` and back.
    pub fn scroll_by(&mut self, delta: i32, row_count: usize, visible: usize) {
        let next = i64::from(self.scroll) + i64::from(delta);
        self.scroll = next.clamp(0, i64::from(Self::max_scroll(row_count, visible))) as u16;
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;
