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

#[test]
fn status_z_parses_xy_and_rename_pairs() {
    // `M ` staged-modified, ` M` worktree-modified, `??` untracked,
    // `R ` rename (new name first under -z, then the source record).
    let out = b"M  staged.rs\0 M dirty.rs\0?? loose.rs\0R  new.rs\0old.rs\0 D gone.rs\0";
    let map = parse_status_z(out);
    assert_eq!(map["staged.rs"], FileStatus::Modified);
    assert_eq!(map["dirty.rs"], FileStatus::Modified);
    assert_eq!(map["loose.rs"], FileStatus::New);
    assert_eq!(map["new.rs"], FileStatus::Renamed);
    assert_eq!(map["gone.rs"], FileStatus::Deleted);
    // The rename's source record is consumed, not keyed.
    assert!(!map.contains_key("old.rs"));
}

#[test]
fn status_z_gone_beats_renamed() {
    let map = parse_status_z(b"RD gone.rs\0old.rs\0");
    assert_eq!(map["gone.rs"], FileStatus::Deleted);
}

#[tokio::test]
async fn worktree_statuses_labels_new_modified_deleted() {
    let fx = fixture();
    // a.txt: tracked, now modified. b.txt: tracked, deleted. u.txt: untracked.
    std::fs::write(fx.dir.join("a.txt"), "a changed\n").unwrap();
    std::fs::remove_file(fx.dir.join("b.txt")).unwrap();
    std::fs::write(fx.dir.join("u.txt"), "u\n").unwrap();
    let paths = vec![
        "a.txt".to_string(),
        "b.txt".to_string(),
        "u.txt".to_string(),
        "c.txt".to_string(),         // clean tracked — no entry expected
        "../escape.txt".to_string(), // outside the worktree — skipped
    ];
    let map = worktree_statuses(&fx.dir, &paths).await.unwrap();
    assert_eq!(map["a.txt"], FileStatus::Modified);
    assert_eq!(map["b.txt"], FileStatus::Deleted);
    assert_eq!(map["u.txt"], FileStatus::New);
    assert!(!map.contains_key("c.txt"));
    assert!(!map.contains_key("../escape.txt"));
}

#[tokio::test]
async fn worktree_statuses_from_a_subdir_key_on_the_input_spelling() {
    let fx = fixture();
    // Session cwd is a repo subdirectory: git reports root-relative paths,
    // the map still answers to the spellings it was asked about.
    let sub = fx.dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("s.txt"), "s\n").unwrap();
    std::fs::write(fx.dir.join("a.txt"), "a changed\n").unwrap();
    let abs = fx.dir.join("u2.rs");
    std::fs::write(&abs, "u2\n").unwrap();
    let paths = vec![
        "s.txt".to_string(),                // sub/u2.rs's sibling, untracked
        "../a.txt".to_string(),             // climbs back into the worktree
        abs.to_string_lossy().into_owned(), // absolute spelling
        "/etc/passwd".to_string(),          // outside — skipped
    ];
    let map = worktree_statuses(&sub, &paths).await.unwrap();
    assert_eq!(map["s.txt"], FileStatus::New);
    assert_eq!(map["../a.txt"], FileStatus::Modified);
    assert_eq!(map[abs.to_string_lossy().as_ref()], FileStatus::New);
    assert!(!map.contains_key("/etc/passwd"));
}

#[tokio::test]
async fn file_diff_modified_shows_worktree_patch() {
    let fx = fixture();
    // Replace the line — a pure append produces no `-` row to assert on.
    std::fs::write(fx.dir.join("a.txt"), "a changed\n").unwrap();
    let d = file_diff(&fx.dir, "a.txt").await;
    assert_eq!(d.status, Some(FileStatus::Modified));
    let patch = d.patch.unwrap();
    assert!(patch.contains("-a"));
    assert!(patch.contains("+a changed"));
    assert!(!d.truncated);
}

#[tokio::test]
async fn file_diff_from_a_subdir_diffs_against_head() {
    let fx = fixture();
    // cwd inside the repo but not at its root: the path is still tracked, so
    // the patch is the real worktree diff, not a synthetic add-all.
    let sub = fx.dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(fx.dir.join("a.txt"), "a changed\n").unwrap();
    let d = file_diff(&sub, "../a.txt").await;
    assert_eq!(d.status, Some(FileStatus::Modified));
    let patch = d.patch.unwrap();
    assert!(patch.contains("-a"), "patch: {patch}");
    assert!(patch.contains("+a changed"), "patch: {patch}");
}

#[tokio::test]
async fn file_diff_untracked_gets_add_all_patch() {
    let fx = fixture();
    std::fs::write(fx.dir.join("fresh.rs"), "fn main() {}\n").unwrap();
    let d = file_diff(&fx.dir, "fresh.rs").await;
    assert_eq!(d.status, Some(FileStatus::New));
    let patch = d.patch.unwrap();
    assert!(patch.contains("+fn main() {}"), "patch: {patch}");
}

#[tokio::test]
async fn file_diff_deleted_shows_removal() {
    let fx = fixture();
    std::fs::remove_file(fx.dir.join("a.txt")).unwrap();
    let d = file_diff(&fx.dir, "a.txt").await;
    assert_eq!(d.status, Some(FileStatus::Deleted));
    let patch = d.patch.unwrap();
    assert!(patch.contains("-a"), "patch: {patch}");
}

#[tokio::test]
async fn file_diff_clean_file_has_no_patch() {
    let fx = fixture();
    let d = file_diff(&fx.dir, "a.txt").await;
    assert_eq!(d.patch, None);
    assert_eq!(d.note, None);
}

#[tokio::test]
async fn file_diff_not_a_repo_still_patches_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("x.rs"), "let x = 1;\n").unwrap();
    let d = file_diff(tmp.path(), "x.rs").await;
    // No repo: the --no-index fallback still produces the add-all patch.
    assert_eq!(d.status, None);
    assert!(d.patch.is_some(), "expected synthetic patch, got {d:?}");
}
