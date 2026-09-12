//! The `Git` level — branch plus the recent-history rows the session knows.
//! The data arrives via `Reply::GitLog` (or the disk snapshot before it, §8.2);
//! `state.lists.git` owns the `loading…`/`offline copy` badge. The dedicated
//! commit/diff submenu arrives with T17.

use pacode_types::time::{format_duration_ms, now_ms};
use pacode_types::vcs::GitStatus;

use crate::state::AppState;

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId};
use super::view::{EmptyState, MenuId, MenuUiState, MenuView, StatusLine};

/// Rows the level shows at most — the dedicated submenu pages deeper (T17).
const MAX_MENU_COMMITS: usize = 50;

/// The branch row plus recent commits, or an honest empty state outside a repo.
pub fn view(state: &AppState, _ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Git);
    view.status = match state.lists.badge(crate::cache::ListKind::GitLog) {
        Some(crate::cache::Badge::Loading) => Some(StatusLine::loading()),
        Some(crate::cache::Badge::OfflineCopy) => Some(StatusLine::offline_copy()),
        None => None,
    };

    let now = now_ms();
    let mut rows = Vec::new();
    if let Some(branch) = state.meta.as_ref().and_then(|m| m.git_branch.as_deref()) {
        rows.push(MenuRow::Item {
            id: RowId::new("branch"),
            mark: Mark::Active,
            label: format!("branch {branch}"),
            right: None,
            detail: Detail::default(),
        });
    }
    for c in state.git.commits.iter().take(MAX_MENU_COMMITS) {
        let age = format_duration_ms(now.saturating_sub(c.date_ms));
        rows.push(MenuRow::Item {
            id: RowId::new(format!("commit:{}", c.hash)),
            mark: Mark::Idle,
            label: format!("{} {}", c.short, c.subject),
            right: Some(age.clone()),
            detail: Detail {
                title: c.short.clone(),
                lines: vec![
                    DetailLine::Text(format!("{} · {}", c.author, age)),
                    DetailLine::Text(c.subject.clone()),
                ],
                ..Detail::default()
            },
        });
    }
    view.rows = rows;

    if view.rows.is_empty() {
        view.empty = match state.git.status {
            Some(GitStatus::NotARepo) => {
                EmptyState::new("not a git repo", "the session cwd is not a checkout")
            }
            Some(GitStatus::GitMissing) => EmptyState::new(
                "git is not installed",
                "the git binary could not be spawned",
            ),
            Some(GitStatus::Ready) | None => {
                if state.lists.git.in_flight {
                    EmptyState::new("loading…", "the first history page is on its way")
                } else {
                    EmptyState::new(
                        "no commits yet",
                        "the history appears after the first commit",
                    )
                }
            }
        };
    }
    view
}
