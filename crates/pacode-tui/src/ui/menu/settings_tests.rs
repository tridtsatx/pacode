//! §5.1: one row per registry entry grouped by section, the detail card, the
//! toggle/enum/edit commit paths, `ctrl+r` reset, and the local filter on the
//! dotted key and description.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_config::registry;
use pacode_types::Config;

use super::*;
use crate::state::AppState;
use crate::ui::menu::{MenuStack, StatusKind, resolve_cursor};

fn state() -> AppState {
    AppState::new(Config::default(), "0.1.0".into(), 120, 34)
}

/// A state whose config paths point at `dir` — write paths stay off the real
/// config dir.
fn state_at(dir: &std::path::Path) -> AppState {
    let mut s = state();
    s.paths = pacode_config::Paths::under(dir);
    s
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pacode-settings-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn find_row<'v>(view: &'v MenuView, key: &str) -> &'v MenuRow {
    view.rows
        .iter()
        .find(|r| r.id().is_some_and(|i| i.as_str() == key))
        .unwrap_or_else(|| panic!("row {key} not found"))
}

#[test]
fn sections_group_every_entry_and_are_not_selectable() {
    let s = state();
    let ui = MenuUiState::default();
    let view = view(&s, &ui);

    let section_labels: Vec<&str> = view
        .rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Section { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let want: Vec<String> = registry::SECTIONS
        .iter()
        .map(|s| format!("[{s}]"))
        .collect();
    assert_eq!(
        section_labels,
        want.iter().map(String::as_str).collect::<Vec<_>>()
    );

    // Sections are headers — the cursor domain excludes them (§3.3).
    assert!(
        view.rows
            .iter()
            .all(|r| !matches!(r, MenuRow::Section { .. }) || !r.is_selectable())
    );

    // Every registry entry produces exactly one selectable row.
    let selectable = view.rows.iter().filter(|r| r.is_selectable()).count();
    assert_eq!(selectable, registry::SETTINGS.len());
}

#[test]
fn entry_kinds_map_to_row_kinds() {
    let s = state();
    let ui = MenuUiState::default();
    let view = view(&s, &ui);

    assert!(matches!(
        find_row(&view, "ui.mouse"),
        MenuRow::Toggle {
            on: true,
            modified: false,
            ..
        }
    ));
    assert!(matches!(
        find_row(&view, "exec.yield_after_secs"),
        MenuRow::Value {
            kind: ValueKind::Int,
            ..
        }
    ));
    assert!(matches!(
        find_row(&view, "context.compaction_threshold"),
        MenuRow::Value {
            kind: ValueKind::Float,
            ..
        }
    ));
    assert!(matches!(
        find_row(&view, "provider.default"),
        MenuRow::Value {
            kind: ValueKind::Text,
            ..
        }
    ));
    assert!(matches!(
        find_row(&view, "provider.effort"),
        MenuRow::Value {
            kind: ValueKind::Enum { .. },
            ..
        }
    ));
    // `Action` entries are commands, not editable values — they come out as
    // plain items.
    assert!(matches!(
        find_row(&view, "mcp.servers"),
        MenuRow::Item { .. }
    ));
}

#[test]
fn the_detail_card_names_key_type_default_current_origin_and_apply() {
    let s = state();
    let ui = MenuUiState::default();
    let view = view(&s, &ui);
    let detail = find_row(&view, "ui.mouse").detail().expect("detail");

    let field = |name: &str| -> &str {
        detail
            .lines
            .iter()
            .find_map(|l| match l {
                DetailLine::Field { label, value } if label == name => Some(value.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("field {name} missing: {:?}", detail.lines))
    };
    assert_eq!(field("key"), "ui.mouse");
    assert_eq!(field("type"), "bool");
    assert_eq!(field("default"), "true");
    assert_eq!(field("current"), "true");
    assert_eq!(field("origin"), "default");
    assert_eq!(field("applies"), "immediate");
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains("mouse tracking")))
    );
}

#[test]
fn an_overridden_value_marks_the_row_and_arms_the_reset_chip() {
    let mut s = state();
    s.config.ui.mouse = false;
    let ui = MenuUiState::default();
    let view = view(&s, &ui);

    let MenuRow::Toggle {
        on,
        modified,
        detail,
        ..
    } = find_row(&view, "ui.mouse")
    else {
        panic!("ui.mouse must be a Toggle");
    };
    assert!(!*on);
    assert!(*modified);
    assert!(
        detail
            .actions
            .iter()
            .any(|a| a.key == RESET_KEY && a.enabled)
    );

    // An untouched row has the chip but disabled — nothing to reset.
    let detail = find_row(&view, "ui.ascii_only").detail().expect("detail");
    assert!(
        detail
            .actions
            .iter()
            .any(|a| a.key == RESET_KEY && !a.enabled)
    );
    // The origin line reports the user file.
    let detail = find_row(&view, "ui.mouse").detail().expect("detail");
    let origin = detail.lines.iter().find_map(|l| match l {
        DetailLine::Field { label, value } if label == "origin" => Some(value.as_str()),
        _ => None,
    });
    assert_eq!(origin, Some("global"));
}

#[test]
fn enter_on_a_toggle_flips_and_writes() {
    let dir = tmpdir("toggle");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Settings);
    let mut ui = MenuUiState {
        cursor: RowId::new("ui.mouse"),
        ..MenuUiState::default()
    };

    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    assert!(!s.config.ui.mouse);

    // The file got the override — a reload reads it back.
    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert!(!loaded.ui.mouse);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enter_on_an_enum_cycles_and_writes() {
    let dir = tmpdir("enum");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Settings);
    // `ui.color` — an enum without an env mapping, so a stray PACODE_* var in
    // the test environment cannot rewrite the reload.
    let mut ui = MenuUiState {
        cursor: RowId::new("ui.color"),
        ..MenuUiState::default()
    };

    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    assert_eq!(s.config.ui.color, "truecolor");

    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert_eq!(loaded.ui.color, "truecolor");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_render_settings_rebuild_context_and_clear_caches() {
    let mut s = state();
    s.transcript.cache.insert(
        pacode_render::cache::CacheKey::cell(1, 0, 80, false, 0),
        vec![ratatui::text::Line::from("stale")],
    );
    s.panel.diff = Some(crate::state::PanelDiff {
        path: "a.rs".into(),
        patch: Some("diff".into()),
        status: None,
        truncated: false,
        note: None,
        rendered: Some((
            pacode_render::RenderCacheKey::new(80, 0),
            vec![ratatui::text::Line::from("stale")],
        )),
    });
    s.git.diff = Some(crate::state::git::GitDiffInfo {
        hash: "abc".into(),
        patch: Some("diff".into()),
        truncated: false,
        note: None,
        rendered: Some((
            pacode_render::RenderCacheKey::new(80, 0),
            vec![ratatui::text::Line::from("stale")],
        )),
    });
    let old_theme = s.theme.clone();
    s.config.theme.name = "pacode-light".to_string();
    s.apply_runtime_config("theme.name");
    assert_ne!(s.theme, old_theme);
    let old_epoch = s.render_epoch;
    s.config.ui.ascii_only = true;
    s.config.ui.thinking = true;
    s.config.ui.color = "ansi".to_string();
    s.apply_runtime_config("ui.color");

    assert!(s.render_epoch > old_epoch);
    assert_eq!(s.render_options(80).color, pacode_render::ColorMode::Ansi);
    assert!(s.render_options(80).glyphs.ascii);
    assert!(s.render_options(80).thinking);
    assert_eq!(s.transcript.cache.total_lines(), 0);
    assert!(s.panel.diff.as_ref().unwrap().rendered.is_none());
    assert!(s.git.diff.as_ref().unwrap().rendered.is_none());
}

#[test]
fn live_transcript_cap_applies_to_main_and_agent_transcripts() {
    let mut s = state();
    for id in 0..3 {
        s.transcript.upsert(
            pacode_types::TranscriptItem {
                seq: id,
                agent: pacode_types::AgentId::main(),
                ts_ms: id,
                kind: pacode_types::TranscriptKind::User {
                    text: id.to_string(),
                },
            },
            std::time::Instant::now(),
        );
        s.panel.agent_transcript.upsert(
            pacode_types::TranscriptItem {
                seq: id,
                agent: pacode_types::AgentId::new("agent"),
                ts_ms: id,
                kind: pacode_types::TranscriptKind::User {
                    text: id.to_string(),
                },
            },
            std::time::Instant::now(),
        );
    }
    s.config.ui.transcript_cells = 2;
    s.apply_runtime_config("ui.transcript_cells");

    assert_eq!(s.transcript.cells.len(), 2);
    assert_eq!(s.panel.agent_transcript.cells.len(), 2);
}

#[test]
fn marketplace_setting_refreshes_the_active_listing() {
    let dir = tmpdir("marketplace");
    let mut s = state_at(&dir);
    s.marketplace = vec![pacode_types::MarketplacePluginInfo {
        name: "old".into(),
        description: String::new(),
        version: "1".into(),
        author: String::new(),
        category: String::new(),
        marketplace: "old".into(),
        installed: false,
        installed_version: None,
        update_available: false,
    }];
    let mut ui = MenuUiState::default();
    let actions = on_edit(
        &mut s,
        &mut ui,
        &RowId::new("plugins.marketplace"),
        "owner/new".into(),
        0,
    );

    assert!(matches!(
        actions.as_slice(),
        [Action::Send(pacode_types::Request::BrowseMarketplace { source, query })]
            if source == "owner/new" && query.is_empty()
    ));
    assert!(s.marketplace.is_empty());
    assert_eq!(s.config.plugins.marketplace, "owner/new");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_edit_stays_in_edit_mode_and_writes_nothing() {
    let dir = tmpdir("bad-edit");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Settings);
    let mut ui = MenuUiState {
        cursor: RowId::new("exec.yield_after_secs"),
        ..MenuUiState::default()
    };

    // Enter opens the inline edit on the Value row.
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    assert!(matches!(ui.mode, MenuMode::Edit { .. }));

    // "99999" is past the 3600 ceiling — the commit fails.
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Backspace));
    for c in "99999".chars() {
        crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char(c)));
    }
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));

    // The editor stays open, the error is the status line, nothing changed.
    assert!(matches!(ui.mode, MenuMode::Edit { .. }));
    let status = ui.status.as_ref().expect("status");
    assert_eq!(status.kind, StatusKind::Error);
    assert!(status.text.contains("out of range"), "{:?}", status.text);
    assert_eq!(s.config.exec.yield_after_secs, 5);
    assert!(
        !s.paths.config_file.exists(),
        "a failed validation writes no config file"
    );

    // Typing on clears the error (it disappeared with its cause).
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('4')));
    assert!(ui.status.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_good_edit_commits_through_the_config_file() {
    let dir = tmpdir("good-edit");
    let mut s = state_at(&dir);
    let mut stack = MenuStack::root(MenuId::Settings);
    let mut ui = MenuUiState {
        cursor: RowId::new("exec.yield_after_secs"),
        ..MenuUiState::default()
    };

    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));
    // The buffer starts at "5" — replace it with "42".
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Backspace));
    for c in "42".chars() {
        crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char(c)));
    }
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));

    assert!(matches!(ui.mode, MenuMode::List));
    assert!(ui.status.is_none());
    assert_eq!(s.config.exec.yield_after_secs, 42);
    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert_eq!(loaded.exec.yield_after_secs, 42);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_r_resets_the_row_and_removes_the_override() {
    let dir = tmpdir("reset");
    let mut s = state_at(&dir);
    pacode_config::update_config_value(&s.paths, "exec.yield_after_secs", toml::Value::Integer(99))
        .expect("seed override");
    s.config.exec.yield_after_secs = 99;

    let mut stack = MenuStack::root(MenuId::Settings);
    let mut ui = MenuUiState {
        cursor: RowId::new("exec.yield_after_secs"),
        ..MenuUiState::default()
    };
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, ctrl('r'));

    assert_eq!(s.config.exec.yield_after_secs, 5);
    // The key left the file — a reload is the default again.
    let loaded = pacode_config::load(&s.paths).expect("reload");
    assert_eq!(loaded.exec.yield_after_secs, 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_r_on_an_action_row_is_a_no_op() {
    let dir = tmpdir("reset-action");
    let mut s = state_at(&dir);
    // A config file exists so a stray remove would visibly rewrite it.
    pacode_config::update_config_value(&s.paths, "ui.mouse", toml::Value::Boolean(false))
        .expect("seed");
    let before = std::fs::read_to_string(&s.paths.config_file).expect("seed file");

    let mut ui = MenuUiState::default();
    let action = MenuAction::new(RESET_KEY, "reset");
    let acts = on_action(&mut s, &mut ui, &action, Some(&RowId::new("mcp.servers")));
    assert!(acts.is_empty());
    assert!(s.toasts.is_empty(), "a refused reset toasts nothing");
    assert_eq!(
        std::fs::read_to_string(&s.paths.config_file).expect("file"),
        before,
        "resetting an Action row must not touch the file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_filter_matches_the_dotted_key_and_the_description() {
    let s = state();
    let ui = MenuUiState::default();
    let mut v = view(&s, &ui);

    // By dotted key — "exec.yield" is not part of the row's label.
    v.apply_filter("exec.yield");
    let kept: Vec<_> = v.rows.iter().filter(|r| r.is_selectable()).collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].id().map(RowId::as_str),
        Some("exec.yield_after_secs")
    );
    // The section header survives because a member survived.
    assert!(v.rows.iter().any(|r| matches!(
        r,
        MenuRow::Section { label, .. } if label == "[exec]"
    )));

    // By description — "clipboard" appears only in the Auto Copy text.
    let mut v = view(&s, &ui);
    v.apply_filter("clipboard");
    let kept: Vec<_> = v.rows.iter().filter(|r| r.is_selectable()).collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id().map(RowId::as_str), Some("ui.auto_copy"));
}

#[test]
fn the_cursor_lands_on_the_first_setting_not_the_section_header() {
    let s = state();
    let ui = MenuUiState::default();
    let view = view(&s, &ui);
    let sel = resolve_cursor(&view.rows, &ui.cursor).expect("a selection");
    assert_eq!(
        view.rows[sel].id().map(RowId::as_str),
        Some("provider.default")
    );
}
