//! The transcript (main dialog and the agent panel share this renderer).

use std::collections::{HashMap, HashSet, VecDeque};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::cache::CacheKey;
use pacode_render::markdown::split_stable_tail;
use pacode_render::{LineCache, RenderOptions};
use pacode_types::TranscriptKind;
use pacode_types::transcript::ToolStatus;

use super::cells::{
    assistant_markdown, assistant_stats_line, render_background_result, render_item_inner,
};
use super::toolgroup::{self, Segment};
use crate::state::image_cache::ImageCellCache;
use crate::state::transcript::{
    Cell, CellKind, HeaderInfo, StableRevision, ToolStreamView, ToolStreams, Transcript,
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

    prime_image_cells(cells, expanded, images, cache, opts, width);
    let (blocks, segments) = build_cell_lines(
        CellsSource {
            cells,
            expanded,
            stable_revisions,
            header: header.as_ref(),
            live_id: *live_cell,
            tool_streams,
            width,
            anim_frame,
            welcome_ms,
        },
        cache,
        opts,
    );

    let header_blocks = usize::from(header.is_some());
    // Scrolled up, the view holds on to what it shows: the cell (and line
    // within it) at the top of the last frame. `scroll_from_bottom` alone
    // counts from the tail, so every line streamed in below would push the
    // text being read up by one row.
    let anchor = (*scroll_from_bottom > 0)
        .then(|| line_anchor(cell_lines, *first_visible_line))
        .flatten();
    // Both are rebuilt every frame; reusing their buffers keeps a long
    // transcript from allocating a fresh pair of index tables per frame.
    let mut spans = std::mem::take(cell_lines);
    spans.clear();
    spans.reserve(cells.len());
    group_heads.clear();
    let mut block_starts = Vec::with_capacity(blocks.len());
    let mut total_lines = 0usize;
    for (index, block) in blocks.iter().enumerate() {
        block_starts.push(total_lines);
        let end = total_lines + block.lines().len();
        match index
            .checked_sub(header_blocks)
            .and_then(|segment| segments.get(segment))
        {
            Some(Segment::Cell(cell)) => {
                if let Some(cell) = cells.get(*cell) {
                    spans.push((cell.id, total_lines, end));
                }
            }
            Some(Segment::Group(range)) => toolgroup::push_spans(
                cells,
                range.clone(),
                total_lines..end,
                &mut spans,
                group_heads,
            ),
            None => {}
        }
        total_lines = end;
    }
    cell_line_index.clear();
    cell_line_index.extend(
        spans
            .iter()
            .enumerate()
            .map(|(index, (id, _, _))| (*id, index)),
    );
    *cell_lines = spans;

    let viewport = area.height as usize;
    *rendered_lines = Some(total_lines);
    *viewport_height = Some(viewport);
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
    if let Some(top) = top {
        *scroll_from_bottom = max_scroll - top.min(max_scroll);
    }
    *scroll_from_bottom = (*scroll_from_bottom).min(max_scroll);
    *first_visible_line = max_scroll.saturating_sub(*scroll_from_bottom);

    let (start_line, is_scrolled_up) = if total_lines <= viewport {
        (0, false)
    } else {
        let scroll = *scroll_from_bottom;
        let start = max_scroll.saturating_sub(scroll);
        (start, scroll > 0)
    };

    let mut visible_lines = collect_visible_lines(&blocks, &block_starts, start_line, viewport);

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

/// Inline images need the decoded-cell cache, which only the transcript owns.
/// Every image cell's lines go into the line cache under the key `plan_cell`
/// looks up, so the planner serves them from the cache like any other cell
/// (`render_cell` alone can only draw the text stub). Image cells never
/// change once written, so after the first frame this is one cache probe each.
fn prime_image_cells(
    cells: &VecDeque<Cell>,
    expanded: &HashSet<u64>,
    images: &mut ImageCellCache,
    cache: &mut LineCache,
    opts: &RenderOptions,
    width: u16,
) {
    for cell in cells {
        let CellKind::Item(TranscriptKind::Images { images: refs }) = &cell.kind else {
            continue;
        };
        let key = CacheKey::cell(
            cell.id,
            cell.version,
            width,
            expanded.contains(&cell.id),
            opts.cache_epoch,
        );
        if cache.get(key).is_none() {
            let rendered = crate::ui::images::render_images(refs, width, opts, images);
            cache.insert(key, rendered);
        }
    }
}

/// What the top row of a scrolled-up view showed last frame.
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

/// The rows from content line `start_line` down, `viewport` of them at most.
/// `starts[i]` is the first content line of `blocks[i]`.
fn collect_visible_lines(
    blocks: &[Block<'_>],
    starts: &[usize],
    start_line: usize,
    viewport: usize,
) -> Vec<Line<'static>> {
    let mut visible = Vec::with_capacity(viewport);
    if viewport == 0 || blocks.is_empty() {
        return visible;
    }
    // The last block starting at or above the line: an empty block shares
    // its start with the next one, which is the one holding the line.
    let block_index = starts
        .partition_point(|start| *start <= start_line)
        .saturating_sub(1);
    let offset = start_line.saturating_sub(starts.get(block_index).copied().unwrap_or(0));

    if let Some(block) = blocks.get(block_index) {
        extend_visible(&mut visible, block.lines(), offset, viewport);
    }
    for block in blocks.iter().skip(block_index.saturating_add(1)) {
        if visible.len() >= viewport {
            break;
        }
        extend_visible(&mut visible, block.lines(), 0, viewport);
    }
    visible
}

fn extend_visible(
    visible: &mut Vec<Line<'static>>,
    lines: &[Line<'static>],
    skip: usize,
    viewport: usize,
) {
    let remaining = viewport.saturating_sub(visible.len());
    visible.extend(lines.iter().skip(skip).take(remaining).cloned());
}

/// One rendered block of the transcript. Cached cells stay borrowed from the
/// line cache — only the cells that must render fresh own their lines.
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
}

/// How a cell's block is produced in the second phase.
enum CellPlan {
    /// Served from `cache` under this key; phase one made sure it is there.
    Cached(CacheKey),
    /// The live assistant cell: its stable part is cached, its tail renders
    /// fresh every frame.
    LiveAssistant(CacheKey),
    /// Rendered fresh and never cached: running tool calls carry the spinner.
    Fresh,
}

/// Everything `build_cell_lines` reads out of the transcript, as one borrow
/// group: the cache and the render options stay separate because the cache is
/// the `&mut` half and the options outlive the frame.
struct CellsSource<'a> {
    cells: &'a VecDeque<Cell>,
    expanded: &'a HashSet<u64>,
    stable_revisions: &'a HashMap<u64, StableRevision>,
    header: Option<&'a HeaderInfo>,
    live_id: Option<u64>,
    /// Output received so far for the running tool calls among `cells`.
    tool_streams: &'a ToolStreams,
    width: u16,
    anim_frame: u64,
    welcome_ms: u64,
}

/// Render every block of the transcript in display order: the header banner
/// first (it lives outside `cells` so a transcript seq can never collide with
/// it), then one block per segment — a cell, or a run of exploration calls
/// folded into one group (`toolgroup`). The segments come back with the
/// blocks, one per block after the banner.
///
/// §8.1: the cells and the render cache are borrowed as separate fields —
/// the previous per-frame `CellKind` clone of every cell (each potentially a
/// whole markdown document) is gone. The per-frame `to_vec` of every cached
/// cell is gone too, in two phases: first a `&mut` pass makes sure every
/// cacheable block is in the cache (rendering misses, refreshing recency),
/// then a `&` pass collects the blocks as borrows. Live assistant cells and
/// running tool calls (and groups holding one) still render fresh every
/// frame and stay `Owned`.
fn build_cell_lines<'a>(
    src: CellsSource<'a>,
    cache: &'a mut LineCache,
    opts: &RenderOptions,
) -> (Vec<Block<'a>>, Vec<Segment>) {
    let CellsSource {
        cells,
        expanded,
        stable_revisions,
        header,
        live_id,
        tool_streams,
        width,
        anim_frame,
        welcome_ms,
    } = src;
    let segments = toolgroup::segments(cells.iter(), expanded, opts.thinking);
    let mut blocks: Vec<Block<'a>> = Vec::with_capacity(segments.len() + 1);

    if let Some(info) = header {
        blocks.push(Block::Owned(crate::ui::header::render(
            info, width, opts, anim_frame, welcome_ms,
        )));
    }

    // Phase one, `&mut cache`: plan every block and fill cache misses. The
    // segment indices come from `cells` itself, so they are in bounds.
    let mut plans: Vec<CellPlan> = Vec::with_capacity(segments.len());
    for segment in &segments {
        plans.push(match segment {
            Segment::Cell(index) => {
                let cell = &cells[*index];
                plan_cell(
                    cell,
                    CellRenderState {
                        live: live_id == Some(cell.id),
                        expanded: expanded.contains(&cell.id),
                        stable_revision: stable_revisions.get(&cell.id).copied().unwrap_or((0, 0)),
                    },
                    opts,
                    cache,
                    width,
                    anim_frame,
                )
            }
            Segment::Group(range) => {
                plan_group(cells, range.clone(), opts, cache, width, anim_frame)
            }
        });
    }

    // Phase two, `& cache`: no more `&mut` calls, so the cached blocks can be
    // borrowed out of the entries phase one left behind.
    let cache: &'a LineCache = cache;
    for (segment, plan) in segments.iter().zip(plans.iter()) {
        blocks.push(match segment {
            Segment::Cell(index) => {
                let cell = &cells[*index];
                cell_block(
                    cell,
                    plan,
                    expanded.contains(&cell.id),
                    opts,
                    cache,
                    tool_streams,
                    anim_frame,
                )
            }
            Segment::Group(range) => {
                group_block(cells, range.clone(), plan, opts, cache, anim_frame)
            }
        });
    }

    (blocks, segments)
}

/// The phase-one decision for a group: fresh while a member runs, cached
/// otherwise under a key that changes with every member. Whenever the group
/// is drawn (fresh, or a block miss) the finished members' rows are memoized
/// first, so a draw redraws only the header and the running rows; a block
/// hit touches no row.
fn plan_group(
    cells: &VecDeque<Cell>,
    range: std::ops::Range<usize>,
    opts: &RenderOptions,
    cache: &mut LineCache,
    width: u16,
    anim_frame: u64,
) -> CellPlan {
    if toolgroup::has_running(cells, range.clone()) {
        toolgroup::warm_rows(cells, range, opts, cache);
        return CellPlan::Fresh;
    }
    let key = toolgroup::cache_key(cells, range.clone(), width, opts.cache_epoch);
    if cache.get(key).is_none() {
        toolgroup::warm_rows(cells, range.clone(), opts, cache);
        let lines = toolgroup::render(cells, range, opts, anim_frame, cache);
        cache.insert(key, lines);
    }
    CellPlan::Cached(key)
}

/// The phase-two block for a group: a borrow of what phase one cached, or a
/// fresh render while a member runs.
fn group_block<'a>(
    cells: &VecDeque<Cell>,
    range: std::ops::Range<usize>,
    plan: &CellPlan,
    opts: &RenderOptions,
    cache: &'a LineCache,
    anim_frame: u64,
) -> Block<'a> {
    if let CellPlan::Cached(key) = plan
        && let Some(lines) = cache.peek(*key)
    {
        return Block::Cached(lines);
    }
    Block::Owned(toolgroup::render(cells, range, opts, anim_frame, cache))
}

#[derive(Clone, Copy)]
struct CellRenderState {
    live: bool,
    expanded: bool,
    stable_revision: StableRevision,
}

/// The phase-one decision for a cell: where its lines come from, making sure
/// the cache holds whatever phase two will borrow.
fn plan_cell(
    cell: &Cell,
    state: CellRenderState,
    opts: &RenderOptions,
    cache: &mut LineCache,
    width: u16,
    anim_frame: u64,
) -> CellPlan {
    let CellRenderState {
        live: is_live,
        expanded,
        stable_revision,
    } = state;
    let key = CacheKey::cell(cell.id, cell.version, width, expanded, opts.cache_epoch);

    let is_running_tool = matches!(
        cell.kind,
        CellKind::Item(TranscriptKind::ToolCall {
            status: ToolStatus::Running,
            ..
        })
    );

    if is_running_tool {
        return CellPlan::Fresh;
    }

    if !is_live {
        if cache.get(key).is_none() {
            // `None`: a running tool call returned `CellPlan::Fresh` above and
            // never reaches the cache, so no live body can be cached here.
            let rendered = render_cell(cell, None, opts, anim_frame, expanded);
            cache.insert(key, rendered);
        }
        return CellPlan::Cached(key);
    }

    if let CellKind::Item(TranscriptKind::Assistant { text, .. }) = &cell.kind {
        // The stable prefix of the live answer is cacheable; the tail is not.
        let (stable, _) = split_stable_tail(text);
        let stable_key = CacheKey::stable_prefix(
            cell.id,
            stable_revision.0,
            stable.len(),
            width,
            opts.cache_epoch,
        );
        if cache.get(stable_key).is_none() {
            cache.insert(stable_key, assistant_markdown(stable, opts, true));
        }
        return CellPlan::LiveAssistant(stable_key);
    }

    // A live cell that is not an answer still caches; the version in the key
    // keeps pace with its updates.
    if cache.get(key).is_none() {
        let rendered = render_cell(cell, None, opts, anim_frame, expanded);
        cache.insert(key, rendered);
    }
    CellPlan::Cached(key)
}

/// The phase-two block for a cell: a borrow of what phase one cached, or the
/// fresh render for the cells that draw anew every frame.
fn cell_block<'a>(
    cell: &Cell,
    plan: &CellPlan,
    expanded: bool,
    opts: &RenderOptions,
    cache: &'a LineCache,
    tool_streams: &ToolStreams,
    anim_frame: u64,
) -> Block<'a> {
    let streamed = tool_streams.get(cell.id);
    match plan {
        CellPlan::Cached(key) => match cache.peek(*key) {
            Some(lines) => Block::Cached(lines),
            // Phase one inserted it, so a miss cannot happen — render anyway
            // rather than panic if the invariant is ever broken.
            None => Block::Owned(render_cell(cell, streamed, opts, anim_frame, expanded)),
        },
        CellPlan::LiveAssistant(stable_key) => {
            if let CellKind::Item(TranscriptKind::Assistant { text, .. }) = &cell.kind {
                let (stable, tail) = split_stable_tail(text);

                let mut combined = match cache.peek(*stable_key) {
                    Some(lines) => lines.to_vec(),
                    None => assistant_markdown(stable, opts, true),
                };
                // The marker sits on the answer's first row: the tail carries
                // it only while the stable part has drawn nothing yet.
                let mut tail_lines = assistant_markdown(tail, opts, combined.is_empty());
                combined.append(&mut tail_lines);
                if let Some(stats) = cell.stats.as_deref() {
                    combined.push(assistant_stats_line(stats, opts));
                }
                combined.push(Line::default());
                return Block::Owned(combined);
            }
            // A live non-assistant cell is planned Cached, so this is
            // unreachable — still render rather than panic.
            Block::Owned(render_cell(cell, streamed, opts, anim_frame, expanded))
        }
        // A running tool call: its body grows without the cell changing, so it
        // can never come from the cache — the same route the spinner takes.
        CellPlan::Fresh => Block::Owned(render_cell(cell, streamed, opts, anim_frame, expanded)),
    }
}

fn render_cell_block<'a>(
    cell: &Cell,
    state: CellRenderState,
    opts: &RenderOptions,
    cache: &'a mut LineCache,
    tool_streams: &ToolStreams,
    width: u16,
    anim_frame: u64,
) -> Block<'a> {
    let plan = plan_cell(cell, state, opts, cache, width, anim_frame);
    let cache: &'a LineCache = cache;
    cell_block(
        cell,
        &plan,
        state.expanded,
        opts,
        cache,
        tool_streams,
        anim_frame,
    )
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
