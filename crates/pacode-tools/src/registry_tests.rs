//! Tool-gating tests: `tools.<name>.enabled = false` removes tools from the
//! registry and therefore from the `ToolDefinition` list sent to the model.

use std::sync::Arc;

use super::ToolRegistry;
use crate::builtin::glob::GlobTool;
use crate::builtin::read::ReadTool;

fn names(registry: &ToolRegistry) -> Vec<String> {
    registry
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect()
}

#[test]
fn disabled_tool_absent_from_definitions() {
    let mut registry = ToolRegistry::new()
        .with(Arc::new(ReadTool))
        .with(Arc::new(GlobTool));
    registry.apply_disabled(["read".to_string()]);

    assert_eq!(names(&registry), ["glob"]);
    assert!(registry.get("read").is_none());
    assert!(registry.get("glob").is_some());
}

#[test]
fn register_refuses_disabled_names() {
    let mut registry = ToolRegistry::new();
    registry.apply_disabled(["read".to_string()]);
    registry.register(Arc::new(ReadTool));
    assert!(registry.get("read").is_none());
    assert!(names(&registry).is_empty());
}

#[test]
fn subset_and_clones_keep_disabled_set() {
    let mut registry = ToolRegistry::new().with(Arc::new(ReadTool));
    registry.apply_disabled(["read".to_string(), "mcp__tool".to_string()]);
    registry.register(Arc::new(ReadTool)); // refused

    let subset = registry.subset(["read", "glob"]);
    assert!(subset.names().next().is_none());
    // A subset refuses late registrations of disabled names as well.
    let mut subset = registry.subset([]);
    subset.register(Arc::new(ReadTool));
    assert!(subset.get("read").is_none());
}

#[test]
fn unknown_disabled_names_are_harmless() {
    let mut registry = ToolRegistry::new().with(Arc::new(ReadTool));
    registry.apply_disabled(["no_such_tool".to_string()]);
    assert_eq!(names(&registry), ["read"]);
}
