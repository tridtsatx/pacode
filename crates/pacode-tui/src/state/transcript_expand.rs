//! Which cells show their whole output: a click on a row, `ctrl+o` on the
//! newest call, and the exploration groups (`ui::toolgroup`) a run of calls
//! opens out of and folds back into.

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
}
