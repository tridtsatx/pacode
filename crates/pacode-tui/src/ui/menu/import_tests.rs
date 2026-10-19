//! §5.7: a missing source keeps its `no config` section, a conflict carries
//! the `⚠` tail and starts unmarked, `space`/`a` write the level's `checks`
//! map so the marks survive the per-frame rebuild, the status line counts
//! the marked set, and `enter` — the one apply-keyed menu — imports the
//! marked set or parks a "nothing selected" status when it is empty.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_import::{DiscoveredMcp, DiscoveredSkill, McpServerConfig};
use pacode_render::RenderOptions;
use pacode_types::{Config, ToastLevel};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::keys_menu::handle_menu_key;
use crate::state::{Focus, Overlay};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A state whose config target lives under `dir` (`Paths::under`), so an
/// apply lands in the tempdir, not the real config.
fn state_at(dir: &Path) -> AppState {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    state.paths = pacode_config::Paths::under(dir);
    state.cwd = dir.to_path_buf();
    state
}

fn mcp(name: &str, source: ImportSource) -> PlanEntry {
    PlanEntry::mcp(DiscoveredMcp {
        source,
        origin: PathBuf::from(format!("/src/{}.json", source.id())),
        name: name.to_string(),
        server: McpServerConfig {
            command: "cmd".to_string(),
            ..McpServerConfig::default()
        },
    })
}

fn skill(name: &str, source: ImportSource) -> PlanEntry {
    PlanEntry::skill(DiscoveredSkill {
        source,
        origin: PathBuf::from(format!("/src/{}/{name}/SKILL.md", source.id())),
        name: name.to_string(),
        description: "does things".to_string(),
        dir: PathBuf::from(format!("/src/{}/{name}", source.id())),
    })
}

/// A scan built by hand: `present` names the sources whose watched config
/// exists (the others render `no config`), and each entry carries the status
/// `check_status` would have stamped on it.
fn scan_with(
    dir: &Path,
    present: &[ImportSource],
    entries: Vec<(PlanEntry, EntryStatus)>,
) -> ImportScan {
    ImportScan {
        target: ApplyTarget::new(dir.join("config.toml"), dir.join("skills"), true),
        sources: ImportSource::all()
            .iter()
            .map(|&source| SourceScan {
                source,
                found: present
                    .contains(&source)
                    .then(|| PathBuf::from(format!("/cfg/{}", source.id()))),
                primary: PathBuf::from(format!("/cfg/{}", source.id())),
            })
            .collect(),
        entries: entries
            .into_iter()
            .map(|(entry, status)| ImportEntry {
                id: entry_id(&entry),
                entry,
                status,
            })
            .collect(),
        warnings: vec![],
    }
}

/// The Check row carrying `id`, matched out of a rebuilt view.
fn check_row<'a>(view: &'a MenuView, id: &str) -> &'a MenuRow {
    view.rows
        .iter()
        .find(|r| r.id().is_some_and(|i| i.as_str() == id))
        .unwrap_or_else(|| panic!("row {id} missing"))
}

fn check_state(row: &MenuRow) -> (bool, Option<&str>) {
    let MenuRow::Check { checked, warn, .. } = row else {
        panic!("a Check row expected, got {row:?}");
    };
    (*checked, warn.as_deref())
}

fn detail_text(d: &Detail) -> String {
    d.lines
        .iter()
        .map(|l| match l {
            DetailLine::Field { label, value } => format!("{label}: {value}"),
            DetailLine::Text(t) => t.clone(),
            DetailLine::Code { text, .. } => text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Draw into a test backend and return the screen rows as strings — the same
/// helper `render_tests` uses, for the hint-edge assertion.
fn screen(view: &MenuView, ui: &MenuUiState, w: u16, h: u16) -> Vec<String> {
    let opts = RenderOptions::new(w, false);
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| crate::ui::menu::draw(f, f.area(), view, ui, &opts))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            row.trim_end().to_string()
        })
        .collect()
}

#[test]
fn open_routes_to_the_menu_level_and_scans() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    // The real HOME/cwd scan is read-only; what matters is the route, not
    // what this machine happens to have installed.
    crate::keys_menu::open(&mut state, MenuId::Import);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Import));
    assert!(state.import.is_some());
}

#[test]
fn every_source_keeps_a_section_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![(mcp("alpha", ImportSource::ClaudeCode), EntryStatus::New)],
    ));
    let view = view(&state, &MenuUiState::default());

    let labels: Vec<&str> = view
        .rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Section { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let want: Vec<String> = ImportSource::all()
        .iter()
        .map(|s| format!("[{}]", s.label()))
        .collect();
    assert_eq!(labels, want.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn a_missing_source_is_marked_no_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![(mcp("alpha", ImportSource::ClaudeCode), EntryStatus::New)],
    ));
    let view = view(&state, &MenuUiState::default());

    let section_right = |label: &str| {
        view.rows.iter().find_map(|r| match r {
            MenuRow::Section { label: l, right } if l == label => right.as_deref(),
            _ => None,
        })
    };
    // A present source carries the config path on the section tail.
    assert_eq!(section_right("[Claude Code]"), Some("/cfg/claude"));
    for &s in ImportSource::all()
        .iter()
        .filter(|&&s| s != ImportSource::ClaudeCode)
    {
        let label = format!("[{}]", s.label());
        assert_eq!(section_right(&label), Some("no config"), "{label}");
    }
}

#[test]
fn a_conflict_is_warned_and_unmarked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![
            (
                mcp("alpha", ImportSource::ClaudeCode),
                EntryStatus::Conflict,
            ),
            (mcp("beta", ImportSource::ClaudeCode), EntryStatus::New),
            (skill("notes", ImportSource::ClaudeCode), EntryStatus::Same),
        ],
    ));
    let view = view(&state, &MenuUiState::default());

    let row = check_row(&view, "claude:mcp:alpha");
    assert_eq!(check_state(row), (false, Some("already exists")));
    // The detail card says what the apply would replace (§5.7).
    let MenuRow::Check { detail, .. } = row else {
        panic!("a Check row expected");
    };
    let text = detail_text(detail);
    assert!(text.contains("overwrites"), "{text}");
    assert!(
        text.contains("cmd"),
        "the incoming command is shown: {text}"
    );

    assert_eq!(
        check_state(check_row(&view, "claude:mcp:beta")),
        (true, None)
    );
    assert_eq!(
        check_state(check_row(&view, "claude:skill:notes")),
        (false, Some("already imported"))
    );
}

#[test]
fn space_marks_and_unmarks_the_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![(mcp("alpha", ImportSource::ClaudeCode), EntryStatus::New)],
    ));
    let mut stack = MenuStack::root(MenuId::Import);
    let mut ui = MenuUiState {
        cursor: RowId::from("claude:mcp:alpha"),
        ..MenuUiState::default()
    };

    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char(' ')));
    let (checked, _) = check_state(check_row(&view(&state, &ui), "claude:mcp:alpha"));
    assert!(!checked, "space unmarks — the mark lives in ui.checks");
    assert_eq!(
        ui.checks.get(&RowId::from("claude:mcp:alpha")),
        Some(&false)
    );

    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char(' ')));
    let (checked, _) = check_state(check_row(&view(&state, &ui), "claude:mcp:alpha"));
    assert!(checked);
}

#[test]
fn a_marks_then_clears_the_whole_source_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode, ImportSource::Codex],
        vec![
            (
                mcp("alpha", ImportSource::ClaudeCode),
                EntryStatus::Conflict,
            ),
            (skill("notes", ImportSource::ClaudeCode), EntryStatus::Same),
            (mcp("gamma", ImportSource::Codex), EntryStatus::Conflict),
        ],
    ));
    let mut stack = MenuStack::root(MenuId::Import);
    let mut ui = MenuUiState {
        cursor: RowId::from("claude:mcp:alpha"),
        ..MenuUiState::default()
    };

    // Any unmarked entry in the group → `a` marks them all.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char('a')));
    let v = view(&state, &ui);
    assert!(check_state(check_row(&v, "claude:mcp:alpha")).0);
    assert!(check_state(check_row(&v, "claude:skill:notes")).0);
    // Other groups are untouched.
    assert!(!check_state(check_row(&v, "codex:mcp:gamma")).0);

    // All marked → `a` clears the group.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char('a')));
    let v = view(&state, &ui);
    assert!(!check_state(check_row(&v, "claude:mcp:alpha")).0);
    assert!(!check_state(check_row(&v, "claude:skill:notes")).0);
    assert!(!check_state(check_row(&v, "codex:mcp:gamma")).0);
}

#[test]
fn the_status_counts_the_marked_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![
            (mcp("alpha", ImportSource::ClaudeCode), EntryStatus::New),
            (mcp("beta", ImportSource::ClaudeCode), EntryStatus::New),
            (
                skill("notes", ImportSource::ClaudeCode),
                EntryStatus::Conflict,
            ),
        ],
    ));
    let view = view(&state, &MenuUiState::default());
    let status = view.status.as_ref().map(|s| s.text.as_str());
    assert_eq!(status, Some("2 of 3 selected"));
}

#[test]
fn enter_with_nothing_marked_parks_a_status_and_stays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        // A conflict starts unmarked — the marked set is empty.
        vec![(
            mcp("alpha", ImportSource::ClaudeCode),
            EntryStatus::Conflict,
        )],
    ));
    let mut stack = MenuStack::root(MenuId::Import);
    let mut ui = MenuUiState {
        cursor: RowId::from("claude:mcp:alpha"),
        ..MenuUiState::default()
    };

    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    let status = ui.status.as_ref().map(|s| s.text.as_str());
    assert_eq!(
        status,
        Some("nothing selected — space marks an entry, a marks the group")
    );
    // No apply ran: the menu stays open and the config file was not written.
    assert_eq!(stack.depth(), 1);
    assert!(state.import.is_some());
    assert!(!state.paths.config_file.exists());
}

// `on_row`'s cache invalidation spawns a blocking task — the apply tests run
// inside a current-thread runtime so `spawn_blocking` has a reactor.
#[tokio::test(flavor = "current_thread")]
async fn enter_applies_the_marked_set_and_closes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![
            (mcp("alpha", ImportSource::ClaudeCode), EntryStatus::New),
            // Unmarked conflicts are left alone — `beta` stays out.
            (mcp("beta", ImportSource::ClaudeCode), EntryStatus::Conflict),
        ],
    ));
    let mut stack = MenuStack::root(MenuId::Import);
    let mut ui = MenuUiState {
        cursor: RowId::from("claude:mcp:alpha"),
        ..MenuUiState::default()
    };

    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    let written = std::fs::read_to_string(&state.paths.config_file).expect("config written");
    assert!(written.contains("alpha"), "{written}");
    assert!(!written.contains("beta"), "{written}");
    // The apply drained the stack (menu closed) and dropped the scan.
    assert!(stack.is_empty());
    assert!(state.import.is_none());
    let toast = state.toasts.back().expect("a result toast");
    assert_eq!(toast.level, ToastLevel::Success);
    assert_eq!(toast.title, "imported 1 servers, 0 skills");
}

#[tokio::test(flavor = "current_thread")]
async fn a_marked_conflict_is_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    std::fs::write(
        &state.paths.config_file,
        "[mcp.servers.alpha]\ncommand = \"old\"\n",
    )
    .expect("seed config");
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![(
            mcp("alpha", ImportSource::ClaudeCode),
            EntryStatus::Conflict,
        )],
    ));
    let mut stack = MenuStack::root(MenuId::Import);
    let mut ui = MenuUiState {
        cursor: RowId::from("claude:mcp:alpha"),
        ..MenuUiState::default()
    };

    // Mark the ⚠ row on purpose, then apply — `force` honors the mark.
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char(' ')));
    handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    let written = std::fs::read_to_string(&state.paths.config_file).expect("config written");
    assert!(written.contains("command = \"cmd\""), "{written}");
    // The in-memory config was reloaded, so the detail cards agree.
    let reloaded = state
        .config
        .mcp
        .servers
        .get("alpha")
        .map(|s| s.command.as_str());
    assert_eq!(reloaded, Some("cmd"));
}

#[test]
fn the_hint_edge_carries_enter_import() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state_at(dir.path());
    state.import = Some(scan_with(
        dir.path(),
        &[ImportSource::ClaudeCode],
        vec![(mcp("alpha", ImportSource::ClaudeCode), EntryStatus::New)],
    ));
    let view = view(&state, &MenuUiState::default());
    assert_eq!(view.enter_label, Some("import"));
    let text = screen(&view, &MenuUiState::default(), 100, 24).join("\n");
    assert!(text.contains("enter import"), "{text}");
    assert!(text.contains("space mark"), "{text}");
    assert!(text.contains("a group"), "{text}");
}
