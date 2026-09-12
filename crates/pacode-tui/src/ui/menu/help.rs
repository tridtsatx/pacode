//! The `Help` level — a search-first palette over the app's whole surface
//! (§5.8): slash commands, the submenus, the settings registry and the
//! rebindable key actions. Every entry is an `Item` row with a kind tag in
//! the label (`/cmd`, `menu`, `set`, `key`) and a description on the right.
//!
//! With an empty query the level is the browseable index — one section per
//! kind. With a query it is a flat ranking (§5.8): exact name matches first,
//! then command → submenu → setting → key, and inside a kind the position
//! the query hits at decides. The base still runs its own `Local` filter
//! pass afterwards — the ranker's haystacks are exactly the strings
//! [`MenuRow::matches`] sees (label, right tail, `Detail::search`), so a row
//! the palette emits can never be reaped by that pass.
//!
//! `enter` executes by kind: a command drains the stack and runs like a
//! typed `/x`, a submenu descends onto the stack, a setting lands in
//! `Settings` with the filter preset to its dotted key, a key action lands
//! in `Keys` with the cursor on its row. `esc` peels back to wherever Help
//! was opened from — the stack restore is the base's job.

use pacode_config::registry::{self, SettingEntry};

use crate::binding::{Keymap, format_binding};
use crate::keys::Action;
use crate::state::AppState;

use super::model::{Detail, DetailLine, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{EmptyState, FilterMode, MenuId, MenuUiState, MenuView};

#[cfg(test)]
#[path = "help_tests.rs"]
mod help_tests;

/// Rank tiers inside a query result (§5.8): commands first, key actions
/// last. The declared order *is* the order — `Kind::rank` reads it off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Command,
    Menu,
    Setting,
    Key,
}

impl Kind {
    const fn rank(self) -> u8 {
        match self {
            Self::Command => 0,
            Self::Menu => 1,
            Self::Setting => 2,
            Self::Key => 3,
        }
    }
}

/// One palette entry: the row the base draws, the names an exact query can
/// equal, and every string the query may hit.
struct Entry {
    kind: Kind,
    /// The `Item` row — `id` carries the `cmd:`/`menu:`/`set:`/`key:`
    /// prefix [`on_row`] dispatches on.
    row: MenuRow,
    /// Names equal to the query count as exact (command name and usage, menu
    /// slug and title, the setting's dotted key and label, the action name).
    exacts: Vec<String>,
    /// The same strings `MenuRow::matches` checks — the label, the right
    /// tail and `Detail::search`. Keeping the predicate on this list (not a
    /// side channel) is what keeps the emitted rows and the base's filter
    /// pass in agreement.
    haystacks: Vec<String>,
}

/// The submenus the palette can jump to, in index order. The hub and Help
/// itself stay out — the user is already inside both. `Import`/`Background`
/// have no provider yet; `enter` on them crosses the legacy bridge inside
/// `keys_menu::descend` exactly like the hub's own rows do (TODO T19).
const MENU_INDEX: &[(MenuId, &str)] = &[
    (MenuId::Settings, "every settable config field"),
    (MenuId::Plugins, "installed plugins and the marketplace"),
    (MenuId::Mcp, "MCP servers, prompts and logs"),
    (MenuId::Keys, "rebindable key actions"),
    (MenuId::Files, "files touched this turn"),
    (MenuId::Sessions, "saved sessions to resume"),
    (MenuId::Import, "import MCP servers and skills"),
    (MenuId::Background, "running background tasks"),
    (MenuId::Git, "recent commits"),
    (MenuId::Skills, "skill directories shipped by plugins"),
];

/// The Help view — rebuilt per frame; `ui.query` picks between the
/// sectioned index and the flat ranking (§5.8).
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Help);
    view.filter = Some(FilterMode::Local);
    view.empty = EmptyState::new(
        "nothing matches",
        "the index covers commands, menus, settings and key actions",
    );

    let entries = index(state);
    let query = ui.query.to_lowercase();
    view.rows = if query.is_empty() {
        browse_rows(&entries)
    } else {
        ranked_rows(&entries, &query)
    };
    view
}

/// `enter` dispatches on the row-id prefix (§5.8): `cmd:` runs the command
/// palette-style — the stack drains before `execute` so a command that does
/// not move `focus` still leaves the menu; `menu:` descends; `set:` descends
/// into Settings with the filter preset to the dotted key; `key:` descends
/// into Keys with the cursor on the action's row.
pub fn on_row(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    let id = row.as_str();
    if let Some(name) = id.strip_prefix("cmd:") {
        while stack.pop().is_some() {}
        return crate::commands::execute(state, &format!("/{name}"));
    }
    if let Some(slug) = id.strip_prefix("menu:") {
        let target = MenuId::ALL
            .iter()
            .find(|m| m.slug() == slug && !matches!(m, MenuId::Root | MenuId::Help));
        return match target {
            Some(target) => crate::keys_menu::descend(state, stack, ui, *target),
            None => vec![],
        };
    }
    if let Some(dotted) = id.strip_prefix("set:") {
        let actions = crate::keys_menu::descend(state, stack, ui, MenuId::Settings);
        // Land on the setting: the new level's filter is its dotted key and
        // the cursor sits on its row (Settings row ids are the dotted
        // keys). A refused push or a legacy bridge leaves `ui` alone — the
        // current level is then still Help.
        if stack.current().is_some_and(|l| l.id == MenuId::Settings) {
            ui.query = dotted.to_string();
            ui.cursor = RowId::new(dotted);
        }
        return actions;
    }
    if let Some(name) = id.strip_prefix("key:") {
        let actions = crate::keys_menu::descend(state, stack, ui, MenuId::Keys);
        // Keys row ids are the actions' TOML names — point the cursor.
        if stack.current().is_some_and(|l| l.id == MenuId::Keys) {
            ui.cursor = RowId::new(name);
        }
        return actions;
    }
    vec![]
}

/// The whole index, commands first — declaration order is the stable
/// tie-break inside a kind (§5.8).
fn index(state: &AppState) -> Vec<Entry> {
    let mut out = Vec::new();
    out.extend(command_entries());
    out.extend(menu_entries());
    out.extend(setting_entries(state));
    out.extend(key_entries(state));
    out
}

fn command_entries() -> Vec<Entry> {
    crate::commands::all_commands()
        .into_iter()
        .map(|c| {
            let label = c.usage.to_string();
            let right = c.help.to_string();
            let name = c.name.to_string();
            let mut lines = vec![DetailLine::Text(c.help.to_string())];
            if !c.arg_hint.is_empty() {
                lines.push(DetailLine::Field {
                    label: "args".to_string(),
                    value: c.arg_hint.to_string(),
                });
            }
            lines.push(DetailLine::Field {
                label: "kind".to_string(),
                value: "command".to_string(),
            });
            let search = vec![name.clone(), label.clone(), right.clone()];
            Entry {
                kind: Kind::Command,
                exacts: vec![name, label.clone()],
                haystacks: [label.clone(), right.clone()]
                    .into_iter()
                    .chain(search.clone())
                    .collect(),
                row: MenuRow::Item {
                    id: RowId::new(format!("cmd:{}", c.name)),
                    mark: super::model::Mark::None,
                    label,
                    right: Some(right),
                    detail: Detail {
                        title: c.usage.to_string(),
                        lines,
                        search,
                        ..Detail::default()
                    },
                },
            }
        })
        .collect()
}

fn menu_entries() -> Vec<Entry> {
    MENU_INDEX
        .iter()
        .map(|(id, desc)| {
            let label = format!("menu {}", id.title());
            let search = vec![
                id.slug().to_string(),
                id.title().to_string(),
                (*desc).to_string(),
            ];
            Entry {
                kind: Kind::Menu,
                exacts: vec![id.slug().to_string(), id.title().to_string()],
                haystacks: [label.clone(), (*desc).to_string()]
                    .into_iter()
                    .chain(search.clone())
                    .collect(),
                row: MenuRow::Item {
                    id: RowId::new(format!("menu:{}", id.slug())),
                    mark: super::model::Mark::None,
                    label,
                    right: Some((*desc).to_string()),
                    detail: Detail {
                        title: id.title().to_string(),
                        lines: vec![
                            DetailLine::Text((*desc).to_string()),
                            DetailLine::Field {
                                label: "kind".to_string(),
                                value: "menu".to_string(),
                            },
                        ],
                        search,
                        ..Detail::default()
                    },
                },
            }
        })
        .collect()
}

fn setting_entries(state: &AppState) -> Vec<Entry> {
    registry::all_settings()
        .iter()
        .map(|e| setting_entry(state, e))
        .collect()
}

fn setting_entry(state: &AppState, e: &'static SettingEntry) -> Entry {
    let label = format!("set {}", e.dotted_key);
    let right = e.label.to_string();
    let current = registry::read_value(&state.config, e.dotted_key).unwrap_or_default();
    let search = vec![
        e.dotted_key.to_string(),
        e.label.to_string(),
        e.description.to_string(),
    ];
    Entry {
        kind: Kind::Setting,
        exacts: vec![e.dotted_key.to_string(), e.label.to_string()],
        haystacks: [label.clone(), right.clone()]
            .into_iter()
            .chain(search.clone())
            .collect(),
        row: MenuRow::Item {
            id: RowId::new(format!("set:{}", e.dotted_key)),
            mark: super::model::Mark::None,
            label,
            right: Some(right),
            detail: Detail {
                title: e.label.to_string(),
                lines: vec![
                    DetailLine::Text(e.description.to_string()),
                    DetailLine::Field {
                        label: "key".to_string(),
                        value: e.dotted_key.to_string(),
                    },
                    DetailLine::Field {
                        label: "section".to_string(),
                        value: e.section.to_string(),
                    },
                    DetailLine::Field {
                        label: "current".to_string(),
                        value: current,
                    },
                ],
                search,
                ..Detail::default()
            },
        },
    }
}

fn key_entries(state: &AppState) -> Vec<Entry> {
    Keymap::action_names()
        .iter()
        .map(|(name, action)| {
            let bound = state.keymap.bindings_for(*action);
            let bindings = if bound.is_empty() {
                "unbound".to_string()
            } else {
                bound
                    .iter()
                    .map(format_binding)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let desc = action.description();
            let label = format!("key {name}");
            let right = if bound.is_empty() {
                desc.to_string()
            } else {
                format!("{bindings} · {desc}")
            };
            let search = vec![(*name).to_string(), desc.to_string(), bindings.clone()];
            Entry {
                kind: Kind::Key,
                exacts: vec![(*name).to_string()],
                haystacks: [label.clone(), right.clone()]
                    .into_iter()
                    .chain(search.clone())
                    .collect(),
                row: MenuRow::Item {
                    id: RowId::new(format!("key:{name}")),
                    mark: super::model::Mark::None,
                    label,
                    right: Some(right),
                    detail: Detail {
                        title: (*name).to_string(),
                        lines: vec![
                            DetailLine::Text(desc.to_string()),
                            DetailLine::Field {
                                label: "binding".to_string(),
                                value: bindings,
                            },
                            DetailLine::Field {
                                label: "scope".to_string(),
                                value: action.scope().label().to_string(),
                            },
                        ],
                        search,
                        ..Detail::default()
                    },
                },
            }
        })
        .collect()
}

/// The empty-query layout: the index grouped under a section per kind —
/// sections with no entries are skipped (§5.8).
fn browse_rows(entries: &[Entry]) -> Vec<MenuRow> {
    let mut rows = Vec::with_capacity(entries.len() + 4);
    for (kind, title) in [
        (Kind::Command, "Commands"),
        (Kind::Menu, "Menus"),
        (Kind::Setting, "Settings"),
        (Kind::Key, "Keys"),
    ] {
        let mut group = entries.iter().filter(|e| e.kind == kind).peekable();
        if group.peek().is_none() {
            continue;
        }
        rows.push(MenuRow::section(title));
        rows.extend(group.map(|e| e.row.clone()));
    }
    rows
}

/// The §5.8 rank tuple — `(matched, not-exact, kind, position)`. A miss is
/// `(1, …)` and sinks below every match.
type Rank = (u8, u8, u8, usize);

/// The query layout: one flat list, matches ranked by the §5.8 order —
/// exact name first, then kind, then the position the query hits at.
/// Non-matching rows trail behind: the base's `apply_filter` reaps them and
/// counts the full index as the `M` of `N of M`.
fn ranked_rows(entries: &[Entry], query: &str) -> Vec<MenuRow> {
    let mut scored: Vec<(Rank, &Entry)> = entries.iter().map(|e| (score(e, query), e)).collect();
    scored.sort_by_key(|(key, _)| *key);
    scored.into_iter().map(|(_, e)| e.row.clone()).collect()
}

fn score(entry: &Entry, query: &str) -> Rank {
    match match_pos(entry, query) {
        Some(pos) => {
            let not_exact = u8::from(!entry.exacts.iter().any(|n| n.eq_ignore_ascii_case(query)));
            (0, not_exact, entry.kind.rank(), pos)
        }
        None => (1, 0, 0, 0),
    }
}

/// The earliest byte position `query` occurs at across the entry's
/// haystacks — `None` when it occurs nowhere.
fn match_pos(entry: &Entry, query: &str) -> Option<usize> {
    entry
        .haystacks
        .iter()
        .filter_map(|h| h.to_lowercase().find(query))
        .min()
}
