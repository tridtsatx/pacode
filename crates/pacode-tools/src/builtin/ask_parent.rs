//! `ask_parent`: subagent → orchestrator question. Subagent-only: the core
//! removes it from the main agent's tool set, and the host refuses it there too.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::parse_input;
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

#[cfg(test)]
#[path = "ask_parent_tests.rs"]
mod ask_parent_tests;

pub const NAME: &str = "ask_parent";

/// Longest question, in characters.
pub const MAX_QUESTION_CHARS: usize = 2000;
/// Most suggested answers attached to one question.
pub const MAX_OPTIONS: usize = 6;
/// Longest suggested answer, in characters.
pub const MAX_OPTION_CHARS: usize = 200;

pub struct AskParentTool;

#[derive(Deserialize)]
#[serde(default)]
struct AskParentInput {
    question: String,
    options: Vec<String>,
    wait: bool,
}

impl Default for AskParentInput {
    fn default() -> Self {
        Self {
            question: String::new(),
            options: Vec::new(),
            wait: true,
        }
    }
}

/// What the host reports back for one `ask_parent` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentReply {
    /// The orchestrator answered while the call was waiting.
    Answered { question_id: String, text: String },
    /// Nobody answered within `agents.ask_parent_timeout_secs`.
    TimedOut { question_id: String },
    /// The agent was stopped or interrupted while waiting.
    Cancelled { question_id: String },
    /// `wait = false`: the question is queued, the answer arrives later.
    Pending { question_id: String },
}

/// The model-visible result text for a reply.
pub fn reply_text(reply: &ParentReply) -> String {
    match reply {
        ParentReply::Answered { question_id, text } => {
            format!("The orchestrator answered {question_id}: {text}")
        }
        ParentReply::TimedOut { .. } => {
            "no answer from orchestrator; make a reasonable assumption and state it".to_string()
        }
        ParentReply::Cancelled { question_id } => format!(
            "Question {question_id} was withdrawn before it was answered; make a reasonable assumption and state it"
        ),
        ParentReply::Pending { question_id } => format!(
            "Question {question_id} sent to the orchestrator. Keep working; the answer arrives \
             later as <answer_from_orchestrator question=\"{question_id}\"> (or as a plain \
             <message_from_orchestrator>)."
        ),
    }
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

#[async_trait]
impl Tool for AskParentTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Ask the orchestrator (the agent that spawned you) a question. Ask only when you \
         are blocked and cannot make a reasonable assumption yourself; keep it short and \
         offer `options` when the choice is closed. With `wait` (default true) the call \
         blocks until the orchestrator answers, or until a timeout, after which you must \
         assume and state the assumption. With `wait: false` it returns at once and the \
         answer arrives later as `<answer_from_orchestrator>`. The number of questions \
         per agent is limited."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["question"],
            "properties": {
                "question": {"type": "string", "description": "Short question, max 2000 chars."},
                "options": {
                    "type": "array",
                    "maxItems": MAX_OPTIONS,
                    "items": {"type": "string"},
                    "description": "Suggested answers, if the choice is closed."
                },
                "wait": {"type": "boolean", "default": true, "description": "Block until answered (default) or return immediately."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _) = parse_input::<AskParentInput>(input)?;
        let question = check_question(&args.question)?;
        let options = clean_options(args.options);
        let short: String = question.chars().take(48).collect();
        let reply = ctx.host.ask_parent(question, options, args.wait).await?;
        let preview = match &reply {
            ParentReply::Answered { .. } => "answered",
            ParentReply::TimedOut { .. } => "no answer",
            ParentReply::Cancelled { .. } => "withdrawn",
            ParentReply::Pending { .. } => "sent",
        };
        Ok(ToolOutput::text(reply_text(&reply))
            .with_title(format!("ask parent: {short}"))
            .with_preview(preview))
    }
}
