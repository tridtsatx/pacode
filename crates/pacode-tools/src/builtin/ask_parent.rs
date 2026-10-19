//! `ask_parent`: subagent → orchestrator question. The call blocks until the
//! orchestrator answers with `agent send` or completes a turn without one.
//! Subagent-only: the core removes it from the main agent's tool set, and the
//! host refuses it there too.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::parse_input;
use crate::host::ParentReply;
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "ask_parent";

/// Model-facing description; see `prompts/ask_parent.md`.
const DESCRIPTION: &str = include_str!("../prompts/ask_parent.md");

/// Longest question, in characters.
pub const MAX_QUESTION_CHARS: usize = 2000;
/// Most suggested answers attached to one question.
pub const MAX_OPTIONS: usize = 6;
/// Longest suggested answer, in characters.
pub const MAX_OPTION_CHARS: usize = 200;

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
    options: Vec<String>,
}

/// Trim and validate a question; `Err` is the message for the model.
pub fn check_question(question: &str) -> Result<String, ToolError> {
    let trimmed = question.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid("question is required"));
    }
    let len = trimmed.chars().count();
    if len > MAX_QUESTION_CHARS {
        return Err(ToolError::invalid(format!(
            "question is {len} characters; the limit is {MAX_QUESTION_CHARS}. Keep it short."
        )));
    }
    Ok(trimmed.to_string())
}

/// Drop blank options, cap their length, and keep at most [`MAX_OPTIONS`].
pub fn clean_options(options: Vec<String>) -> Vec<String> {
    options
        .into_iter()
        .map(|o| o.trim().chars().take(MAX_OPTION_CHARS).collect::<String>())
        .filter(|o| !o.is_empty())
        .take(MAX_OPTIONS)
        .collect()
}

/// The question as the orchestrator sees it: the text, then the suggested
/// answers (if any) as a numbered list.
pub fn compose_question(question: &str, options: &[String]) -> String {
    if options.is_empty() {
        return question.to_string();
    }
    let mut text = format!("{question}\n\nOptions:");
    for (i, option) in options.iter().enumerate() {
        text.push_str(&format!("\n{}. {option}", i + 1));
    }
    text
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
                "question": {"type": "string", "description": "Self-contained question, max 2000 chars."},
                "options": {
                    "type": "array",
                    "maxItems": MAX_OPTIONS,
                    "items": {"type": "string"},
                    "description": "The answers you see, if the choice is closed."
                }
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
        let question = check_question(&args.question)?;
        let options = clean_options(args.options);
        let reply = ctx
            .host
            .ask_parent(&ctx.call_id, compose_question(&question, &options))
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
