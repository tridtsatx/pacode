use std::sync::Mutex;

use pacode_types::{Config, McpServerInfo, PluginInfo, Request};

use crate::commands;
use crate::keys::Action;
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

    // 3. Execution without argument: opens the model palette and asks for
    //    the catalog and the auth status it groups by.
    let actions = commands::execute(&mut state, "/login");
    assert_eq!(
        actions,
        vec![
            crate::keys::Action::Send(Request::ListModels),
            crate::keys::Action::Send(Request::ListAuth),
        ]
    );
    assert!(matches!(
        state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::ModelPalette { .. })
    ));

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

fn attached_meta() -> pacode_types::SessionMeta {
    pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_old"),
        name: Some("old".into()),
        cwd: std::path::PathBuf::from("/work/project"),
        git_branch: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        model: pacode_types::ModelRoute::new("prov", "model-x"),
        effort: pacode_types::Effort::Max,
        mode: pacode_types::Mode::Plan,
        first_prompt: None,
        zdr: false,
    }
}

#[test]
fn clear_asks_the_daemon_and_leaves_the_view_until_it_answers() {
    let mut state = make_test_state();
    state.meta = Some(attached_meta());
    commands::execute(&mut state, "/mcp:malformed");
    let cells = state.transcript.cells.len();
    assert!(cells > 0);

    let actions = commands::execute(&mut state, "/clear");

    assert_eq!(actions, vec![Action::Send(Request::ClearContext)]);
    assert_eq!(
        state.transcript.cells.len(),
        cells,
        "a refused clear must find the transcript where it was"
    );
}

#[test]
fn new_attaches_a_fresh_session_with_the_current_settings() {
    let mut state = make_test_state();
    state.meta = Some(attached_meta());

    let actions = commands::execute(&mut state, "/new");

    let expected = pacode_types::Attach::New {
        cwd: std::path::PathBuf::from("/work/project"),
        model: Some(pacode_types::ModelRoute::new("prov", "model-x")),
        effort: Some(pacode_types::Effort::Max),
        mode: Some(pacode_types::Mode::Plan),
    };
    assert_eq!(
        actions,
        vec![Action::SwitchSession {
            attach: expected.clone()
        }]
    );
    assert_eq!(
        state.pending_attach.as_ref().map(|p| p.attach.clone()),
        Some(expected)
    );
    assert!(state.meta.is_none(), "the old session is left, not closed");
    assert!(commands::find_command("new").is_some());
}
