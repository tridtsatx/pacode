//! View-domain data model for the shared centred-submenu base (§3.1):
//! [`MenuView`] — what a submenu hands the base on every frame — the declared
//! [`MenuAction`]s, [`MenuId`], and the base-key table [`is_base_key`] (§3.4).

use crossterm::event::{KeyCode, KeyModifiers};

use crate::binding::{Binding, format_binding};

use super::model::{MenuRow, RowId};

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;

/// The key a [`MenuAction`] is declared on — the crate's existing `Binding`.
pub type KeyBinding = Binding;

/// A submenu's declared action: its key, its hint label, and whether it is
/// currently offered (§3.1). Base-menu keys are not declared — a collision is
/// a bug (§3.4).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuAction {
    pub key: KeyBinding,
    pub label: &'static str,
    pub enabled: bool,
}

impl MenuAction {
    pub fn new(key: KeyBinding, label: &'static str) -> Self {
        Self {
            key,
            label,
            enabled: true,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// The actions available for one row, drawn as chips in its detail card.
pub type ActionChip = MenuAction;

/// Every submenu the base can host. `MenuId` replaces the legacy `focus_key`
/// and `overlay_name` tables: names and identifiers derive from it (§3.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MenuId {
    /// The hub itself (§4).
    #[default]
    Root,
    Settings,
    Plugins,
    Mcp,
    Keys,
    Files,
    Sessions,
    Import,
    Help,
    Background,
    Git,
    /// Plugin-shipped skill dirs — a hub section without a legacy overlay.
    Skills,
}

impl MenuId {
    /// All variants — the completeness check for derived tables.
    pub const ALL: [MenuId; 12] = [
        Self::Root,
        Self::Settings,
        Self::Plugins,
        Self::Mcp,
        Self::Keys,
        Self::Files,
        Self::Sessions,
        Self::Import,
        Self::Help,
        Self::Background,
        Self::Git,
        Self::Skills,
    ];

    /// The human name — the breadcrumb segment and the title (§3.2).
    pub fn title(&self) -> &'static str {
        match self {
            Self::Root => "Menu",
            Self::Settings => "Settings",
            Self::Plugins => "Plugins",
            Self::Mcp => "MCP",
            Self::Keys => "Keys",
            Self::Files => "Files",
            Self::Sessions => "Sessions",
            Self::Import => "Import",
            Self::Help => "Help",
            Self::Background => "Background",
            Self::Git => "Git",
            Self::Skills => "Skills",
        }
    }

    /// The stable lowercase identifier — what `overlay_name` used to hand out
    /// (§3.1), and what log lines and state dumps carry.
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Settings => "settings",
            Self::Plugins => "plugins",
            Self::Mcp => "mcp",
            Self::Keys => "keys",
            Self::Files => "files",
            Self::Sessions => "sessions",
            Self::Import => "import",
            Self::Help => "help",
            Self::Background => "background",
            Self::Git => "git",
            Self::Skills => "skills",
        }
    }

    /// A stable numeric key for `Focus`-style dispatch — the replacement for
    /// the `focus_key` match. The legacy `Focus`/`Overlay` tables occupy
    /// 0..=24, so menu ids start at 25.
    pub fn focus_key(&self) -> u8 {
        match self {
            Self::Root => 25,
            Self::Settings => 26,
            Self::Plugins => 27,
            Self::Mcp => 28,
            Self::Keys => 29,
            Self::Files => 30,
            Self::Sessions => 31,
            Self::Import => 32,
            Self::Help => 33,
            Self::Background => 34,
            Self::Git => 35,
            Self::Skills => 36,
        }
    }
}

/// One peer list in the tab strip: label and the count badge (§3.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub label: String,
    pub count: usize,
}

/// The tab strip. It draws only when there are at least two peer lists —
/// `layout::shows_tabs` owns that rule (§3.2).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tabs {
    pub items: Vec<Tab>,
}

/// How the filter line behaves (§3.2).
#[derive(Clone, Debug, PartialEq)]
pub enum FilterMode {
    /// Printable keys are row actions, not query input.
    Off,
    /// The query filters the rows already in `view.rows`.
    Local,
    /// The query is debounced and sent to the daemon (`debounce_ms`, §3.2).
    Remote { debounce_ms: u64 },
}

/// Tone of the single status line (§3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Loading,
    Warn,
    Error,
}

/// The one-line status above the hint edge — it disappears with its cause
/// (§3.2).
#[derive(Clone, Debug, PartialEq)]
pub struct StatusLine {
    pub kind: StatusKind,
    pub text: String,
}

impl StatusLine {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Info,
            text: text.into(),
        }
    }

    /// "loading" — the renderer appends the ellipsis glyph.
    pub fn loading() -> Self {
        Self {
            kind: StatusKind::Loading,
            text: "loading".to_string(),
        }
    }

    /// "offline copy" — stale data shown while the daemon is unreachable (§5).
    pub fn offline_copy() -> Self {
        Self {
            kind: StatusKind::Warn,
            text: "offline copy".to_string(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Error,
            text: text.into(),
        }
    }
}

/// What an empty list says: what is missing and what to do about it (§3.1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmptyState {
    pub primary: String,
    pub hint: String,
}

impl EmptyState {
    pub fn new(primary: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            hint: hint.into(),
        }
    }
}

/// The interaction mode the current level is in (§3.4, §5.4).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MenuMode {
    /// Browsing: keys are cursor movement, tabs, declared actions.
    #[default]
    List,
    /// In-place edit of a `Value` row: the live buffer and its cursor
    /// (a char index into `buffer`).
    Edit { buffer: String, cursor: usize },
    /// Key capture (Keys menu): the whole list hides behind a "press a key"
    /// prompt; `action` is the action being rebound. `pending` holds a
    /// captured chord that hit a conflict — pressing the same chord again
    /// confirms the takeover (§5.4).
    Capture {
        action: &'static str,
        pending: Option<Binding>,
    },
}

/// The per-level UI state that lives between frames — cursor by [`RowId`],
/// the scroll-window start, the filter query, the active tab and the mode
/// (§3.5). This plus the stack is everything the base keeps (§3.6).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuUiState {
    /// The selected row's id; empty means "first selectable row".
    pub cursor: RowId,
    /// First visible row — a row index, not a line offset.
    pub scroll: usize,
    /// The filter query while `filter` is `Local`/`Remote`.
    pub query: String,
    /// Active tab index.
    pub tab: usize,
    pub mode: MenuMode,
    /// `FilterMode::Remote` debounce: while `Some`, the current level owes
    /// the daemon a query send at this instant (§3.2). The event loop arms a
    /// sleep on it; `keys_menu::remote_filter_due` is the fire.
    pub remote_at: Option<std::time::Instant>,
    /// The status line a key handler parked for the next frame (§3.2) — the
    /// view is rebuilt every frame, so a commit failure's validation error
    /// has to live here to reach the renderer. It disappears with its cause:
    /// the next edit keystroke, `esc`, a successful commit or an action.
    pub status: Option<StatusLine>,
}

/// What a submenu hands the base on every frame (§3.1). The provider is a
/// free function `fn view(state: &AppState) -> MenuView` — no trait.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuView {
    pub id: MenuId,
    /// The title of this level — `id.title()` unless the submenu overrides.
    pub title: &'static str,
    /// Intermediate breadcrumb segments between `Menu` and `title` — empty on
    /// the first stack level, parent titles deeper down (§3.2).
    pub breadcrumb: Vec<String>,
    pub tabs: Option<Tabs>,
    /// `Off | Local | Remote { debounce_ms }`; `None` draws no filter line.
    pub filter: Option<FilterMode>,
    /// Total items before filtering — the `M` of the `N of M` counter.
    pub filter_total: Option<usize>,
    pub rows: Vec<MenuRow>,
    /// Loading / offline copy / edit error (§3.2).
    pub status: Option<StatusLine>,
    /// Declared actions feeding the hint edge (§3.2).
    pub actions: Vec<MenuAction>,
    /// "what is missing" + "what to do" when `rows` is empty.
    pub empty: EmptyState,
}

impl MenuView {
    pub fn new(id: MenuId) -> Self {
        Self {
            id,
            title: id.title(),
            ..Self::default()
        }
    }

    /// Declare the menu's actions (§3.4). A base-key collision is a bug:
    /// `debug_assert` in debug builds; in release the action is dropped and
    /// warned about — it never reaches the hint or the dispatch.
    pub fn with_actions(mut self, actions: Vec<MenuAction>) -> Self {
        let conflicts: Vec<String> = actions.iter().filter_map(conflict_desc).collect();
        debug_assert!(
            conflicts.is_empty(),
            "menu '{}' declares base keys: {conflicts:?}",
            self.id.slug()
        );
        if !conflicts.is_empty() {
            log::warn!(
                "menu '{}' ignores declared actions on base keys: {conflicts:?}",
                self.id.slug()
            );
        }
        self.actions = actions
            .into_iter()
            .filter(|a| !is_base_key(&a.key))
            .collect();
        self
    }

    /// `FilterMode::Local` matching (§3.2): keep selectable rows whose label
    /// or right-hand text contains `query` case-insensitively; a `Section`
    /// survives only while a kept row sits under it. `filter_total` becomes
    /// the pre-filter count — the `M` of the `N of M` counter.
    pub fn apply_filter(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        let total = super::model::selectable(&self.rows).count();
        let q = query.to_lowercase();
        let mut kept: Vec<MenuRow> = Vec::with_capacity(self.rows.len());
        for row in self.rows.drain(..) {
            if matches!(row, MenuRow::Section { .. }) || row.matches(&q) {
                kept.push(row);
            }
        }
        // Reap the headers nothing survived under.
        let mut out = Vec::with_capacity(kept.len());
        for (i, row) in kept.iter().enumerate() {
            if matches!(row, MenuRow::Section { .. })
                && kept[i + 1..]
                    .iter()
                    .take_while(|r| !matches!(r, MenuRow::Section { .. }))
                    .next()
                    .is_none()
            {
                continue;
            }
            out.push(row.clone());
        }
        self.rows = out;
        self.filter_total = Some(total);
    }

    /// Descriptions of declared actions — menu-level and row chips — sitting
    /// on base keys. Empty means clean; the renderer asserts and warns on it
    /// (§3.4).
    pub fn base_key_conflicts(&self) -> Vec<String> {
        let mut out: Vec<String> = self.actions.iter().filter_map(conflict_desc).collect();
        for row in &self.rows {
            if let Some(detail) = row.detail() {
                out.extend(detail.actions.iter().filter_map(conflict_desc));
            }
        }
        out
    }
}

/// `Some("…")` describing the collision, `None` when the action is clean.
fn conflict_desc(action: &MenuAction) -> Option<String> {
    is_base_key(&action.key).then(|| {
        format!(
            "action '{}' on base key {}",
            action.label,
            format_binding(&action.key)
        )
    })
}

/// The keys the base owns (§3.4): navigation, paging, tabs, `enter`, `esc`,
/// `ctrl+c`, `?`. Both spellings of shift+tab (`BackTab` and `Tab`+SHIFT) and
/// both of `?` are covered — terminals disagree on which they send.
const BASE_KEYS: [Binding; 17] = [
    b(KeyCode::Up),
    b(KeyCode::Down),
    b(KeyCode::Left),
    b(KeyCode::Right),
    c(KeyCode::Char('p')),
    c(KeyCode::Char('n')),
    b(KeyCode::PageUp),
    b(KeyCode::PageDown),
    b(KeyCode::Home),
    b(KeyCode::End),
    b(KeyCode::Tab),
    b(KeyCode::BackTab),
    Binding {
        code: KeyCode::Tab,
        mods: KeyModifiers::SHIFT,
    },
    b(KeyCode::Enter),
    b(KeyCode::Esc),
    c(KeyCode::Char('c')),
    b(KeyCode::Char('?')),
];

/// Plain binding — no modifiers.
const fn b(code: KeyCode) -> Binding {
    Binding {
        code,
        mods: KeyModifiers::NONE,
    }
}

/// Ctrl+binding.
const fn c(code: KeyCode) -> Binding {
    Binding {
        code,
        mods: KeyModifiers::CONTROL,
    }
}

/// Whether `key` belongs to the base (§3.4). `?` keeps working when the
/// terminal reports it with a SHIFT modifier.
pub fn is_base_key(key: &Binding) -> bool {
    BASE_KEYS.contains(key) || (key.code == KeyCode::Char('?') && key.mods == KeyModifiers::SHIFT)
}
