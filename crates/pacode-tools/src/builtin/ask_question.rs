//! `ask_question`: put a choice to the user and wait for the answer.

use async_trait::async_trait;
use pacode_types::QuestionOption;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

#[cfg(test)]
#[path = "ask_question_tests.rs"]
mod ask_question_tests;

pub const NAME: &str = "ask_question";

/// Model-facing description; see `prompts/ask_question.md` for the convention.
const DESCRIPTION: &str = include_str!("../prompts/ask_question.md");

#[derive(Default)]
pub struct AskQuestionTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AskInput {
    question: String,
    header: String,
    options: Vec<OptionInput>,
    multi_select: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OptionInput {
    label: String,
    description: String,
    recommended: bool,
}

#[async_trait]
impl Tool for AskQuestionTool {
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
            "required": ["question", "options"],
            "properties": {
                "question": {"type": "string", "description": "The full question, ending in a question mark."},
                "header": {"type": "string", "description": "Very short chip above it, e.g. `Storage`."},
                "multi_select": {"type": "boolean", "default": false, "description": "Allow several options at once."},
                "options": {
                    "type": "array",
                    "minItems": pacode_types::MIN_OPTIONS,
                    "maxItems": pacode_types::MAX_OPTIONS,
                    "items": {
                        "type": "object",
                        "required": ["label", "description"],
                        "properties": {
                            "label": {"type": "string", "description": "1-5 words."},
                            "description": {"type": "string", "description": "Upside, downside, pitfall."},
                            "recommended": {"type": "boolean", "default": false}
                        }
                    }
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _): (AskInput, bool) = parse_input(input)?;

        // Refused here, before the question reaches the host: the picker can only
        // highlight one recommendation, and a correctable input error is cheaper
        // than a question that fails downstream.
        let recommended = args.options.iter().filter(|o| o.recommended).count();
        if recommended > 1 {
            return Err(ToolError::invalid(format!(
                "at most one option may be marked as recommended, {recommended} were"
            )));
        }

        let options: Vec<QuestionOption> = args
            .options
            .into_iter()
            .map(|o| {
                let option = QuestionOption::new(o.label, o.description);
                if o.recommended {
                    option.recommended()
                } else {
                    option
                }
            })
            .collect();

        let question_text = args.question.clone();
        // Keep the labels as sent, so the answer can be reported as the words the
        // user saw rather than as bare indices.
        let labels: Vec<String> = options.iter().map(|o| o.label.clone()).collect();
        let answer = ctx
            .host
            .ask_question(
                &ctx.call_id,
                args.header,
                args.question,
                options,
                args.multi_select,
            )
            .await?;

        let content = answer_text(&answer, &labels, &question_text);
        let preview = preview_body(&content);
        Ok(ToolOutput::text(content)
            .with_title(format!("ask {}", short(&question_text)))
            .with_preview(preview))
    }
}

fn short(question: &str) -> String {
    let trimmed = question.trim();
    if trimmed.chars().count() <= 48 {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(47).collect();
    format!("{head}…")
}

fn answer_text(answer: &pacode_types::QuestionAnswer, labels: &[String], question: &str) -> String {
    if answer.cancelled {
        return format!(
            "The user dismissed the question without answering: {}",
            short(question)
        );
    }
    let mut parts = Vec::new();
    let chosen: Vec<&str> = answer
        .selected
        .iter()
        .filter_map(|i| labels.get(*i).map(String::as_str))
        .collect();
    if !chosen.is_empty() {
        parts.push(format!("the user chose: {}", chosen.join(", ")));
    }
    if let Some(text) = answer.free_text.as_ref().filter(|t| !t.trim().is_empty()) {
        parts.push(format!("the user wrote: {text}"));
    }
    if parts.is_empty() {
        return "The user answered with nothing.".to_string();
    }
    parts.join("\n")
}
