//! Exploration runs drawn as one compact cell (omp's `read-tool-group`):
//!
//! ```text
//!  ✓ EXPLORE  2 reads · 1 search
//!  │ Read crates/pacode-tui/src/ui/input.rs → 200 lines
//!  │ Search "prompt_frame" → 4 matches
//!  │ Read layout.rs running
//! ```
//!
//! Two or more consecutive read-only exploration calls fold into one block of
//! one row per call. Every transcript is one agent's, so a run never mixes
//! agents. Grouping is purely a view: the cells, the history and the events
//! are untouched, and the dialog works out per frame which cells fold.
//!
//! - A call that failed or was refused never folds: it ends the run and keeps
//!   its own error cell, so failures stay in sight.
//! - Neither does an expanded call. Expanding shows a call's own full cell,
//!   so a member clicked open breaks out of its group and the rest fold
//!   around it; collapsing any member of a run folds the whole run back.
//! - A reasoning cell that draws nothing (thinking hidden) is invisible and
//!   does not break a run; anything that draws does.

use std::collections::{HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use ratatui::text::Line;

use pacode_render::cache::CacheKey;
use pacode_render::{LineCache, RenderOptions};
use pacode_types::TranscriptKind;
use pacode_types::transcript::ToolStatus;

use crate::state::transcript::{Cell, CellKind};
use crate::ui::toolcell::{self, ToolCallView};

#[cfg(test)]
#[path = "toolgroup_tests.rs"]
mod toolgroup_tests;

#[cfg(test)]
thread_local! {
    /// Row memo lookups `warm_rows` made on this thread: the dialog test
    /// asserts a cached finished group makes none per frame.
    pub(crate) static ROW_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Member rows a group shows; older members fold into a `… N more` row.
pub(crate) const MAX_GROUP_ROWS: usize = 8;

/// The read-only exploration tools that fold, with the noun each one counts
/// as in the group's tally.
const EXPLORE_TOOLS: [(&str, &str, &str); 5] = [
    ("read", "read", "reads"),
    ("grep", "search", "searches"),
    ("glob", "search", "searches"),
    ("ls", "list", "lists"),
    ("symbols", "lookup", "lookups"),
];

/// One block of the transcript view: a cell drawn on its own, or a run of
/// cells drawn as one group. Indices are positions in the transcript's cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    Cell(usize),
    Group(Range<usize>),
}

/// The header rows of a group drawn last frame (`lines`, content lines) and
/// the first and last cell it folds. A click there opens the whole group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GroupHead {
    pub lines: Range<usize>,
    pub first: u64,
    pub last: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Member,
    Transparent,
    Breaker,
}

fn noun_of(tool: &str) -> Option<(&'static str, &'static str)> {
    EXPLORE_TOOLS
        .iter()
        .find(|(name, _, _)| *name == tool)
        .map(|(_, one, many)| (*one, *many))
}

/// `open`: the cell is expanded, so it draws its own full cell.
fn role(cell: &Cell, open: bool, thinking: bool) -> Role {
    match &cell.kind {
        CellKind::Item(TranscriptKind::ToolCall {
            name,
            status: ToolStatus::Ok | ToolStatus::Running,
            ..
        }) if !open && noun_of(name).is_some() => Role::Member,
        CellKind::Item(TranscriptKind::Reasoning { .. }) if !thinking => Role::Transparent,
        CellKind::Item(_) | CellKind::Gap | CellKind::BackgroundResult(_) => Role::Breaker,
    }
}

/// Cut the cells into the blocks the dialog draws: every maximal run of two
/// or more members (with only invisible cells between them) is one group,
/// everything else is a cell of its own.
pub(crate) fn segments<'a>(
    cells: impl Iterator<Item = &'a Cell>,
    expanded: &HashSet<u64>,
    thinking: bool,
) -> Vec<Segment> {
    let roles: Vec<Role> = cells
        .map(|cell| role(cell, expanded.contains(&cell.id), thinking))
        .collect();
    let mut out = Vec::with_capacity(roles.len());
    let mut i = 0;
    while i < roles.len() {
        match run_end(&roles, i) {
            Some(end) => {
                out.push(Segment::Group(i..end));
                i = end;
            }
            None => {
                out.push(Segment::Cell(i));
                i += 1;
            }
        }
    }
    out
}

/// One past the last member of the run that starts at `start`, when a run of
/// two or more members starts there. Trailing invisible cells stay outside.
fn run_end(roles: &[Role], start: usize) -> Option<usize> {
    if roles.get(start) != Some(&Role::Member) {
        return None;
    }
    let mut members = 0usize;
    let mut end = start;
    for (i, role) in roles.iter().enumerate().skip(start) {
        match role {
            Role::Member => {
                members += 1;
                end = i + 1;
            }
            Role::Transparent => {}
            Role::Breaker => break,
        }
    }
    (members >= 2).then_some(end)
}

/// The run the cell at `index` belongs to, expansion aside — what a toggle
/// on any of its cells acts on. `None` when it is in no run of two or more.
pub(crate) fn run_around(
    cells: &VecDeque<Cell>,
    index: usize,
    thinking: bool,
) -> Option<Range<usize>> {
    group_around(cells, index, &HashSet::new(), thinking)
}

/// The group the cell at `index` is drawn in with `expanded` open — `None`
/// when it is drawn as its own cell.
pub(crate) fn group_around(
    cells: &VecDeque<Cell>,
    index: usize,
    expanded: &HashSet<u64>,
    thinking: bool,
) -> Option<Range<usize>> {
    segments(cells.iter(), expanded, thinking)
        .into_iter()
        .find_map(|segment| match segment {
            Segment::Group(range) => range.contains(&index).then_some(range),
            Segment::Cell(_) => None,
        })
}

/// One call of a group: its cell's id and version, and the call itself.
struct Member<'a> {
    id: u64,
    version: u32,
    call: ToolCallView<'a>,
}

impl Member<'_> {
    /// Where the call's row is memoized — `None` while it runs: a running
    /// row moves every frame, a finished one never changes (a change bumps
    /// the version, which is a new key).
    fn row_key(&self, opts: &RenderOptions) -> Option<CacheKey> {
        (self.call.status != ToolStatus::Running)
            .then(|| CacheKey::row(self.id, self.version, opts.width, opts.cache_epoch))
    }
}

/// The calls among `range`; the invisible cells between them are skipped.
fn calls(cells: &VecDeque<Cell>, range: Range<usize>) -> impl Iterator<Item = Member<'_>> {
    cells.range(range).filter_map(|cell| match &cell.kind {
        CellKind::Item(kind) => ToolCallView::of(kind, None).map(|call| Member {
            id: cell.id,
            version: cell.version,
            call,
        }),
        CellKind::Gap | CellKind::BackgroundResult(_) => None,
    })
}

/// Ids of the calls among `range`.
pub(crate) fn member_ids(cells: &VecDeque<Cell>, range: Range<usize>) -> Vec<u64> {
    calls(cells, range).map(|member| member.id).collect()
}

/// A group with a running member redraws every frame (the spinner and the
/// sweep move); a finished one comes from the cache.
pub(crate) fn has_running(cells: &VecDeque<Cell>, range: Range<usize>) -> bool {
    calls(cells, range).any(|member| member.call.status == ToolStatus::Running)
}

/// Put the row of every finished call the group shows into `cache`, so
/// drawing the group — every frame while a sibling runs, or anew when a call
/// joins — never recounts a finished call's preview. The rows live and die
/// with their cells' cache entries, under the cache's line cap.
pub(crate) fn warm_rows(
    cells: &VecDeque<Cell>,
    range: Range<usize>,
    opts: &RenderOptions,
    cache: &mut LineCache,
) {
    let members: Vec<Member<'_>> = calls(cells, range).collect();
    let hidden = members.len().saturating_sub(MAX_GROUP_ROWS);
    for member in &members[hidden..] {
        let Some(key) = member.row_key(opts) else {
            continue;
        };
        #[cfg(test)]
        ROW_LOOKUPS.with(|n| n.set(n.get() + 1));
        // `peek`: presence only. The rows are read by the draw that follows,
        // and a lookup here must not keep them young on its own.
        if cache.peek(key).is_none() {
            let row = toolcell::compact_row(&member.call, opts.width as usize, opts);
            cache.insert(key, vec![row]);
        }
    }
}

/// Cache key of a group's block: its first cell plus every cell's id and
/// version, so adding, dropping or updating a member is a new key.
pub(crate) fn cache_key(
    cells: &VecDeque<Cell>,
    range: Range<usize>,
    width: u16,
    context: u64,
) -> CacheKey {
    let first = cells.get(range.start).map_or(0, |cell| cell.id);
    let mut hasher = DefaultHasher::new();
    for cell in cells.range(range) {
        cell.id.hash(&mut hasher);
        cell.version.hash(&mut hasher);
    }
    CacheKey::group(first, hasher.finish(), width, context)
}

/// `2 reads · 1 search` — the group's members counted by kind.
fn tally(members: &[Member<'_>], opts: &RenderOptions) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (_, one, many) in EXPLORE_TOOLS {
        if seen.contains(&one) {
            continue;
        }
        seen.push(one);
        let n = members
            .iter()
            .filter(|member| noun_of(member.call.name).is_some_and(|(noun, _)| noun == one))
            .count();
        if n > 0 {
            parts.push(format!("{n} {}", if n == 1 { one } else { many }));
        }
    }
    parts.join(opts.glyphs.dot_sep)
}

/// The group's block: header, `… N more` when the run outgrew the cap, one
/// row per shown call (the newest), all on one status band, then the blank
/// separator every cell ends with. A finished call's row comes from `cache`
/// when `warm_rows` put it there; only a running call's row is drawn here.
pub(crate) fn render(
    cells: &VecDeque<Cell>,
    range: Range<usize>,
    opts: &RenderOptions,
    anim_frame: u64,
    cache: &LineCache,
) -> Vec<Line<'static>> {
    let width = opts.width as usize;
    let members: Vec<Member<'_>> = calls(cells, range).collect();
    let running = members
        .iter()
        .any(|member| member.call.status == ToolStatus::Running);
    let hidden = members.len().saturating_sub(MAX_GROUP_ROWS);
    let mut out = Vec::with_capacity(members.len().min(MAX_GROUP_ROWS) + 3);
    out.push(toolcell::group_header(
        &tally(&members, opts),
        running,
        width,
        opts,
        anim_frame,
    ));
    if hidden > 0 {
        out.push(toolcell::more_row(hidden, opts));
    }
    for member in &members[hidden..] {
        let memo = member
            .row_key(opts)
            .and_then(|key| cache.peek(key))
            .and_then(|rows| rows.first());
        out.push(match memo {
            Some(row) => row.clone(),
            None => toolcell::compact_row(&member.call, width, opts),
        });
    }
    let status = if running {
        ToolStatus::Running
    } else {
        ToolStatus::Ok
    };
    toolcell::paint_block(&mut out, toolcell::status_bg(status, opts), width);
    out.push(Line::default());
    out
}

/// Which content lines of a group's block (`lines`) belong to which cell:
/// the header rows to the first shown call, each call's row to that call,
/// the closing blank to the last. Hidden calls and invisible cells take no
/// line. The header rows also go to `heads`: a click there opens the group.
pub(crate) fn push_spans(
    cells: &VecDeque<Cell>,
    range: Range<usize>,
    lines: Range<usize>,
    spans: &mut Vec<(u64, usize, usize)>,
    heads: &mut Vec<GroupHead>,
) {
    let total = calls(cells, range.clone()).count();
    let hidden = total.saturating_sub(MAX_GROUP_ROWS);
    let head_rows = 1 + usize::from(hidden > 0);
    let mut cursor = lines.start;
    let mut call = 0usize;
    for cell in cells.range(range.clone()) {
        let is_call =
            matches!(&cell.kind, CellKind::Item(kind) if ToolCallView::of(kind, None).is_some());
        if !is_call || call < hidden {
            spans.push((cell.id, cursor, cursor));
            call += usize::from(is_call);
            continue;
        }
        let row = lines.start + head_rows + (call - hidden);
        let end = if call + 1 == total {
            lines.end
        } else {
            row + 1
        };
        spans.push((cell.id, cursor, end));
        cursor = end;
        call += 1;
    }
    if let (Some(first), Some(last)) = (
        cells.get(range.start),
        cells.get(range.end.saturating_sub(1)),
    ) {
        heads.push(GroupHead {
            lines: lines.start..lines.start + head_rows,
            first: first.id,
            last: last.id,
        });
    }
}
