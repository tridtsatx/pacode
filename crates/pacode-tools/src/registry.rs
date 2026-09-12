//! Named tool registry with per-agent subsets.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use pacode_types::ToolDefinition;

use crate::{Tool, ToolKind};

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Names disabled via `tools.<name>.enabled = false`: refused by `register`
    /// (so plugin/MCP tools joining later are gated too) and absent from
    /// `definitions`, i.e. from the schema sent to the model.
    disabled: Arc<HashSet<String>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        if self.disabled.contains(tool.name()) {
            log::debug!(
                "tool '{}' is disabled via config; not registered",
                tool.name()
            );
            return;
        }
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Drop registered tools whose names are disabled and refuse future
    /// registrations of those names. Called once with `Config::disabled_tool_names`
    /// when the daemon wires the shared registry; clones and subsets keep the set.
    pub fn apply_disabled(&mut self, names: impl IntoIterator<Item = String>) {
        let set: HashSet<String> = names.into_iter().collect();
        if set.is_empty() {
            return;
        }
        self.tools.retain(|name, _| !set.contains(name));
        self.disabled = Arc::new(set);
    }

    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Definitions for the model, in name order (stable for prompt caching).
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    /// A registry restricted to `names` (unknown names ignored).
    pub fn subset<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> ToolRegistry {
        let mut out = ToolRegistry {
            tools: BTreeMap::new(),
            disabled: Arc::clone(&self.disabled),
        };
        for name in names {
            if let Some(tool) = self.tools.get(name) {
                out.register(tool.clone());
            }
        }
        out
    }

    /// Only tools of the given kinds (e.g. read-only set for Plan mode or subagents).
    pub fn filter_kinds(&self, kinds: &[ToolKind]) -> ToolRegistry {
        let mut out = ToolRegistry {
            tools: BTreeMap::new(),
            disabled: Arc::clone(&self.disabled),
        };
        for tool in self.tools.values() {
            if kinds.contains(&tool.kind()) {
                out.register(tool.clone());
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
