//! Transcript layout and drawing: which rendered lines land on screen.
//!
//! A frame measures every cell without rendering all of them (line counts come from
//! `Transcript::extents`, see `state::transcript::cell_extent`), then fetches lines only
//! for the cells that overlap the viewport and clones only the visible rows. Cached
//! lines are borrowed from the `LineCache`; nothing is cloned for cells off screen.
//! The output is identical to rendering every cell and slicing the viewport out.

use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::cache::{CacheKey, LineCache};
use pacode_render::markdown::split_stable_tail;
use pacode_render::{RenderOptions, render_markdown};
use pacode_types::TranscriptKind;
use pacode_types::transcript::ToolStatus;

use super::render_cell;

#[cfg(test)]
#[path = "dialog_layout_tests.rs"]
mod dialog_layout_tests;

use crate::state::image_cache::ImageCellCache;
use crate::state::transcript::cell_extent::ExtentKey;
use crate::state::transcript::{Cell, CellKind, Transcript};

/// The pieces of a `Transcript` that rendering a cell touches, borrowed separately
/// from `cells` so a cell can be read while the caches are written.
struct Parts<'a> {
    cache: &'a mut LineCache,
    images: &'a mut ImageCellCache,
    expanded: &'a HashSet<u64>,
}

/// Where a cell's lines are after `resolve_cell`.
enum Resolved {
    /// In the line cache under this key (just fetched or just inserted).
    Cached(CacheKey),
    /// Not cacheable (a running tool call, the live answer's tail): owned lines.
    Owned(Vec<Line<'static>>),
}

fn lines_of<'a>(cache: &'a mut LineCache, resolved: &'a Resolved) -> &'a [Line<'static>] {
    match resolved {
        Resolved::Cached(key) => cache.get(*key).unwrap_or(&[]),
        Resolved::Owned(lines) => lines.as_slice(),
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

fn extent_key(
    cell: &Cell,
    is_live: bool,
    width: u16,
    expanded: bool,
    opts: &RenderOptions,
) -> ExtentKey {
    ExtentKey {
        version: cell.version,
        width,
        expanded,
        thinking: opts.thinking,
        has_stats: cell.stats.is_some(),
        live: is_live,
    }
}

/// Make sure `cell`'s lines exist, rendering (and caching) them when they do not.
fn resolve_cell(
    cell: &Cell,
    is_live: bool,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    parts: &mut Parts<'_>,
) -> Resolved {
    // An expanded cell renders differently, so it is a different cache entry
    // rather than a stale one.
    let expanded = parts.expanded.contains(&cell.id);
    let expansion_bit = if expanded { 0x4000_0000_0000_0000 } else { 0 };
    let key = CacheKey {
        cell: cell.id ^ ((cell.version as u64) << 48) ^ expansion_bit,
        width,
    };
    let running = is_running_tool(cell);
    let stats = cell.stats.as_deref();

    // Inline images need the decoded-cell cache, which only the transcript owns.
    if let CellKind::Item(TranscriptKind::Images { images }) = &cell.kind {
        if parts.cache.get(key).is_none() {
            let rendered = crate::ui::images::render_images(images, width, opts, parts.images);
            parts.cache.insert(key, rendered);
        }
        return Resolved::Cached(key);
    }

    if !is_live && !running {
        if parts.cache.get(key).is_none() {
            let rendered = render_cell(&cell.kind, stats, width, opts, anim_frame, expanded);
            parts.cache.insert(key, rendered);
        }
        return Resolved::Cached(key);
    }

    if let CellKind::Item(TranscriptKind::Assistant { text, .. }) = &cell.kind {
        let (stable, tail) = split_stable_tail(text);
        let stable_key = CacheKey {
            cell: (cell.id << 1) ^ (stable.len() as u64) ^ 0x8000_0000_0000_0000,
            width,
        };

        let mut combined = if let Some(cached) = parts.cache.get(stable_key) {
            cached.to_vec()
        } else {
            let rendered = render_markdown(stable, opts);
            parts.cache.insert(stable_key, rendered).to_vec()
        };
        combined.append(&mut render_markdown(tail, opts));
        if let Some(stats) = stats {
            combined.push(Line::from(Span::styled(
                stats.to_string(),
                opts.theme.faint,
            )));
        }
        combined.push(Line::default());
        return Resolved::Owned(combined);
    }

    let rendered = render_cell(&cell.kind, stats, width, opts, anim_frame, expanded);
    if running {
        Resolved::Owned(rendered)
    } else {
        parts.cache.insert(key, rendered);
        Resolved::Cached(key)
    }
}

/// Append `lines[..]` rows that fall in the global line range `[start, end)`, given
/// that `lines[0]` is global line `cursor`.
fn take_range(
    cursor: usize,
    start: usize,
    end: usize,
    lines: &[Line<'static>],
    out: &mut Vec<Line<'static>>,
) {
    let lo = start.saturating_sub(cursor).min(lines.len());
    let hi = end.saturating_sub(cursor).min(lines.len());
    if lo < hi {
        out.extend(lines[lo..hi].iter().cloned());
    }
}

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    transcript: &mut Transcript,
    opts: &RenderOptions,
    anim_frame: u64,
    welcome_ms: u64,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let width = area.width;
    let viewport = area.height as usize;

    // Cells measured or fetched this frame, by index (counted once each).
    let mut laid_out: HashSet<usize> = HashSet::new();
    // Lines of cells that had to be rendered to be measured and are not cached.
    let mut fresh: HashMap<usize, Vec<Line<'static>>> = HashMap::new();
    let mut counts: Vec<usize> = Vec::new();

    // Measure: header block, then one count per cell.
    let (header_lines, total_lines) = {
        let Transcript {
            cells,
            header,
            cache,
            images,
            expanded,
            extents,
            live_cell,
            cell_lines,
            ..
        } = &mut *transcript;
        extents.sync(cache.generation());
        let header_lines = match header.as_ref() {
            Some(info) => crate::ui::header::render(info, width, opts, anim_frame, welcome_ms),
            None => Vec::new(),
        };
        let mut parts = Parts {
            cache,
            images,
            expanded,
        };
        counts.reserve(cells.len());
        for (i, cell) in cells.iter().enumerate() {
            let is_live = *live_cell == Some(cell.id);
            let key = extent_key(
                cell,
                is_live,
                width,
                parts.expanded.contains(&cell.id),
                opts,
            );
            if let Some(n) = extents.get(cell.id, &key) {
                counts.push(n);
                continue;
            }
            laid_out.insert(i);
            let resolved = resolve_cell(cell, is_live, width, opts, anim_frame, &mut parts);
            let n = lines_of(parts.cache, &resolved).len();
            extents.set(cell.id, key, n);
            counts.push(n);
            if let Resolved::Owned(lines) = resolved {
                fresh.insert(i, lines);
            }
        }

        // Remember where each cell landed, so a click on a row finds its cell. The
        // header block, when there is one, is not a cell and gets no entry.
        cell_lines.clear();
        let mut cursor = header_lines.len();
        for (cell, n) in cells.iter().zip(counts.iter()) {
            cell_lines.push((cell.id, cursor, cursor + n));
            cursor += n;
        }
        (header_lines, cursor)
    };

    transcript.record_render(total_lines, viewport);

    let (start_line, is_scrolled_up) = if total_lines <= viewport {
        (0, false)
    } else {
        let max_scroll = total_lines.saturating_sub(viewport);
        let scroll = transcript.scroll_from_bottom;
        let start = max_scroll.saturating_sub(scroll);
        (start, scroll > 0)
    };
    let end_line = start_line + viewport;

    // Fetch: only the cells overlapping the viewport, cloning only visible rows.
    let mut visible_lines: Vec<Line<'static>> = Vec::with_capacity(viewport);
    take_range(0, start_line, end_line, &header_lines, &mut visible_lines);
    {
        let Transcript {
            cells,
            cache,
            images,
            expanded,
            live_cell,
            ..
        } = &mut *transcript;
        let mut parts = Parts {
            cache,
            images,
            expanded,
        };
        let mut cursor = header_lines.len();
        for (i, cell) in cells.iter().enumerate() {
            let cell_start = cursor;
            cursor += counts.get(i).copied().unwrap_or(0);
            if cursor <= start_line {
                continue;
            }
            if cell_start >= end_line {
                break;
            }
            if let Some(lines) = fresh.remove(&i) {
                take_range(cell_start, start_line, end_line, &lines, &mut visible_lines);
                continue;
            }
            laid_out.insert(i);
            let is_live = *live_cell == Some(cell.id);
            let resolved = resolve_cell(cell, is_live, width, opts, anim_frame, &mut parts);
            let lines = lines_of(parts.cache, &resolved);
            take_range(cell_start, start_line, end_line, lines, &mut visible_lines);
        }
    }
    transcript.cells_laid_out = laid_out.len();

    if is_scrolled_up && !visible_lines.is_empty() {
        let last_idx = visible_lines.len() - 1;
        let msg = format!("↓ {} new lines", transcript.scroll_from_bottom);
        visible_lines[last_idx] = Line::from(Span::styled(msg, opts.theme.dim));
    }

    frame.render_widget(Paragraph::new(visible_lines), area);
}

/// Every rendered line of the transcript in display order: the header banner first
/// (it lives outside `cells` so a transcript seq can never collide with it), then
/// each cell. Renders and clones all of it; drawing never calls this, it exists for
/// copying a selection and as the reference the windowed `draw` is tested against.
pub(crate) fn all_lines(
    transcript: &mut Transcript,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    welcome_ms: u64,
) -> Vec<Line<'static>> {
    let Transcript {
        cells,
        header,
        cache,
        images,
        expanded,
        live_cell,
        ..
    } = &mut *transcript;
    let mut out = match header.as_ref() {
        Some(info) => crate::ui::header::render(info, width, opts, anim_frame, welcome_ms),
        None => Vec::new(),
    };
    let mut parts = Parts {
        cache,
        images,
        expanded,
    };
    for cell in cells.iter() {
        let is_live = *live_cell == Some(cell.id);
        let resolved = resolve_cell(cell, is_live, width, opts, anim_frame, &mut parts);
        out.extend(lines_of(parts.cache, &resolved).iter().cloned());
    }
    out
}

/// The transcript as plain text, one entry per rendered line, in the same order
/// and with the same indices the selection uses.
pub(crate) fn plain_lines(
    transcript: &mut Transcript,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    welcome_ms: u64,
) -> Vec<String> {
    all_lines(transcript, width, opts, anim_frame, welcome_ms)
        .into_iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}
