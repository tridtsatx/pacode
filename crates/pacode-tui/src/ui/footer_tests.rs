use pacode_render::RenderOptions;
use pacode_types::Config;
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus};
use pacode_types::{AgentId, AgentInfo};

use super::*;
use crate::state::AppState;

fn make_test_state() -> AppState {
    let config = Config::default();
    AppState::new(config, "0.1.0".into(), 120, 34)
}

#[test]
fn test_footer_render_plugin_status() {
    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);

    // 1. Without plugin_status
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row1_text.contains("git-sync"));

    let row2 = render_row2(120, &state, &opts);
    let row2_text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row2_text.contains("git-sync"));

    // 2. With plugin_status
    state.plugin_status = Some(("git".into(), "git-sync: rebasing".into()));

    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    // Mode label is followed by " · git-sync: rebasing"
    assert!(
        row1_text.contains("Build · git-sync: rebasing"),
        "row1_text was: {row1_text}"
    );

    let row2 = render_row2(120, &state, &opts);
    let row2_text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    // Chevrons + permission line followed by " · git-sync: rebasing"
    assert!(
        row2_text.contains(" · git-sync: rebasing"),
        "row2_text was: {row2_text}"
    );
    assert!(row2_text.contains("ask before edits and commands"));
}

#[test]
fn test_footer_select_agent_shows_main_and_subagents() {
    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);
    let main_agent = AgentInfo {
        id: AgentId::main(),
        name: "main".into(),
        kind: AgentKind::Main,
        status: AgentStatus::Thinking,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: None,
        summary: None,
        error: None,
    };
    let sub1 = AgentInfo {
        id: AgentId::new("sub_1"),
        name: "waiter-1".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::RunningTool,
        activity: None,
        started_at_ms: 1000,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    let sub2 = AgentInfo {
        id: AgentId::new("sub_2"),
        name: "waiter-2".into(),
        kind: AgentKind::Sub,
        status: AgentStatus::RunningTool,
        activity: None,
        started_at_ms: 2000,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("p", "m"),
        effort: Effort::High,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    state.rail.upsert_agent(main_agent);
    state.rail.upsert_agent(sub1);
    state.rail.upsert_agent(sub2);

    // 1. Select main (index 0)
    state.focus = Focus::SelectAgent { index: 0 };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("main 1/3"), "text was: {text}");
    assert!(
        text.contains("enter open · alt+b follow · esc clear"),
        "text was: {text}"
    );

    // 2. Select waiter-1 (index 1)
    state.focus = Focus::SelectAgent { index: 1 };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("waiter-1 2/3"), "text was: {text}");
    assert!(
        text.contains("enter open · alt+b follow · esc clear"),
        "text was: {text}"
    );

    // 3. Panel with follow = true: hint shows alt+b release
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(AgentId::new("sub_1")),
        follow: true,
        follow_paused: false,
    };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("pgup pause · alt+b release"),
        "text was: {text}"
    );

    // 4. Panel with follow = false: hint shows alt+b follow
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(AgentId::new("sub_1")),
        follow: false,
        follow_paused: false,
    };
    let row2 = render_row2(120, &state, &opts);
    let text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("esc back · alt+b follow · s stop"),
        "text was: {text}"
    );
}

#[test]
fn test_footer_vim_mode_badges() {
    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);

    // With vim = false, no badge is present
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row1_text.contains("NOR"));
    assert!(!row1_text.contains("INS"));
    assert!(!row1_text.contains("VIS"));

    // Enable vim mode
    state.config.ui.vim = true;

    // Normal mode badge
    state.vim.mode = crate::state::vim::VimMode::Normal;
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(row1_text.contains("NOR"), "row1_text was: {row1_text}");
    assert!(!row1_text.contains("INS"));

    // Insert mode badge
    state.vim.mode = crate::state::vim::VimMode::Insert;
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(row1_text.contains("INS"), "row1_text was: {row1_text}");
    assert!(!row1_text.contains("NOR"));

    // Visual mode badge
    state.vim.mode = crate::state::vim::VimMode::Visual;
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(row1_text.contains("VIS"), "row1_text was: {row1_text}");
    assert!(!row1_text.contains("NOR"));
}
