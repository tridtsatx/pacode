//! The `GitBranches` level (§5.10) — the refs collected from the loaded
//! history, pushed onto the stack by `ctrl+b` on the Git level.
//!
//! Approximation, by design: the wire has no branch-list request — the only
//! source is `CommitRow.refs`, i.e. refs sitting on the ≤
//! [`crate::state::git::GIT_COMMITS_CAP`] commits already loaded. A branch
//! whose tip scrolled past the loaded window is simply absent, and a worktree
//! branch without a loaded commit is too. The listing is read-only: `enter`
//! does nothing — there is no checkout in this version.

use pacode_types::vcs::RefBadge;

use crate::keys::Action;
use crate::state::AppState;

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{EmptyState, FilterMode, MenuId, MenuUiState, MenuView};

#[cfg(test)]
#[path = "git_branches_tests.rs"]
mod git_branches_tests;

/// One collected ref: the name, its kind, and the newest loaded commit it
/// decorated (the tip, unless history moved past it).
struct RefRow {
    name: String,
    remote: bool,
    commit: usize,
}

/// The refs in first-seen (newest-commit-first) order, current branch first.
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::GitBranches);
    view.filter = Some(FilterMode::Local);

    let current = state.meta.as_ref().and_then(|m| m.git_branch.as_deref());
    let mut refs: Vec<RefRow> = Vec::new();
    for (i, commit) in state.git.commits.iter().enumerate() {
        for badge in &commit.refs {
            let (name, remote) = match badge {
                RefBadge::Branch(name) => (name.as_str(), false),
                RefBadge::Remote(name) => (name.as_str(), true),
                RefBadge::Head | RefBadge::Tag(_) => continue,
            };
            if refs.iter().any(|r| r.name == name && r.remote == remote) {
                continue;
            }
            refs.push(RefRow {
                name: name.to_string(),
                remote,
                commit: i,
            });
        }
    }
    // The checked-out branch leads — the stable sort keeps first-seen order
    // (which already mirrors recency) for the rest.
    refs.sort_by_key(|r| usize::from(!(is_current(r, current))));

    view.rows = refs.iter().map(|r| ref_row(state, r, current)).collect();
    // Unconditional like the parent level: the local filter can still drain
    // the list — a dead query then says "no matches", not "no refs" (§3.2).
    view.empty = if ui.query.is_empty() {
        EmptyState::new(
            "no refs in the loaded history",
            "branch and origin/… badges land here once a decorated commit is loaded",
        )
    } else {
        EmptyState::new("no matches", "the filter hides every ref")
    };
    view
}

/// `enter` — nothing: the listing is read-only, no checkout in this version.
pub fn on_row(
    _state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    _row: &RowId,
) -> Vec<Action> {
    vec![]
}

/// A local branch named `current` is the checked-out one — a `origin/x`
/// remote is never "the current branch".
fn is_current(r: &RefRow, current: Option<&str>) -> bool {
    !r.remote && Some(r.name.as_str()) == current
}

fn ref_row(state: &AppState, r: &RefRow, current: Option<&str>) -> MenuRow {
    let current = is_current(r, current);
    let commit = &state.git.commits[r.commit];
    MenuRow::Item {
        id: RowId::new(format!("ref:{}", r.name)),
        mark: if current { Mark::Active } else { Mark::None },
        label: r.name.clone(),
        right: Some(if r.remote { "remote" } else { "local" }.to_string()),
        detail: Detail {
            title: r.name.clone(),
            lines: vec![
                DetailLine::Field {
                    label: "kind".to_string(),
                    value: if r.remote { "remote" } else { "local" }.to_string(),
                },
                DetailLine::Field {
                    label: "tip".to_string(),
                    value: format!("{} {}", commit.short, commit.subject),
                },
                DetailLine::Text("read-only — checkout is not in this version".to_string()),
            ],
            actions: Vec::new(),
            search: vec![r.name.clone()],
        },
    }
}
