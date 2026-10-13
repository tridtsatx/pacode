//! Reading and parsing the files of a `symbols` scan, a wave of upcoming
//! files at a time on the shared thread budget.
//!
//! The scan's budgets (`max_files`, `max_symbols`) are decided by the caller in
//! path order, consuming one result per file exactly as a serial scan would.
//! Parsing is pure, so it can run ahead of those checks: when no result is
//! ready, the next wave of files the scan may still reach is parsed in parallel
//! and handed back in path order. A wave never goes past `max_files`; what it
//! parsed past a `max_symbols` stop is dropped unread. Waves start at four files
//! and double up to [`WAVE`], so a scan that `max_symbols` ends after three big
//! files parses four, not thirty-two.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use pacode_symbols::{FileSymbols, Language, MAX_PARSE_BYTES, extract};
use tokio_util::sync::CancellationToken;

use super::SkipReason;
use crate::ToolError;

/// What one file produced: its source (kept only when asked for) and outline,
/// or why it has none.
pub(super) type Parsed = Result<(String, FileSymbols), SkipReason>;

/// Files in the first wave; each later wave doubles, up to [`WAVE`].
const FIRST_WAVE: usize = 4;
/// Largest wave: bounds the outlines held before the scan reads them.
const WAVE: usize = 32;
/// Helper threads one wave may ask for (the scanning thread works too).
/// Measured on 100-500 file scans: one helper keeps the resident heap at the
/// serial scan's level for a +6 MB peak; three were another 1.3x faster but
/// left +6 MB resident and +12 MB more peak (every thread's arena keeps parse
/// tree fragments that a trim cannot return).
const MAX_HELPER_THREADS: usize = 1;
/// NUL sniff window — same as `read`.
const SNIFF_BYTES: usize = 8192;

pub(super) struct ParseAhead {
    /// The files the scan may parse, in path order.
    files: Vec<(PathBuf, Language)>,
    /// First file of `files` not yet parsed.
    next: usize,
    /// Size of the next wave.
    wave: usize,
    ready: VecDeque<Parsed>,
    keep_source: bool,
    /// A wave ran on helper threads, whose arenas now hold what they freed.
    fanned_out: bool,
    /// The scanning thread counts against the budget while it parses, so a
    /// concurrent scan or grep leaves it fewer helpers rather than more threads.
    _own: crate::threads::Reservation<'static>,
}

impl ParseAhead {
    /// `keep_source`: hold each file's text for the caller (`context` mode);
    /// otherwise it is dropped as soon as the file is parsed.
    pub(super) fn new(files: Vec<(PathBuf, Language)>, keep_source: bool) -> Self {
        Self {
            files,
            next: 0,
            wave: FIRST_WAVE,
            ready: VecDeque::new(),
            keep_source,
            fanned_out: false,
            _own: crate::threads::shared().occupy(),
        }
    }

    /// The result for the scan's next file. `allowed` is how many more files
    /// the scan may parse, this one included, so a wave stops at `max_files`.
    pub(super) fn next(
        &mut self,
        allowed: usize,
        cancel: &CancellationToken,
    ) -> Result<Parsed, ToolError> {
        if self.ready.is_empty() {
            let end = self
                .files
                .len()
                .min(self.next + allowed.clamp(1, self.wave));
            let wave = &self.files[self.next..end];
            let (parsed, fanned_out) = parse_wave(wave, self.keep_source, cancel)?;
            self.ready = parsed.into();
            self.fanned_out |= fanned_out;
            self.next = end;
            self.wave = (self.wave * 2).min(WAVE);
        }
        self.ready
            .pop_front()
            .ok_or_else(|| ToolError::failed("symbols: more files consumed than queued"))
    }
}

impl Drop for ParseAhead {
    fn drop(&mut self) {
        // Parse trees are freed on the threads that built them; without this
        // the helpers' share of the scan's peak stays resident.
        if self.fanned_out {
            crate::threads::release_freed_memory();
        }
    }
}

/// Parse `wave` on this thread plus whatever helpers the budget grants;
/// results come back in `wave` order, with whether any helper ran.
fn parse_wave(
    wave: &[(PathBuf, Language)],
    keep_source: bool,
    cancel: &CancellationToken,
) -> Result<(Vec<Parsed>, bool), ToolError> {
    let helpers =
        crate::threads::shared().reserve(wave.len().saturating_sub(1).min(MAX_HELPER_THREADS));
    let next = AtomicUsize::new(0);
    let work = || {
        let mut out = Vec::new();
        while !cancel.is_cancelled() {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some((path, lang)) = wave.get(index) else {
                break;
            };
            let parsed = parse_file(path, *lang).map(|(source, symbols)| {
                (if keep_source { source } else { String::new() }, symbols)
            });
            out.push((index, parsed));
        }
        out
    };
    let mut parsed = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..helpers.count()).map(|_| scope.spawn(work)).collect();
        let mut all = work();
        for handle in handles {
            let part = handle
                .join()
                .map_err(|_| ToolError::failed("symbols parse thread panicked"))?;
            all.extend(part);
        }
        Ok::<_, ToolError>(all)
    })?;
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    parsed.sort_unstable_by_key(|(index, _)| *index);
    let parsed = parsed.into_iter().map(|(_, parsed)| parsed).collect();
    Ok((parsed, helpers.count() > 0))
}

/// Read + extract one file; every failure mode maps to a [`SkipReason`] so the
/// file always lands in the `skipped` ledger. Size is checked before reading
/// so an oversized file costs a stat, not a 2 MiB+ read.
pub(super) fn parse_file(path: &Path, lang: Language) -> Parsed {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > MAX_PARSE_BYTES as u64 {
        return Err(SkipReason::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|_| SkipReason::Unreadable)?;
    if bytes[..bytes.len().min(SNIFF_BYTES)].contains(&0) {
        return Err(SkipReason::Binary);
    }
    let source = String::from_utf8(bytes).map_err(|_| SkipReason::NotUtf8)?;
    let symbols =
        extract(&source, lang, Default::default()).map_err(|_| SkipReason::ParseFailed)?;
    Ok((source, symbols))
}

#[cfg(test)]
#[path = "symbols_parse_tests.rs"]
mod symbols_parse_tests;
