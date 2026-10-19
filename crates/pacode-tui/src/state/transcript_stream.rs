//! The paced reveal of a live answer or reasoning cell, and the output a
//! running tool call streams into its cell.

use std::time::Instant;

use pacode_render::{StreamBuffer, StreamKind, StreamOp};
use pacode_types::{ToolStatus, TranscriptKind};

use super::{Cell, CellKind, Transcript};

impl Transcript {
    pub(super) fn stable_prefix_len(cell: &Cell) -> Option<usize> {
        match &cell.kind {
            CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
                Some(pacode_render::markdown::split_stable_tail(text).0.len())
            }
            _ => None,
        }
    }

    pub(super) fn refresh_stable_revision(
        &mut self,
        cell_id: u64,
        current_len: Option<usize>,
        force: bool,
    ) {
        let Some(current_len) = current_len else {
            self.stable_revisions.remove(&cell_id);
            return;
        };
        let (mut revision, previous_len) = self
            .stable_revisions
            .remove(&cell_id)
            .unwrap_or((0, current_len));
        if force || previous_len != current_len {
            revision = revision.wrapping_add(1);
        }
        self.stable_revisions
            .insert(cell_id, (revision, current_len));
    }

    /// Hand the paced stream over to a different cell.
    ///
    /// The buffer belongs to whichever cell is live. Carrying it across a switch
    /// replayed one cell's undrained tail into the next one, which is how the
    /// first grapheme of an answer ended up alone in the reasoning cell above it
    /// while the rest of the sentence went to the answer cell. Anything still
    /// buffered is flushed into the cell that produced it before the new cell
    /// starts with an empty buffer.
    pub(super) fn set_live_cell(&mut self, seq: u64, now: Instant) {
        if self.live_cell == Some(seq) {
            self.stream.get_or_insert_with(|| StreamBuffer::new(now));
            return;
        }
        if self.live_cell.is_some() && self.has_backlog() {
            self.flush_stream();
        }
        self.live_cell = Some(seq);
        self.stream = Some(StreamBuffer::new(now));
    }

    /// `TextDelta`/`ReasoningDelta` for `item_seq`: queue into the stream buffer.
    pub fn push_delta(&mut self, item_seq: u64, text: &str, reasoning: bool, now: Instant) {
        if text.is_empty() {
            return;
        }
        let kind = if reasoning {
            StreamKind::Reasoning
        } else {
            StreamKind::Text
        };
        if self.live_cell == Some(item_seq) {
            let sb = self.stream.get_or_insert_with(|| StreamBuffer::new(now));
            sb.push(kind, text);
        } else if self.cell_index(item_seq).is_some() {
            self.set_live_cell(item_seq, now);
            if let Some(sb) = self.stream.as_mut() {
                sb.push(kind, text);
            }
            if let Some(index) = self.cell_index(item_seq) {
                let cell = &mut self.cells[index];
                cell.version = cell.version.wrapping_add(1);
            }
        }
    }

    /// Output a running tool call has produced, for the cell at `cell_id`.
    ///
    /// Anything that is not a tool cell still `Running` is refused, and clears
    /// the cell's buffer if it had one: a delta that outlives its call (a
    /// chunk in flight when the final item lands) must not leave a live body
    /// behind that nothing ever draws and nothing ever frees.
    pub fn push_tool_output(&mut self, cell_id: u64, chunk: &str) {
        let running = self.cell_index(cell_id).is_some_and(|index| {
            matches!(
                &self.cells[index].kind,
                CellKind::Item(TranscriptKind::ToolCall {
                    status: ToolStatus::Running,
                    ..
                })
            )
        });

        if running {
            self.tool_streams.push(cell_id, chunk);
        } else {
            self.tool_streams.remove(cell_id);
        }
    }

    /// Reveal into the live cell: the stable prefix length, and the bytes the
    /// cell grew by.
    fn apply_stream_ops(&mut self, index: usize, ops: Vec<StreamOp>) -> Option<(usize, usize)> {
        let cell = self.cells.get_mut(index)?;
        let mut added = 0usize;
        for op in ops {
            match op {
                StreamOp::Text(text) => {
                    if let CellKind::Item(TranscriptKind::Assistant {
                        text: cell_text, ..
                    }) = &mut cell.kind
                    {
                        cell_text.push_str(&text);
                        added = added.saturating_add(text.len());
                    }
                }
                StreamOp::Reasoning(text) => {
                    if let CellKind::Item(TranscriptKind::Reasoning {
                        text: cell_text, ..
                    }) = &mut cell.kind
                    {
                        cell_text.push_str(&text);
                        added = added.saturating_add(text.len());
                    }
                }
                StreamOp::CloseReasoning => {}
            }
        }
        cell.version = cell.version.wrapping_add(1);
        Self::stable_prefix_len(cell).map(|stable_len| (stable_len, added))
    }

    fn grow_cell_bytes(&mut self, cell_id: u64, added: usize) {
        if added == 0 {
            return;
        }
        let size = self.cell_bytes.entry(cell_id).or_default();
        *size = size.saturating_add(added);
        self.bytes = self.bytes.saturating_add(added);
    }

    /// Reveal paced text into the live cell. Returns true when something was revealed.
    pub fn tick_stream(&mut self, now: Instant) -> bool {
        let Some(live_id) = self.live_cell else {
            return false;
        };
        let Some(ref mut sb) = self.stream else {
            return false;
        };
        let ops = sb.reveal(now);
        if ops.is_empty() {
            return false;
        }

        let Some(index) = self.cell_index(live_id) else {
            return false;
        };
        let Some((stable_len, added)) = self.apply_stream_ops(index, ops) else {
            return false;
        };
        self.grow_cell_bytes(live_id, added);
        self.refresh_stable_revision(live_id, Some(stable_len), false);
        self.apply_pending_final();
        true
    }

    /// When the backlog is drained and a final item is waiting, install it.
    fn apply_pending_final(&mut self) {
        if self.has_backlog() {
            return;
        }
        if let Some(item) = self.pending_final.take() {
            self.live_cell = None;
            self.stream = None;
            self.insert_or_replace(item.seq, item.kind, item.ts_ms);
        }
    }

    /// Flush the stream (item complete or interrupted).
    pub fn flush_stream(&mut self) {
        let Some(live_id) = self.live_cell else {
            return;
        };
        let Some(ref mut sb) = self.stream else {
            return;
        };
        let ops = sb.flush();
        if let Some(index) = self.cell_index(live_id)
            && let Some((stable_len, added)) = self.apply_stream_ops(index, ops)
        {
            self.grow_cell_bytes(live_id, added);
            self.refresh_stable_revision(live_id, Some(stable_len), false);
        }
        if let Some(item) = self.pending_final.take() {
            self.live_cell = None;
            self.stream = None;
            self.insert_or_replace(item.seq, item.kind, item.ts_ms);
        }
    }

    pub fn has_backlog(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn stream_backlog_chars(&self) -> usize {
        self.stream.as_ref().map(|s| s.backlog_chars()).unwrap_or(0)
    }

    pub fn has_pending_final(&self) -> bool {
        self.pending_final.is_some()
    }
}
