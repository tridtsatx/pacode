//! Tool trait, `ToolHost` (the only door from tools into the core), and the built-in
//! tool set. Tools never see `Session`; everything they need arrives through
//! [`ToolCtx`].

pub mod builtin;
pub mod host;
pub mod output;
pub mod prompts;
pub mod registry;
pub mod test_support;
pub mod walk;

pub use builtin::{builtin_tools, subagent_tool_names};
pub use host::{AgentSpec, PermissionOutcome, ToolCtx, ToolHost, WaitOutcome};
pub use output::{ToolError, ToolOutput};
pub use registry::ToolRegistry;

use std::sync::OnceLock;

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

/// Memoised [`ToolDefinition`] owned by the tool that owns the schema.
///
/// `ToolRegistry::definitions()` is called on every model step, and building a
/// definition means assembling a `serde_json::Value` from a `json!` macro and
/// then running [`ensure_common_properties`] over it — for `symbols` that schema
/// is the single most expensive thing on the path. The result is a pure function
/// of `name`/`description`/`schema()`, so it is computed once per tool and
/// cloned (a `ToolDefinition` clone is a few allocations, far cheaper than the
/// build).
///
/// The cell is a `OnceLock` and there is deliberately no `invalidate`. That is a
/// conclusion, not an omission: the definition is a pure function of
/// `name`/`description`/`schema()`, and those come from fields the tool owns for
/// its whole life. A tool whose schema *can* change does not mutate itself — the
/// owner of the tool set re-lists and builds a **new** proxy with a **new**
/// cache (`mcp_tools` after a pool refresh, `plugin_tools` after a host
/// re-scan), so the old object and its cache go out of scope together. A stale
/// schema is therefore not reachable, and an invalidation hook would be a
/// documented way to introduce one.
///
/// `OnceLock` over a lock because there is nothing to clear: the read path is a
/// single atomic load, the write happens once per tool at startup, and
/// `get_or_init` gives the same "first writer wins, both callers see one value"
/// guarantee the hand-rolled version had to assemble from two lock scopes.
#[derive(Debug, Default)]
pub struct DefinitionCache(OnceLock<ToolDefinition>);

impl DefinitionCache {
    pub fn new() -> Self {
        Self(OnceLock::new())
    }

    /// Cached definition, building it with `build` on the first call.
    ///
    /// The closure is not run while the cell is claimed, so a `build` that
    /// re-entered this cache would recurse rather than deadlock. Two threads
    /// racing the very first build may both run it; `get_or_init` keeps the
    /// first value and hands it to both callers, and both were built from the
    /// same schema anyway.
    pub fn get(&self, build: impl FnOnce() -> ToolDefinition) -> ToolDefinition {
        self.0.get_or_init(build).clone()
    }
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

    /// This tool's definition cache, or `None` to not memoise.
    ///
    /// Opt-in rather than required: a tool memoises by holding a
    /// [`DefinitionCache`] field and returning a reference to it, and a tool
    /// with no cache simply leaves this alone and rebuilds its definition per
    /// call. The default is what a test stub or a one-off tool wants; the
    /// built-ins all cache, because [`ToolRegistry::definitions`] runs on every
    /// model step.
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        None
    }

    /// The definition advertised to the model, memoised when the tool holds a
    /// cache (see [`definition_cache`](Self::definition_cache)).
    fn definition(&self) -> ToolDefinition {
        let build = || ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: ensure_common_properties(self.schema()),
        };
        match self.definition_cache() {
            Some(cache) => cache.get(build),
            None => build(),
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
                    "description": "Return the full output instead of the head+tail cut; costs the size the cut marker reported."
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
