use std::sync::Arc;

use async_trait::async_trait;
use pacode_skills::SkillRegistry;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "skill";

pub struct SkillTool {
    registry: Arc<SkillRegistry>,
    max_body_bytes: usize,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>, max_body_bytes: usize) -> Self {
        Self {
            registry,
            max_body_bytes,
        }
    }
}

impl Default for SkillTool {
    fn default() -> Self {
        Self {
            registry: Arc::new(SkillRegistry::default()),
            max_body_bytes: 16384,
        }
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
        "Load a skill's full instructions on demand: call it when one of the \
         skills listed in the system prompt matches the task. The body is \
         capped (~16K) and a truncated body ends with a `[... truncated ...]` \
         marker."
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
