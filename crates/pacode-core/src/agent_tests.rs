use std::sync::Arc;

use pacode_tools::ToolRegistry;
use pacode_types::{AgentId, AgentInfo, AgentKind, AgentStatus, ModelRoute};

use super::*;

fn dummy_agent_info(id: AgentId) -> AgentInfo {
    AgentInfo {
        id,
        name: "test".to_string(),
        kind: AgentKind::Main,
        status: AgentStatus::Idle,
        activity: None,
        started_at_ms: 0,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("provider", "model"),
        effort: pacode_types::Effort::Low,
        parent: None,
        summary: None,
        error: None,
    }
}

#[test]
fn test_agent_id_accessor_and_swap() {
    let main_id = AgentId::main();
    let agent = Agent::new(
        main_id.clone(),
        dummy_agent_info(main_id.clone()),
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    );

    assert_eq!(agent.id(), main_id);
    assert_eq!(*agent.id_arc(), main_id);
    assert!(agent.with_id(|id| id.is_main()));

    let new_id = AgentId::new("agt_sub123");
    agent.set_id(new_id.clone());

    assert_eq!(agent.id(), new_id);
    assert_eq!(*agent.id_arc(), new_id);
    assert!(agent.with_id(|id| !id.is_main()));
}

#[test]
fn test_agent_concurrent_id_read() {
    let agent = Arc::new(Agent::new(
        AgentId::main(),
        dummy_agent_info(AgentId::main()),
        History::default(),
        ToolRegistry::new(),
        50,
        None,
    ));

    let agent_clone = agent.clone();
    let handle = std::thread::spawn(move || {
        for _ in 0..1000 {
            let _ = agent_clone.id();
            let _ = agent_clone.id_arc();
            agent_clone.with_id(|id| {
                assert!(!id.as_str().is_empty());
            });
        }
    });

    for i in 0..100 {
        agent.set_id(AgentId::new(format!("agt_{i}")));
    }

    handle.join().unwrap();
}
