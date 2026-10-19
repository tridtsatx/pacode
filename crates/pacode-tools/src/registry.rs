//! Named tool registry with per-agent subsets.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use pacode_types::ToolDefinition;

use crate::{Tool, ToolKind};

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Cached definitions. Cap: one slice per registry (tool count). Invalidation: reset by
    /// `register`. Owner: this registry (clones start with the same cached `Arc`).
    defs: OnceLock<Arc<[ToolDefinition]>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
        self.defs = OnceLock::new();
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
        self.definitions_arc().to_vec()
    }

    /// The same definitions as a shared slice, built once per registry state (see `defs`).
    pub fn definitions_arc(&self) -> Arc<[ToolDefinition]> {
        self.defs
            .get_or_init(|| self.tools.values().map(|tool| tool.definition()).collect())
            .clone()
    }

    /// A copy without the tool `name`.
    pub fn without(&self, name: &str) -> ToolRegistry {
        let mut out = self.clone();
        out.tools.remove(name);
        out.defs = OnceLock::new();
        out
    }

    /// A registry restricted to `names` (unknown names ignored).
    pub fn subset<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> ToolRegistry {
        let mut out = ToolRegistry::new();
        for name in names {
            if let Some(tool) = self.tools.get(name) {
                out.register(tool.clone());
            }
        }
        out
    }

    /// Only tools of the given kinds (e.g. read-only set for Plan mode or subagents).
    pub fn filter_kinds(&self, kinds: &[ToolKind]) -> ToolRegistry {
        let mut out = ToolRegistry::new();
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
