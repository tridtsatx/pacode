//! Tests for the Plugins level (§5.2): the four status glyphs, the disabled
//! uninstall chip, the remote-filter debounce and the hub summary.

use std::time::{Duration, Instant};

use pacode_types::{Config, MarketplacePluginInfo, PluginInfo, Request};

use super::*;
use crate::keys::Action;
use crate::state::AppState;
use crate::ui::menu::{MenuUiState, RowId, StatusKind, root};

fn fixture() -> AppState {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    state.config.plugins.marketplace = "owner/repo".to_string();
    state
}

/// `view` as a plain callable — a `let view = …` binding in a test must not
/// shadow the provider for the next call.
fn built(state: &AppState, ui: &MenuUiState) -> MenuView {
    view(state, ui)
}

fn loaded(name: &str) -> PluginInfo {
    PluginInfo {
        name: name.to_string(),
        version: "1.0".to_string(),
        kind: "lua".to_string(),
        tools: vec!["t1".to_string()],
        loaded: true,
        ..PluginInfo::default()
    }
}

fn record(name: &str) -> PluginInfo {
    PluginInfo {
        name: name.to_string(),
        version: "2.0".to_string(),
        source: "owner/repo".to_string(),
        installed_at_ms: 1,
        ..PluginInfo::default()
    }
}

fn market(name: &str) -> MarketplacePluginInfo {
    MarketplacePluginInfo {
        name: name.to_string(),
        version: "3.0".to_string(),
        author: "someone".to_string(),
        description: "does things".to_string(),
        marketplace: "owner/repo".to_string(),
        installed: false,
        installed_version: None,
        update_available: false,
        category: "tools".to_string(),
    }
}

fn mark_of(view: &MenuView, name: &str) -> Mark {
    let want = format!("{INSTALLED_ROW}{name}");
    view.rows
        .iter()
        .find_map(|r| {
            if let MenuRow::Item { id, mark, .. } = r
                && id.as_str() == want
            {
                return Some(*mark);
            }
            None
        })
        .expect("row exists")
}

#[test]
fn installed_glyph_per_status() {
    let mut state = fixture();
    let mut failed = record("broken");
    failed.error = Some("init failed".to_string());
    let mut orphan = record("skill-only");
    orphan.skill_dirs = 2;
    state.plugins = vec![loaded("live"), failed, orphan];

    let v = built(&state, &MenuUiState::default());
    assert_eq!(mark_of(&v, "live"), Mark::Ok);
    assert_eq!(mark_of(&v, "broken"), Mark::Fail);
    // Installed record, subsystem on, nothing to load — the idle ring.
    assert_eq!(mark_of(&v, "skill-only"), Mark::Idle);

    // `plugins.enabled = false` switches every non-loaded row off.
    state.config.plugins.enabled = false;
    let v = built(&state, &MenuUiState::default());
    assert_eq!(mark_of(&v, "live"), Mark::Ok);
    assert_eq!(mark_of(&v, "broken"), Mark::Fail);
    assert_eq!(mark_of(&v, "skill-only"), Mark::Disabled);
}

#[test]
fn failed_plugin_detail_carries_the_reason() {
    let mut state = fixture();
    let mut p = record("broken");
    p.error = Some("manifest parse: line 4".to_string());
    state.plugins = vec![p];

    let v = built(&state, &MenuUiState::default());
    let detail = v.rows[0].detail().expect("item detail");
    assert!(detail.lines.iter().any(|l| {
        matches!(l, DetailLine::Field { label, value } if label == "error" && value.contains("line 4"))
    }));
}

#[test]
fn uninstall_chip_is_disabled_for_manual_plugins() {
    let mut state = fixture();
    let manual = PluginInfo {
        name: "dropped".to_string(),
        loaded: true,
        ..PluginInfo::default()
    };
    state.plugins = vec![record("bought"), manual];

    let v = built(&state, &MenuUiState::default());
    let chip_enabled = |name: &str| {
        let want = format!("{INSTALLED_ROW}{name}");
        v.rows
            .iter()
            .find(|r| r.id().is_some_and(|i| *i.as_str() == *want))
            .and_then(|r| r.detail())
            .and_then(|d| d.actions.first())
            .expect("chip")
            .enabled
    };
    assert!(
        chip_enabled("bought"),
        "marketplace plugin can be uninstalled"
    );
    assert!(
        !chip_enabled("dropped"),
        "a manual drop has no install record"
    );

    // The menu-level action follows the selection: cursor on "dropped"
    // disables `x`, cursor on "bought" enables it.
    let ui = MenuUiState {
        cursor: RowId::new(format!("{INSTALLED_ROW}dropped")),
        ..MenuUiState::default()
    };
    let v = built(&state, &ui);
    assert!(!v.actions[0].enabled, "x is not offered on a manual plugin");
    let v = built(&state, &MenuUiState::default());
    assert!(v.actions[0].enabled);
}

#[test]
fn uninstall_sends_uninstall_and_refresh() {
    let mut state = fixture();
    state.plugins = vec![record("bought")];
    let mut ui = MenuUiState::default();
    let action = MenuAction::new(key_char('x'), LABEL_UNINSTALL);
    let row = RowId::new(format!("{INSTALLED_ROW}bought"));

    let acts = on_action(&mut state, &mut ui, &action, Some(&row));
    assert!(matches!(
        acts.as_slice(),
        [Action::Send(Request::UninstallPlugin { name }), Action::Send(Request::ListPlugins)]
            if name == "bought"
    ));

    // A manual plugin under the cursor — the action does nothing.
    state.plugins = vec![PluginInfo {
        name: "dropped".to_string(),
        ..PluginInfo::default()
    }];
    let row = RowId::new(format!("{INSTALLED_ROW}dropped"));
    assert!(on_action(&mut state, &mut ui, &action, Some(&row)).is_empty());
}

#[test]
fn discover_tab_filter_is_remote_and_rows_carry_marks() {
    let mut state = fixture();
    let mut have = market("owned");
    have.installed = true;
    have.installed_version = Some("1.0".to_string());
    state.marketplace = vec![market("fresh"), have];

    let ui = MenuUiState {
        tab: TAB_DISCOVER,
        ..MenuUiState::default()
    };
    let v = built(&state, &ui);
    assert!(matches!(
        v.filter,
        Some(FilterMode::Remote { debounce_ms: 150 })
    ));
    assert_eq!(v.rows.len(), 2);
    let marks: Vec<Mark> = v
        .rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Item { mark, .. } => Some(*mark),
            _ => None,
        })
        .collect();
    assert_eq!(marks, vec![Mark::None, Mark::Ok]);
    // Not-installed rows name the enter affordance in the detail.
    let detail = v.rows[0].detail().expect("detail");
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains("install")))
    );
}

#[test]
fn discover_without_marketplace_has_no_filter() {
    let mut state = fixture();
    state.config.plugins.marketplace.clear();
    let ui = MenuUiState {
        tab: TAB_DISCOVER,
        ..MenuUiState::default()
    };
    let v = built(&state, &ui);
    assert!(v.filter.is_none());
    assert!(v.empty.primary.contains("no marketplace"));
}

#[test]
fn enter_on_a_market_row_installs() {
    let mut state = fixture();
    state.marketplace = vec![market("fresh")];
    let mut stack = MenuStack::root(MenuId::Plugins);
    let mut ui = MenuUiState {
        tab: TAB_DISCOVER,
        ..MenuUiState::default()
    };
    let row = RowId::new(format!("{MARKET_ROW}fresh"));
    let acts = on_row(&mut state, &mut stack, &mut ui, &row);
    assert!(matches!(
        acts.as_slice(),
        [Action::Send(Request::InstallPlugin { source, name }), Action::Send(Request::ListPlugins)]
            if source == "owner/repo" && name == "fresh"
    ));

    // Already installed — enter is a no-op.
    let mut owned = market("owned");
    owned.installed = true;
    state.marketplace = vec![owned];
    let row = RowId::new(format!("{MARKET_ROW}owned"));
    assert!(on_row(&mut state, &mut stack, &mut ui, &row).is_empty());
}

#[test]
fn on_open_and_remote_query_send_the_legacy_requests() {
    let state = fixture();
    let acts = on_open(&state);
    assert!(matches!(
        acts.as_slice(),
        [Action::Send(Request::ListPlugins), Action::Send(Request::BrowseMarketplace { source, query })]
            if source == "owner/repo" && query.is_empty()
    ));

    let acts = remote_query(&state, "fuzzy");
    assert!(matches!(
        acts.as_slice(),
        [Action::Send(Request::BrowseMarketplace { source, query })]
            if source == "owner/repo" && query == "fuzzy"
    ));

    // No marketplace configured — nothing goes out.
    let mut bare_state = fixture();
    bare_state.config.plugins.marketplace.clear();
    assert!(remote_query(&bare_state, "fuzzy").is_empty());
    assert_eq!(on_open(&bare_state).len(), 1);
}

#[test]
fn remote_filter_debounces_to_one_request() {
    use crate::keys_menu::{open, remote_deadline, remote_filter_due};
    let mut state = fixture();
    let _ = open(&mut state, MenuId::Plugins);

    // Switch to Discover, then type five chars fast — one pending send.
    let press = |state: &mut AppState, c: char| {
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        );
        crate::keys::handle_key(state, key, Instant::now())
    };
    let tab = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    );
    let _ = crate::keys::handle_key(&mut state, tab, Instant::now());
    for c in "query".chars() {
        let _ = press(&mut state, c);
    }
    assert!(remote_deadline(&state).is_some(), "the debounce is armed");

    // Before the deadline — nothing.
    assert!(remote_filter_due(&mut state, Instant::now()).is_empty());
    // After it — exactly one BrowseMarketplace for the whole query.
    let acts = remote_filter_due(&mut state, Instant::now() + Duration::from_millis(200));
    let browses: Vec<_> = acts
        .iter()
        .filter(|a| matches!(a, Action::Send(Request::BrowseMarketplace { .. })))
        .collect();
    assert_eq!(browses.len(), 1, "five keystrokes, one request");
    assert!(matches!(
        browses[0],
        Action::Send(Request::BrowseMarketplace { query, .. }) if query == "query"
    ));
    // Disarmed — a second poll sends nothing.
    assert!(remote_deadline(&state).is_none());
    assert!(remote_filter_due(&mut state, Instant::now() + Duration::from_secs(1)).is_empty());
}

#[test]
fn status_badges_map_to_the_status_line() {
    let mut state = fixture();
    state.plugins = vec![loaded("live")];
    state.lists.mark_sent(crate::cache::ListKind::Plugins);
    let v = built(&state, &MenuUiState::default());
    assert!(matches!(
        v.status.map(|s| s.kind),
        Some(StatusKind::Loading)
    ));

    let mut stale_state = fixture();
    stale_state
        .lists
        .apply_cached(crate::cache::ListKind::Plugins, true);
    let v = built(&stale_state, &MenuUiState::default());
    let status = v.status.expect("offline badge");
    assert_eq!(status.text, "offline copy");
}

#[test]
fn the_hub_row_summarises_installed_and_available() {
    let mut state = fixture();
    state.plugins = vec![loaded("live")];
    state.marketplace = vec![market("fresh"), market("other")];
    let plugins_right = |v: &MenuView| {
        v.rows
            .iter()
            .find(|r| r.id().is_some_and(|i| i.as_str() == "plugins"))
            .and_then(|r| match r {
                MenuRow::Submenu { right, .. } => right.clone(),
                _ => None,
            })
    };
    let v = root::view(&state, &MenuUiState::default());
    assert_eq!(
        plugins_right(&v).as_deref(),
        Some("1 installed · 2 available")
    );

    // Nothing installed, nothing listed — no count at all.
    let mut empty_state = fixture();
    empty_state.marketplace.clear();
    empty_state.plugins.clear();
    let v = root::view(&empty_state, &MenuUiState::default());
    assert_eq!(plugins_right(&v), None);
}
