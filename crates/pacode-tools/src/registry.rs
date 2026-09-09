//! Named tool registry with per-agent subsets.

use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_types::ToolDefinition;

use crate::{Tool, ToolKind};

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
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
