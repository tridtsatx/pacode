//! Screen-coordinate mouse selection state in the dialog area.

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

/// Active mouse selection state in screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
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

    pub fn start(&mut self, col: u16, row: u16, dialog_rect: Rect) {
        let (c, r) = clamp_to_rect(col, row, dialog_rect);
        self.anchor = (c, r);
        self.head = (c, r);
        self.active = true;
        self.dragging = true;
        self.copy_request = CopyRequest::None;
    }

    pub fn drag(&mut self, col: u16, row: u16, dialog_rect: Rect) {
        if self.dragging {
            let (c, r) = clamp_to_rect(col, row, dialog_rect);
            self.head = (c, r);
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

    /// Returns normalized start and end: `((start_col, start_row), (end_col, end_row))`
    /// in reading order (top-to-bottom, left-to-right).
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        let (a_col, a_row) = self.anchor;
        let (h_col, h_row) = self.head;

        if a_row < h_row || (a_row == h_row && a_col <= h_col) {
            ((a_col, a_row), (h_col, h_row))
        } else {
            ((h_col, h_row), (a_col, a_row))
        }
    }

    /// Returns the selected row range clamped to the dialog rect, if active and non-empty.
    pub fn row_range(&self, dialog_rect: Rect) -> Option<RangeInclusive<u16>> {
        if !self.active || self.is_empty() || dialog_rect.width == 0 || dialog_rect.height == 0 {
            return None;
        }
        let ((_, start_row), (_, end_row)) = self.normalized();
        let min_dialog_row = dialog_rect.y;
        let max_dialog_row = dialog_rect
            .y
            .saturating_add(dialog_rect.height)
            .saturating_sub(1);
        let r_start = start_row.max(min_dialog_row);
        let r_end = end_row.min(max_dialog_row);
        if r_start <= r_end {
            Some(r_start..=r_end)
        } else {
            None
        }
    }

    /// Returns the inclusive column range for a specific row within the dialog rect.
    pub fn col_range_for_row(&self, row: u16, dialog_rect: Rect) -> Option<RangeInclusive<u16>> {
        let row_range = self.row_range(dialog_rect)?;
        if !row_range.contains(&row) {
            return None;
        }
        let ((start_col, start_row), (end_col, end_row)) = self.normalized();
        let dialog_left = dialog_rect.x;
        let dialog_right = dialog_rect
            .x
            .saturating_add(dialog_rect.width)
            .saturating_sub(1);

        let (c_start, c_end) = if start_row == end_row {
            (start_col.max(dialog_left), end_col.min(dialog_right))
        } else if row == start_row {
            (start_col.max(dialog_left), dialog_right)
        } else if row == end_row {
            (dialog_left, end_col.min(dialog_right))
        } else {
            (dialog_left, dialog_right)
        };

        if c_start <= c_end {
            Some(c_start..=c_end)
        } else {
            None
        }
    }

    /// Whether a specific (col, row) cell is inside the selection.
    pub fn contains_cell(&self, col: u16, row: u16, dialog_rect: Rect) -> bool {
        self.col_range_for_row(row, dialog_rect)
            .is_some_and(|range| range.contains(&col))
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
