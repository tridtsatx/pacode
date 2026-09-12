//! Git history state for the VCS submenu (spec §5.10, task T16).
//!
//! Filled by `Reply::GitLog` / `Reply::GitShow` / `Reply::GitDiff`; the menu
//! renderer (T17) reads it. `commits` pages in 200-row chunks keyed by the
//! last row's hash; appended pages are deduped so a shifted history cannot
//! double a row.

use pacode_types::vcs::{CommitRow, GitStatus};

/// Cap on retained rows: paging beyond this drops the oldest page boundary,
/// keeping memory bounded for long browsing sessions.
pub const GIT_COMMITS_CAP: usize = 1000;

/// `git show --stat` result for the detail pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitShowInfo {
    pub hash: String,
    pub stat: String,
}

/// `git diff` result for the diff panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffInfo {
    pub hash: String,
    pub patch: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GitState {
    /// Loaded commit rows, newest first.
    pub commits: Vec<CommitRow>,
    /// The daemon reported more history beyond `commits`.
    pub has_more: bool,
    /// `None` until the first reply lands; `NotARepo`/`GitMissing` are normal
    /// outcomes the menu renders as an empty state.
    pub status: Option<GitStatus>,
    /// A `GitLog` request is in flight.
    pub loading: bool,
    /// The in-flight `GitLog` continues an existing list (append, not
    /// replace). `app.rs` reads it to tell a first-page reply (which rewrites
    /// the cache file) from a page append (which must not).
    pub loading_more: bool,
    /// Last fetched `show --stat`, keyed by hash so a stale reply is ignored.
    pub detail: Option<GitShowInfo>,
    /// Last fetched patch, keyed by hash.
    pub diff: Option<GitDiffInfo>,
}

impl GitState {
    /// Mark a `GitLog` request as in flight. `after: Some` keeps the loaded
    /// rows for appending; `None` means a fresh first page.
    pub fn begin_log(&mut self, after: Option<&str>) {
        self.loading = true;
        self.loading_more = after.is_some();
    }

    /// Fill from the disk snapshot before the first frame (§8.2 cache-first).
    /// Unlike `apply_log` this touches no loading flags — the refresh badge is
    /// owned by `state.lists.git`, and the next real reply still lands through
    /// `apply_log` and replaces `commits` wholesale.
    pub fn apply_cached(&mut self, commits: Vec<CommitRow>, has_more: bool, status: GitStatus) {
        self.commits = commits;
        self.has_more = has_more;
        self.status = Some(status);
    }

    /// Fold a `Reply::GitLog` into the list.
    pub fn apply_log(&mut self, commits: Vec<CommitRow>, has_more: bool, status: GitStatus) {
        self.loading = false;
        self.status = Some(status);
        if status != GitStatus::Ready {
            self.commits.clear();
            self.has_more = false;
            self.loading_more = false;
            return;
        }
        if self.loading_more {
            // Owned hashes: the set must not borrow `self.commits` while it
            // grows.
            let mut known: std::collections::HashSet<String> =
                self.commits.iter().map(|c| c.hash.clone()).collect();
            for row in commits {
                if known.insert(row.hash.clone()) {
                    self.commits.push(row);
                }
            }
            self.loading_more = false;
        } else {
            self.commits = commits;
        }
        self.has_more = has_more;
        if self.commits.len() > GIT_COMMITS_CAP {
            self.commits.truncate(GIT_COMMITS_CAP);
            self.has_more = false;
        }
    }

    /// Fold a `Reply::GitShow` into the detail slot.
    pub fn apply_show(&mut self, hash: String, stat: String) {
        self.detail = Some(GitShowInfo { hash, stat });
    }

    /// Fold a `Reply::GitDiff` into the diff slot.
    pub fn apply_diff(&mut self, hash: String, patch: String, truncated: bool) {
        self.diff = Some(GitDiffInfo {
            hash,
            patch,
            truncated,
        });
    }
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod git_tests;
