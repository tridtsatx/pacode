//! Git history state for the VCS submenu (spec §5.10, task T16).
//!
//! Filled by `Reply::GitLog` / `Reply::GitShow` / `Reply::GitDiff`; the menu
//! renderer (T17) reads it. `commits` pages in 200-row chunks keyed by the
//! last row's hash; appended pages are deduped so a shifted history cannot
//! double a row.
//!
//! The detail stat and the next page are want-driven (§8.1): the draw records
//! [`GitState::show_want`] / [`GitState::more_want`], `dispatch_deferred` turns
//! them into `Action::GitShow` / `Action::GitLog` — never a fetch inside the
//! key handler or the draw.

use pacode_render::RenderCacheKey;
use pacode_types::vcs::{CommitRow, GitStatus};

/// Cap on retained rows: paging beyond this drops the oldest page boundary,
/// keeping memory bounded for long browsing sessions.
pub const GIT_COMMITS_CAP: usize = 1000;

/// `git show --stat` result for the detail pane. `stat: None` means the
/// daemon could not produce it — `error` says why; an empty `Some` means the
/// commit touched no files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitShowInfo {
    pub hash: String,
    pub stat: Option<String>,
    pub error: Option<String>,
}

/// `git diff` result for the diff panel.
#[derive(Clone, Debug)]
pub struct GitDiffInfo {
    pub hash: String,
    /// `None` when the request failed — `note` carries the reason the panel
    /// shows instead of an eternal "loading".
    pub patch: Option<String>,
    pub truncated: bool,
    pub note: Option<String>,
    /// `render_diff_ex` output for a width and render-context epoch.
    pub rendered: Option<(RenderCacheKey, Vec<ratatui::text::Line<'static>>)>,
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
    /// The selected commit the Git level's draw recorded — the detail-stat
    /// want `dispatch_deferred` turns into `Action::GitShow`. `None` while
    /// another level (or no menu) is on screen.
    pub show_want: Option<String>,
    /// A `GitShow` request is in flight for this hash. The gate in
    /// `dispatch_deferred` allows at most one outstanding request: the want
    /// moves with the cursor, the request follows once the previous lands.
    pub show_in_flight: Option<String>,
    /// The cursor rests on the last loaded commit — `dispatch_deferred`
    /// turns it into `Action::GitLog { after }` once `has_more` and no
    /// in-flight page confirm it.
    pub more_want: Option<String>,
    /// The `after` of an append that already failed once. While the want
    /// still names it the gate does not refire — a dead daemon would
    /// otherwise busy-retry every frame; moving the cursor off the last row
    /// (or a successful `apply_log`) clears it, so coming back retries.
    pub more_failed: Option<String>,
    /// The `after` the in-flight `GitLog` was sent with — `fail_log` records
    /// it as `more_failed`, because the reply itself names no cursor key.
    pub log_after: Option<String>,
}

impl GitState {
    /// Mark a `GitLog` request as in flight. `after: Some` keeps the loaded
    /// rows for appending; `None` means a fresh first page.
    pub fn begin_log(&mut self, after: Option<&str>) {
        self.loading = true;
        self.loading_more = after.is_some();
        self.log_after = after.map(str::to_string);
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
        // A landed reply proves the channel is alive — a page that failed
        // once is allowed to be wanted again.
        self.more_failed = None;
    }

    /// The `GitLog` request failed (transport error or `Reply::Error`) —
    /// release the loading flags so the badge does not stick. An append also
    /// records the `after` it failed on: the want-gate must not busy-retry
    /// the same page while the cursor rests on the last row (§5.10's
    /// "exactly once").
    pub fn fail_log(&mut self) {
        if self.loading_more {
            // The failed request's `after`, not the current want — the cursor
            // may have moved while it was in flight.
            self.more_failed = self.log_after.clone();
        }
        self.loading = false;
        self.loading_more = false;
        self.log_after = None;
    }

    /// Fold a `Reply::GitShow` into the detail slot.
    pub fn apply_show(&mut self, hash: String, stat: String) {
        self.end_show(&hash);
        self.detail = Some(GitShowInfo {
            hash,
            stat: Some(stat),
            error: None,
        });
    }

    /// The `GitShow` request failed (daemon `Reply::Error` or a transport
    /// error) — record it so the detail pane stops saying "loading" and the
    /// want-gate stops refiring for this hash while it stays selected.
    pub fn fail_show(&mut self, hash: String, error: String) {
        self.end_show(&hash);
        self.detail = Some(GitShowInfo {
            hash,
            stat: None,
            error: Some(error),
        });
    }

    /// Fold a `Reply::GitDiff` into the diff slot.
    pub fn apply_diff(&mut self, hash: String, patch: String, truncated: bool) {
        self.diff = Some(GitDiffInfo {
            hash,
            patch: Some(patch),
            truncated,
            note: None,
            rendered: None,
        });
    }

    /// The `GitDiff` request failed — same contract as [`Self::fail_show`].
    pub fn fail_diff(&mut self, hash: String, error: String) {
        self.diff = Some(GitDiffInfo {
            hash,
            patch: None,
            truncated: false,
            note: Some(error),
            rendered: None,
        });
    }

    /// Release `show_in_flight` when it names this hash — keyed, so a late
    /// reply for a previous want cannot unblock a fetch already replaced.
    fn end_show(&mut self, hash: &str) {
        if self.show_in_flight.as_deref() == Some(hash) {
            self.show_in_flight = None;
        }
    }
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod git_tests;
