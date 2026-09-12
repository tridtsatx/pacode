//! §5.10's second stack level: refs collected from `CommitRow.refs` —
//! locals and `origin/…` remotes, deduped, the checked-out branch first with
//! the live mark. No branch-list request exists, so the listing is exactly
//! what the loaded history decorated.

use std::path::PathBuf;

use pacode_types::Config;
use pacode_types::vcs::{CommitRow, RefBadge};

use super::*;
use crate::ui::menu::{Mark, RowId};

fn state() -> AppState {
    let mut s = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    s.cwd = PathBuf::from("/repo");
    s
}

fn commit(hash: &str, refs: Vec<RefBadge>) -> CommitRow {
    CommitRow {
        hash: hash.to_string(),
        short: hash.chars().take(7).collect(),
        subject: format!("subject of {hash}"),
        refs,
        ..CommitRow::default()
    }
}

fn labels(view: &MenuView) -> Vec<&str> {
    view.rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Item { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn refs_collect_once_locals_and_remotes() {
    let mut s = state();
    s.git.commits = vec![
        commit(
            "a",
            vec![
                RefBadge::Head,
                RefBadge::Branch("main".into()),
                RefBadge::Remote("origin/main".into()),
                RefBadge::Tag("v1.0".into()), // tags are not branches
            ],
        ),
        commit(
            "b",
            vec![
                RefBadge::Branch("dev".into()),
                RefBadge::Remote("origin/dev".into()),
            ],
        ),
        commit("c", vec![RefBadge::Branch("main".into())]), // moved tip: no dup
    ];
    let v = view(&s, &MenuUiState::default());
    assert_eq!(labels(&v), vec!["main", "origin/main", "dev", "origin/dev"]);
    // `main` is the current branch: live mark, `local` tail.
    match &v.rows[0] {
        MenuRow::Item { mark, right, .. } => {
            assert_eq!(*mark, Mark::None, "no session meta — nothing current");
            assert_eq!(right.as_deref(), Some("local"));
        }
        other => panic!("not an Item: {other:?}"),
    }
    match &v.rows[1] {
        MenuRow::Item { right, .. } => assert_eq!(right.as_deref(), Some("remote")),
        other => panic!("not an Item: {other:?}"),
    }
}

#[test]
fn the_current_branch_leads_with_the_live_mark() {
    let mut s = state();
    s.meta = Some(pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_1"),
        name: None,
        cwd: PathBuf::from("/repo"),
        git_branch: Some("dev".to_string()),
        created_at_ms: 0,
        updated_at_ms: 0,
        model: pacode_types::ModelRoute::new("prov", "model-x"),
        effort: pacode_types::Effort::High,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
        zdr: false,
    });
    s.git.commits = vec![
        commit("a", vec![RefBadge::Branch("main".into())]),
        commit("b", vec![RefBadge::Branch("dev".into())]),
    ];
    let v = view(&s, &MenuUiState::default());
    assert_eq!(labels(&v), vec!["dev", "main"]);
    match &v.rows[0] {
        MenuRow::Item { mark, detail, .. } => {
            assert_eq!(*mark, Mark::Active);
            assert!(
                detail
                    .lines
                    .iter()
                    .any(|l| matches!(l, DetailLine::Field { value, .. } if value.contains("subject of b"))),
                "the tip field names the commit the ref decorates: {detail:?}"
            );
        }
        other => panic!("not an Item: {other:?}"),
    }
}

#[test]
fn an_undecorated_history_is_an_honest_empty_state() {
    let mut s = state();
    s.git.commits = vec![commit("a", vec![])];
    let v = view(&s, &MenuUiState::default());
    assert_eq!(v.empty.primary, "no refs in the loaded history");
}

#[test]
fn enter_is_a_no_op_the_listing_is_read_only() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::GitBranches);
    let mut ui = MenuUiState::default();
    assert!(on_row(&mut s, &mut stack, &mut ui, &RowId::new("ref:dev")).is_empty());
}
