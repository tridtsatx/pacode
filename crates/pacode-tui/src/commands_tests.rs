use std::sync::Mutex;

use pacode_types::{Config, McpServerInfo, PluginInfo, Request};

use crate::commands;
use crate::state::AppState;
use crate::ui::input::command_inline_hint;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

fn make_test_state() -> AppState {
    let config = Config::default();
    AppState::new(config, "0.1.0".into(), 120, 34)
}

#[test]
fn test_mcp_prompt_registration_and_execution() {
    let _guard = TEST_MUTEX.lock().unwrap();
    commands::clear_dynamic_commands();
    let mut state = make_test_state();

    let server = McpServerInfo {
        name: "my_server".into(),
        status: "ready".into(),
        error: None,
        tools: 1,
        resources: 0,
        prompts: 2,
        prompt_names: vec!["review".into(), "summarize".into()],
    };
    commands::register_mcp_servers(&[server]);

    // 1. Registered in commands
    let all = commands::all_commands();
    assert!(all.iter().any(|c| c.name == "mcp:my_server:review"));
    assert!(all.iter().any(|c| c.name == "mcp:my_server:summarize"));

    // 2. Matching prefix
    let matches = commands::matching("mcp:my_server");
    assert_eq!(matches.len(), 2);

    // 3. Inline hint
    let hint = command_inline_hint("/mcp:my_server:review");
    assert_eq!(hint, Some(" [key=value ...]".into()));

    // 4. Execution with key=value pairs and ignored tokens
    let actions = commands::execute(
        &mut state,
        "/mcp:my_server:review pr=100 branch=main positional_token verbose=true",
    );
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        crate::keys::Action::Send(Request::GetMcpPrompt { server, name, args }) => {
            assert_eq!(server, "my_server");
            assert_eq!(name, "review");
            assert_eq!(args.get("pr"), Some(&"100".to_string()));
            assert_eq!(args.get("branch"), Some(&"main".to_string()));
            assert_eq!(args.get("verbose"), Some(&"true".to_string()));
            assert!(!args.contains_key("positional_token"));
        }
        other => panic!("expected Send(GetMcpPrompt), got {other:?}"),
    }

    commands::clear_dynamic_commands();
}

#[test]
fn test_plugin_command_registration_and_execution() {
    let _guard = TEST_MUTEX.lock().unwrap();
    commands::clear_dynamic_commands();
    let mut state = make_test_state();

    let plugin = PluginInfo {
        name: "test_plugin".into(),
        version: "1.0.0".into(),
        kind: "lua".into(),
        tools: vec![],
        commands: vec!["format_code".into()],
        error: None,
        loaded: true,
        ..PluginInfo::default()
    };
    commands::register_plugins(std::slice::from_ref(&plugin));
    state.plugins.push(plugin);

    // 1. Registered in commands
    let all = commands::all_commands();
    assert!(all.iter().any(|c| c.name == "format_code"));

    // 2. Matching
    let matches = commands::matching("format");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "format_code");

    // 3. Execution sends RunPluginCommand
    let actions = commands::execute(&mut state, "/format_code --style=compact src/main.rs");
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        crate::keys::Action::Send(Request::RunPluginCommand { name, args }) => {
            assert_eq!(name, "format_code");
            assert_eq!(args, "--style=compact src/main.rs");
        }
        other => panic!("expected Send(RunPluginCommand), got {other:?}"),
    }

    commands::clear_dynamic_commands();
}

#[test]
fn test_login_command_registration_and_execution() {
    let mut state = make_test_state();

    // 1. Registered in commands
    let all = commands::all_commands();
    let login_cmd = all
        .iter()
        .find(|c| c.name == "login")
        .expect("login command exists");
    assert_eq!(login_cmd.usage, "/login");
    assert_eq!(login_cmd.help, "sign in to a model provider");
    assert_eq!(login_cmd.arg_hint, "[provider]");

    // 2. Matching
    let matches = commands::matching("log");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "login");

    // 3. Execution without argument: opens Overlay::LoginPicker and sends ListAuth
    let actions = commands::execute(&mut state, "/login");
    assert_eq!(actions, vec![crate::keys::Action::Send(Request::ListAuth)]);
    assert_eq!(
        state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::LoginPicker {
            query: String::new(),
            index: 0,
        })
    );

    // 4. Execution with argument: sends Request::Login { provider }
    state.focus = crate::state::Focus::Normal;
    let actions = commands::execute(&mut state, "/login devin");
    assert_eq!(
        actions,
        vec![crate::keys::Action::Send(Request::Login {
            provider: "devin".into(),
        })]
    );
    assert_eq!(state.focus, crate::state::Focus::Normal);
}

#[test]
fn test_mcp_invalid_format_pushes_notice() {
    let mut state = make_test_state();
    let initial_cell_count = state.transcript.cells.len();

    let actions = commands::execute(&mut state, "/mcp:malformed");
    assert!(actions.is_empty());
    assert!(state.transcript.cells.len() > initial_cell_count);
}

/// The last notice in the transcript: (level, text).
fn last_notice(state: &AppState) -> (pacode_types::ToastLevel, String) {
    state
        .transcript
        .cells
        .iter()
        .rev()
        .find_map(|c| {
            if let crate::state::CellKind::Item(pacode_types::TranscriptKind::Notice {
                level,
                text,
            }) = &c.kind
            {
                Some((*level, text.clone()))
            } else {
                None
            }
        })
        .expect("a notice")
}

/// B16: a theme name that is neither builtin nor a user theme is refused —
/// nothing is applied, persisted, or reported as set.
#[test]
fn test_theme_typo_is_refused_and_not_persisted() {
    let root = std::env::temp_dir().join(format!("pacode-b16-{}", std::process::id()));
    let mut state = make_test_state();
    state.paths = pacode_config::Paths::under(&root);
    let before = state.config.theme.name.clone();
    commands::execute(&mut state, "/theme no-such-theme");
    assert_eq!(state.config.theme.name, before);
    assert!(!state.paths.config_file.exists());
    let (level, text) = last_notice(&state);
    assert_eq!(level, pacode_types::ToastLevel::Warn);
    assert!(text.contains("Unknown theme 'no-such-theme'"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// B18: the valid list comes from `Effort::ALL`, so `xhigh` is in it.
#[test]
fn test_invalid_effort_lists_every_level() {
    let mut state = make_test_state();
    commands::execute(&mut state, "/effort bogus");
    let (_, text) = last_notice(&state);
    assert!(text.ends_with("low, medium, high, xhigh, max"), "{text}");
}

/// B20: a command's notice is the answer to what the user just typed; a
/// reader scrolled up is brought down to it.
#[test]
fn test_command_notice_scrolls_into_view() {
    let mut state = make_test_state();
    state.transcript.scroll_from_bottom = 7;
    commands::execute(&mut state, "/no-such-command");
    assert_eq!(state.transcript.scroll_from_bottom, 0);
}
