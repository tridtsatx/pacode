//! The `checkpoint` tool: hand back a word, for the context-quality reading.
//!
//! The tool itself does nothing. It exists so a checkpoint has something the
//! model can *call*: an answer the model composes in a tool argument is
//! evidence it read the context, while an answer it composes in prose could be
//! anything — a guess, a paraphrase, or a repeat of the last thing it said.
//!
//! It is registered only when the client was started with `--analyze-mode`.
//! Exposed always, it would be a tool the model can call for its own reasons,
//! and a checkpoint the model asked for measures nothing.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "checkpoint";

const DESCRIPTION: &str = include_str!("../prompts/checkpoint.md");

/// Schema and definition are built once per registry: the tool takes one string
/// and returns nothing, and a tool that allocates a schema per turn is a tool
/// that costs more than it measures.
#[derive(Default)]
pub struct CheckpointTool {
    definition: DefinitionCache,
}

#[async_trait]
impl Tool for CheckpointTool {
    fn name(&self) -> &'static str {
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
            "properties": {
                "word": {
                    "type": "string",
                    "description": "The word from the instruction, exactly as written.",
                }
            },
            "required": ["word"],
            "additionalProperties": false,
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    async fn call(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        // The word is read by the turn loop out of the call's arguments, not
        // out of this result: the result only has to be *a* result, so that the
        // model's next step is a normal turn rather than an error.
        let _word = input
            .get("word")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid("checkpoint needs a `word` string"))?;
        Ok(ToolOutput::text("recorded"))
    }
}
