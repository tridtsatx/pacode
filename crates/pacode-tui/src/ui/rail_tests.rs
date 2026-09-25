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
