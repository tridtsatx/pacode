//! The transcript (main dialog and the agent panel share this renderer).

use std::collections::{HashMap, HashSet, VecDeque};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::cache::{CacheKey, KeyPart};
use pacode_render::markdown::split_stable_tail;
use pacode_render::{LineCache, RenderOptions};
use pacode_types::TranscriptKind;
use pacode_types::transcript::ToolStatus;

use super::cells::{
    assistant_markdown, assistant_stats_line, render_background_result, render_item_inner,
};
use super::toolgroup::{self, Segment};
use crate::state::image_cache::ImageCellCache;
use crate::state::transcript::cell_extent::{CellExtents, ExtentKey};
use crate::state::transcript::{
    Cell, CellKind, StableRevision, ToolStreamView, ToolStreams, Transcript,
};

#[path = "dialog_plain.rs"]
mod plain;

pub(crate) use plain::{PlainLines, PlainWindow, measure_cell_lines, plain_lines};

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod dialog_tests;

#[cfg(test)]
#[path = "dialog_group_tests.rs"]
mod dialog_group_tests;

#[cfg(test)]
#[path = "dialog_layout_tests.rs"]
mod dialog_layout_tests;

#[derive(Default)]
pub(crate) struct PreparedDialog {
    lines: Vec<Line<'static>>,
}

pub(crate) fn prepare(
    area: Rect,
    transcript: &mut Transcript,
    opts: &RenderOptions,
    anim_frame: u64,
    welcome_ms: u64,
) -> PreparedDialog {
    if area.width == 0 || area.height == 0 {
        return PreparedDialog::default();
    }

    let width = area.width;
    transcript.set_render_profile(width, opts, anim_frame);
    let Transcript {
        cells,
        expanded,
        cache,
        extents,
        cells_laid_out,
        stable_revisions,
        header,
        cell_lines,
        cell_line_index,
        rendered_lines,
        viewport_height,
        scroll_from_bottom,
        first_visible_line,
        live_cell,
        tool_streams,
        group_heads,
        images,
        ..
    } = transcript;

    // The banner lives outside `cells` so a transcript seq can never collide
    // with it; it is drawn every frame.
    let header_lines = header.as_ref().map_or_else(Vec::new, |info| {
        crate::ui::header::render(info, width, opts, anim_frame, welcome_ms)
    });
    let src = CellsSource {
        cells,
        expanded,
        stable_revisions,
        live_id: *live_cell,
        tool_streams,
        width,
        anim_frame,
    };
    let mut parts = RenderParts { cache, images };
    let segments = toolgroup::segments(cells.iter(), expanded, opts.thinking);
    let mut laid_out = vec![false; segments.len()];
    let (mut counts, fresh) = measure(&src, &segments, opts, &mut parts, extents, &mut laid_out);

    // Scrolled up, the view holds on to what it shows: the cell (and line
    // within it) at the top of the last frame. `scroll_from_bottom` alone
    // counts from the tail, so every line streamed in below would push the
    // text being read up by one row.
    let anchor = (*scroll_from_bottom > 0)
        .then(|| line_anchor(cell_lines, *first_visible_line))
        .flatten();
    let asked_scroll = *scroll_from_bottom;
    let viewport = area.height as usize;
    let mut pass = 0usize;
    let (visible_lines, total_lines) = loop {
        pass += 1;
        let (starts, total_lines) = lay_out(
            cells,
            &segments,
            &counts,
            header_lines.len(),
            cell_lines,
            group_heads,
        );
        cell_line_index.clear();
        cell_line_index.extend(
            cell_lines
                .iter()
                .enumerate()
                .map(|(index, (id, _, _))| (*id, index)),
        );

        let max_scroll = total_lines.saturating_sub(viewport);
        let top = match anchor {
            Some(ViewAnchor::Cell { id, offset }) => cell_line_index
                .get(&id)
                .and_then(|&index| cell_lines.get(index))
                .map(|&(_, start, end)| {
                    if start < end {
                        return (start + offset).min(end - 1);
                    }
                    // Drawn as no line: a call that slid into its group's
                    // `… N more` row (its empty span sits on the group's header)
                    // holds on to the last head row, not the line above it.
                    group_heads
                        .iter()
                        .find(|head| head.lines.start == start)
                        .map_or(start, |head| head.lines.end.saturating_sub(1))
                }),
            Some(ViewAnchor::Header { line }) => Some(line),
            None => None,
        };
        *scroll_from_bottom = asked_scroll;
        if let Some(top) = top {
            *scroll_from_bottom = max_scroll - top.min(max_scroll);
        }
        *scroll_from_bottom = (*scroll_from_bottom).min(max_scroll);
        *first_visible_line = max_scroll.saturating_sub(*scroll_from_bottom);

        let start_line = if total_lines <= viewport {
            0
        } else {
            max_scroll.saturating_sub(*scroll_from_bottom)
        };
        // A block that rendered to another height than its record (an entry
        // the line cache dropped, drawn again after its image's cells aged
        // out) is re-recorded and the frame laid out again, so it matches a
        // full render. A fixed record is a cache hit the next time, so this
        // settles at once; the last pass takes the rows as they come.
        let fetched = fetch_visible(
            &src,
            &segments,
            &starts,
            &mut counts,
            &fresh,
            &header_lines,
            start_line..start_line + viewport,
            opts,
            &mut parts,
            extents,
            &mut laid_out,
            pass < MAX_LAYOUT_PASSES,
        );
        if let Some(lines) = fetched {
            break (lines, total_lines);
        }
    };
    *cells_laid_out = laid_out.iter().filter(|done| **done).count();
    *rendered_lines = Some(total_lines);
    *viewport_height = Some(viewport);

    let mut visible_lines = visible_lines;
    let is_scrolled_up = total_lines > viewport && *scroll_from_bottom > 0;
    if is_scrolled_up && !visible_lines.is_empty() {
        let last_idx = visible_lines.len() - 1;
        let below = *scroll_from_bottom;
        let noun = if below == 1 { "line" } else { "lines" };
        visible_lines[last_idx] = Line::from(Span::styled(
            format!("↓ {below} more {noun} below"),
            opts.theme.dim,
        ));
    }

    PreparedDialog {
        lines: visible_lines,
    }
}

/// Layout passes one frame may take before it gives up on settling.
const MAX_LAYOUT_PASSES: usize = 8;

/// The caches a block renders into: the line cache and the decoded inline images.
struct RenderParts<'a> {
    cache: &'a mut LineCache,
    images: &'a mut ImageCellCache,
}

/// The line count of every segment, without rendering what was measured
/// before: a cacheable segment answers from `extents` (§8.1 — a long
/// transcript lays out in O(cells) lookups and renders only the viewport),
/// and only a miss renders, caching the lines and recording the count. The
/// segments that draw anew every frame — a running call or group, the live
/// answer — render here and their lines come back by segment index, so the
/// fetch reuses them instead of drawing them twice.
fn measure(
    src: &CellsSource<'_>,
    segments: &[Segment],
    opts: &RenderOptions,
    parts: &mut RenderParts<'_>,
    extents: &mut CellExtents,
    laid_out: &mut [bool],
) -> (Vec<usize>, HashMap<usize, Vec<Line<'static>>>) {
    extents.sync(parts.cache.generation());
    // Records of cells gone by a path that did not drop them (a reset of the
    // deque from outside) are dead weight; past twice the cells, sweep.
    if extents.len() > src.cells.len().saturating_mul(2) + 64 {
        let live: HashSet<u64> = src.cells.iter().map(|cell| cell.id).collect();
        extents.retain(|id| live.contains(&id));
    }
    let mut counts = Vec::with_capacity(segments.len());
    let mut fresh = HashMap::new();
    for (index, segment) in segments.iter().enumerate() {
        if is_fresh(src, segment) {
            let lines = segment_block(src, segment, opts, parts).into_owned();
            counts.push(lines.len());
            fresh.insert(index, lines);
            laid_out[index] = true;
            continue;
        }
        let (id, key) = extent_key(src, segment, opts);
        if let Some(lines) = extents.get(id, &key) {
            counts.push(lines);
            continue;
        }
        let lines = segment_block(src, segment, opts, parts).lines().len();
        extents.set(id, key, lines);
        counts.push(lines);
        laid_out[index] = true;
    }
    (counts, fresh)
}

/// Where every segment starts, from the counts: the click map (`spans`, one
/// entry per cell) and the group header rows are rebuilt with them.
fn lay_out(
    cells: &VecDeque<Cell>,
    segments: &[Segment],
    counts: &[usize],
    header_lines: usize,
    spans: &mut Vec<(u64, usize, usize)>,
    group_heads: &mut Vec<toolgroup::GroupHead>,
) -> (Vec<usize>, usize) {
    spans.clear();
    spans.reserve(cells.len());
    group_heads.clear();
    let mut starts = Vec::with_capacity(segments.len());
    let mut total_lines = header_lines;
    for (segment, count) in segments.iter().zip(counts) {
        starts.push(total_lines);
        let end = total_lines + count;
        match segment {
            Segment::Cell(cell) => {
                if let Some(cell) = cells.get(*cell) {
                    spans.push((cell.id, total_lines, end));
                }
            }
            Segment::Group(range) => {
                toolgroup::push_spans(cells, range.clone(), total_lines..end, spans, group_heads)
            }
        }
        total_lines = end;
    }
    (starts, total_lines)
}

/// The rows of `window` (content lines): the banner's, then those of every
/// segment overlapping it, cloned one row at a time — a block off screen is
/// never rendered or copied. Each block is fetched and copied before the next
/// one is rendered, so an insert can never evict a block this frame still
/// borrows. With `strict`, `None` when a block rendered to another height than
/// its record: the record is fixed and the caller lays out again.
#[allow(clippy::too_many_arguments)]
fn fetch_visible(
    src: &CellsSource<'_>,
    segments: &[Segment],
    starts: &[usize],
    counts: &mut [usize],
    fresh: &HashMap<usize, Vec<Line<'static>>>,
    header_lines: &[Line<'static>],
    window: std::ops::Range<usize>,
    opts: &RenderOptions,
    parts: &mut RenderParts<'_>,
    extents: &mut CellExtents,
    laid_out: &mut [bool],
    strict: bool,
) -> Option<Vec<Line<'static>>> {
    let mut visible = Vec::with_capacity(window.len());
    take_range(0, &window, header_lines, &mut visible);
    for (index, segment) in segments.iter().enumerate() {
        let start = starts[index];
        let end = start + counts[index];
        if end <= window.start {
            continue;
        }
        if start >= window.end {
            break;
        }
        laid_out[index] = true;
        if let Some(lines) = fresh.get(&index) {
            take_range(start, &window, lines, &mut visible);
            continue;
        }
        let block = segment_block(src, segment, opts, parts);
        let lines = block.lines();
        if lines.len() != counts[index] {
            let rendered = lines.len();
            let (id, key) = extent_key(src, segment, opts);
            extents.set(id, key, rendered);
            counts[index] = rendered;
            if strict {
                return None;
            }
            log::warn!("transcript layout did not settle; drawing as rendered");
        }
        take_range(start, &window, lines, &mut visible);
    }
    Some(visible)
}

/// Append the rows of `lines` that fall in `window` (content lines), given
/// that `lines[0]` is content line `at`.
fn take_range(
    at: usize,
    window: &std::ops::Range<usize>,
    lines: &[Line<'static>],
    out: &mut Vec<Line<'static>>,
) {
    let lo = window.start.saturating_sub(at).min(lines.len());
    let hi = window.end.saturating_sub(at).min(lines.len());
    if lo < hi {
        out.extend(lines[lo..hi].iter().cloned());
    }
}

/// A segment that draws anew every frame, so no record can answer for it: a
/// running call (its body grows without the cell changing), a group holding
/// one (the spinner and the sweep move), the live answer (its tail).
fn is_fresh(src: &CellsSource<'_>, segment: &Segment) -> bool {
    match segment {
        Segment::Cell(index) => {
            let cell = &src.cells[*index];
            is_running_tool(cell)
                || (src.live_id == Some(cell.id)
                    && matches!(cell.kind, CellKind::Item(TranscriptKind::Assistant { .. })))
        }
        Segment::Group(range) => toolgroup::has_running(src.cells, range.clone()),
    }
}

/// The record a cacheable segment's height is kept under: the cell's id, or a
/// group's first cell's id with the group's fingerprint (`toolgroup::cache_key`).
fn extent_key(src: &CellsSource<'_>, segment: &Segment, opts: &RenderOptions) -> (u64, ExtentKey) {
    match segment {
        Segment::Cell(index) => {
            let cell = &src.cells[*index];
            (
                cell.id,
                ExtentKey {
                    version: cell.version,
                    width: src.width,
                    expanded: src.expanded.contains(&cell.id),
                    thinking: opts.thinking,
                    has_stats: cell.stats.is_some(),
                    live: src.live_id == Some(cell.id),
                    context: opts.cache_epoch,
                    group: 0,
                },
            )
        }
        Segment::Group(range) => {
            let key = toolgroup::cache_key(src.cells, range.clone(), src.width, opts.cache_epoch);
            let fingerprint = match key.part {
                KeyPart::Group(fingerprint) => fingerprint,
                KeyPart::Whole | KeyPart::StablePrefix(_) | KeyPart::Row => 0,
            };
            (
                key.cell,
                ExtentKey {
                    version: 0,
                    width: src.width,
                    expanded: false,
                    thinking: opts.thinking,
                    has_stats: false,
                    live: false,
                    context: opts.cache_epoch,
                    // Never 0: that is a lone cell's record under the same id.
                    group: fingerprint.max(1),
                },
            )
        }
    }
}

fn is_running_tool(cell: &Cell) -> bool {
    matches!(
        cell.kind,
        CellKind::Item(TranscriptKind::ToolCall {
            status: ToolStatus::Running,
            ..
        })
    )
}

/// What the top row of a scrolled-up view showed last frame.
#[derive(Clone, Copy)]
enum ViewAnchor {
    /// A transcript cell, and how far into it.
    Cell { id: u64, offset: usize },
    /// The welcome header above every cell: it never moves, so the absolute
    /// line holds.
    Header { line: usize },
}

/// The anchor at content line `line` of the last frame. `None` past the end.
fn line_anchor(spans: &[(u64, usize, usize)], line: usize) -> Option<ViewAnchor> {
    if spans.first().is_none_or(|(_, first, _)| line < *first) {
        return Some(ViewAnchor::Header { line });
    }
    let index = spans.partition_point(|(_, _, end)| *end <= line);
    let (cell_id, start, end) = spans.get(index)?;
    // Lazy: `line - start` must not be computed when `line` is not inside.
    (*start <= line && line < *end).then(|| ViewAnchor::Cell {
        id: *cell_id,
        offset: line - start,
    })
}

pub(crate) fn draw(frame: &mut Frame, area: Rect, prepared: &PreparedDialog) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Paragraph::new(prepared.lines.clone()), area);
}

/// One rendered block of the transcript. Cached blocks stay borrowed from the
/// line cache — only the blocks that must render fresh own their lines.
enum Block<'a> {
    Cached(&'a [Line<'static>]),
    Owned(Vec<Line<'static>>),
}

impl Block<'_> {
    fn lines(&self) -> &[Line<'static>] {
        match self {
            Block::Cached(lines) => lines,
            Block::Owned(lines) => lines,
        }
    }

    fn into_owned(self) -> Vec<Line<'static>> {
        match self {
            Block::Cached(lines) => lines.to_vec(),
            Block::Owned(lines) => lines,
        }
    }
}

/// Everything a block reads out of the transcript, as one borrow group: the
/// caches stay separate (`RenderParts`) because they are the `&mut` half and
/// the options outlive the frame.
struct CellsSource<'a> {
    cells: &'a VecDeque<Cell>,
    expanded: &'a HashSet<u64>,
    stable_revisions: &'a HashMap<u64, StableRevision>,
    live_id: Option<u64>,
    /// Output received so far for the running tool calls among `cells`.
    tool_streams: &'a ToolStreams,
    width: u16,
    anim_frame: u64,
}

/// One segment's block: a cell, or a run of exploration calls folded into one
/// group (`toolgroup`). Cacheable blocks come from the line cache, rendered
/// into it on a miss — the entry just inserted is the one returned, so a miss
/// is handled here rather than assumed away. Live answers and running calls
/// (and groups holding one) render fresh and come back `Owned`. Segment
/// indices come from `src.cells` itself, so they are in bounds.
fn segment_block<'c>(
    src: &CellsSource<'_>,
    segment: &Segment,
    opts: &RenderOptions,
    parts: &'c mut RenderParts<'_>,
) -> Block<'c> {
    match segment {
        Segment::Cell(index) => {
            let cell = &src.cells[*index];
            let state = CellRenderState {
                live: src.live_id == Some(cell.id),
                expanded: src.expanded.contains(&cell.id),
                stable_revision: src
                    .stable_revisions
                    .get(&cell.id)
                    .copied()
                    .unwrap_or((0, 0)),
            };
            cell_block(cell, state, src, opts, parts)
        }
        Segment::Group(range) => group_block(
            src.cells,
            range.clone(),
            opts,
            parts.cache,
            src.width,
            src.anim_frame,
        ),
    }
}

/// A group's block: fresh while a member runs, cached otherwise under a key
/// that changes with every member. Whenever the group is drawn (fresh, or a
/// block miss) the finished members' rows are memoized first, so a draw
/// redraws only the header and the running rows; a block hit touches no row.
fn group_block<'c>(
    cells: &VecDeque<Cell>,
    range: std::ops::Range<usize>,
    opts: &RenderOptions,
    cache: &'c mut LineCache,
    width: u16,
    anim_frame: u64,
) -> Block<'c> {
    if toolgroup::has_running(cells, range.clone()) {
        toolgroup::warm_rows(cells, range.clone(), opts, cache);
        return Block::Owned(toolgroup::render(cells, range, opts, anim_frame, cache));
    }
    let key = toolgroup::cache_key(cells, range.clone(), width, opts.cache_epoch);
    if !cache.contains(key) {
        toolgroup::warm_rows(cells, range.clone(), opts, cache);
        let lines = toolgroup::render(cells, range, opts, anim_frame, cache);
        return Block::Cached(cache.insert(key, lines));
    }
    Block::Cached(cache.get(key).unwrap_or(&[]))
}

#[derive(Clone, Copy)]
struct CellRenderState {
    live: bool,
    expanded: bool,
    stable_revision: StableRevision,
}

/// A cell's block: from the cache (rendered into it on a miss), or fresh for
/// the cells that draw anew every frame.
fn cell_block<'c>(
    cell: &Cell,
    state: CellRenderState,
    src: &CellsSource<'_>,
    opts: &RenderOptions,
    parts: &'c mut RenderParts<'_>,
) -> Block<'c> {
    let CellRenderState {
        live: is_live,
        expanded,
        stable_revision,
    } = state;
    let width = src.width;
    let anim_frame = src.anim_frame;

    // A running tool call: its body grows without the cell changing, so it
    // can never come from the cache — the same route the spinner takes.
    if is_running_tool(cell) {
        let streamed = src.tool_streams.get(cell.id);
        return Block::Owned(render_cell(cell, streamed, opts, anim_frame, expanded));
    }

    if is_live && let CellKind::Item(TranscriptKind::Assistant { text, .. }) = &cell.kind {
        // The stable prefix of the live answer is cacheable; the tail is not.
        let (stable, tail) = split_stable_tail(text);
        let stable_key = CacheKey::stable_prefix(
            cell.id,
            stable_revision.0,
            stable.len(),
            width,
            opts.cache_epoch,
        );
        let mut combined = if parts.cache.contains(stable_key) {
            parts.cache.get(stable_key).unwrap_or(&[]).to_vec()
        } else {
            parts
                .cache
                .insert(stable_key, assistant_markdown(stable, opts, true))
                .to_vec()
        };
        // The marker sits on the answer's first row: the tail carries it only
        // while the stable part has drawn nothing yet.
        let mut tail_lines = assistant_markdown(tail, opts, combined.is_empty());
        combined.append(&mut tail_lines);
        if let Some(stats) = cell.stats.as_deref() {
            combined.push(assistant_stats_line(stats, opts));
        }
        combined.push(Line::default());
        return Block::Owned(combined);
    }

    // Everything else caches — a live cell that is not an answer too: the
    // version in the key keeps pace with its updates.
    let key = CacheKey::cell(cell.id, cell.version, width, expanded, opts.cache_epoch);
    if !parts.cache.contains(key) {
        let rendered = match &cell.kind {
            // Inline images need the decoded-cell cache, which only the
            // transcript owns (`render_cell` alone draws the text stub).
            CellKind::Item(TranscriptKind::Images { images }) => {
                crate::ui::images::render_images(images, width, opts, parts.images)
            }
            _ => render_cell(cell, None, opts, anim_frame, expanded),
        };
        return Block::Cached(parts.cache.insert(key, rendered));
    }
    Block::Cached(parts.cache.get(key).unwrap_or(&[]))
}

/// Test-only entry point: render one cell exactly as the transcript would.
#[cfg(test)]
pub(crate) fn render_cell_for_test(cell: &Cell, opts: &RenderOptions) -> Vec<Line<'static>> {
    render_cell(cell, None, opts, 0, false)
}

/// `streamed` is the output a running tool call has produced so far; `None`
/// for every other cell, and for a running call that has produced nothing yet.
fn render_cell(
    cell: &Cell,
    streamed: Option<ToolStreamView<'_>>,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    // The width comes from the options rather than beside them: `RenderOptions`
    // carries it, and a second copy is a second answer to "how wide is this".
    let width = opts.width;
    match &cell.kind {
        CellKind::Gap => vec![Line::default()],
        CellKind::Item(kind) => render_item_inner(
            kind,
            cell.perm_explain.as_ref(),
            cell.stats.as_deref(),
            streamed,
            opts,
            anim_frame,
            expanded,
        ),
        CellKind::BackgroundResult(result) => render_background_result(result, width, opts),
    }
}

/// Render one item in its folded form. Only tests reach for this directly; the
/// transcript goes through the cache, which knows whether the cell is expanded.
#[cfg(test)]
pub(crate) fn render_item(
    kind: &TranscriptKind,
    stats: Option<&str>,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<Line<'static>> {
    render_item_inner(kind, None, stats, None, opts, anim_frame, false)
}
