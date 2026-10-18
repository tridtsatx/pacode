//! Inline (in-transcript) image rendering: half-block cells only.
//!
//! Cells behave like ordinary styled text, so they scroll, clip and redraw correctly
//! under cell-diffing renderers and never leave ghost images behind. Raw graphics
//! escapes (kitty/iTerm2/sixel) are deliberately NOT used inside the scrolling
//! transcript.

use crate::halfblocks::{PreviewCell, render_halfblocks};
use crate::render::{ImageError, MAX_IMAGE_HEIGHT, MAX_IMAGE_WIDTH};

#[cfg(test)]
#[path = "inline_tests.rs"]
mod inline_tests;

/// Tallest inline image, in terminal rows.
pub const INLINE_MAX_ROWS: u16 = 14;

/// A rendered inline image: `cells.len() == rows`, every row `cols` cells wide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineImage {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Vec<PreviewCell>>,
}

impl InlineImage {
    /// Approximate heap size, for cache accounting.
    pub fn approx_bytes(&self) -> usize {
        self.cols as usize * self.rows as usize * std::mem::size_of::<PreviewCell>()
    }
}

/// Decode `bytes` and render at most `max_cols` x `max_rows` cells, aspect preserved
/// (a cell is about twice as tall as wide, which half-blocks compensate for).
pub fn render_inline(
    bytes: &[u8],
    max_cols: u16,
    max_rows: u16,
) -> Result<InlineImage, ImageError> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    if w > MAX_IMAGE_WIDTH || h > MAX_IMAGE_HEIGHT {
        return Err(ImageError::DimensionsExceeded {
            width: w,
            height: h,
            max_width: MAX_IMAGE_WIDTH,
            max_height: MAX_IMAGE_HEIGHT,
        });
    }
    let img = image::load_from_memory(bytes)?;
    let cells = render_halfblocks(&img, max_cols, max_rows.min(INLINE_MAX_ROWS));
    let rows = cells.len() as u16;
    let cols = cells.first().map(|r| r.len() as u16).unwrap_or(0);
    Ok(InlineImage { cols, rows, cells })
}
