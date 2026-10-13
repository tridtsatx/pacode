//! The transcript as the last draw left it: where the view is scrolled,
//! which cell sits under a row, and which cells show their whole output — a
//! click on a row, `ctrl+o` on the newest call, and the exploration groups
//! (`ui::toolgroup`) a run of calls opens out of and folds back into.

use super::{CellKind, Transcript};
use crate::ui::toolgroup;
use pacode_types::TranscriptKind;

impl Transcript {
    /// The cell drawn at content line `line`, from the most recent draw.
    pub fn cell_at_line(&self, line: usize) -> Option<u64> {
        let index = self.cell_lines.partition_point(|(_, _, end)| *end <= line);
        let (cell_id, start, end) = self.cell_lines.get(index)?;
        (*start <= line && line < *end).then_some(*cell_id)
    }

    /// `ctrl+o`: expand the newest tool cell, or collapse it back when it is
    /// already expanded. Every tool call is expandable — any status (a denied
    /// call reveals the refused command) and any preview length (expansion
    /// also shows the call's full argument, which the collapsed cell never
    /// had room for). A `!cmd` result is drawn as a bash call and folds the
    /// same way, so it expands too. When the newest call is part of an
    /// exploration run, the whole run opens (or folds back into its group).
    /// Returns true on change.
    pub fn toggle_last_expandable(&mut self) -> bool {
        let id = self.cells.iter().rev().find_map(|c| match &c.kind {
            CellKind::Item(
                TranscriptKind::ToolCall { .. } | TranscriptKind::BashCommand { .. },
            ) => Some(c.id),
            _ => None,
        });
        let Some(id) = id else { return false };
        self.toggle_run(id)
    }

    /// A click on content line `line` of the most recent draw: a group's
    /// header opens the whole group, any other row toggles the cell it
    /// belongs to (a group's call row opens that one call). Returns true on
    /// change.
    pub fn toggle_at_line(&mut self, line: usize) -> bool {
        let head = self
            .group_heads
            .iter()
            .find(|head| head.lines.contains(&line))
            .map(|head| (head.first, head.last));
        if let Some((first, last)) = head {
            return self.open_group(first, last);
        }
        self.cell_at_line(line)
            .is_some_and(|cell_id| self.toggle_expanded(cell_id))
    }

    /// Show or hide a cell's full output. Collapsing a call of an exploration
    /// run folds the whole run back into its group. Returns true when
    /// something changed.
    pub fn toggle_expanded(&mut self, cell_id: u64) -> bool {
        let Some(index) = self.cell_index(cell_id) else {
            return false;
        };
        if self.expanded.contains(&cell_id)
            && let Some(run) = toolgroup::run_around(&self.cells, index, self.thinking())
        {
            self.set_run_expanded(run, false);
            return true;
        }
        if !self.expanded.remove(&cell_id) {
            self.expanded.insert(cell_id);
        }
        // The rendered height changes, so the cached lines for that cell are stale.
        self.cache.invalidate_cell(cell_id);
        true
    }

    /// Open the run around `cell_id` call by call, or fold it back when any
    /// call of it is open; a cell in no run toggles alone.
    fn toggle_run(&mut self, cell_id: u64) -> bool {
        let Some(index) = self.cell_index(cell_id) else {
            return false;
        };
        let Some(run) = toolgroup::run_around(&self.cells, index, self.thinking()) else {
            return self.toggle_expanded(cell_id);
        };
        let open = !self
            .cells
            .range(run.clone())
            .any(|cell| self.expanded.contains(&cell.id));
        self.set_run_expanded(run, open);
        true
    }

    /// Open every call from `first` to `last` — a group drawn last frame.
    fn open_group(&mut self, first: u64, last: u64) -> bool {
        let (Some(start), Some(end)) = (self.cell_index(first), self.cell_index(last)) else {
            return false;
        };
        if start > end {
            return false;
        }
        self.set_run_expanded(start..end + 1, true);
        true
    }

    fn set_run_expanded(&mut self, run: std::ops::Range<usize>, open: bool) {
        for id in toolgroup::member_ids(&self.cells, run) {
            if open {
                self.expanded.insert(id);
            } else {
                self.expanded.remove(&id);
            }
            self.cache.invalidate_cell(id);
        }
    }

    /// Whether the last draw showed reasoning: hidden reasoning draws nothing
    /// and so never breaks an exploration run.
    fn thinking(&self) -> bool {
        self.render_profile
            .as_ref()
            .is_some_and(|profile| profile.options.thinking)
    }

    pub fn is_expanded(&self, cell_id: u64) -> bool {
        self.expanded.contains(&cell_id)
    }

    /// Record the rendered line count and viewport height from the most recent draw,
    /// clamping `scroll_from_bottom` so the view can never sit past the top.
    pub fn record_render(&mut self, total_lines: usize, viewport: usize) {
        self.rendered_lines = Some(total_lines);
        self.viewport_height = Some(viewport);
        let max_scroll = total_lines.saturating_sub(viewport);
        self.scroll_from_bottom = self.scroll_from_bottom.min(max_scroll);
        self.first_visible_line = max_scroll.saturating_sub(self.scroll_from_bottom);
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let max_scroll = match (self.rendered_lines, self.viewport_height) {
            (Some(total), Some(vp)) => total.saturating_sub(vp),
            _ => 0,
        };
        let new_scroll =
            (self.scroll_from_bottom as i64 + delta as i64).clamp(0, max_scroll as i64);
        self.scroll_from_bottom = new_scroll as usize;
        self.first_visible_line = max_scroll.saturating_sub(self.scroll_from_bottom);
    }

    pub fn scroll_to_top(&mut self) {
        let max_scroll = match (self.rendered_lines, self.viewport_height) {
            (Some(total), Some(vp)) => total.saturating_sub(vp),
            _ => 0,
        };
        self.scroll_from_bottom = max_scroll;
        self.first_visible_line = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
        self.first_visible_line = self
            .rendered_lines
            .zip(self.viewport_height)
            .map_or(0, |(total, viewport)| total.saturating_sub(viewport));
    }

    pub fn is_at_top(&self) -> bool {
        match (self.rendered_lines, self.viewport_height) {
            (Some(total), Some(vp)) => {
                if total == 0 {
                    return false;
                }
                let max_scroll = total.saturating_sub(vp);
                self.scroll_from_bottom >= max_scroll
            }
            _ => false,
        }
    }

    /// Screen position (0-based line offset from the top of the viewport) for a cell.
    /// Returns `None` if the cell is not currently within the visible viewport.
    pub fn cell_screen_position(&mut self, cell_id: u64, viewport: usize) -> Option<usize> {
        if viewport == 0 {
            return None;
        }
        self.sync_cell_line_index();
        let index = *self.cell_line_index.get(&cell_id)?;
        let (id, start, end) = self.cell_lines.get(index)?;
        if *id != cell_id {
            return None;
        }
        let first = self.first_visible_line;
        let viewport_end = first.saturating_add(viewport);
        if *end <= first || *start >= viewport_end {
            None
        } else {
            Some(start.saturating_sub(first))
        }
    }

    /// `cell_lines` is rewritten by the render pass, which keeps the index in
    /// step; anything that clears the spans without the index (a reset) leaves
    /// a map pointing at rows that no longer exist, so rebuild on the length
    /// mismatch before trusting a position.
    fn sync_cell_line_index(&mut self) {
        if self.cell_line_index.len() == self.cell_lines.len() {
            return;
        }
        self.cell_line_index.clear();
        self.cell_line_index.extend(
            self.cell_lines
                .iter()
                .enumerate()
                .map(|(index, (id, _, _))| (*id, index)),
        );
    }
}
