//! The transcript (main dialog and the agent panel share this renderer).

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::RangeInclusive;

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
    assistant_markdown, assistant_stats_line, decoration_width, render_background_result,
    render_item_inner,
};
use crate::state::selection::{MAX_SELECTION_BYTES, MAX_SELECTION_LINES};
use crate::state::transcript::{
    Cell, CellKind, HeaderInfo, StableRevision, ToolStreamView, ToolStreams, Transcript,
};

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod dialog_tests;

/// How much of a transcript a copy needs: the lines it reads, and how much
/// plain text it may hold.
#[derive(Clone, Debug)]
pub(crate) struct PlainWindow {
    pub lines: Option<RangeInclusive<usize>>,
    pub max_bytes: usize,
}

impl PlainWindow {
    pub fn for_lines(lines: RangeInclusive<usize>) -> Self {
        Self {
            lines: Some(lines),
            max_bytes: MAX_SELECTION_BYTES,
        }
    }
}

/// Transcript lines as plain text, for the clipboard. `first_line` is the
/// content line number `lines[0]` stands for, so the caller can index the
/// result by transcript line without the copy having materialized everything
/// above the selection.
///
/// `leads[i]` is how many leading columns of `lines[i]` are decoration — the
/// user band's bar, the answer's marker and indent, a tool body's gutter. The
/// text keeps them so columns stay aligned with the screen; the copy drops
/// them. A missing entry means none.
#[derive(Default)]
pub(crate) struct PlainLines {
    pub lines: Vec<String>,
    pub leads: Vec<usize>,
    pub first_line: usize,
}

/// Collects the lines a window covers, under the line and byte caps.
struct PlainSink {
    out: PlainLines,
    bytes: usize,
    start: usize,
    end: usize,
    max_bytes: usize,
}

impl PlainSink {
    fn new(window: PlainWindow) -> Self {
        Self {
            out: PlainLines::default(),
            bytes: 0,
            start: window.lines.as_ref().map_or(0, |range| *range.start()),
            end: window
                .lines
                .as_ref()
                .map_or(usize::MAX, |range| *range.end()),
            max_bytes: window.max_bytes,
        }
    }

    /// `rendered` holds each row's text and its decoration width.
    fn push(&mut self, rendered: &[(String, usize)], at: usize) {
        for (offset, (text, lead)) in rendered.iter().enumerate() {
            let line = at.saturating_add(offset);
            if line < self.start || line > self.end {
                continue;
            }
            self.bytes = self.bytes.saturating_add(text.len());
            if self.out.lines.is_empty() {
                self.out.first_line = line;
            }
            self.out.lines.push(text.clone());
            self.out.leads.push(*lead);
            if self.full() {
                return;
            }
        }
    }

    fn full(&self) -> bool {
        self.bytes > self.max_bytes || self.out.lines.len() >= MAX_SELECTION_LINES
    }
}

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
        ..
    } = transcript;

    let blocks = build_cell_lines(
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
    let mut total_lines = 0usize;
    for (index, block) in blocks.iter().enumerate() {
        let end = total_lines + block.lines().len();
        if index >= header_blocks
            && let Some(cell) = cells.get(index - header_blocks)
        {
            spans.push((cell.id, total_lines, end));
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
            .map(|(_, start, end)| (start + offset).min(end.saturating_sub(1))),
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

    let mut visible_lines =
        collect_visible_lines(&blocks, cell_lines, header_blocks > 0, start_line, viewport);

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

fn collect_visible_lines<'a>(
    blocks: &[Block<'a>],
    spans: &[(u64, usize, usize)],
    has_header: bool,
    start_line: usize,
    viewport: usize,
) -> Vec<Line<'static>> {
    let mut visible = Vec::with_capacity(viewport);
    if viewport == 0 || blocks.is_empty() {
        return visible;
    }

    let (block_index, offset) = if start_line == 0 {
        (0, 0)
    } else {
        let header_lines = spans
            .first()
            .map(|(_, start, _)| *start)
            .unwrap_or_else(|| blocks[0].lines().len());
        if start_line < header_lines {
            (0, start_line)
        } else {
            let cell_index = spans.partition_point(|(_, _, end)| *end <= start_line);
            let Some((_, cell_start, _)) = spans.get(cell_index) else {
                return visible;
            };
            (
                (if has_header { 1 } else { 0 }) + cell_index,
                start_line.saturating_sub(*cell_start),
            )
        }
    };

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
/// it), then one block per cell.
///
/// §8.1: the cells and the render cache are borrowed as separate fields —
/// the previous per-frame `CellKind` clone of every cell (each potentially a
/// whole markdown document) is gone. The per-frame `to_vec` of every cached
/// cell is gone too, in two phases: first a `&mut` pass makes sure every
/// cacheable cell is in the cache (rendering misses, refreshing recency), then
/// a `&` pass collects the blocks as borrows. Live assistant cells and running
/// tool calls still render fresh every frame and stay `Owned`.
fn build_cell_lines<'a>(
    src: CellsSource<'a>,
    cache: &'a mut LineCache,
    opts: &RenderOptions,
) -> Vec<Block<'a>> {
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
    let mut blocks: Vec<Block<'a>> = Vec::with_capacity(cells.len() + 1);

    if let Some(info) = header {
        blocks.push(Block::Owned(crate::ui::header::render(
            info, width, opts, anim_frame, welcome_ms,
        )));
    }

    // Phase one, `&mut cache`: plan every cell and fill cache misses.
    let mut plans: Vec<CellPlan> = Vec::with_capacity(cells.len());
    for cell in cells.iter() {
        plans.push(plan_cell(
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
        ));
    }

    // Phase two, `& cache`: no more `&mut` calls, so the cached blocks can be
    // borrowed out of the entries phase one left behind.
    let cache: &'a LineCache = cache;
    for (cell, plan) in cells.iter().zip(plans.iter()) {
        blocks.push(cell_block(
            cell,
            plan,
            expanded.contains(&cell.id),
            opts,
            cache,
            tool_streams,
            anim_frame,
        ));
    }

    blocks
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

pub(crate) fn measure_cell_lines<'a>(
    cells: impl Iterator<Item = &'a Cell>,
    expanded: &HashSet<u64>,
    cache: &mut LineCache,
    tool_streams: &ToolStreams,
    opts: &RenderOptions,
    width: u16,
    anim_frame: u64,
) -> usize {
    let mut lines = 0usize;
    for cell in cells {
        let state = CellRenderState {
            live: false,
            expanded: expanded.contains(&cell.id),
            stable_revision: (0, 0),
        };
        let block = render_cell_block(cell, state, opts, cache, tool_streams, width, anim_frame);
        lines = lines.saturating_add(block.lines().len());
    }
    lines
}

pub(crate) fn plain_lines(
    transcript: &mut Transcript,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    welcome_ms: u64,
    window: PlainWindow,
) -> PlainLines {
    transcript.set_render_profile(width, opts, anim_frame);
    let Transcript {
        cells,
        expanded,
        cache,
        stable_revisions,
        header,
        live_cell,
        tool_streams,
        ..
    } = &mut *transcript;
    let mut sink = PlainSink::new(window);
    let mut line = 0usize;

    if let Some(info) = header.as_ref() {
        let rendered: Vec<(String, usize)> =
            crate::ui::header::render(info, width, opts, anim_frame, welcome_ms)
                .iter()
                .map(|l| (line_text(l), 0))
                .collect();
        sink.push(&rendered, line);
        line += rendered.len();
    }

    for cell in cells.iter() {
        if line > sink.end || sink.full() {
            break;
        }
        let state = CellRenderState {
            live: *live_cell == Some(cell.id),
            expanded: expanded.contains(&cell.id),
            stable_revision: stable_revisions.get(&cell.id).copied().unwrap_or((0, 0)),
        };
        let block = render_cell_block(cell, state, opts, cache, tool_streams, width, anim_frame);
        let rendered: Vec<(String, usize)> = block
            .lines()
            .iter()
            .map(|l| (line_text(l), decoration_width(&cell.kind, l, opts)))
            .collect();
        sink.push(&rendered, line);
        line += rendered.len();
    }
    sink.out
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
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
