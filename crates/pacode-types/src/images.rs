//! Image DTOs: wire attachments, stored-image references and the token estimate.
//!
//! Image bytes never live in a [`crate::Message`]: the daemon stores them once,
//! content-addressed by `hash`, and messages and transcript items carry only the
//! small [`ImageRef`] metadata. Whoever needs pixels (a provider transport, the TUI
//! renderer) loads them lazily by hash.

use serde::{Deserialize, Serialize};

/// Hard cap on images attached to one user message.
pub const MAX_IMAGES_PER_MESSAGE: usize = 8;

/// Upper bound of the token estimate for one image.
pub const IMAGE_TOKEN_CAP: u32 = 1600;

/// Estimate used when the pixel size is unknown.
pub const IMAGE_TOKEN_DEFAULT: u32 = 1200;

/// A file the client sends along with a user message. `data` is the base64 of the
/// original (unprepared) bytes; the daemon decodes, downsizes and stores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// `image/png`, `image/jpeg`, `image/webp`, `image/gif`.
    pub mime: String,
    /// Standard base64 (no `data:` prefix).
    pub data: String,
    /// File name or label for display, when the client has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Metadata of an image kept in the daemon's content-addressed image store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    /// Lowercase hex SHA-256 of the stored bytes.
    pub hash: String,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// File name or path shown in stubs and tool placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Fixed token estimate for one image: `w*h/750`, capped at [`IMAGE_TOKEN_CAP`].
/// Never derived from the base64 length.
pub fn estimate_image_tokens(width: Option<u32>, height: Option<u32>) -> u32 {
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => {
            let px = u64::from(w) * u64::from(h);
            ((px / 750).max(1) as u32).min(IMAGE_TOKEN_CAP)
        }
        _ => IMAGE_TOKEN_DEFAULT,
    }
}

/// Human size for chips and stubs: `812 B`, `1.2 KB`, `3.4 MB`.
pub fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod images_tests;
