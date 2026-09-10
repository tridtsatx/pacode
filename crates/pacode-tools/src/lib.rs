//! Tool trait, `ToolHost` (the only door from tools into the core), and the built-in
//! tool set. Tools never see `Session`; everything they need arrives through
//! [`ToolCtx`].

pub mod builtin;
pub mod host;
pub mod output;
pub mod registry;
pub mod test_support;

pub use builtin::{builtin_tools, subagent_tool_names};
pub use host::{AgentSpec, ToolCtx, ToolHost, WaitOutcome};
pub use output::{ToolError, ToolOutput};
pub use registry::ToolRegistry;

use async_trait::async_trait;
use pacode_types::ToolDefinition;
use serde_json::Value;

/// What a tool does to the world; drives the permission matrix (spec §6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// Reads files or state only.
    ReadOnly,
    /// Writes files inside the workspace.
    Edit,
    /// Runs processes.
    Exec,
    /// Talks to the network.
    Network,
    /// Controls the session itself: plan, agents, background tasks.
    Control,
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Name sent to the model (snake_case, unique in the registry).
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema of the input object. `intent` and `accept_large_output` are added
    /// centrally by [`ensure_common_properties`].
    fn schema(&self) -> Value;
    fn kind(&self) -> ToolKind;
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError>;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: ensure_common_properties(self.schema()),
        }
    }
}

pub const INTENT_KEY: &str = "intent";
pub const ACCEPT_LARGE_OUTPUT_KEY: &str = "accept_large_output";

/// Add the shared `intent` (required) and `accept_large_output` (optional) properties
/// to an object schema. Port of jcode's `ensure_intent_in_schema`.
pub fn ensure_common_properties(mut schema: Value) -> Value {
    let Some(object) = schema.as_object_mut() else {
        return schema;
    };
    let is_object_schema = object
        .get("type")
        .and_then(Value::as_str)
        .map(|t| t == "object")
        .unwrap_or_else(|| object.contains_key("properties"));
    if !is_object_schema {
        return schema;
    }
    let properties = object
        .entry("properties")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(properties) = properties.as_object_mut() {
        properties.entry(INTENT_KEY).or_insert_with(|| {
            serde_json::json!({
                "type": "string",
                "description": "Required short label shown in the UI: why this call is being made."
            })
        });
        properties
            .entry(ACCEPT_LARGE_OUTPUT_KEY)
            .or_insert_with(|| {
                serde_json::json!({
                    "type": "boolean",
                    "description": "Re-run accepting the stated token cost of a withheld result."
                })
            });
    } else {
        return schema;
    }
    match object.get_mut("required") {
        Some(Value::Array(required)) => {
            if !required.iter().any(|v| v.as_str() == Some(INTENT_KEY)) {
                required.push(Value::String(INTENT_KEY.to_string()));
            }
        }
        Some(_) | None => {
            object.insert(
                "required".to_string(),
                Value::Array(vec![Value::String(INTENT_KEY.to_string())]),
            );
        }
    }
    schema
}

/// Pull the model-supplied `intent` out of a tool input.
pub fn intent_of(input: &Value) -> Option<String> {
    input
        .get(INTENT_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn common_properties_added_once() {
        let schema = serde_json::json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}});
        let out = ensure_common_properties(schema);
        assert!(out["properties"]["intent"].is_object());
        assert!(out["properties"]["accept_large_output"].is_object());
        let required: Vec<&str> = out["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(required, vec!["path", "intent"]);
        let again = ensure_common_properties(out.clone());
        assert_eq!(again, out);
    }
}
