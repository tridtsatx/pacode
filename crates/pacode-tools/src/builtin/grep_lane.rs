//! The long-line lane: one lazily started thread runs every search of a file
//! whose lines outgrow the searchers' buffer cap, for all calls in the process.
//! It is one thread outside the `crate::threads` budget.
//!
//! Why a thread and not just a lock: a searcher's buffer grows to the file's
//! longest line (an 11 MB one-line JSON makes it ~16 MB). Freed memory stays in
//! the malloc arena of the thread that allocated it, and every blocking-pool
//! thread has its own arena, so 16 concurrent greps that each met such a file
//! kept 16 such buffers resident (measured: 260 MB vs 39 MB without those
//! files). On one thread the big buffer comes from one arena.
//!
//! Bounds and who frees: the lane's buffer is capped at [`LANE_LINE_LIMIT`]; a
//! file with a longer line is skipped and the grep output names it. Freed
//! buffers otherwise stay in the lane's arena; whenever its queue drains, the
//! lane returns them to the OS (`malloc_trim` on glibc), so the daemon does
//! not keep the largest line it ever met resident.
//!
//! Owner: the [`LANE`] singleton. The thread blocks on its queue when idle (no
//! timer, no CPU) and lives as long as the process. If it cannot be started or
//! has died, the search runs on the caller's thread instead.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use super::{FileHits, Query, search_file, searcher_with_limit};

/// Longest line buffer the lane grows to. Lines past it (a multi-hundred-MB
/// one-line dump) are not searched: holding them would cost that much memory.
/// Tests use a small cap so the skip path runs on a small file.
#[cfg(not(test))]
pub(super) const LANE_LINE_LIMIT: usize = 64 * 1024 * 1024;
#[cfg(test)]
pub(super) const LANE_LINE_LIMIT: usize = 4 * 1024 * 1024;

/// A lane search: the rows found, and whether the file was cut short because
/// a line is longer than [`LANE_LINE_LIMIT`].
pub(super) struct LaneResult {
    pub hits: FileHits,
    pub too_long: bool,
}

struct Job {
    query: Query,
    path: PathBuf,
    display: String,
    reply: Sender<LaneResult>,
}

static LANE: OnceLock<Option<Sender<Job>>> = OnceLock::new();

fn lane() -> Option<&'static Sender<Job>> {
    LANE.get_or_init(|| {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("pacode-grep-long-lines".to_string())
            .spawn(move || {
                let mut next = queue.recv().ok();
                while let Some(job) = next {
                    let result = search_at_lane_limit(&job.query, &job.path, &job.display);
                    // The caller may have gone (its call was dropped); nothing to do.
                    let _ = job.reply.send(result);
                    // Trim once the queue is drained, after the reply: a burst
                    // of jobs pays for one trim, and no caller waits on it.
                    next = match queue.try_recv() {
                        Ok(job) => Some(job),
                        Err(mpsc::TryRecvError::Empty) => {
                            release_freed_memory();
                            queue.recv().ok()
                        }
                        Err(mpsc::TryRecvError::Disconnected) => None,
                    };
                }
            })
            .map_err(|err| log::warn!("grep long-line lane did not start: {err}"))
            .ok()
            .map(|_| jobs)
    })
    .as_ref()
}

/// Search `path` on the lane thread, waiting for the result.
pub(super) fn search_long_lines(query: &Query, path: &Path, display: &str) -> LaneResult {
    if let Some(lane) = lane() {
        let (reply, result) = mpsc::channel();
        let job = Job {
            query: query.clone(),
            path: path.to_path_buf(),
            display: display.to_string(),
            reply,
        };
        if lane.send(job).is_ok()
            && let Ok(result) = result.recv()
        {
            return result;
        }
    }
    let result = search_at_lane_limit(query, path, display);
    release_freed_memory();
    result
}

/// Any other failure is an IO error; its partial rows stand, as in a plain
/// search. The searcher and its buffer are dropped on return.
fn search_at_lane_limit(query: &Query, path: &Path, display: &str) -> LaneResult {
    let mut searcher = searcher_with_limit(query, Some(LANE_LINE_LIMIT));
    match search_file(query, &mut searcher, path, display) {
        Ok(hits) => LaneResult {
            hits,
            too_long: false,
        },
        Err((hits, err)) => LaneResult {
            hits,
            too_long: is_heap_limit_error(&err),
        },
    }
}

/// HACK: grep-searcher reports its heap limit only as an `io::Error` of kind
/// `Other` with the text "configured allocation limit (N) exceeded" (no typed
/// error); matched on that text, pinned by
/// `a_line_over_the_lane_cap_is_skipped_and_reported`. Remove when the crate
/// exposes a typed error.
fn is_heap_limit_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::Other && err.to_string().contains("allocation limit")
}

/// Every lane job grew a buffer past the normal 1 MiB cap; return the freed
/// pages to the OS instead of leaving them in the arena for the daemon's life.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release_freed_memory() {
    // SAFETY: `malloc_trim` only releases free heap memory back to the OS; it
    // takes no pointers and is thread-safe in glibc. The return value (whether
    // anything was released) needs no handling.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn release_freed_memory() {}
