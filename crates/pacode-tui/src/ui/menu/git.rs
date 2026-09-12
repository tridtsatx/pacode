//! The `Git` level — the hub row lands here. The real submenu (commits,
//! diffs) arrives with the daemon git-log integration (TODO T17); until then
//! the level reports the one git fact the session knows — the branch.

use crate::state::AppState;

use super::model::{Detail, Mark, MenuRow, RowId};
use super::view::{EmptyState, MenuId, MenuUiState, MenuView};

/// The branch row, or an honest empty state outside a repo.
pub fn view(state: &AppState, _ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Git);
    if let Some(branch) = state.meta.as_ref().and_then(|m| m.git_branch.as_deref()) {
        view.rows = vec![MenuRow::Item {
            id: RowId::new("branch"),
            mark: Mark::Active,
            label: format!("branch {branch}"),
            right: None,
            detail: Detail::default(),
        }];
    } else {
        view.empty = EmptyState::new(
            "not a git repo",
            "commits and diffs land here with the git integration",
        );
    }
    view
}
