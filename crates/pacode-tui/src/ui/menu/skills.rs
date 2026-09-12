//! The `Skills` level (§4.1): which plugins ship skill dirs. The full skills
//! surface is a later task — this level is honest about the little data there
//! is rather than drawing a placeholder.

use crate::state::AppState;

use super::model::{Detail, Mark, MenuRow, RowId};
use super::view::{EmptyState, MenuId, MenuUiState, MenuView, StatusLine};

/// One row per plugin carrying `skills/` dirs.
pub fn view(state: &AppState, _ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Skills);
    // §8.2: the skills list rides on the plugins list's cache health.
    view.status = match state.lists.badge(crate::cache::ListKind::Plugins) {
        Some(crate::cache::Badge::Loading) => Some(StatusLine::loading()),
        Some(crate::cache::Badge::OfflineCopy) => Some(StatusLine::offline_copy()),
        None => None,
    };
    view.rows = state
        .plugins
        .iter()
        .filter(|p| p.skill_dirs > 0)
        .map(|p| {
            let lines = if p.description.is_empty() {
                Vec::new()
            } else {
                vec![super::model::DetailLine::Text(p.description.clone())]
            };
            MenuRow::Item {
                id: RowId::new(format!("plugin:{}", p.name)),
                mark: if p.loaded { Mark::Ok } else { Mark::Idle },
                label: p.name.clone(),
                right: Some(format!("{} skill dirs", p.skill_dirs)),
                detail: Detail {
                    title: p.name.clone(),
                    lines,
                    ..Detail::default()
                },
            }
        })
        .collect();
    view.empty = EmptyState::new(
        "no skills installed",
        "skills ship with plugins — /plugins to browse the marketplace",
    );
    view
}
