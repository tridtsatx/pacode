//! `report_status`: subagent → orchestrator brief status.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::parse_input;
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "report_status";

/// Model-facing description; see `prompts/report_status.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/report_status.md");

#[derive(Default)]
pub struct ReportStatusTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReportStatusInput {
    text: String,
}

#[async_trait]
impl Tool for ReportStatusTool {
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
            "required": ["text"],
            "properties": {
                "text": {"type": "string", "description": "Brief status, capped at 1000 chars."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _) = parse_input::<ReportStatusInput>(input)?;
        let text = args.text.trim().to_string();
        if text.is_empty() {
            return Err(ToolError::invalid("text is required"));
        }
        let text: String = text.chars().take(1000).collect();
        ctx.host.report_status(text)?;
        Ok(ToolOutput::text("Status delivered.")
            .with_title("Report status")
            .with_preview("sent"))
    }
}
