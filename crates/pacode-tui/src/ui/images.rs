//! Pasted-image preview strip above the input (spec §1): one row of
//! ~20-column thumbnails, one per live `[Image N]` token. `Preview::Cells`
//! renders in-frame as half blocks; `Preview::Escape` payloads are stashed in
//! `state.pending_image_escapes` and flushed post-draw by the app loop —
//! escapes are never re-emitted while the set is unchanged.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use pacode_render::RenderOptions;

use crate::state::AppState;
use crate::state::images::{CachedThumb, THUMB_SLOT_W};

pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState, opts: &RenderOptions) {
    state.pending_image_escapes.clear();
    let live = state.live_pasted_images();
    if area.width == 0 || area.height == 0 || live.is_empty() {
        state.image_strip.retain(&[]);
        return;
    }

    // (token, path) pairs — cloned so the thumbs cache can be mutated below.
    let live: Vec<(usize, std::path::PathBuf)> =
        live.iter().map(|r| (r.token, r.path.clone())).collect();
    let tokens: Vec<usize> = live.iter().map(|(t, _)| *t).collect();
    state.image_strip.retain(&tokens);

    let protocol = pacode_image::detect();

    for (i, (token, path)) in live.iter().enumerate() {
        let x = area.x + (i as u16) * THUMB_SLOT_W;
        if x >= area.right() {
            break;
        }
        let slot = Rect::new(x, area.y, THUMB_SLOT_W.min(area.right() - x), area.height);
        if slot.width < 4 || slot.height == 0 {
            continue;
        }

        let cached = state
            .image_strip
            .thumbs
            .get(token)
            .filter(|t| t.cell_w == slot.width && t.cell_h == slot.height);
        let preview = match cached {
            Some(t) => t.preview.clone(),
            None => {
                let req = pacode_image::PreviewRequest {
                    path: path.clone(),
                    cell_w: slot.width,
                    cell_h: slot.height,
                };
                let preview = pacode_image::render(&req, protocol).ok();
                state.image_strip.thumbs.insert(
                    *token,
                    CachedThumb {
                        cell_w: slot.width,
                        cell_h: slot.height,
                        preview: preview.clone(),
                    },
                );
                preview
            }
        };

        match preview {
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
                state.pending_image_escapes.push((slot, esc));
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
