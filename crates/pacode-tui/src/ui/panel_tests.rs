use super::tail_start;

#[test]
fn task_scroll_is_an_offset_from_the_tail() {
    assert_eq!(tail_start(20, 5, 0), 15);
    assert_eq!(tail_start(20, 5, 4), 11);
    assert_eq!(tail_start(20, 5, 15), 0);
    assert_eq!(tail_start(20, 5, 100), 0);
}

#[test]
fn task_scroll_clamps_short_output_to_the_head() {
    assert_eq!(tail_start(3, 5, 0), 0);
    assert_eq!(tail_start(3, 5, 100), 0);
}

use pacode_types::{AgentId, Config, TaskId};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::state::{AppState, Focus, PanelTarget};

fn state_with(target: PanelTarget, cols: u16, rows: u16) -> AppState {
    let mut state = AppState::new(Config::default(), "test".into(), cols, rows);
    state.panel.target = Some(target.clone());
    state.focus = Focus::Panel {
        target,
        follow: false,
        follow_paused: false,
    };
    state
}

fn drawn(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(state.cols, state.rows)).unwrap();
    terminal
        .draw(|frame| {
            let prepared = crate::ui::prepare_frame(frame.area(), state, std::time::Instant::now());
            crate::ui::draw(frame, state, &prepared);
        })
        .unwrap();
    format!("{}", terminal.backend())
}

#[test]
fn the_agent_panel_never_paints_another_agents_transcript() {
    let first = AgentId::new("agt_first");
    let second = AgentId::new("agt_second");
    let mut state = state_with(PanelTarget::Agent(first.clone()), 120, 30);
    state.panel.agent_transcript = crate::state::transcript::Transcript::new(32);
    state
        .panel
        .agent_transcript
        .push_cell(notice_cell("answer-marker-first"));
    state.panel.agent_transcript_for(&first);

    let owned = drawn(&mut state);
    assert!(owned.contains("answer-marker-first"), "{owned}");

    // The panel moves to another agent: the previous buffer must go with it,
    // even before the load for the new one answers.
    state.focus = Focus::Panel {
        target: PanelTarget::Agent(second.clone()),
        follow: false,
        follow_paused: false,
    };
    state.panel.target = Some(PanelTarget::Agent(second.clone()));
    let moved = drawn(&mut state);
    assert!(
        !moved.contains("answer-marker-first"),
        "the previous agent's answer is still on screen:\n{moved}"
    );
    assert!(state.panel.agent_transcript.cells.is_empty());
}

#[test]
fn a_task_panel_never_paints_another_targets_output() {
    let task = PanelTarget::Task(TaskId::new("task_one"));
    let server = PanelTarget::McpServer("srv".into());
    let mut state = state_with(task.clone(), 120, 30);
    state.panel.task_lines = vec!["spool-marker-one".to_string()];
    state.panel.task_total_lines = 1;

    let owned = drawn(&mut state);
    assert!(owned.contains("spool-marker-one"), "{owned}");

    state.focus = Focus::Panel {
        target: server.clone(),
        follow: false,
        follow_paused: false,
    };
    state.panel.target = Some(server);
    let moved = drawn(&mut state);
    assert!(
        !moved.contains("spool-marker-one"),
        "another target's output is still on screen:\n{moved}"
    );
    assert!(state.panel.task_lines.is_empty());
}

fn notice_cell(text: &str) -> crate::state::Cell {
    crate::state::Cell {
        id: 0,
        kind: crate::state::CellKind::Item(pacode_types::TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Info,
            text: text.to_string(),
        }),
        version: 0,
        ts_ms: 0,
        stats: None,
        perm_explain: None,
    }
}
