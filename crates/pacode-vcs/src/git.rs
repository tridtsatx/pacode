//! Running `git` through the exec layer's one-shot capture and classifying the
//! outcomes the UI treats as normal (no repo, no git, no commits).
//!
//! Pagination: `git log --skip=N` is the only continuation that exactly matches
//! the first walk's ordering — a cursor-based `<hash>` walk silently drops
//! commits on parallel lines dated older than the cursor. The wire cursor is a
//! hash, so [`log`] first resolves it to a walk offset via `git rev-list HEAD`
//! (one extra bounded call per page flip), then pages with `--skip`.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
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
/// `git status` stdout cap for the touched-file scan — porcelain is a few
/// bytes per path, so this covers absurdly long lists.
pub const STATUS_CAP_BYTES: usize = 256 * 1024;

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

// ─── Worktree status & per-file diffs (§5.5) ────────────────────────────────

/// What `git status` says about one touched path — the `new` / `deleted` /
/// `renamed` labels the Files level shows instead of diff numbers (§5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    /// `??` or `A` — HEAD has never seen the file.
    New,
    /// `D` in either column — gone from the worktree (or staged away).
    Deleted,
    /// `R` — git detected a rename; the map key is the *new* name.
    Renamed,
    /// Anything else dirty (`M`, `T`, `U`, `C`…).
    Modified,
}

/// The `enter` probe behind the file-diff panel: the worktree-vs-`HEAD` patch
/// plus the path's status. Nothing here is an `Err` — an absent patch has an
/// ordinary reason carried in `note` (not a repo, no git, timed out), while a
/// `None` patch with `note: None` simply means "no changes".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    /// The unified patch — `None` when there is nothing to show.
    pub patch: Option<String>,
    /// `git status`'s verdict for the path; `None` outside a worktree or
    /// without git.
    pub status: Option<FileStatus>,
    /// The patch hit [`DIFF_CAP_BYTES`].
    pub truncated: bool,
    /// Context for the header: why the patch is missing, or where it came
    /// from when it is synthetic ("outside the worktree").
    pub note: Option<String>,
}

/// `git status --porcelain=v1 -z` scoped to `paths` — the touched-file rows.
/// Porcelain output is *repo-root*-relative even from a subdirectory, so the
/// map cannot be keyed by it directly: each reported path is matched back to
/// the input spelling through canonicalized absolutes, and the result is
/// keyed by `paths` exactly as given — callers look their own string up.
/// Paths resolving outside the worktree are skipped — one bad pathspec must
/// not fail the whole scan.
pub async fn worktree_statuses(
    cwd: &Path,
    paths: &[String],
) -> Result<BTreeMap<String, FileStatus>, VcsError> {
    let root = worktree_root(cwd).await?;
    statuses_in(cwd, &root, paths).await
}

/// The scan with the root already known — `file_diff` asks for one path and
/// needs the root itself for its in-worktree gate, so it must not pay a
/// second `rev-parse`.
async fn statuses_in(
    cwd: &Path,
    root: &Path,
    paths: &[String],
) -> Result<BTreeMap<String, FileStatus>, VcsError> {
    let cwd_n = std::fs::canonicalize(cwd).unwrap_or_else(|_| normalize_path(cwd));
    // abs → every input spelling resolving to it. Absolute pathspecs are the
    // unambiguous form git accepts from any cwd depth; spellings ride along
    // so reported root-relative paths land back on the row's own string.
    let mut by_abs: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for p in paths {
        if p.is_empty() {
            continue;
        }
        let abs = abs_for(&cwd_n, p);
        if abs.strip_prefix(root).is_ok() {
            by_abs.entry(abs).or_default().push(p.clone());
        }
    }
    if by_abs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut args: Vec<String> = vec![
        "status".to_string(),
        "--porcelain=v1".to_string(),
        "-z".to_string(),
        "--".to_string(),
    ];
    args.extend(by_abs.keys().map(|a| a.to_string_lossy().into_owned()));
    let out = run_git(&args, cwd, STATUS_CAP_BYTES).await?;
    let mut map = BTreeMap::new();
    for (rel, status) in parse_status_z(&out.stdout) {
        let abs = normalize_path(&root.join(rel));
        for spelling in by_abs.get(&abs).into_iter().flatten() {
            map.insert(spelling.clone(), status);
        }
    }
    Ok(map)
}

/// `git rev-parse --show-toplevel`, canonicalized into the same physical
/// space [`std::fs::canonicalize`] puts a cwd in — without it a symlinked
/// `/tmp` (macOS `/var`) makes every `strip_prefix` miss.
async fn worktree_root(cwd: &Path) -> Result<PathBuf, VcsError> {
    let out = run_git(
        &["rev-parse".to_string(), "--show-toplevel".to_string()],
        cwd,
        4096,
    )
    .await?;
    let reported = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Ok(std::fs::canonicalize(&reported).unwrap_or(reported))
}

/// `git ls-files --error-unmatch <abs>` — is the path in the index. The
/// tiebreaker for an empty `diff HEAD` when the status scan could not label
/// the file; read-only, so it still answers under an index lock.
async fn index_tracks(cwd: &Path, abs: &Path) -> bool {
    run_git(
        &[
            "ls-files".to_string(),
            "--error-unmatch".to_string(),
            "--".to_string(),
            abs.to_string_lossy().into_owned(),
        ],
        cwd,
        1024,
    )
    .await
    .is_ok()
}

/// `path` resolved against `cwd_n` and `.`/`..`-folded — the absolute form
/// both the worktree gate and the pathspec table key on.
fn abs_for(cwd_n: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        normalize_path(p)
    } else {
        normalize_path(&cwd_n.join(p))
    }
}

/// The Files level's `enter` probe. `git diff HEAD -- <abs>` covers modified,
/// deleted and renamed tracked files; for paths `HEAD` cannot name —
/// untracked (`New`), an unborn `HEAD`, no repo at all, outside the worktree
/// — the fallback is `git diff --no-index /dev/null <abs>`, the honest
/// "everything added" patch. Errors fold into [`FileDiff::note`]: the panel
/// always has something to say.
pub async fn file_diff(cwd: &Path, raw_path: &str) -> FileDiff {
    let cwd_n = std::fs::canonicalize(cwd).unwrap_or_else(|_| normalize_path(cwd));
    let abs = abs_for(&cwd_n, raw_path);
    let root = worktree_root(cwd).await.ok();
    let in_tree = root.as_ref().is_some_and(|r| abs.strip_prefix(r).is_ok());

    // The status letter pair decides the label; a scan failure only means the
    // label falls back to the tool's own diff stats.
    let status = match &root {
        Some(r) => statuses_in(cwd, r, &[raw_path.to_string()])
            .await
            .ok()
            .and_then(|m| m.get(raw_path).copied()),
        None => None,
    };

    let mut out = FileDiff {
        status,
        ..FileDiff::default()
    };
    // `HEAD` cannot name the file: outside the worktree, untracked (`New` —
    // `git diff HEAD` on `??` is empty by definition), unborn HEAD, no repo.
    // Only then is the `--no-index` add-all patch honest.
    let mut head_blind = !in_tree || matches!(status, Some(FileStatus::New));
    if !in_tree {
        out.note = Some(
            if root.is_some() {
                "outside the worktree"
            } else {
                "not a git worktree"
            }
            .to_string(),
        );
    }

    if in_tree {
        match run_git(
            &[
                "diff".to_string(),
                "HEAD".to_string(),
                "--".to_string(),
                abs.to_string_lossy().into_owned(),
            ],
            cwd,
            DIFF_CAP_BYTES,
        )
        .await
        {
            Ok(cap) => {
                let text = String::from_utf8_lossy(&cap.stdout).into_owned();
                if !text.trim().is_empty() {
                    out.patch = Some(text);
                    out.truncated = cap.stdout_truncated;
                    return out;
                }
                // Empty output: clean tracked file — or untracked, which HEAD
                // cannot see. The status usually decides, but the scan can
                // fail where the diff succeeds (`git status` refreshes the
                // index — a lock fails it while `diff`/`ls-files` still
                // read): then `ls-files` is the tiebreaker.
                if status.is_none() {
                    head_blind = !index_tracks(cwd, &abs).await;
                }
            }
            // Unborn HEAD — everything is new; fall through to `--no-index`.
            Err(VcsError::EmptyRepo) => head_blind = true,
            Err(e @ (VcsError::NotARepo | VcsError::GitMissing)) => {
                head_blind = true;
                if out.note.is_none() {
                    out.note = Some(e.to_string());
                }
            }
            Err(e) => {
                out.note = Some(e.to_string());
                return out;
            }
        }
    }

    // Synthetic add-all patch when HEAD has no name for the file. The one
    // sync `metadata` stat runs on the caller's task — a µs syscall on a
    // background task, never on the draw path (§8.1).
    let exists = std::fs::metadata(&abs).is_ok_and(|m| m.is_file());
    if head_blind && exists {
        let nul = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let args = vec![
            "diff".to_string(),
            "--no-index".to_string(),
            "--".to_string(),
            nul.to_string(),
            abs.to_string_lossy().into_owned(),
        ];
        // `run_capture` directly: `--no-index` exits 1 when the sides differ —
        // that exit code *is* the patch, not a failure `run_git` may classify.
        if let Ok(cap) = run_capture("git", &args, cwd, GIT_TIMEOUT, DIFF_CAP_BYTES).await
            && matches!(cap.status, Some(0) | Some(1))
        {
            let text = String::from_utf8_lossy(&cap.stdout).into_owned();
            if !text.trim().is_empty() {
                out.patch = Some(text);
                out.truncated = cap.stdout_truncated;
            }
        }
    }
    out
}

/// Lexical `.`/`..` folding — a pathspec is a name, so no FS touch and no
/// symlink resolution (git does that internally).
fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `--porcelain=v1 -z` output: `XY <path>\0` records; a rename or copy record
/// carries a second NUL-terminated field — under `-z` the order is the *new*
/// name first, then the source.
fn parse_status_z(out: &[u8]) -> BTreeMap<String, FileStatus> {
    let mut map = BTreeMap::new();
    let mut it = out.split(|b| *b == 0);
    while let Some(rec) = it.next() {
        // `XY ` is three bytes; the trailing empty field and shorts are junk.
        if rec.len() <= 3 {
            continue;
        }
        let (x, y) = (rec[0], rec[1]);
        let path = String::from_utf8_lossy(&rec[3..]).into_owned();
        if x == b'R' || y == b'R' || x == b'C' || y == b'C' {
            // Consume the source-name field — the map key is the new name.
            it.next();
        }
        map.insert(path, classify_xy(x, y));
    }
    map
}

/// `XY` letter pair → label. Gone beats everything; `??`/`A` means "not in
/// `HEAD`"; `R` is a rename; the rest is ordinary modification.
fn classify_xy(x: u8, y: u8) -> FileStatus {
    if x == b'D' || y == b'D' {
        FileStatus::Deleted
    } else if x == b'?' || x == b'A' || y == b'A' {
        FileStatus::New
    } else if x == b'R' || y == b'R' {
        FileStatus::Renamed
    } else {
        FileStatus::Modified
    }
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
        // `git diff HEAD` in a non-repo reports the unnameable revision, not
        // the missing repo (`error: Could not access 'HEAD'`) — the same
        // "HEAD names nothing" family as an unborn HEAD.
        || stderr.contains("Could not access 'HEAD'")
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
