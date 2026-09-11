//! Image content-block helpers shared by every provider adapter (spec §1).
//!
//! [`load_image_data`] resolves an [`ImageSource`] to wire-ready `(mime, base64)`
//! under a byte cap; [`image_fallback_text`] produces the
//! `[image file: <name> (<n> bytes)]` placeholder — the same shape
//! `expand.rs` emits for `@`-referenced images — for models without vision and
//! for sources that fail to load.

use std::path::Path;

use base64::Engine;
use pacode_types::{ContentBlock, ImageSource, Message};

#[cfg(test)]
#[path = "image_tests.rs"]
mod image_tests;

/// Per-file byte cap, mirroring `PastedImages` (spec §1: ≤20 MiB each).
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Text stand-in for an image block the model cannot receive. Prefers the
/// size-annotated `[image file: <name> (<n> bytes)]` form; falls back to
/// [`ImageSource::placeholder`] when the size is unknown.
pub fn image_fallback_text(source: &ImageSource) -> String {
    match source {
        ImageSource::Path(path) => match std::fs::metadata(path) {
            Ok(meta) => format!("[image file: {} ({} bytes)]", path.display(), meta.len()),
            Err(_) => source.placeholder(),
        },
        ImageSource::Bytes { .. } => source.placeholder(),
    }
}

/// `msg.text()` plus one fallback line per image block — the degrade path for
/// roles and adapters that can only carry text.
pub fn text_with_image_fallbacks(msg: &Message) -> String {
    let mut out = msg.text();
    for block in &msg.content {
        if let ContentBlock::Image { source } = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&image_fallback_text(source));
        }
    }
    out
}

/// Resolve `source` to `(mime, base64 data)` for a wire image payload.
///
/// `None` when the file is unreadable, larger than `cap`, or of an unknown
/// image type; the caller emits [`image_fallback_text`] instead so the turn
/// never fails on a bad attachment.
pub fn load_image_data(source: &ImageSource, cap: usize) -> Option<(String, String)> {
    match source {
        ImageSource::Bytes { mime, data } => {
            if data.len() > cap {
                return None;
            }
            Some((mime.clone(), encode_b64(data)))
        }
        ImageSource::Path(path) => {
            let mime = mime_for_path(path)?;
            if std::fs::metadata(path).ok()?.len() > cap as u64 {
                return None;
            }
            let data = std::fs::read(path).ok()?;
            if data.len() > cap {
                return None;
            }
            Some((mime.to_string(), encode_b64(&data)))
        }
    }
}

/// MIME type guessed from the extension — the same list
/// `expand.rs::is_image_path` accepts.
fn mime_for_path(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn encode_b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}
