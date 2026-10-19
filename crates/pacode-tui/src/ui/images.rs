//! Pasted-image preview strip above the input (spec §1): one row of
//! ~20-column thumbnails, one per live `[Image N]` token, each centered over
//! the first occurrence of its token in the input text. `Preview::Cells`
//! renders in-frame as half blocks; `Preview::Escape` payloads are stashed in
//! `state.pending_image_escapes` and flushed post-draw by the app loop —
//! escapes are never re-emitted while the set is unchanged.
//!
//! Inline images in the transcript (below the strip code): drawn as half-block
//! cells (`▀` with the top pixel as foreground and the bottom pixel as background).
//!
//! Cells are ordinary styled text, so they scroll, clip and redraw correctly under
//! ratatui's cell diffing and never leave ghost images. Kitty/iTerm2/sixel escapes are
//! deliberately not used inside the scrolling transcript; kitty Unicode placeholders
//! are not implemented (halfblocks everywhere).
//!
//! An image is at most [`INLINE_MAX_ROWS`] rows high and as wide as the cell (minus the
//! two-column indent), aspect preserved. With `[ui] images = "off"` (or an ASCII-only
//! terminal) a one-line `[image: name 800×600]` stub is drawn instead.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use pacode_image::{INLINE_MAX_ROWS, InlineImage};
use pacode_render::{RenderOptions, display_width, rgb_to_ansi16, truncate_to_width};
use pacode_types::ImageRef;

use crate::state::AppState;
use crate::state::image_cache::ImageCellCache;
use crate::state::images::THUMB_SLOT_W;

#[cfg(test)]
#[path = "images_tests.rs"]
mod images_tests;

enum PreparedImage {
    Lines {
        slot: Rect,
        lines: Vec<Line<'static>>,
        wrap: bool,
    },
    Escape {
        slot: Rect,
    },
}

#[derive(Default)]
pub(crate) struct PreparedImages {
    images: Vec<PreparedImage>,
}

pub(crate) fn prepare(
    area: Rect,
    input: Rect,
    state: &mut AppState,
    opts: &RenderOptions,
) -> PreparedImages {
    state.pending_image_escapes.clear();
    let live = state.live_pasted_images();
    if area.width == 0 || area.height == 0 || live.is_empty() {
        state.image_strip.retain(&[]);
        return PreparedImages::default();
    }

    let live: Vec<(usize, String, std::path::PathBuf)> = live
        .iter()
        .map(|record| (record.token, record.token_label(), record.path.clone()))
        .collect();
    let tokens: Vec<usize> = live.iter().map(|(token, ..)| *token).collect();
    state.image_strip.retain(&tokens);
    let mut images = Vec::new();
    for (token, path, slot) in layout_slots(state, &live, input, area) {
        if slot.width < 4 || slot.height == 0 {
            continue;
        }
        let cached = state
            .image_strip
            .thumbs
            .get(&token)
            .filter(|thumb| thumb.cell_w == slot.width && thumb.cell_h == slot.height);
        let Some(thumb) = cached else {
            state
                .image_strip
                .want
                .push((token, path.clone(), slot.width, slot.height));
            images.push(PreparedImage::Lines {
                slot,
                lines: vec![Line::from(Span::styled(
                    format!("[image {token} …]"),
                    opts.theme.faint,
                ))],
                wrap: false,
            });
            continue;
        };
        match &thumb.preview {
            Some(pacode_image::Preview::Cells(grid)) => {
                let lines = grid
                    .iter()
                    .take(slot.height as usize)
                    .map(|row| {
                        Line::from(
                            row.iter()
                                .take(slot.width as usize)
                                .map(|cell| {
                                    Span::styled(
                                        "▀",
                                        Style::default()
                                            .fg(Color::Rgb(cell.top.0, cell.top.1, cell.top.2))
                                            .bg(Color::Rgb(
                                                cell.bottom.0,
                                                cell.bottom.1,
                                                cell.bottom.2,
                                            )),
                                    )
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                images.push(PreparedImage::Lines {
                    slot,
                    lines,
                    wrap: true,
                });
            }
            Some(pacode_image::Preview::Escape(escape)) => {
                state.pending_image_escapes.push((slot, escape.clone()));
                images.push(PreparedImage::Escape { slot });
            }
            None => images.push(PreparedImage::Lines {
                slot,
                lines: vec![Line::from(Span::styled(
                    format!("[image {token} unreadable]"),
                    opts.theme.faint,
                ))],
                wrap: false,
            }),
        }
    }
    PreparedImages { images }
}

pub(crate) fn draw(frame: &mut Frame, prepared: &PreparedImages) {
    for image in &prepared.images {
        match image {
            PreparedImage::Lines { slot, lines, wrap } => {
                let paragraph = Paragraph::new(lines.clone());
                frame.render_widget(
                    if *wrap {
                        paragraph.wrap(Wrap { trim: false })
                    } else {
                        paragraph
                    },
                    *slot,
                );
            }
            PreparedImage::Escape { slot } => frame.render_widget(Clear, *slot),
        }
    }
}

/// One slot per live record. Each slot is centered over the first occurrence
/// of its `[Image N]` token in the input text — `input` is the input rect:
/// text renders at `input.x + 2` and wraps at `input.width - 2`, bash mode
/// included (it draws the same wrapped rows behind the same prefix).
/// Records in text order (sorted by center, not insertion order); a record
/// whose token position did not resolve — a mid-frame deletion race,
/// `live_pasted_images` requires the token in the text — packs at the right
/// end. When NO token resolves, the strip falls back to a left-packed row.
/// Slots never overlap and never draw outside `area`.
fn layout_slots(
    state: &AppState,
    live: &[(usize, String, std::path::PathBuf)],
    input: Rect,
    area: Rect,
) -> Vec<(usize, std::path::PathBuf, Rect)> {
    let centers: Vec<Option<u16>> = live
        .iter()
        .map(|(_, label, _)| token_center(&state.input, input, label))
        .collect();

    // No token resolved at all (the whole set was deleted mid-frame):
    // the pre-centering left-packed row.
    if centers.iter().all(Option::is_none) {
        return live
            .iter()
            .enumerate()
            .filter_map(|(i, (token, _, path))| {
                let x = area.x.saturating_add(i as u16 * THUMB_SLOT_W);
                (x < area.right()).then(|| {
                    (
                        *token,
                        path.clone(),
                        Rect::new(x, area.y, THUMB_SLOT_W.min(area.right() - x), area.height),
                    )
                })
            })
            .collect();
    }

    let n = live.len() as u16;
    let mut slot_w = if n.saturating_mul(THUMB_SLOT_W) <= area.width {
        THUMB_SLOT_W
    } else {
        (area.width / n).max(4)
    };
    // Two token centres `g` columns apart can only carry non-overlapping
    // centred slots up to `g` wide — narrow the thumbs so neighbours still
    // sit over their tokens instead of packing into a left-to-right row.
    let mut resolved: Vec<u16> = centers.iter().flatten().copied().collect();
    resolved.sort_unstable();
    for gap in resolved.windows(2).map(|p| p[1] - p[0]) {
        slot_w = slot_w.min(gap);
    }
    slot_w = slot_w.max(4);

    // Text order: resolved tokens sorted by their center column first, then
    // unresolved ones (sentinel ideal packs them at the right end).
    let mut order: Vec<usize> = (0..live.len()).collect();
    order.sort_by_key(|&i| centers[i].unwrap_or(u16::MAX));
    let ideals: Vec<u16> = order
        .iter()
        .map(|&i| match centers[i] {
            Some(center) => center.saturating_sub(slot_w / 2),
            None => u16::MAX,
        })
        .collect();
    let xs = place_slots(&ideals, slot_w, area);

    order
        .iter()
        .zip(xs)
        .filter_map(|(&i, x)| {
            let (token, _, path) = &live[i];
            let w = slot_w.min(area.right().saturating_sub(x));
            (x < area.right() && w > 0)
                .then(|| (*token, path.clone(), Rect::new(x, area.y, w, area.height)))
        })
        .collect()
}

/// Absolute screen column of the center cell of `label`'s first occurrence
/// in the input text — `None` while the text holds no such token.
fn token_center(input: &crate::state::InputState, input_rect: Rect, label: &str) -> Option<u16> {
    let byte = input.text.find(label)?;
    let token_w = display_width(label);
    // Bash mode draws the same wrapped rows behind the same 2-cell prefix as
    // a plain prompt, so one rule places both. A token may split across rows
    // (`[Image` / `N]`) — anchor by the column where it STARTS.
    let start = 2 + input.wrapped_position_of_byte(byte, input_rect.width).1;
    Some(
        input_rect
            .x
            .saturating_add(start as u16)
            .saturating_add((token_w / 2) as u16),
    )
}

/// Resolve slot left edges inside `[area.x, area.right())` from ideal edges:
/// forward pass for left bounds and non-overlap, backward pass for the right
/// bound, forward once more for the head the pull-back may have pushed off
/// `area.x`. With `n * slot_w <= area.width` the result is fully in bounds;
/// tighter strips degrade to packing and the caller clips each slot's width.
fn place_slots(ideals: &[u16], slot_w: u16, area: Rect) -> Vec<u16> {
    let mut xs = ideals.to_vec();
    let mut right = area.x;
    for x in xs.iter_mut() {
        *x = (*x).max(right);
        right = x.saturating_add(slot_w);
    }
    let mut right = area.right();
    for x in xs.iter_mut().rev() {
        *x = (*x).min(right.saturating_sub(slot_w));
        right = *x;
    }
    let mut right = area.x;
    for x in xs.iter_mut() {
        *x = (*x).max(right);
        right = x.saturating_add(slot_w);
    }
    xs
}

const INDENT: &str = "  ";

/// `[image: name 800×600]`, with a suffix when the bytes could not be shown.
fn stub_text(image: &ImageRef, note: Option<&str>) -> String {
    let name = image
        .name
        .as_deref()
        .and_then(|n| Path::new(n).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let mut out = format!("[image: {name}");
    if let (Some(w), Some(h)) = (image.width, image.height) {
        out.push_str(&format!(" {w}\u{d7}{h}"));
    }
    if let Some(note) = note {
        out.push_str(&format!(" ({note})"));
    }
    out.push(']');
    out
}

fn stub_line(
    image: &ImageRef,
    note: Option<&str>,
    width: u16,
    opts: &RenderOptions,
) -> Line<'static> {
    let text = truncate_to_width(
        &stub_text(image, note),
        (width as usize).saturating_sub(INDENT.len()).max(1),
        true,
    );
    Line::from(vec![
        Span::raw(INDENT),
        Span::styled(text, opts.theme.faint),
    ])
}

/// One stub line per image, then a blank line.
pub fn stub_lines(images: &[ImageRef], width: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = images
        .iter()
        .map(|image| stub_line(image, None, width, opts))
        .collect();
    out.push(Line::default());
    out
}

fn color(rgb: (u8, u8, u8), truecolor: bool) -> Color {
    if truecolor {
        Color::Rgb(rgb.0, rgb.1, rgb.2)
    } else {
        rgb_to_ansi16(rgb.0, rgb.1, rgb.2)
    }
}

/// Styled lines for decoded cells: runs of identical cells share one span.
pub fn cells_to_lines(image: &InlineImage, truecolor: bool) -> Vec<Line<'static>> {
    image
        .cells
        .iter()
        .map(|row| {
            let mut spans = Vec::with_capacity(1 + row.len() / 4);
            spans.push(Span::raw(INDENT));
            let mut i = 0;
            while i < row.len() {
                let cell = row[i];
                let mut run = 1;
                while i + run < row.len() && row[i + run] == cell {
                    run += 1;
                }
                spans.push(Span::styled(
                    "\u{2580}".repeat(run),
                    Style::default()
                        .fg(color(cell.top, truecolor))
                        .bg(color(cell.bottom, truecolor)),
                ));
                i += run;
            }
            Line::from(spans)
        })
        .collect()
}

/// The transcript rows of an `Images` item: each picture below the text, then a blank
/// line. Decoded cells come from (and go to) `cache`, keyed by hash and size.
pub fn render_images(
    images: &[ImageRef],
    width: u16,
    opts: &RenderOptions,
    cache: &mut ImageCellCache,
) -> Vec<Line<'static>> {
    if !cache.enabled || opts.glyphs.ascii {
        return stub_lines(images, width, opts);
    }
    let max_cols = width.saturating_sub(INDENT.len() as u16).max(1);
    let mut out = Vec::new();
    for image in images {
        let rendered = cache.get_or_render(&image.hash, max_cols, INLINE_MAX_ROWS, || {
            let bytes = pacode_image::store::load(&image.hash).map_err(|e| e.to_string())?;
            pacode_image::render_inline(&bytes, max_cols, INLINE_MAX_ROWS)
                .map_err(|e| e.to_string())
        });
        match rendered {
            Ok(cells) => out.extend(cells_to_lines(&cells, cache.truecolor)),
            Err(err) => {
                log::debug!("inline image {} not drawn: {err}", image.hash);
                out.push(stub_line(image, Some("unavailable"), width, opts));
            }
        }
    }
    out.push(Line::default());
    out
}

/// Split one input line into spans, drawing image tokens in `chip` style and the
/// rest in `base`: the paste tokens (`[Image 1]`) and the sized attachment chips
/// (`[Image 1 \u{b7} 1.2 MB]`).
pub fn style_chips(line: &str, base: Style, chip: Style) -> Vec<Span<'static>> {
    let chips = image_token_ranges(line);
    if chips.is_empty() {
        return vec![Span::styled(line.to_string(), base)];
    }
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::with_capacity(chips.len() * 2 + 1);
    let mut pos = 0;
    for (start, end) in chips {
        if start > pos {
            spans.push(Span::styled(
                chars[pos..start].iter().collect::<String>(),
                base,
            ));
        }
        spans.push(Span::styled(
            chars[start..end].iter().collect::<String>(),
            chip,
        ));
        pos = end;
    }
    if pos < chars.len() {
        spans.push(Span::styled(chars[pos..].iter().collect::<String>(), base));
    }
    spans
}

/// Char ranges (`end` exclusive, in order, non-overlapping) of the image tokens in
/// `line`: attachment chips first, then `[Image N]` paste tokens outside them.
fn image_token_ranges(line: &str) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = crate::attachments::find_chips(line)
        .into_iter()
        .map(|c| (c.start, c.end))
        .collect();
    const HEAD: &str = "[Image ";
    let chars: Vec<char> = line.chars().collect();
    let head: Vec<char> = HEAD.chars().collect();
    let mut i = 0;
    while i + head.len() < chars.len() {
        if chars[i..i + head.len()] == head[..] {
            let digits = chars[i + head.len()..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            let close = i + head.len() + digits;
            if digits > 0 && chars.get(close) == Some(&']') {
                let end = close + 1;
                if !ranges.iter().any(|(s, e)| i < *e && *s < end) {
                    ranges.push((i, end));
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    ranges.sort_unstable();
    ranges
}
