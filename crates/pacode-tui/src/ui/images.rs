//! Pasted-image preview strip above the input (spec §1): one row of
//! ~20-column thumbnails, one per live `[Image N]` token, each centered over
//! the first occurrence of its token in the input text. `Preview::Cells`
//! renders in-frame as half blocks; `Preview::Escape` payloads are stashed in
//! `state.pending_image_escapes` and flushed post-draw by the app loop —
//! escapes are never re-emitted while the set is unchanged.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use pacode_render::{RenderOptions, display_width};

use crate::state::AppState;
use crate::state::images::THUMB_SLOT_W;

#[cfg(test)]
#[path = "images_tests.rs"]
mod images_tests;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    input: Rect,
    state: &mut AppState,
    opts: &RenderOptions,
) {
    state.pending_image_escapes.clear();
    let live = state.live_pasted_images();
    if area.width == 0 || area.height == 0 || live.is_empty() {
        state.image_strip.retain(&[]);
        return;
    }

    // (token, label, path) triples — cloned so the thumbs cache can be
    // mutated below.
    let live: Vec<(usize, String, std::path::PathBuf)> = live
        .iter()
        .map(|r| (r.token, r.token_label(), r.path.clone()))
        .collect();
    let tokens: Vec<usize> = live.iter().map(|(t, ..)| *t).collect();
    state.image_strip.retain(&tokens);

    for (token, path, slot) in layout_slots(state, &live, input, area, opts) {
        if slot.width < 4 || slot.height == 0 {
            continue;
        }

        let cached = state
            .image_strip
            .thumbs
            .get(&token)
            .filter(|t| t.cell_w == slot.width && t.cell_h == slot.height);
        let Some(thumb) = cached else {
            // §8.1: `pacode_image::render` does file IO + decode/encode — it
            // runs on a blocking thread. Record the want and draw a
            // placeholder until `ThumbReady` lands in `thumbs`.
            state
                .image_strip
                .want
                .push((token, path.clone(), slot.width, slot.height));
            let label = Line::from(Span::styled(format!("[image {token} …]"), opts.theme.faint));
            frame.render_widget(Paragraph::new(label), slot);
            continue;
        };

        match &thumb.preview {
            Some(pacode_image::Preview::Cells(grid)) => {
                let mut lines = Vec::new();
                for row in grid.iter().take(slot.height as usize) {
                    let mut spans = Vec::new();
                    for cell in row.iter().take(slot.width as usize) {
                        let style = Style::default()
                            .fg(Color::Rgb(cell.top.0, cell.top.1, cell.top.2))
                            .bg(Color::Rgb(cell.bottom.0, cell.bottom.1, cell.bottom.2));
                        spans.push(Span::styled("▀", style));
                    }
                    lines.push(Line::from(spans));
                }
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), slot);
            }
            Some(pacode_image::Preview::Escape(esc)) => {
                frame.render_widget(Clear, slot);
                state.pending_image_escapes.push((slot, esc.clone()));
            }
            None => {
                let label = Line::from(Span::styled(
                    format!("[image {token} unreadable]"),
                    opts.theme.faint,
                ));
                frame.render_widget(Paragraph::new(label), slot);
            }
        }
    }
}

/// One slot per live record. Each slot is centered over the first occurrence
/// of its `[Image N]` token in the input text — `input` is the input rect:
/// text renders at `input.x + 2` and wraps at `input.width - 2`, except in
/// bash mode, where `<prefix>` + `text[1:]` draw on ONE unwrapped line.
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
    opts: &RenderOptions,
) -> Vec<(usize, std::path::PathBuf, Rect)> {
    // The bash-mode prefix the input renderer actually drew: `! `, or
    // `<running> (busy) ` while a captured command runs.
    let bash_prefix_w = if state.is_running_bash() {
        display_width(&format!("{} (busy) ", opts.glyphs.running))
    } else {
        2
    };
    let centers: Vec<Option<u16>> = live
        .iter()
        .map(|(_, label, _)| token_center(&state.input, input, label, bash_prefix_w))
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
fn token_center(
    input: &crate::state::InputState,
    input_rect: Rect,
    label: &str,
    bash_prefix_w: usize,
) -> Option<u16> {
    let text = &input.text;
    let byte = text.find(label)?;
    let token_w = display_width(label);
    let start = if text.starts_with('!') {
        // Bash mode: `<prefix>` + text[1:] on one unwrapped line — the `!`
        // itself is the prefix's first cell.
        bash_prefix_w + display_width(text.get(1..byte).unwrap_or(""))
    } else {
        // Wrapped mode: a token may split across lines (`[Image` / `N]`) —
        // anchor by the column where it STARTS, on the line where it begins.
        2 + input.wrapped_position_of_byte(byte, input_rect.width).1
    };
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
