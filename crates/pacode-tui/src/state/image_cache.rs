//! Decoded inline-image cells of the transcript.
//!
//! * Owner: one per [`crate::state::transcript::Transcript`] (the main dialog and the
//!   agent panel each have their own).
//! * Key: `(image hash, columns, rows)`. A resize changes the key, so a stale entry is
//!   never served; the entries of the old width age out through the LRU.
//! * Cap: [`MAX_ENTRIES`] entries and [`MAX_BYTES`] bytes of cells, least recently
//!   used first. Cleared on terminal resize, `/clear` and snapshot reset.
//! * The cells are plain RGB data, independent of the theme, so a theme change needs
//!   no invalidation (the styled lines are rebuilt from them by the line cache).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use pacode_image::InlineImage;

#[cfg(test)]
#[path = "image_cache_tests.rs"]
mod image_cache_tests;

pub const MAX_ENTRIES: usize = 32;
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    hash: String,
    cols: u16,
    rows: u16,
}

#[derive(Debug)]
pub struct ImageCellCache {
    /// `[ui] images` is not `off`.
    pub enabled: bool,
    /// Whether the terminal takes 24-bit colour (else cells fold to ANSI-16).
    pub truecolor: bool,
    entries: HashMap<Key, Arc<InlineImage>>,
    lru: VecDeque<Key>,
    bytes: usize,
}

impl Default for ImageCellCache {
    fn default() -> Self {
        Self {
            enabled: true,
            truecolor: true,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            bytes: 0,
        }
    }
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
    }

    /// The cells for `(hash, cols, rows)`, rendering them with `render` on a miss.
    /// A failed render is not cached.
    pub fn get_or_render<E>(
        &mut self,
        hash: &str,
        cols: u16,
        rows: u16,
        render: impl FnOnce() -> Result<InlineImage, E>,
    ) -> Result<Arc<InlineImage>, E> {
        let key = Key {
            hash: hash.to_string(),
            cols,
            rows,
        };
        if let Some(found) = self.entries.get(&key) {
            let found = Arc::clone(found);
            if let Some(pos) = self.lru.iter().position(|k| *k == key) {
                self.lru.remove(pos);
            }
            self.lru.push_back(key);
            return Ok(found);
        }
        let image = Arc::new(render()?);
        self.bytes += image.approx_bytes();
        self.entries.insert(key.clone(), Arc::clone(&image));
        self.lru.push_back(key);
        while (self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES) && self.lru.len() > 1 {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.approx_bytes());
            }
        }
        Ok(image)
    }
}
