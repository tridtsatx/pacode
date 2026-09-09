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

    let tool = PluginTool::new(host, def);
    assert_eq!(tool.name(), "my_plugin_tool");
    assert_eq!(tool.description(), "A test tool");
    assert_eq!(tool.kind(), ToolKind::Network);
    let schema = tool.schema();
    assert_eq!(schema["type"], "object");
}
