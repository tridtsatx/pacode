use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

#[test]
fn result_action_is_in_schema_and_description() {
    let schema = AgentTool.schema();
    let actions = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("enum");
    assert!(actions.iter().any(|a| a == "result"));
    assert!(AgentTool.description().contains("result"));
}

#[tokio::test]
async fn result_requires_a_known_agent_id() {
    let ctx = stub_ctx(Arc::new(StubToolHost::default()));
    let missing = AgentTool
        .call(json!({"action": "result"}), &ctx)
        .await
        .unwrap_err();
    assert!(
        missing.to_string().contains("agent_id is required"),
        "{missing}"
    );

    let unknown = AgentTool
        .call(json!({"action": "result", "agent_id": "ag_nope"}), &ctx)
        .await
        .unwrap_err();
    assert!(unknown.to_string().contains("unknown agent"), "{unknown}");
}
