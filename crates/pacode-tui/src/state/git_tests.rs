use pacode_types::vcs::CommitRow;

use super::*;

fn row(hash: &str) -> CommitRow {
    CommitRow {
        hash: hash.into(),
        short: hash.chars().take(7).collect(),
        parents: Vec::new(),
        refs: Vec::new(),
        subject: format!("commit {hash}"),
        body: String::new(),
        author: "t".into(),
        date_ms: 0,
    }
}

#[test]
fn first_page_replaces_rows() {
    let mut g = GitState::default();
    g.begin_log(None);
    assert!(g.loading);
    g.apply_log(vec![row("a1"), row("b2")], true, GitStatus::Ready);
    assert!(!g.loading);
    assert_eq!(g.commits.len(), 2);
    assert!(g.has_more);
    assert_eq!(g.status, Some(GitStatus::Ready));
}

#[test]
fn next_page_appends_and_dedupes() {
    let mut g = GitState::default();
    g.begin_log(None);
    g.apply_log(vec![row("a1"), row("b2")], true, GitStatus::Ready);
    g.begin_log(Some("b2"));
    // b2 repeats as the page overlap — it must not double.
    g.apply_log(vec![row("b2"), row("c3")], false, GitStatus::Ready);
    assert_eq!(
        g.commits
            .iter()
            .map(|c| c.hash.as_str())
            .collect::<Vec<_>>(),
        vec!["a1", "b2", "c3"]
    );
    assert!(!g.has_more);
}

#[test]
fn not_a_repo_clears_the_list() {
    let mut g = GitState::default();
    g.begin_log(None);
    g.apply_log(vec![row("a1")], false, GitStatus::Ready);
    g.begin_log(None);
    g.apply_log(Vec::new(), false, GitStatus::NotARepo);
    assert!(g.commits.is_empty());
    assert!(!g.has_more);
    assert_eq!(g.status, Some(GitStatus::NotARepo));
}

#[test]
fn cap_truncates_and_stops_paging() {
    let mut g = GitState::default();
    g.begin_log(None);
    let big: Vec<CommitRow> = (0..GIT_COMMITS_CAP + 10)
        .map(|i| row(&format!("h{i}")))
        .collect();
    g.apply_log(big, true, GitStatus::Ready);
    assert_eq!(g.commits.len(), GIT_COMMITS_CAP);
    assert!(!g.has_more);
}
