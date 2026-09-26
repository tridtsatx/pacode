//! Output a running tool call has produced so far, kept client-side until the
//! call's final transcript item replaces it with the real body.
//!
//! Two caps, and both are needed because the buffer is redrawn from scratch
//! every frame (a running cell is never served from the line cache, so its
//! spinner and its sweep move):
//!
//! * `MAX_TOOL_STREAM_CHARS` per call. This is the same display cap the daemon
//!   puts on a *finished* tool preview (`PREVIEW_CAP_CHARS`), so a live body
//!   never shows more than the body it turns into, and the per-frame cost of
//!   scanning, ANSI-stripping and wrapping the text is bounded by what the
//!   finished cell would cost anyway — a 10 MB `cargo build` log costs 16 000
//!   characters of work per frame, not 10 MB.
//! * `MAX_TOOL_STREAMS` calls. A turn runs a handful of tool calls at once;
//!   past the bound the lowest id (the oldest call) is dropped, so a long
//!   session cannot grow the map without end.
//!
//! Freed by [`ToolStreams::remove`] when a call leaves `Running` or its cell is
//! evicted, and by `clear` on a snapshot reset.

use std::collections::HashMap;

use pacode_types::PREVIEW_CAP_CHARS;

#[cfg(test)]
#[path = "tool_stream_tests.rs"]
mod tool_stream_tests;

/// Characters one running call's live body may hold. Equal to the daemon's
/// display cap for a finished preview: the live body is a preview of the
/// finished one, not a bigger thing.
pub const MAX_TOOL_STREAM_CHARS: usize = PREVIEW_CAP_CHARS;

/// How many running calls may hold a live body at once. Every entry is at
/// most `MAX_TOOL_STREAM_CHARS`, so the map is bounded by the product of the
/// two.
pub const MAX_TOOL_STREAMS: usize = 8;

#[derive(Default)]
struct ToolStream {
    /// The retained tail of the output, oldest byte first.
    text: String,
    /// `text`'s length in characters. Counted as chunks arrive rather than by
    /// scanning `text` per chunk: a tool streams in many small pieces, and a
    /// rescan of the whole window per piece is the cost this module exists to
    /// avoid.
    chars: usize,
    /// Characters that arrived and fell outside `text`.
    dropped: usize,
}

/// The live body of each running tool call, keyed by transcript cell id.
#[derive(Default)]
pub struct ToolStreams {
    entries: HashMap<u64, ToolStream>,
}

/// What a cell's body draws: the retained text, and how much output the cap
/// keeps out of it (shown as a marker — a silently shortened body reads as the
/// whole of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolStreamView<'a> {
    pub text: &'a str,
    pub dropped: usize,
}

impl ToolStreams {
    pub fn get(&self, cell_id: u64) -> Option<ToolStreamView<'_>> {
        self.entries.get(&cell_id).map(|stream| ToolStreamView {
            text: stream.text.as_str(),
            dropped: stream.dropped,
        })
    }

    /// Append what a running call has produced since the last chunk.
    ///
    /// The *tail* is kept, not the head: a live body that kept its oldest
    /// bytes would show lines from minutes ago while the line the reader is
    /// waiting for had already scrolled off the top of the cell.
    pub fn push(&mut self, cell_id: u64, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let stream = self.entries.entry(cell_id).or_default();
        stream.text.push_str(chunk);
        stream.chars = stream.chars.saturating_add(chunk.chars().count());
        stream.trim_to_cap();
        self.prune(cell_id);
    }

    /// Forget a call's live body: it finished, was denied, or its cell is gone.
    pub fn remove(&mut self, cell_id: u64) {
        self.entries.remove(&cell_id);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Ids are monotonic (daemon seqs, or client ids above the tag), so the
    /// lowest one is the oldest call. The call being pushed stays even if it
    /// is the lowest — dropping the chunk just received is the one eviction
    /// that would make the cell flicker.
    fn prune(&mut self, protected: u64) {
        while self.entries.len() > MAX_TOOL_STREAMS {
            let Some(victim) = self
                .entries
                .keys()
                .copied()
                .filter(|cell_id| *cell_id != protected)
                .min()
            else {
                return;
            };
            self.entries.remove(&victim);
        }
    }
}

impl ToolStream {
    fn trim_to_cap(&mut self) {
        if self.chars <= MAX_TOOL_STREAM_CHARS {
            return;
        }
        let excess = self.chars - MAX_TOOL_STREAM_CHARS;
        // `char_indices` lands on a boundary, so the retained window never
        // starts in the middle of a code point.
        let cut = self
            .text
            .char_indices()
            .nth(excess)
            .map_or(self.text.len(), |(at, _)| at);
        self.text.drain(..cut);
        self.chars -= excess;
        self.dropped = self.dropped.saturating_add(excess);
    }
}
