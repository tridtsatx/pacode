//! Pasted-image preview strip state (spec §1): the prompt holds `[Image N]`
//! tokens; on submit they are rewritten to `@<path>` text refs and resolved
//! into `ImageSource`s for the daemon. Thumbnails rendered for the strip are
//! cached per token — escape payloads (kitty/sixel) can be tens of MB, so they
//! are never re-encoded while the layout is unchanged.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use pacode_types::ImageSource;

use crate::clipboard_read::PastedImages;

use super::AppState;

#[cfg(test)]
#[path = "images_tests.rs"]
mod images_tests;

/// Width of one thumbnail slot in terminal columns.
pub const THUMB_SLOT_W: u16 = 20;
/// Rows a single strip of thumbnails occupies.
pub const STRIP_ROWS: u16 = 6;
/// Spec cap: the strip never exceeds 8 rows.
pub const STRIP_MAX_ROWS: u16 = 8;
/// Maximum number of image decoders that may run at once.
pub const IMAGE_RENDER_MAX_IN_FLIGHT: usize = 4;
/// Maximum number of pending render requests retained between frames.
pub const IMAGE_RENDER_MAX_WANTS: usize = 16;

/// A rendered thumbnail for one pasted image.
#[derive(Clone, Debug)]
pub struct CachedThumb {
    /// Slot size (columns, half-block rows) the preview was rendered for;
    /// re-render when it changes.
    pub cell_w: u16,
    pub cell_h: u16,
    /// `None` when decoding or protocol rendering failed — the slot stays but
    /// shows a dim placeholder instead of retrying every frame.
    pub preview: Option<pacode_image::Preview>,
}

/// Thumbnail cache for the input preview strip, keyed by paste token.
/// Entries are evicted when their token leaves the input (`retain`), so the
/// map is bounded by the number of live `[Image N]` references (≤ 16 by the
/// paste cap).
#[derive(Debug, Default)]
pub struct ImageStripState {
    pub thumbs: HashMap<usize, CachedThumb>,
    /// `(token, cell_w, cell_h)` renders running on blocking threads — draw
    /// shows a placeholder for these and never renders inline (§8.1).
    pub in_flight: HashSet<(usize, u16, u16)>,
    /// Thumbnails draw asked for — `(token, path, cell_w, cell_h)`. The event
    /// loop drains this into `in_flight` jobs; each entry arrives at most once
    /// per frame pass.
    pub want: Vec<(usize, PathBuf, u16, u16)>,
}

impl ImageStripState {
    pub fn can_start_render(&self) -> bool {
        self.in_flight.len() < IMAGE_RENDER_MAX_IN_FLIGHT
    }

    /// Drop cached thumbs for tokens that are no longer referenced by the
    /// input. In-flight renders for dead tokens are dropped too — their
    /// replies land in `thumbs` and get evicted on the next `retain`.
    pub fn retain(&mut self, live: &[usize]) {
        self.thumbs.retain(|token, _| live.contains(token));
        self.in_flight.retain(|(token, _, _)| live.contains(token));
        self.want.retain(|(token, ..)| live.contains(token));

        let mut seen = HashSet::new();
        let mut latest: Vec<_> = self
            .want
            .drain(..)
            .rev()
            .filter(|want| seen.insert(want.0))
            .take(IMAGE_RENDER_MAX_WANTS)
            .collect();
        latest.reverse();
        self.want = latest;
    }
}

/// Rewrite every `[Image N]` token backed by a record into `@<path>` and
/// collect the matching `ImageSource`s, deduplicated, in order of first
/// appearance. Tokens with no matching record stay literal text.
pub fn resolve_image_tokens(images: &PastedImages, text: &str) -> (String, Vec<ImageSource>) {
    const TOKEN_PREFIX: &str = "[Image ";
    let mut out = String::with_capacity(text.len());
    let mut sources: Vec<ImageSource> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(TOKEN_PREFIX) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + TOKEN_PREFIX.len()..];
        // Byte length of the leading ASCII-digit run in `after`.
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let record = if digits > 0 && after[digits..].starts_with(']') {
            after[..digits]
                .parse::<usize>()
                .ok()
                .and_then(|n| images.records().iter().find(|r| r.token == n))
        } else {
            None
        };
        match record {
            Some(rec) => {
                out.push_str(&format!("@{}", rec.path.display()));
                if !seen.contains(&rec.token) {
                    seen.push(rec.token);
                    sources.push(ImageSource::Path(rec.path.clone()));
                }
                rest = &after[digits + 1..];
            }
            None => {
                // Not a token (or no such record): copy the prefix literally and
                // keep scanning — the digits and `]` remain ordinary text.
                out.push_str(TOKEN_PREFIX);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, sources)
}

impl AppState {
    pub fn cleanup_pasted_images(&mut self) {
        let text = self.input.text.clone();
        self.pasted_images.cleanup_unreferenced(&text);
    }

    /// Records whose `[Image N]` token is still referenced by the current
    /// input. Submitted records are excluded: they stay on disk for the
    /// daemon but the preview strip no longer shows them.
    pub fn live_pasted_images(&self) -> Vec<&crate::clipboard_read::PastedImageRecord> {
        self.pasted_images
            .records()
            .iter()
            .filter(|r| !r.submitted && self.input.text.contains(r.token_label().as_str()))
            .collect()
    }

    /// Preview strip height in terminal rows: 0 when images are disabled or
    /// no live `[Image N]` token remains in the input.
    pub fn image_strip_rows(&self) -> u16 {
        if self.config.ui.images_enabled() && !self.live_pasted_images().is_empty() {
            STRIP_ROWS.min(STRIP_MAX_ROWS)
        } else {
            0
        }
    }
}
