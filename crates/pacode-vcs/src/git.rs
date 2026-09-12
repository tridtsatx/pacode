//! Running `git` through the exec layer's one-shot capture and classifying the
//! outcomes the UI treats as normal (no repo, no git, no commits).
//!
//! Pagination: `git log --skip=N` is the only continuation that exactly matches
//! the first walk's ordering — a cursor-based `<hash>` walk silently drops
//! commits on parallel lines dated older than the cursor. The wire cursor is a
//! hash, so [`log`] first resolves it to a walk offset via `git rev-list HEAD`
//! (one extra bounded call per page flip), then pages with `--skip`.

use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use pacode_exec::{CapturedOutput, ExecError, run_capture};
use pacode_types::vcs::CommitRow;

use crate::error::VcsError;
use crate::parse::{LOG_FORMAT, parse_log};

/// Commits per page (spec §5.10: `-n 200`).
pub const LOG_PAGE_SIZE: usize = 200;
/// Hard bound on every git invocation.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(5);
/// `git log` stdout cap.
pub const LOG_CAP_BYTES: usize = 4 * 1024 * 1024;
/// `git rev-list` scan cap; one hash per line ≈ 41 bytes, so this covers
/// ~200k-commit histories before pagination gives up.
pub const REVLIST_CAP_BYTES: usize = 8 * 1024 * 1024;
/// `git show --stat` cap — far more than any reasonable stat block.
pub const STAT_CAP_BYTES: usize = 64 * 1024;
/// Patch cap for the diff panel.
pub const DIFF_CAP_BYTES: usize = 1024 * 1024;

/// One page of the `HEAD` history walk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogPage {
    pub commits: Vec<CommitRow>,
    /// A further page probably exists (the page came back full).
    pub has_more: bool,
}

/// `git log` of `HEAD`, newest first. `after` is the hash of the last commit
/// the caller already holds; `None` fetches the first page.
pub async fn log(cwd: &Path, after: Option<&str>, limit: usize) -> Result<LogPage, VcsError> {
    let skip = match after {
        None => 0,
        Some(hash) => {
            validate_hash(hash)?;
            match walk_offset(cwd, hash).await {
                Ok(Some(offset)) => offset,
                // Cursor not in the HEAD walk (rewritten history): end of
                // list, not an error — the client just stops paging.
                Ok(None) | Err(VcsError::EmptyRepo) => return Ok(LogPage::default()),
                Err(e) => return Err(e),
            }
        }
    };

    let out = run_git(
        &[
            "log".to_string(),
            "HEAD".to_string(),
            "--parents".to_string(),
            "--decorate=full".to_string(),
            format!("--format={LOG_FORMAT}"),
            format!("-n{}", limit + 1),
            format!("--skip={skip}"),
        ],
        cwd,
        LOG_CAP_BYTES,
    )
    .await;

    let out = match out {
        Err(VcsError::EmptyRepo) => return Ok(LogPage::default()),
        other => other?,
    };

    let mut commits = parse_log(&out.stdout);
    let has_more = commits.len() > limit;
    commits.truncate(limit);
    Ok(LogPage { commits, has_more })
}

/// `git show --stat`-equivalent block for the detail pane. For merges and
/// regular commits this is `git diff --stat <hash>^ <hash>` (the first-parent
/// diff, which is also what forges show for a merge); a root commit has no
/// `^`, so it falls back to `git show --stat`.
pub async fn show_stat(cwd: &Path, hash: &str) -> Result<String, VcsError> {
    validate_hash(hash)?;
    let parent = format!("{hash}^");
    let out = match run_git(
        &[
            "diff".to_string(),
            "--stat".to_string(),
            parent,
            hash.to_string(),
        ],
        cwd,
        STAT_CAP_BYTES,
    )
    .await
    {
        Ok(out) => out,
        Err(VcsError::Failed(_)) => {
            run_git(
                &[
                    "show".to_string(),
                    "--stat".to_string(),
                    "--format=".to_string(),
                    hash.to_string(),
                ],
                cwd,
                STAT_CAP_BYTES,
            )
            .await?
        }
        Err(e) => return Err(e),
    };
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The commit's patch for the diff panel: `git diff <hash>^ <hash>`, with the
/// same root-commit fallback as [`show_stat`]. Returns `(patch, truncated)`.
pub async fn commit_diff(cwd: &Path, hash: &str) -> Result<(String, bool), VcsError> {
    validate_hash(hash)?;
    let parent = format!("{hash}^");
    let out = match run_git(
        &["diff".to_string(), parent, hash.to_string()],
        cwd,
        DIFF_CAP_BYTES,
    )
    .await
    {
        Ok(out) => out,
        Err(VcsError::Failed(_)) => {
            run_git(
                &[
                    "show".to_string(),
                    "--format=".to_string(),
                    hash.to_string(),
                ],
                cwd,
                DIFF_CAP_BYTES,
            )
            .await?
        }
        Err(e) => return Err(e),
    };
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.stdout_truncated,
    ))
}

/// Position just past `hash` in the `HEAD` walk — the `--skip` value that
/// continues the log exactly where the previous page ended.
async fn walk_offset(cwd: &Path, hash: &str) -> Result<Option<usize>, VcsError> {
    let out = run_git(
        &["rev-list".to_string(), "HEAD".to_string()],
        cwd,
        REVLIST_CAP_BYTES,
    )
    .await?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().position(|line| line == hash).map(|i| i + 1))
}

/// Spawn `git`, wait, classify. Normal-failure mapping:
/// `not a git repository` → [`VcsError::NotARepo`]; missing `HEAD` / zero
/// commits → [`VcsError::EmptyRepo`]; spawn `ENOENT` → [`VcsError::GitMissing`].
async fn run_git(args: &[String], cwd: &Path, cap: usize) -> Result<CapturedOutput, VcsError> {
    let out = run_capture("git", args, cwd, GIT_TIMEOUT, cap)
        .await
        .map_err(|e| match e {
            ExecError::Spawn(io) if io.kind() == ErrorKind::NotFound => VcsError::GitMissing,
            other => VcsError::Io(other.to_string()),
        })?;
    if out.timed_out {
        return Err(VcsError::Timeout(GIT_TIMEOUT.as_secs()));
    }
    if out.success() {
        return Ok(out);
    }
    Err(classify_failure(&out.stderr_text()))
}

fn classify_failure(stderr: &str) -> VcsError {
    if stderr.contains("not a git repository") {
        VcsError::NotARepo
    } else if stderr.contains("does not have any commits")
        || stderr.contains("bad default revision")
        || stderr.contains("ambiguous argument 'HEAD'")
    {
        VcsError::EmptyRepo
    } else {
        VcsError::Failed(stderr.chars().take(512).collect())
    }
}

/// Refuse anything that is not a lowercase hex commit hash, so a client-supplied
/// string can never become a flag or a pathspec on git's argv.
fn validate_hash(hash: &str) -> Result<(), VcsError> {
    let ok = (4..=64).contains(&hash.len())
        && hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if ok {
        Ok(())
    } else {
        Err(VcsError::InvalidHash(hash.to_string()))
    }
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod git_tests;
