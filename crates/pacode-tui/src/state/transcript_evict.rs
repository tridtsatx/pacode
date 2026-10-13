//! The transcript's caps: evicting the oldest cells past the cell and byte
//! ceilings, and splicing in an older history page under the same caps.

use std::collections::{HashSet, VecDeque};

use pacode_types::{PermissionId, TranscriptItem, TranscriptKind};

use super::{Cell, CellKind, Transcript, cell_bytes, is_client_id};

enum CapSide {
    Oldest,
    Newest,
}

impl Transcript {
    pub fn set_max_cells(&mut self, max_cells: usize) {
        self.max_cells = max_cells;
        self.enforce_limits();
    }

    pub fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
        self.enforce_limits();
    }

    pub(super) fn is_evicted(&self, cell_id: u64) -> bool {
        !is_client_id(cell_id)
            && self.evicted_floor.is_some_and(|floor| cell_id <= floor)
            && self.cell_index(cell_id).is_none()
    }

    fn rendered_line_count(&self, cell_id: u64) -> usize {
        self.cell_line_index
            .get(&cell_id)
            .and_then(|index| self.cell_lines.get(*index))
            .filter(|(id, _, _)| *id == cell_id)
            .map(|(_, start, end)| end.saturating_sub(*start))
            .unwrap_or(0)
    }

    /// Whether the cell must survive an eviction pass: the streaming cell and
    /// any prompt still waiting for a verdict.
    fn is_pinned(cell_id: u64, live_cell: Option<u64>, kind: &CellKind) -> bool {
        Some(cell_id) == live_cell || matches!(kind, CellKind::Item(TranscriptKind::Permission(_)))
    }

    fn forget_evicted(&mut self, cell_id: u64) -> usize {
        self.expanded.remove(&cell_id);
        self.stable_revisions.remove(&cell_id);
        self.cache.invalidate_cell(cell_id);
        self.tool_streams.remove(cell_id);
        let size = self.forget_bytes(cell_id);
        if !is_client_id(cell_id) {
            self.evicted_floor = Some(
                self.evicted_floor
                    .map_or(cell_id, |floor| floor.max(cell_id)),
            );
        }
        size
    }

    fn replace_with_capped(
        &mut self,
        source: VecDeque<Cell>,
        side: CapSide,
        prefix_len: usize,
    ) -> (usize, usize) {
        let live_present = self
            .live_cell
            .is_some_and(|live_id| source.iter().any(|cell| cell.id == live_id));
        let limit = match side {
            CapSide::Oldest => self.max_cells,
            CapSide::Newest if live_present => self.max_cells.saturating_add(1),
            CapSide::Newest => self.max_cells,
        };
        if source.len() <= limit && self.bytes <= self.max_bytes {
            let retained_prefix = prefix_len.min(source.len());
            self.cells = source;
            return (0, retained_prefix);
        }

        let source_len = source.len();
        let mut to_evict = source_len.saturating_sub(limit);
        let mut to_free = self.bytes.saturating_sub(self.max_bytes);
        let mut retained = VecDeque::with_capacity(source_len.min(limit.max(1)));
        let mut retained_prefix = 0usize;
        let mut evicted_lines = 0usize;
        let live_cell = self.live_cell;

        match side {
            CapSide::Newest => {
                for (index, cell) in source.into_iter().enumerate().rev() {
                    let over = to_evict > 0 || to_free > 0;
                    if over && index > 0 && !Self::is_pinned(cell.id, live_cell, &cell.kind) {
                        evicted_lines =
                            evicted_lines.saturating_add(self.rendered_line_count(cell.id));
                        to_free = to_free.saturating_sub(self.forget_evicted(cell.id));
                        to_evict = to_evict.saturating_sub(1);
                    } else {
                        if index < prefix_len {
                            retained_prefix += 1;
                        }
                        retained.push_front(cell);
                    }
                }
            }
            CapSide::Oldest => {
                for (index, cell) in source.into_iter().enumerate() {
                    let over = to_evict > 0 || to_free > 0;
                    if over
                        && index + 1 < source_len
                        && !Self::is_pinned(cell.id, live_cell, &cell.kind)
                    {
                        evicted_lines =
                            evicted_lines.saturating_add(self.rendered_line_count(cell.id));
                        to_free = to_free.saturating_sub(self.forget_evicted(cell.id));
                        to_evict = to_evict.saturating_sub(1);
                    } else {
                        retained.push_back(cell);
                    }
                }
            }
        }

        self.cells = retained;
        (evicted_lines, retained_prefix)
    }

    /// Prepend older items (history page).
    pub fn prepend(&mut self, items: Vec<TranscriptItem>, has_more: bool) -> usize {
        self.has_more_history = has_more;
        self.loading_history = false;

        let mut seen: HashSet<u64> = self.cells.iter().map(|cell| cell.id).collect();
        let mut asked: HashSet<PermissionId> = self
            .cells
            .iter()
            .filter_map(|cell| match &cell.kind {
                CellKind::Item(TranscriptKind::Permission(req)) => Some(req.id.clone()),
                _ => None,
            })
            .collect();
        let mut older = VecDeque::with_capacity(items.len());
        for item in items {
            if let TranscriptKind::Permission(req) = &item.kind
                && !asked.insert(req.id.clone())
            {
                continue;
            }
            if !seen.insert(item.seq) {
                continue;
            }
            let cell = Cell {
                id: item.seq,
                kind: CellKind::Item(item.kind),
                version: 0,
                ts_ms: item.ts_ms,
                stats: None,
                perm_explain: None,
            };
            let size = cell_bytes(&cell);
            self.set_cell_bytes(cell.id, size);
            older.push_back(cell);
        }
        if older.is_empty() {
            return 0;
        }

        let older_len = older.len();
        let current = std::mem::take(&mut self.cells);
        let mut combined = VecDeque::with_capacity(older_len + current.len());
        for cell in older {
            combined.push_back(cell);
        }
        for cell in current {
            combined.push_back(cell);
        }

        let (evicted_lines, retained_prefix) =
            self.replace_with_capped(combined, CapSide::Newest, older_len);
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(evicted_lines);

        let Transcript {
            cells,
            expanded,
            cache,
            render_profile,
            tool_streams,
            ..
        } = self;
        render_profile.as_ref().map_or(0, |profile| {
            crate::ui::dialog::measure_cell_lines(
                cells,
                retained_prefix,
                expanded,
                cache,
                tool_streams,
                profile,
            )
        })
    }

    /// Oldest item seq still on screen: the cursor a history page is asked for.
    /// Client rows are skipped — a notice carries no seq, and handing its id to
    /// `GetHistory` would page from the start of the session forever.
    pub fn oldest_seq(&self) -> Option<u64> {
        self.cells
            .iter()
            .find(|cell| !is_client_id(cell.id))
            .map(|cell| cell.id)
    }

    /// Evict cells past `max_cells` or `max_bytes` — oldest first, never the
    /// streaming cell or a prompt waiting for a verdict — dropping the cache of
    /// everything that goes.
    pub(super) fn enforce_limits(&mut self) {
        if self.cells.len() <= self.max_cells && self.bytes <= self.max_bytes {
            return;
        }
        let source = std::mem::take(&mut self.cells);
        let _ = self.replace_with_capped(source, CapSide::Oldest, 0);
    }
}
