//! Mouse selection in the dialog area, anchored to transcript content.
//!
//! Selection used to live in screen coordinates and was copied out of the screen
//! buffer. That made it wrong the moment the transcript moved: scrolling left the
//! highlight on the same rows while different text sat under it, and a selection
//! could never reach further than one screenful. Anchoring both ends to content
//! line indices fixes both — scrolling now carries the highlight with its text,
//! and dragging past the edge keeps extending a selection the viewport cannot show.

use std::ops::RangeInclusive;

use ratatui::layout::Rect;

#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;

/// Pending copy actions triggered by mouse up or keyboard shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CopyRequest {
    #[default]
    None,
    /// Triggered on mouse Up when auto_copy is enabled.
    Auto,
    /// Explicitly requested via ctrl+shift+c.
    Explicit,
}

/// One end of a selection: a column, and the transcript content line it sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord)]
pub struct Point {
    /// Index into the transcript's rendered lines, independent of scrolling.
    pub line: usize,
    /// Column offset from the left edge of the dialog area.
    pub col: u16,
}

/// Active mouse selection, in transcript content coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    pub active: bool,
    pub dragging: bool,
    pub copy_request: CopyRequest,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn clear(&mut self) {
        self.active = false;
        self.dragging = false;
        self.copy_request = CopyRequest::None;
    }

    /// Begin a selection at a screen position. `first_visible_line` is the content
    /// line currently drawn at the top of `dialog_rect`.
    pub fn start(&mut self, col: u16, row: u16, dialog_rect: Rect, first_visible_line: usize) {
        let point = point_at(col, row, dialog_rect, first_visible_line);
        self.anchor = point;
        self.head = point;
        self.active = true;
        self.dragging = true;
        self.copy_request = CopyRequest::None;
    }

    /// Move the moving end of the selection to a screen position.
    pub fn drag(&mut self, col: u16, row: u16, dialog_rect: Rect, first_visible_line: usize) {
        if self.dragging {
            self.head = point_at(col, row, dialog_rect, first_visible_line);
        }
    }

    /// Extend the moving end to a content line directly. Used while the reader
    /// scrolls with a drag still in progress: the pointer has not moved, but the
    /// text under it has, and the selection must follow the text.
    pub fn drag_to_line(&mut self, line: usize, col: u16) {
        if self.dragging {
            self.head = Point { line, col };
        }
    }

    pub fn finish(&mut self) {
        if self.dragging {
            self.dragging = false;
            if self.is_empty() {
                self.active = false;
                self.copy_request = CopyRequest::None;
            } else {
                self.active = true;
                self.copy_request = CopyRequest::Auto;
            }
        }
    }

    pub fn request_explicit_copy(&mut self) {
        if self.active && !self.is_empty() {
            self.copy_request = CopyRequest::Explicit;
        }
    }

    /// Both ends in reading order (top-to-bottom, left-to-right).
    pub fn normalized(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Content lines the selection covers, if it covers anything.
    pub fn line_range(&self) -> Option<RangeInclusive<usize>> {
        if !self.active || self.is_empty() {
            return None;
        }
        let (start, end) = self.normalized();
        Some(start.line..=end.line)
    }

    /// Column range selected on `line`, as offsets from the left edge of the
    /// dialog area. `width` bounds the last column of a fully covered line.
    pub fn col_range_for_line(&self, line: usize, width: u16) -> Option<RangeInclusive<u16>> {
        if width == 0 {
            return None;
        }
        let range = self.line_range()?;
        if !range.contains(&line) {
            return None;
        }
        let (start, end) = self.normalized();
        let last = width - 1;

        let (c_start, c_end) = if start.line == end.line {
            (start.col, end.col.min(last))
        } else if line == start.line {
            (start.col, last)
        } else if line == end.line {
            (0, end.col.min(last))
        } else {
            (0, last)
        };

        if c_start <= c_end {
            Some(c_start..=c_end)
        } else {
            None
        }
    }

    /// Whether a content cell is selected.
    pub fn contains_cell(&self, line: usize, col: u16, width: u16) -> bool {
        self.col_range_for_line(line, width)
            .is_some_and(|range| range.contains(&col))
    }
}

/// The content point under a screen position, clamped into the dialog area.
fn point_at(col: u16, row: u16, dialog_rect: Rect, first_visible_line: usize) -> Point {
    let (c, r) = clamp_to_rect(col, row, dialog_rect);
    Point {
        line: first_visible_line + (r.saturating_sub(dialog_rect.y)) as usize,
        col: c.saturating_sub(dialog_rect.x),
    }
}

/// Clamps given (col, row) coordinate to the interior of `rect`.
pub fn clamp_to_rect(col: u16, row: u16, rect: Rect) -> (u16, u16) {
    if rect.width == 0 || rect.height == 0 {
        return (col, row);
    }
    let min_col = rect.x;
    let max_col = rect.x.saturating_add(rect.width).saturating_sub(1);
    let min_row = rect.y;
    let max_row = rect.y.saturating_add(rect.height).saturating_sub(1);

    (col.clamp(min_col, max_col), row.clamp(min_row, max_row))
}
