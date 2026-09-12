//! Read-only git data for the VCS submenu (spec §5.10).
//!
//! - [`log`] pages `git log` of `HEAD` through the exec layer's bounded
//!   one-shot capture (5 s timeout, output caps), with "not a repo", "no git"
//!   and "no commits" reported as ordinary outcomes via [`VcsError`];
//! - [`lanes`] folds the `(hash, parents)` rows into per-row graph cells;
//! - [`show_stat`] / [`commit_diff`] feed the detail pane and the diff panel.
//!
//! The transport types ([`CommitRow`], [`RefBadge`], [`GitStatus`]) live in
//! `pacode-types` so the protocol does not depend on this crate.

pub mod error;
pub mod git;
pub mod lanes;
mod parse;

pub use error::VcsError;
pub use git::{
    DIFF_CAP_BYTES, GIT_TIMEOUT, LOG_CAP_BYTES, LOG_PAGE_SIZE, LogPage, REVLIST_CAP_BYTES,
    STAT_CAP_BYTES, commit_diff, log, show_stat,
};
pub use lanes::{LaneCell, LaneMark, MAX_LANES, lanes};
pub use pacode_types::vcs::{CommitRow, GitStatus, RefBadge};
