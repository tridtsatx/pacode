use std::sync::Arc;

use pacode_plugin::{PluginHost, PluginToolDef};

use super::*;

#[tokio::test]
async fn test_plugin_tool_metadata() {
    let host = Arc::new(PluginHost::new());
    let def = PluginToolDef {
        name: "my_plugin_tool".to_string(),
        description: "A test tool".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "arg1": { "type": "string" }
            }
        }),
    };

    let tool = PluginTool::new(host.clone(), def.clone());
    assert_eq!(tool.name(), "my_plugin_tool");
    assert_eq!(tool.description(), "A test tool");
    // Defaults to Exec to fail closed
    assert_eq!(tool.kind(), ToolKind::Exec);
    let schema = tool.schema();
    assert_eq!(schema["type"], "object");

    let tool_custom = PluginTool::with_kind(host, def, ToolKind::ReadOnly);
    assert_eq!(tool_custom.kind(), ToolKind::ReadOnly);
}
