//! `ask_parent`: subagent → orchestrator question. The call blocks until the
//! orchestrator answers with `agent send` or completes a turn without one.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::parse_input;
use crate::host::ParentReply;
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "ask_parent";

/// Model-facing description; see `prompts/ask_parent.md`.
const DESCRIPTION: &str = include_str!("../prompts/ask_parent.md");

/// The result when the orchestrator completed a turn without answering.
pub const UNANSWERED: &str = "The orchestrator ended its turn without answering. Proceed with \
     your best judgment and state the assumption in your final report.";

/// The result when the orchestrator's turn failed with the question in it.
pub const PARENT_FAILED: &str = "The orchestrator's turn failed before it answered. Proceed with \
     your best judgment and state the assumption in your final report.";

/// The result when earlier questions kept going unanswered: this one was not
/// sent, so the orchestrator is not woken for a question it will not take.
pub const NOT_ANSWERING: &str = "The orchestrator is not answering your questions, so this one \
     was not sent. Proceed on your own judgment and state your assumptions in your final \
     report.";

#[derive(Default)]
pub struct AskParentTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AskParentInput {
    question: String,
}

#[async_trait]
impl Tool for AskParentTool {
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
            "required": ["question"],
            "properties": {
                "question": {"type": "string", "description": "Self-contained question, with the options you see."}
            }
        })
    }

    fn can_exceed_output_cap(&self) -> bool {
        false
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _) = parse_input::<AskParentInput>(input)?;
        let question = args.question.trim();
        if question.is_empty() {
            return Err(ToolError::invalid("question is required"));
        }
        let reply = ctx
            .host
            .ask_parent(&ctx.call_id, question.to_string())
            .await?;
        let (content, preview) = match reply {
            ParentReply::Answered(answer) => (
                format!("The orchestrator answered:\n\n{answer}"),
                "answered",
            ),
            ParentReply::Unanswered => (UNANSWERED.to_string(), "no answer"),
            ParentReply::ParentFailed => (PARENT_FAILED.to_string(), "no answer"),
            ParentReply::NotAnswering => (NOT_ANSWERING.to_string(), "not sent"),
        };
        Ok(ToolOutput::text(content)
            .with_title("Ask orchestrator")
            .with_preview(preview))
    }
}

#[cfg(test)]
#[path = "ask_parent_tests.rs"]
mod ask_parent_tests;
