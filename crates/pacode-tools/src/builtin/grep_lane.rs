//! The long-line lane: one lazily started thread runs every search of a file
//! whose lines outgrow the searchers' buffer cap, for all calls in the process.
//!
//! Why a thread and not just a lock: an uncapped searcher's buffer grows to the
//! file's longest line (an 11 MB one-line JSON makes it ~16 MB). Freed memory
//! stays in the malloc arena of the thread that allocated it, and every
//! blocking-pool thread has its own arena, so 16 concurrent greps that each met
//! such a file kept 16 such buffers resident (measured: 260 MB vs 39 MB without
//! those files). On one thread the big buffer comes from one arena, and each
//! search reuses what the previous one freed.
//!
//! Owner: the [`LANE`] singleton. The thread blocks on its queue when idle (no
//! timer, no CPU) and lives as long as the process. If it cannot be started or
//! has died, the search runs on the caller's thread instead.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use super::{FileHits, Query, search_file, searcher_with_limit};

struct Job {
    query: Query,
    path: PathBuf,
    display: String,
    reply: Sender<FileHits>,
}

static LANE: OnceLock<Option<Sender<Job>>> = OnceLock::new();

fn lane() -> Option<&'static Sender<Job>> {
    LANE.get_or_init(|| {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("pacode-grep-long-lines".to_string())
            .spawn(move || {
                for job in queue {
                    let hits = search_uncapped(&job.query, &job.path, &job.display);
                    // The caller may have gone (its call was dropped); nothing to do.
                    let _ = job.reply.send(hits);
                }
            })
            .map_err(|err| log::warn!("grep long-line lane did not start: {err}"))
            .ok()
            .map(|_| jobs)
    })
    .as_ref()
}

/// Search `path` with no line cap on the lane thread, waiting for the result.
pub(super) fn search_long_lines(query: &Query, path: &Path, display: &str) -> FileHits {
    if let Some(lane) = lane() {
        let (reply, result) = mpsc::channel();
        let job = Job {
            query: query.clone(),
            path: path.to_path_buf(),
            display: display.to_string(),
            reply,
        };
        if lane.send(job).is_ok()
            && let Ok(hits) = result.recv()
        {
            return hits;
        }
    }
    search_uncapped(query, path, display)
}

/// A failure here is an IO error; its partial rows stand, as in a plain search.
fn search_uncapped(query: &Query, path: &Path, display: &str) -> FileHits {
    search_file(query, &mut searcher_with_limit(query, None), path, display)
        .unwrap_or_else(|partial| partial)
}
