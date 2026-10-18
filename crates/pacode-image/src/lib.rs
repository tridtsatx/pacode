//! Image preview rendering and terminal graphics protocol detection.
//!
//! Provides protocol detection (Kitty, iTerm2, Sixel, HalfBlocks) and rendering
//! of images into either escape sequences or half-block UTF-8 cells.

pub mod detect;
pub mod halfblocks;
pub mod inline;
pub mod iterm2;
pub mod kitty;
pub mod prepare;
pub mod render;
pub mod store;

pub use detect::{GraphicsProtocol, detect, detect_from};
pub use halfblocks::{PreviewCell, compute_halfblock_dimensions, render_halfblocks};
pub use inline::{INLINE_MAX_ROWS, InlineImage, render_inline};
pub use iterm2::encode_iterm2;
pub use kitty::{chunk_kitty_base64, encode_kitty};
pub use prepare::{MAX_IMAGES, PrepareError, Prepared, prepare};
pub use render::{
    ImageError, MAX_FILE_SIZE, MAX_IMAGE_HEIGHT, MAX_IMAGE_WIDTH, Preview, PreviewRequest, render,
};
