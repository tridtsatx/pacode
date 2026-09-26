use std::sync::Arc;

use async_trait::async_trait;
use pacode_skills::SkillRegistry;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "skill";

/// Model-facing description; see `prompts/skill.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/skill.md");

pub struct SkillTool {
    registry: Arc<SkillRegistry>,
    max_body_bytes: usize,
    definition: DefinitionCache,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>, max_body_bytes: usize) -> Self {
        Self {
            registry,
            max_body_bytes,
            definition: DefinitionCache::new(),
        }
    }
}

impl Default for SkillTool {
    fn default() -> Self {
        Self::new(Arc::new(SkillRegistry::default()), 16384)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SkillInput {
    name: String,
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        DESCRIPTION.trim()
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the skill to load."
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<SkillInput>(input)?;
        let name = args.name.trim();
        if name.is_empty() {
            return Err(ToolError::invalid("name cannot be empty"));
        }

        let body = self
            .registry
            .body(name, self.max_body_bytes)
            .map_err(|e| ToolError::failed(e.to_string()))?;

        let content = cap_output(&body, accept_large_output, ctx.output_cap_chars);
        let title = format!("Skill {name}");
        let preview = preview_body(&content);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}

#[cfg(test)]
#[path = "skill_tests.rs"]
mod skill_tests;
