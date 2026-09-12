//! The navigation stack of menu levels (§3.5): at most four levels deep, and a
//! level's cursor, scroll, query and tab survive the descent and the return.

use super::model::RowId;
use super::view::{MenuId, MenuMode, MenuUiState};

#[cfg(test)]
#[path = "stack_tests.rs"]
mod stack_tests;

/// Deepest allowed nesting; a fifth level means a broken hierarchy (§3.5).
pub const MAX_DEPTH: usize = 4;

/// One level of the stack: the menu plus the UI state it returns to (§3.5).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuLevel {
    pub id: MenuId,
    pub cursor: RowId,
    pub scroll: usize,
    pub query: String,
    pub tab: usize,
}

impl MenuLevel {
    pub fn new(id: MenuId) -> Self {
        Self {
            id,
            cursor: RowId::default(),
            scroll: 0,
            query: String::new(),
            tab: 0,
        }
    }

    /// Snapshot `ui` into the level before the stack descends.
    pub fn save(&mut self, ui: &MenuUiState) {
        self.cursor = ui.cursor.clone();
        self.scroll = ui.scroll;
        self.query.clone_from(&ui.query);
        self.tab = ui.tab;
    }

    /// Restore the level into a UI state. A level always comes back in `List`
    /// mode: a half-finished edit or capture never survives a descent, and
    /// neither does an armed remote-filter send — `keys_menu::descend`
    /// flushes it before the snapshot is taken — nor a parked status.
    pub fn ui(&self) -> MenuUiState {
        MenuUiState {
            cursor: self.cursor.clone(),
            scroll: self.scroll,
            query: self.query.clone(),
            tab: self.tab,
            mode: MenuMode::List,
            remote_at: None,
            status: None,
        }
    }
}

/// The stack itself (§3.5). `levels[0]` is the root, `levels.last()` the level
/// on screen. An empty stack means "menu closed" — the caller owns `Focus`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuStack {
    levels: Vec<MenuLevel>,
}

impl MenuStack {
    /// An empty stack; the first level arrives via [`Self::push`] or
    /// [`Self::root`].
    pub fn new() -> Self {
        Self::default()
    }

    /// A stack of a single level — the direct entry (`/plugins`, `.`), where
    /// `esc` closes (§3.5).
    pub fn root(id: MenuId) -> Self {
        Self {
            levels: vec![MenuLevel::new(id)],
        }
    }

    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// The level on screen.
    pub fn current(&self) -> Option<&MenuLevel> {
        self.levels.last()
    }

    /// The level on screen, mutably — e.g. to [`MenuLevel::save`] the live UI
    /// state into it before descending.
    pub fn current_mut(&mut self) -> Option<&mut MenuLevel> {
        self.levels.last_mut()
    }

    /// The levels, root first.
    pub fn levels(&self) -> &[MenuLevel] {
        &self.levels
    }

    /// Push a deeper level. Past [`MAX_DEPTH`] this refuses: `debug_assert` in
    /// debug builds, a warning and `false` in release (§3.5).
    pub fn push(&mut self, level: MenuLevel) -> bool {
        if self.levels.len() >= MAX_DEPTH {
            debug_assert!(false, "menu stack deeper than {MAX_DEPTH} levels");
            log::warn!(
                "menu stack full ({MAX_DEPTH} levels); refusing to push {}",
                level.id.slug()
            );
            return false;
        }
        self.levels.push(level);
        true
    }

    /// Pop the level on screen. The last level pops fine — the caller turns
    /// the empty stack into "menu closed" (§3.4). `None` on an empty stack.
    pub fn pop(&mut self) -> Option<MenuLevel> {
        self.levels.pop()
    }
}
