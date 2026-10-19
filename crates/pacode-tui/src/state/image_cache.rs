//! Decoded inline-image cells of the transcript.
//!
//! * Owner: one per [`crate::state::transcript::Transcript`] (the main dialog and the
//!   agent panel each have their own). The decode jobs in flight are the app's
//!   ([`InlineImageJobs`], one per process), so a result lands in whichever transcript
//!   still shows the image.
//! * Key: `(image hash, columns, rows)`. A resize changes the key, so a stale entry is
//!   never served; the entries of the old width age out through the LRU.
//! * Cap: [`MAX_ENTRIES`] entries and [`MAX_BYTES`] bytes of cells, least recently
//!   used first; at most [`MAX_WANTS`] decodes waiting and [`MAX_IN_FLIGHT`] running.
//!   Cleared on terminal resize, `/clear` and snapshot reset.
//! * Decoding never runs on the frame path or in a key handler: a miss records a
//!   want and draws the text stub; the event loop decodes on a blocking thread
//!   ([`decode`]) and hands the cells back to `Transcript::image_ready`, which drops
//!   the stub lines of every cell showing that image. A failed decode is kept (as an
//!   error) so it is not retried every frame.
//! * The cells are plain RGB data, independent of the theme, so a theme change needs
//!   no invalidation (the styled lines are rebuilt from them by the line cache).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use pacode_image::InlineImage;

#[cfg(test)]
#[path = "image_cache_tests.rs"]
mod image_cache_tests;

pub const MAX_ENTRIES: usize = 32;
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Decodes one transcript asks for between two passes of the event loop.
pub const MAX_WANTS: usize = 16;
/// Decodes running at once across the whole client.
pub const MAX_IN_FLIGHT: usize = 2;

/// One decoded rendering: an image at a size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub hash: String,
    pub cols: u16,
    pub rows: u16,
}

/// What the cache holds for a key.
#[derive(Clone, Debug)]
pub enum Lookup {
    Ready(Arc<InlineImage>),
    /// The decode failed; the message says why.
    Failed(String),
    /// Not decoded yet: a want was recorded.
    Pending,
}

type Decoded = Result<Arc<InlineImage>, String>;

#[derive(Debug)]
pub struct ImageCellCache {
    /// `[ui] images` is not `off`.
    pub enabled: bool,
    /// Whether the terminal takes 24-bit colour (else cells fold to ANSI-16).
    pub truecolor: bool,
    entries: HashMap<ImageKey, Decoded>,
    lru: VecDeque<ImageKey>,
    bytes: usize,
    /// Keys drawn this frame without cells, in first-asked order.
    wants: Vec<ImageKey>,
}

impl Default for ImageCellCache {
    fn default() -> Self {
        Self {
            enabled: true,
            truecolor: true,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            bytes: 0,
            wants: Vec::new(),
        }
    }
}

fn entry_bytes(entry: &Decoded) -> usize {
    entry.as_ref().map_or(0, |image| image.approx_bytes())
}

impl ImageCellCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.bytes = 0;
        self.wants.clear();
    }

    /// The cells for `(hash, cols, rows)`. A miss records a want for the event
    /// loop and answers [`Lookup::Pending`]; nothing is decoded here.
    pub fn lookup(&mut self, hash: &str, cols: u16, rows: u16) -> Lookup {
        let key = ImageKey {
            hash: hash.to_string(),
            cols,
            rows,
        };
        let Some(found) = self.entries.get(&key) else {
            if !self.wants.contains(&key) && self.wants.len() < MAX_WANTS {
                self.wants.push(key);
            }
            return Lookup::Pending;
        };
        let found = match found {
            Ok(image) => Lookup::Ready(Arc::clone(image)),
            Err(err) => Lookup::Failed(err.clone()),
        };
        if let Some(pos) = self.lru.iter().position(|k| *k == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key);
        found
    }

    /// Keep a finished decode (success or failure), evicting the least recently
    /// used entries past the caps.
    pub fn insert(&mut self, key: ImageKey, decoded: Result<InlineImage, String>) {
        self.wants.retain(|want| *want != key);
        let decoded = decoded.map(Arc::new);
        self.bytes += entry_bytes(&decoded);
        if let Some(old) = self.entries.insert(key.clone(), decoded) {
            self.bytes = self.bytes.saturating_sub(entry_bytes(&old));
            if let Some(pos) = self.lru.iter().position(|k| *k == key) {
                self.lru.remove(pos);
            }
        }
        self.lru.push_back(key);
        while (self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES) && self.lru.len() > 1 {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(entry_bytes(&evicted));
            }
        }
    }

    /// The decodes the last draw asked for, oldest first.
    pub fn take_wants(&mut self) -> Vec<ImageKey> {
        std::mem::take(&mut self.wants)
    }
}

/// The decodes running on blocking threads, across every transcript.
#[derive(Debug, Default)]
pub struct InlineImageJobs {
    in_flight: HashSet<ImageKey>,
}

impl InlineImageJobs {
    /// Claim `key` for a decode: false when it already runs or the pool is full
    /// (the cell keeps its stub and asks again on a later draw).
    pub fn start(&mut self, key: &ImageKey) -> bool {
        if self.in_flight.contains(key) || self.in_flight.len() >= MAX_IN_FLIGHT {
            return false;
        }
        self.in_flight.insert(key.clone());
        true
    }

    /// A decode finished; false when nothing was waiting for it.
    pub fn finish(&mut self, key: &ImageKey) -> bool {
        self.in_flight.remove(key)
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

/// Load `key`'s image from the content store and decode it to cells. Blocking:
/// file I/O and image decoding, so it runs on `spawn_blocking`, never in draw.
pub fn decode(key: &ImageKey) -> Result<InlineImage, String> {
    let bytes = pacode_image::store::load(&key.hash).map_err(|e| e.to_string())?;
    pacode_image::render_inline(&bytes, key.cols, key.rows).map_err(|e| e.to_string())
}
