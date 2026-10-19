//! The `Plugins` level (§5.2): `Installed` merges runtime-loaded plugins
//! with marketplace install records, `Discover` browses the configured
//! marketplace through a debounced remote filter.
//!
//! Installed status glyph: `✓` loaded, `✗` installed but failed to load (the
//! detail card carries the reason), `○` installed with nothing to load —
//! a skills/MCP-only record — and `⊘` the whole subsystem switched off
//! (`plugins.enabled = false`). The protocol has no per-plugin enable
//! toggle, so `enter` acts only on the Discover tab (install); on Installed
//! the one row action is `x` — uninstall, offered solely for plugins a
//! marketplace installed (`PluginInfo::is_installed`).

use crossterm::event::{KeyCode, KeyModifiers};
use pacode_types::{MarketplacePluginInfo, PluginInfo, Request};

use crate::binding::Binding;
use crate::keys::Action;
use crate::state::AppState;

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId, resolve_cursor};
use super::stack::MenuStack;
use super::view::{
    EmptyState, FilterMode, KeyBinding, MenuAction, MenuId, MenuUiState, MenuView, StatusLine, Tab,
    Tabs,
};

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod plugins_tests;

/// `ui.tab` values — the tab strip order is Installed then Discover.
pub(crate) const TAB_INSTALLED: usize = 0;
pub(crate) const TAB_DISCOVER: usize = 1;

/// Row-id prefixes — `on_row`/`on_action` recover the plugin name from them.
const INSTALLED_ROW: &str = "installed:";
const MARKET_ROW: &str = "market:";

/// Discover's remote-filter debounce (§5.2).
const DEBOUNCE_MS: u64 = 150;

const LABEL_UNINSTALL: &str = "uninstall";

fn key_char(c: char) -> KeyBinding {
    Binding {
        code: KeyCode::Char(c),
        mods: KeyModifiers::NONE,
    }
}

fn field(label: &str, value: impl Into<String>) -> DetailLine {
    DetailLine::Field {
        label: label.to_string(),
        value: value.into(),
    }
}

/// `loading…` / `offline copy` from the list's cache health (§8.2).
fn status_of(state: &AppState, kind: crate::cache::ListKind) -> Option<StatusLine> {
    match state.lists.badge(kind) {
        Some(crate::cache::Badge::Loading) => Some(StatusLine::loading()),
        Some(crate::cache::Badge::OfflineCopy) => Some(StatusLine::offline_copy()),
        None => None,
    }
}

/// The view — rebuilt per frame; the tab decides the row set and the filter.
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Plugins);
    view.tabs = Some(Tabs {
        items: vec![
            Tab {
                label: "Installed".to_string(),
                count: state.plugins.len(),
            },
            Tab {
                label: "Discover".to_string(),
                count: state.marketplace.len(),
            },
        ],
    });
    let actions = if ui.tab == TAB_DISCOVER {
        discover_view(state, ui, &mut view)
    } else {
        installed_view(state, ui, &mut view)
    };
    view.with_actions(actions)
}

fn installed_view(state: &AppState, ui: &MenuUiState, view: &mut MenuView) -> Vec<MenuAction> {
    view.status = status_of(state, crate::cache::ListKind::Plugins);
    view.rows = state
        .plugins
        .iter()
        .map(|p| installed_row(state, p))
        .collect();
    // `x` is offered only while the selected row is actually removable:
    // uninstall tracks an install record, so manual drops are not (§5.2).
    let removable = resolve_cursor(&view.rows, &ui.cursor)
        .and_then(|i| view.rows[i].id().cloned())
        .and_then(|id| id.as_str().strip_prefix(INSTALLED_ROW).map(str::to_string))
        .is_some_and(|name| {
            state
                .plugins
                .iter()
                .any(|p| p.name == name && p.is_installed())
        });
    view.empty = EmptyState::new(
        "no plugins installed",
        "tab Discover — browse the marketplace",
    );
    vec![MenuAction::new(key_char('x'), LABEL_UNINSTALL).enabled(removable)]
}

fn discover_view(state: &AppState, ui: &MenuUiState, view: &mut MenuView) -> Vec<MenuAction> {
    view.status = status_of(state, crate::cache::ListKind::Marketplace);
    if state.config.plugins.marketplace.is_empty() {
        // No filter line without a source — typing would go nowhere.
        view.empty = EmptyState::new(
            "no marketplace configured",
            "/plugins owner/repo to browse one",
        );
        return vec![];
    }
    view.filter = Some(FilterMode::Remote {
        debounce_ms: DEBOUNCE_MS,
    });
    view.rows = state.marketplace.iter().map(discover_row).collect();
    view.empty = if ui.query.is_empty() {
        EmptyState::new(
            "this marketplace lists no plugins",
            "try another source with /plugins owner/repo",
        )
    } else {
        EmptyState::new(
            "nothing matches that search",
            "backspace to clear the filter",
        )
    };
    // Install rides on `enter`: while the remote filter owns printable keys
    // no declared char action could fire (§3.4).
    vec![]
}

/// `✓` loaded · `✗` failed (reason in detail) · `⊘` subsystem off · `○`
/// installed but with nothing to load (§5.2).
fn installed_mark(state: &AppState, p: &PluginInfo) -> Mark {
    if p.error.is_some() {
        Mark::Fail
    } else if p.loaded {
        Mark::Ok
    } else if !state.config.plugins.enabled {
        Mark::Disabled
    } else {
        Mark::Idle
    }
}

fn installed_row(state: &AppState, p: &PluginInfo) -> MenuRow {
    // The right tail is the legacy head: version · kind · tools · commands.
    let mut tail: Vec<String> = Vec::new();
    if !p.version.is_empty() {
        tail.push(p.version.clone());
    }
    if !p.kind.is_empty() {
        tail.push(p.kind.clone());
    }
    if !p.tools.is_empty() {
        tail.push(format!("{} tools", p.tools.len()));
    }
    if !p.commands.is_empty() {
        tail.push(format!("{} commands", p.commands.len()));
    }
    MenuRow::Item {
        id: RowId::new(format!("{INSTALLED_ROW}{}", p.name)),
        mark: installed_mark(state, p),
        label: p.name.clone(),
        right: (!tail.is_empty()).then(|| tail.join(" · ")),
        detail: installed_detail(p),
    }
}

/// §5.2 detail: version, source, composition (tools/skills/mcp/hooks), the
/// update date, the load-error reason and the action chips.
fn installed_detail(p: &PluginInfo) -> Detail {
    let mut lines: Vec<DetailLine> = Vec::new();
    if !p.description.is_empty() {
        lines.push(DetailLine::Text(p.description.clone()));
    }
    if !p.version.is_empty() {
        lines.push(field("version", p.version.clone()));
    }
    lines.push(field(
        "source",
        if p.source.is_empty() {
            "local (plugins dir)".to_string()
        } else {
            p.source.clone()
        },
    ));
    let mut parts: Vec<String> = Vec::new();
    if !p.tools.is_empty() {
        parts.push(format!("{} tools", p.tools.len()));
    }
    if p.skill_dirs > 0 {
        parts.push(format!("{} skills", p.skill_dirs));
    }
    if !p.mcp_servers.is_empty() {
        parts.push(format!("mcp: {}", p.mcp_servers.join(", ")));
    }
    if !p.commands.is_empty() {
        parts.push(format!("{} commands", p.commands.len()));
    }
    if !parts.is_empty() {
        lines.push(field("contents", parts.join(" · ")));
    }
    if !p.unsupported.is_empty() {
        // hooks/agents/commands the runtime does not apply (§5.2 "hooks").
        lines.push(DetailLine::Text(format!(
            "! {} declared but not applied",
            p.unsupported.join(", ")
        )));
    }
    if p.installed_at_ms > 0 {
        let age = crate::ui::overlays::age_ago(p.installed_at_ms, pacode_types::time::now_ms());
        lines.push(field("installed", age));
    }
    if !p.author.is_empty() {
        lines.push(field("author", p.author.clone()));
    }
    if let Some(err) = &p.error {
        lines.push(field("error", err.clone()));
    }
    Detail {
        title: p.name.clone(),
        lines,
        actions: vec![MenuAction::new(key_char('x'), LABEL_UNINSTALL).enabled(p.is_installed())],
        ..Detail::default()
    }
}

fn discover_row(p: &MarketplacePluginInfo) -> MenuRow {
    // The right tail is the legacy head: version · author · update ·
    // installed-marker.
    let mut tail: Vec<String> = Vec::new();
    if !p.version.is_empty() {
        tail.push(p.version.clone());
    }
    if !p.author.is_empty() {
        tail.push(p.author.clone());
    }
    if p.update_available {
        tail.push("update".to_string());
    }
    if let Some(v) = &p.installed_version {
        tail.push(format!("installed {v}"));
    }
    MenuRow::Item {
        id: RowId::new(format!("{MARKET_ROW}{}", p.name)),
        mark: if p.installed { Mark::Ok } else { Mark::None },
        label: p.name.clone(),
        right: (!tail.is_empty()).then(|| tail.join(" · ")),
        detail: discover_detail(p),
    }
}

fn discover_detail(p: &MarketplacePluginInfo) -> Detail {
    let mut lines: Vec<DetailLine> = Vec::new();
    if !p.description.is_empty() {
        lines.push(DetailLine::Text(p.description.clone()));
    }
    if !p.version.is_empty() {
        lines.push(field("version", p.version.clone()));
    }
    if !p.author.is_empty() {
        lines.push(field("author", p.author.clone()));
    }
    if !p.category.is_empty() {
        lines.push(field("category", p.category.clone()));
    }
    if !p.marketplace.is_empty() {
        lines.push(field("marketplace", p.marketplace.clone()));
    }
    if p.installed {
        let mut s = "installed".to_string();
        if let Some(v) = &p.installed_version {
            s = format!("installed {v}");
        }
        if p.update_available {
            s.push_str(" · update available");
        }
        lines.push(field("status", s));
    } else {
        // The affordance the hint edge cannot name — `enter` is a base key.
        lines.push(DetailLine::Text("enter — install".to_string()));
    }
    Detail {
        title: p.name.clone(),
        lines,
        actions: vec![],
        ..Detail::default()
    }
}

/// Refresh-on-open — the pair the legacy picker sent: `ListPlugins` always,
/// `BrowseMarketplace` when a marketplace is configured (§8.2 marks the
/// lists in-flight so the status line shows `loading…`).
pub fn on_open(state: &AppState) -> Vec<Action> {
    let mut actions = vec![Action::Send(Request::ListPlugins)];
    let source = state.config.plugins.marketplace.clone();
    if !source.is_empty() {
        actions.push(Action::Send(Request::BrowseMarketplace {
            source,
            query: String::new(),
        }));
    }
    actions
}

/// The debounced remote-filter send (§5.2): `BrowseMarketplace` for the
/// configured source and the live query; nothing without a marketplace.
pub fn remote_query(state: &AppState, query: &str) -> Vec<Action> {
    let source = state.config.plugins.marketplace.clone();
    if source.is_empty() {
        return vec![];
    }
    vec![Action::Send(Request::BrowseMarketplace {
        source,
        query: query.to_string(),
    })]
}

/// `enter`: installs the Discover row. Installed rows have no enter action —
/// the protocol carries no per-plugin enable toggle.
pub fn on_row(
    state: &mut AppState,
    _stack: &mut MenuStack,
    ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    if ui.tab == TAB_DISCOVER
        && let Some(name) = row.as_str().strip_prefix(MARKET_ROW)
    {
        return install(state, name);
    }
    vec![]
}

/// Declared actions — `x` uninstall on Installed (§5.2). `row` is the
/// cursor's row; the action is a no-op for anything without an install
/// record (the chip and the hint are already disabled then — this is the
/// belt to their suspenders).
pub fn on_action(
    state: &mut AppState,
    ui: &mut MenuUiState,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    if action.label == LABEL_UNINSTALL
        && ui.tab == TAB_INSTALLED
        && let Some(name) = row.and_then(|r| r.as_str().strip_prefix(INSTALLED_ROW))
        && state
            .plugins
            .iter()
            .any(|p| p.name == name && p.is_installed())
    {
        return vec![
            Action::Send(Request::UninstallPlugin {
                name: name.to_string(),
            }),
            Action::Send(Request::ListPlugins),
        ];
    }
    vec![]
}

/// `InstallPlugin` + the `ListPlugins` refresh the legacy picker issued; a
/// no-op for a row already installed or without a configured source.
fn install(state: &AppState, name: &str) -> Vec<Action> {
    let source = state.config.plugins.marketplace.clone();
    if source.is_empty() {
        return vec![];
    }
    let already = state
        .marketplace
        .iter()
        .any(|p| p.name == name && p.installed);
    if already {
        return vec![];
    }
    vec![
        Action::Send(Request::InstallPlugin {
            source,
            name: name.to_string(),
        }),
        Action::Send(Request::ListPlugins),
    ]
}
