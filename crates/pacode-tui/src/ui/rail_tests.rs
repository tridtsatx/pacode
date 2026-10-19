use pacode_types::AgentId;

use super::*;

#[test]
fn selectable_agent_indices_include_main_without_shifting_subagents() {
    let agents = vec![
        AgentId::main(),
        AgentId::new("agt_1"),
        AgentId::new("agt_2"),
    ];

    assert_eq!(
        selected_positions(&agents, &AgentSelection::Index(0), 2),
        (true, None)
    );
    assert_eq!(
        selected_positions(&agents, &AgentSelection::Index(1), 2),
        (false, Some(0))
    );
    assert_eq!(
        selected_positions(&agents, &AgentSelection::Index(2), 2),
        (false, Some(1))
    );
}

#[test]
fn a_subagent_waiting_on_the_orchestrator_has_its_own_dot() {
    use pacode_render::{ColorMode, Theme};
    use pacode_types::{AgentInfo, AgentKind, AgentStatus, Effort, ModelRoute};

    let opts = RenderOptions::with_theme_color(40, false, Theme::truecolor(), ColorMode::Truecolor);
    let agent = |status| AgentInfo {
        id: AgentId::new("agt_1"),
        name: "tests".into(),
        kind: AgentKind::Sub,
        status,
        activity: Some("asking the orchestrator".into()),
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("mock", "m"),
        effort: Effort::Low,
        parent: Some(AgentId::main()),
        summary: None,
        error: None,
    };
    for lit in [true, false] {
        let (waiting, waiting_style) = agent_dot(&agent(AgentStatus::WaitingParent), lit, &opts);
        let (working, working_style) = agent_dot(&agent(AgentStatus::Thinking), lit, &opts);
        assert_eq!(waiting, "?");
        assert_ne!(waiting, working);
        assert_ne!(waiting_style, working_style, "a distinct tone, lit={lit}");
    }
}

#[test]
fn rail_overlay_selection_resolves_by_agent_id() {
    let agents = vec![
        AgentId::main(),
        AgentId::new("agt_1"),
        AgentId::new("agt_2"),
    ];

    assert_eq!(
        selected_positions(&agents, &AgentSelection::Id(AgentId::new("agt_2")), 2),
        (false, Some(1))
    );
    assert_eq!(
        selected_positions(&agents, &AgentSelection::Id(AgentId::new("missing")), 2),
        (false, None)
    );
}
