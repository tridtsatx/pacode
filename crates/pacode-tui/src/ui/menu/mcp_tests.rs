//! §5.3: status glyphs distinguish the four states, the error text is capped
//! instead of stretching the card, the declared actions dispatch, the `l`
//! panel gets a filtered daemon-log tail, and the hub summary counts the
//! fallen (`N · M <failed_word>` — the word itself lives in
//! `pacode_render::text`, Cyrillic is banned in literals here).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use pacode_types::{Config, McpServerInfo, Request};

use super::*;
use crate::state::AppState;

fn server(name: &str, status: &str) -> McpServerInfo {
    McpServerInfo {
        name: name.to_string(),
        status: status.to_string(),
        error: None,
        tools: 0,
        resources: 0,
        prompts: 0,
        prompt_names: Vec::new(),
    }
}

fn make_state(servers: Vec<McpServerInfo>) -> AppState {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    state.mcp_servers = servers;
    state
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn each_status_gets_a_distinct_glyph() {
    let marks: Vec<Mark> = ["ready", "failed", "disabled", "starting"]
        .iter()
        .map(|s| status_mark(s))
        .collect();
    assert_eq!(
        marks,
        vec![Mark::Active, Mark::Fail, Mark::Disabled, Mark::Pending]
    );
    // `stopped` and anything unknown get the stopped glyph — distinct from
    // the four spec states.
    assert_eq!(status_mark("stopped"), Mark::Stopped);
    assert_eq!(status_mark("mystery"), Mark::Stopped);
}

#[test]
fn rows_carry_mark_name_and_tail() {
    let mut s = server("db", "ready");
    s.tools = 3;
    s.resources = 1;
    s.prompts = 2;
    s.prompt_names = vec!["migrate".to_string()];
    let state = make_state(vec![s]);
    let view = view(&state, &MenuUiState::default());
    let [
        MenuRow::Item {
            mark,
            label,
            right,
            detail,
            ..
        },
    ] = view.rows.as_slice()
    else {
        panic!("one Item row expected, got {:?}", view.rows);
    };
    assert_eq!(*mark, Mark::Active);
    assert_eq!(label, "db");
    assert_eq!(right.as_deref(), Some("3 tools · 1 res · 2 prompts"));
    // The detail carries transport/counts and the action chips.
    assert!(
        detail
            .actions
            .iter()
            .any(|a| a.label == ACT_RESTART && a.key.code == KeyCode::Char('r'))
    );
    assert!(
        detail
            .actions
            .iter()
            .any(|a| a.label == ACT_LOG && a.key.code == KeyCode::Char('l'))
    );
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Field { label, value } if label == "prompts" && value.contains("migrate")))
    );
}

#[test]
fn a_huge_error_is_capped_not_sprawled() {
    let mut s = server("bad", "failed");
    // ~10 KiB over many lines — far past both caps.
    s.error = Some("line of failure text that is reasonably long\n".repeat(300));
    let state = make_state(vec![s]);
    let view = view(&state, &MenuUiState::default());
    let Some(detail) = view.rows[0].detail() else {
        panic!("Item detail expected");
    };
    // The error block contributes at most ERROR_MAX_LINES tail lines plus a
    // marker — the whole card stays small regardless of input size.
    let text_lines = detail
        .lines
        .iter()
        .filter(|l| matches!(l, DetailLine::Text(_)))
        .count();
    assert!(text_lines <= ERROR_MAX_LINES + 1, "got {text_lines}");
    let total_bytes: usize = detail
        .lines
        .iter()
        .map(|l| match l {
            DetailLine::Text(t) => t.len(),
            DetailLine::Field { label, value } => label.len() + value.len(),
        })
        .sum();
    assert!(total_bytes <= ERROR_CAP_BYTES + 512, "got {total_bytes}");
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains("cut") && t.contains("bytes")))
    );
    // The tail is what survives — the newest line is the last shown.
    let last = detail.lines.last();
    assert!(matches!(last, Some(DetailLine::Text(t)) if t.contains("line of failure")));
}

#[test]
fn actions_are_declared_and_dispatch() {
    let state = make_state(vec![server("fs", "ready")]);
    let view = view(&state, &MenuUiState::default());
    assert!(
        view.actions
            .iter()
            .any(|a| a.label == ACT_RESTART && a.key.code == KeyCode::Char('r'))
    );
    assert!(
        view.actions
            .iter()
            .any(|a| a.label == ACT_LOG && a.key.code == KeyCode::Char('l'))
    );

    // `r` on the selected row → restart + refresh, like the legacy picker.
    let mut state = make_state(vec![server("fs", "ready")]);
    let mut stack = MenuStack::root(MenuId::Mcp);
    let mut ui = MenuUiState::default();
    let actions =
        crate::keys_menu::handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Char('r')));
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::RestartMcpServer {
                server: "fs".to_string()
            }),
            Action::Send(Request::ListMcpServers),
        ]
    );
}

#[test]
fn enter_toggles_enabled_both_ways() {
    // Disabled → re-enable.
    let mut state = make_state(vec![server("fs", "disabled")]);
    let row = RowId::new("fs");
    let mut stack = MenuStack::root(MenuId::Mcp);
    let mut ui = MenuUiState::default();
    let actions = on_row(&mut state, &mut stack, &mut ui, &row);
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::SetMcpServerEnabled {
                server: "fs".to_string(),
                enabled: true,
            }),
            Action::Send(Request::ListMcpServers),
        ]
    );
    // Ready → disable.
    let mut state = make_state(vec![server("fs", "ready")]);
    let actions = on_row(&mut state, &mut stack, &mut ui, &row);
    assert!(
        matches!(
            actions.first(),
            Some(Action::Send(Request::SetMcpServerEnabled {
                enabled: false,
                ..
            }))
        ),
        "got {actions:?}"
    );
    // A row the daemon no longer lists is a no-op, not a panic.
    let actions = on_row(&mut state, &mut stack, &mut ui, &RowId::new("gone"));
    assert!(actions.is_empty());
}

#[test]
fn l_opens_the_log_in_a_panel_not_a_menu() {
    let dir = std::env::temp_dir().join(format!("pacode-mcp-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("state")).unwrap();
    let log = dir.join("state").join("daemon.log");
    std::fs::write(
        &log,
        concat!(
            "2026-09-12T00:00:00.000Z INFO  pacode_mcp::pool: starting MCP server 'fs'\n",
            "2026-09-12T00:00:01.000Z DEBUG pacode_mcp::transport::stdio: [MCP stderr fs] ready\n",
            "2026-09-12T00:00:02.000Z INFO  pacode_mcp::pool: starting MCP server 'other'\n",
            "2026-09-12T00:00:03.000Z INFO  pacode_core::something: unrelated line about 'fs'\n",
        ),
    )
    .unwrap();

    let mut state = make_state(vec![server("fs", "ready"), server("other", "ready")]);
    state.paths = pacode_config::Paths::under(&dir);
    let log_action = action(KeyCode::Char('l'), ACT_LOG);
    let actions = on_action(
        &mut state,
        &mut MenuUiState::default(),
        &log_action,
        Some(&RowId::new("fs")),
    );
    assert!(actions.is_empty());
    // The menu stack is left behind — the focus is the side panel on the log.
    assert_eq!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::McpServer("fs".to_string()),
            follow: false,
            follow_paused: false,
        }
    );
    // Only this server's pacode_mcp lines made it in — the sibling server and
    // the non-mcp target that names 'fs' are filtered out.
    assert_eq!(state.panel.task_lines.len(), 2);
    assert!(state.panel.task_lines[0].contains("starting MCP server 'fs'"));
    assert!(state.panel.task_lines[1].contains("[MCP stderr fs]"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_log_still_opens_an_honest_panel() {
    let dir = std::env::temp_dir().join(format!("pacode-mcp-test-none-{}", std::process::id()));
    let mut state = make_state(vec![server("fs", "ready")]);
    state.paths = pacode_config::Paths::under(&dir);
    let log_action = action(KeyCode::Char('l'), ACT_LOG);
    on_action(
        &mut state,
        &mut MenuUiState::default(),
        &log_action,
        Some(&RowId::new("fs")),
    );
    assert!(matches!(
        state.focus,
        Focus::Panel {
            target: PanelTarget::McpServer(_),
            ..
        }
    ));
    assert_eq!(state.panel.task_lines.len(), 1);
    assert!(state.panel.task_lines[0].contains("not found"));
}

#[test]
fn empty_and_badge_states() {
    let mut state = make_state(vec![]);
    let v = view(&state, &MenuUiState::default());
    assert!(v.rows.is_empty());
    assert_eq!(v.empty.primary, "no MCP servers");
    assert!(v.status.is_none());

    state.lists.mark_sent(crate::cache::ListKind::McpServers);
    let v = view(&state, &MenuUiState::default());
    assert_eq!(v.status, Some(StatusLine::loading()));
    assert_eq!(v.empty.primary, "loading…");
}

#[test]
fn hub_summary_counts_failures() {
    let state = make_state(vec![
        server("a", "ready"),
        server("b", "failed"),
        server("c", "failed"),
        server("d", "disabled"),
    ]);
    let hub = super::super::root::view(&state, &MenuUiState::default());
    let row = hub
        .rows
        .iter()
        .find(|r| r.id().is_some_and(|i| i.as_str() == "mcp"))
        .expect("hub has an mcp row");
    let MenuRow::Submenu { right, .. } = row else {
        panic!("mcp is a Submenu row");
    };
    // `4 · 2 <упали>` — the word comes from pacode_render::text.
    let want = format!("4 · 2 {}", pacode_render::text::failed_word(2));
    assert_eq!(right.as_deref(), Some(want.as_str()));

    // No failures — the bare count.
    let state = make_state(vec![server("a", "ready"), server("b", "ready")]);
    let hub = super::super::root::view(&state, &MenuUiState::default());
    let row = hub
        .rows
        .iter()
        .find(|r| r.id().is_some_and(|i| i.as_str() == "mcp"))
        .unwrap();
    let MenuRow::Submenu { right, .. } = row else {
        panic!("mcp is a Submenu row");
    };
    assert_eq!(right.as_deref(), Some("2"));
}
