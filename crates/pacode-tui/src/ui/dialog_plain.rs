//! The transcript drawn outside the frame pass: its rows as plain text for a
//! copy, and the height of a prefix of it for a history page. Both fold
//! exploration runs exactly as the frame does, through the same cache.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::RangeInclusive;

use ratatui::text::Line;

use pacode_render::{LineCache, RenderOptions};

use super::toolgroup::{self, Segment};
use super::{Block, CellRenderState, CellsSource, group_block, plan_group, render_cell_block};
use crate::state::selection::{MAX_SELECTION_BYTES, MAX_SELECTION_LINES};
use crate::state::transcript::{Cell, RenderProfile, ToolStreams, Transcript};
use crate::ui::cells::decoration_width;

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

/// One segment's block outside the frame pass: the same plan-then-borrow the
/// frame does, one segment at a time. Segment indices come from `src.cells`
/// itself, so they are in bounds.
fn segment_block<'a>(
    src: &CellsSource<'_>,
    segment: &Segment,
    opts: &RenderOptions,
    cache: &'a mut LineCache,
) -> Block<'a> {
    let cells = src.cells;
    match segment {
        Segment::Cell(index) => {
            let cell = &cells[*index];
            let state = CellRenderState {
                live: src.live_id == Some(cell.id),
                expanded: src.expanded.contains(&cell.id),
                stable_revision: src
                    .stable_revisions
                    .get(&cell.id)
                    .copied()
                    .unwrap_or((0, 0)),
            };
            render_cell_block(
                cell,
                state,
                opts,
                cache,
                src.tool_streams,
                src.width,
                src.anim_frame,
            )
        }
        Segment::Group(range) => {
            let plan = plan_group(cells, range.clone(), opts, cache, src.width, src.anim_frame);
            let cache: &'a LineCache = cache;
            group_block(cells, range.clone(), &plan, opts, cache, src.anim_frame)
        }
    }
}

/// Lines the first `prefix` cells draw, folded the way the view folds them.
/// A group straddling the prefix's end is measured as the prefix alone has it.
pub(crate) fn measure_cell_lines(
    cells: &VecDeque<Cell>,
    prefix: usize,
    expanded: &HashSet<u64>,
    cache: &mut LineCache,
    tool_streams: &ToolStreams,
    profile: &RenderProfile,
) -> usize {
    let no_revisions = HashMap::new();
    let src = CellsSource {
        cells,
        expanded,
        stable_revisions: &no_revisions,
        header: None,
        live_id: None,
        tool_streams,
        width: profile.width,
        anim_frame: profile.anim_frame,
        welcome_ms: 0,
    };
    let opts = &profile.options;
    let mut lines = 0usize;
    for segment in toolgroup::segments(cells.iter().take(prefix), expanded, opts.thinking) {
        let block = segment_block(&src, &segment, opts, cache);
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

    let src = CellsSource {
        cells,
        expanded,
        stable_revisions,
        header: None,
        live_id: *live_cell,
        tool_streams,
        width,
        anim_frame,
        welcome_ms,
    };
    for segment in toolgroup::segments(cells.iter(), expanded, opts.thinking) {
        if line > sink.end || sink.full() {
            break;
        }
        // A group's rows are tool rows: its first cell's kind names their
        // decoration (the body gutter).
        let first = match &segment {
            Segment::Cell(index) => *index,
            Segment::Group(range) => range.start,
        };
        let Some(kind) = cells.get(first).map(|cell| &cell.kind) else {
            break;
        };
        let block = segment_block(&src, &segment, opts, cache);
        let rendered: Vec<(String, usize)> = block
            .lines()
            .iter()
            .map(|l| (line_text(l), decoration_width(kind, l, opts)))
            .collect();
        sink.push(&rendered, line);
        line += rendered.len();
    }
    sink.out
}

pub(super) fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}
