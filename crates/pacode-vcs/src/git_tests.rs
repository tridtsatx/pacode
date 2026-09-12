use std::path::{Path, PathBuf};
use std::process::Command;

use pacode_types::vcs::RefBadge;

use super::*;

/// Run a git command in the fixture repo; tests fail loudly if git is absent.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git binary required for pacode-vcs tests");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A small repo: `root` adds a.txt, `second` adds b.txt; branch `side` (from
/// root) adds c.txt, merged back with --no-ff. Returns (dir, head, merge,
/// second, root).
struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    merge: String,
    second: String,
    root: String,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    git(&dir, &["init", "-b", "main", "."]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "T"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "root commit\n\nwith a body"]);
    let root = git(&dir, &["rev-parse", "HEAD"]).trim().to_string();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "second"]);
    let second = git(&dir, &["rev-parse", "HEAD"]).trim().to_string();
    git(&dir, &["checkout", "-qb", "side", "HEAD~1"]);
    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "side change"]);
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["merge", "--no-ff", "-qm", "merge side", "side"]);
    let merge = git(&dir, &["rev-parse", "HEAD"]).trim().to_string();
    git(&dir, &["tag", "v1"]);
    Fixture {
        _tmp: tmp,
        dir,
        merge,
        second,
        root,
    }
}

#[tokio::test]
async fn log_lists_newest_first_with_parents_and_refs() {
    let fx = fixture();
    let page = log(&fx.dir, None, 10).await.unwrap();
    assert_eq!(page.commits.len(), 4);
    assert!(!page.has_more);

    let head = &page.commits[0];
    assert_eq!(head.hash, fx.merge);
    assert_eq!(head.subject, "merge side");
    assert_eq!(head.parents.len(), 2);
    assert_eq!(head.parents[0], fx.second);
    // HEAD -> refs/heads/main, tag: refs/tags/v1, refs/heads/side also exists.
    assert!(head.refs.contains(&RefBadge::Head));
    assert!(head.refs.contains(&RefBadge::Branch("main".into())));
    assert!(head.refs.contains(&RefBadge::Tag("v1".into())));

    let second = &page.commits[1];
    assert_eq!(second.parents, vec![fx.root.clone()]);

    let root = page.commits.iter().find(|c| c.hash == fx.root).unwrap();
    assert_eq!(root.subject, "root commit");
    assert_eq!(root.body, "with a body");
    assert!(root.parents.is_empty());
    assert_eq!(root.author, "T");
    assert!(root.date_ms > 0);
}

#[tokio::test]
async fn log_paginates_by_cursor_without_gaps() {
    let fx = fixture();
    let p1 = log(&fx.dir, None, 2).await.unwrap();
    assert!(p1.has_more);
    assert_eq!(p1.commits.len(), 2);
    let cursor = p1.commits.last().unwrap().hash.clone();

    let p2 = log(&fx.dir, Some(&cursor), 2).await.unwrap();
    assert_eq!(p2.commits.len(), 2);
    // Continuation must not repeat page 1 and must reach the root.
    let all: Vec<String> = p1
        .commits
        .iter()
        .chain(p2.commits.iter())
        .map(|c| c.hash.clone())
        .collect();
    assert_eq!(
        all,
        git(&fx.dir, &["rev-list", "HEAD"])
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    );

    let p3 = log(&fx.dir, Some(&p2.commits.last().unwrap().hash), 2)
        .await
        .unwrap();
    assert!(p3.commits.is_empty());
    assert!(!p3.has_more);
}

#[tokio::test]
async fn not_a_repo_is_a_normal_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let err = log(tmp.path(), None, 10).await.unwrap_err();
    assert_eq!(err, VcsError::NotARepo);
    // show/diff classify the same way.
    let h = "a".repeat(40);
    assert_eq!(
        show_stat(tmp.path(), &h).await.unwrap_err(),
        VcsError::NotARepo
    );
    assert_eq!(
        commit_diff(tmp.path(), &h).await.unwrap_err(),
        VcsError::NotARepo
    );
}

#[tokio::test]
async fn empty_repo_is_an_empty_page() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-b", "main", "."]);
    let page = log(tmp.path(), None, 10).await.unwrap();
    assert!(page.commits.is_empty());
    assert!(!page.has_more);
}

#[tokio::test]
async fn bad_cursor_is_an_empty_page() {
    let fx = fixture();
    let ghost = "b".repeat(40);
    let page = log(&fx.dir, Some(&ghost), 10).await.unwrap();
    assert!(page.commits.is_empty());
}

#[tokio::test]
async fn show_stat_and_diff() {
    let fx = fixture();
    // Merge: stat of the first-parent diff (the branch's c.txt).
    let stat = show_stat(&fx.dir, &fx.merge).await.unwrap();
    assert!(stat.contains("c.txt"), "stat was: {stat}");

    // Root commit has no parent: falls back to `git show`.
    let stat = show_stat(&fx.dir, &fx.root).await.unwrap();
    assert!(stat.contains("a.txt"), "root stat was: {stat}");

    let (patch, truncated) = commit_diff(&fx.dir, &fx.second).await.unwrap();
    assert!(!truncated);
    assert!(patch.contains("+b"), "patch was: {patch}");
}

#[tokio::test]
async fn invalid_hash_is_refused() {
    let fx = fixture();
    let err = show_stat(&fx.dir, "--help").await.unwrap_err();
    assert!(matches!(err, VcsError::InvalidHash(_)));
    let err = commit_diff(&fx.dir, "deadbeef; rm -rf /")
        .await
        .unwrap_err();
    assert!(matches!(err, VcsError::InvalidHash(_)));
}
