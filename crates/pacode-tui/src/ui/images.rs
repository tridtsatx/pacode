//! Inline images in the transcript, drawn as half-block cells (`▀` with the top pixel
//! as foreground and the bottom pixel as background).
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

use pacode_image::{INLINE_MAX_ROWS, InlineImage};
use pacode_render::{RenderOptions, rgb_to_ansi16, truncate_to_width};
use pacode_types::ImageRef;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::state::image_cache::ImageCellCache;

#[cfg(test)]
#[path = "images_tests.rs"]
mod images_tests;

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

/// Split one input line into spans, drawing attachment chips (`[Image 1 \u{b7} 1.2 MB]`)
/// in `chip` style and the rest in `base`.
pub fn style_chips(line: &str, base: Style, chip: Style) -> Vec<Span<'static>> {
    let chips = crate::attachments::find_chips(line);
    if chips.is_empty() {
        return vec![Span::styled(line.to_string(), base)];
    }
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::with_capacity(chips.len() * 2 + 1);
    let mut pos = 0;
    for c in chips {
        if c.start > pos {
            spans.push(Span::styled(
                chars[pos..c.start].iter().collect::<String>(),
                base,
            ));
        }
        spans.push(Span::styled(
            chars[c.start..c.end].iter().collect::<String>(),
            chip,
        ));
        pos = c.end;
    }
    if pos < chars.len() {
        spans.push(Span::styled(chars[pos..].iter().collect::<String>(), base));
    }
    spans
}
