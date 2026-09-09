//! Image preview rendering dispatcher for detected terminal protocols.

use std::path::PathBuf;

use crate::detect::GraphicsProtocol;
use crate::halfblocks::{self, PreviewCell};
use crate::iterm2;
use crate::kitty;

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;

/// Maximum allowed file size for image previews: 16 MiB.
pub const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;
/// Maximum allowed image width in pixels: 8000.
pub const MAX_IMAGE_WIDTH: u32 = 8000;
/// Maximum allowed image height in pixels: 8000.
pub const MAX_IMAGE_HEIGHT: u32 = 8000;

/// Request for rendering an image preview at a specified terminal cell size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewRequest {
    pub path: PathBuf,
    pub cell_w: u16,
    pub cell_h: u16,
}

/// Rendered preview: either an escape sequence to emit verbatim, or coloured half-block cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    Escape(String),
    Cells(Vec<Vec<PreviewCell>>),
}

/// Typed errors returned by `render`.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("file size {size} exceeds maximum {limit} bytes")]
    FileSizeExceeded { size: u64, limit: u64 },

    #[error("image dimensions {width}x{height} exceed limit {max_width}x{max_height}")]
    DimensionsExceeded {
        width: u32,
        height: u32,
        max_width: u32,
        max_height: u32,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("image decode error: {0}")]
    Image(#[from] image::ImageError),
}

/// Render an image preview using the specified graphics protocol.
///
/// Refuses files over 16 MiB and images over 8000x8000 with typed errors before allocating.
pub fn render(req: &PreviewRequest, proto: GraphicsProtocol) -> Result<Preview, ImageError> {
    // 1. Check file size before reading into memory
    let metadata = std::fs::metadata(&req.path)?;
    let size = metadata.len();
    if size > MAX_FILE_SIZE {
        return Err(ImageError::FileSizeExceeded {
            size,
            limit: MAX_FILE_SIZE,
        });
    }

    let bytes = std::fs::read(&req.path)?;

    // 2. Inspect dimensions before allocating full decoded pixel buffer
    let cursor = std::io::Cursor::new(&bytes);
    let reader = image::ImageReader::new(cursor).with_guessed_format()?;
    let (width, height) = reader.into_dimensions()?;
    if width > MAX_IMAGE_WIDTH || height > MAX_IMAGE_HEIGHT {
        return Err(ImageError::DimensionsExceeded {
            width,
            height,
            max_width: MAX_IMAGE_WIDTH,
            max_height: MAX_IMAGE_HEIGHT,
        });
    }

    // 3. Render according to graphics protocol
    match proto {
        GraphicsProtocol::Kitty => {
            let img = image::load_from_memory(&bytes)?;
            let rgba = img.to_rgba8();
            let esc = kitty::encode_kitty(&rgba, width, height, req.cell_w, req.cell_h);
            Ok(Preview::Escape(esc))
        }
        GraphicsProtocol::Iterm2 => {
            let esc = iterm2::encode_iterm2(&bytes, req.cell_w, req.cell_h);
            Ok(Preview::Escape(esc))
        }
        GraphicsProtocol::Sixel | GraphicsProtocol::HalfBlocks => {
            // Sixel falls back to HalfBlocks as specified
            let img = image::load_from_memory(&bytes)?;
            let cells = halfblocks::render_halfblocks(&img, req.cell_w, req.cell_h);
            Ok(Preview::Cells(cells))
        }
    }
}
