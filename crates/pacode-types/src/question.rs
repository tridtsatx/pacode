//! A question the model asks the user mid-turn, and the answer it waits for.
//!
//! It works like a permission prompt: the turn blocks until the user answers, and
//! the answer comes back as the tool's result. Unlike a permission prompt it is
//! the model's own question, with its own options, one of which may be marked as
//! recommended.

#[cfg(test)]
#[path = "question_tests.rs"]
mod question_tests;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, CallId, QuestionId};

/// Caps on everything the model writes into a question, so an overlong option
/// cannot break the picker or an unbounded prompt reach the transcript.
pub const HEADER_MAX_CHARS: usize = 16;
pub const QUESTION_MAX_CHARS: usize = 1000;
pub const LABEL_MAX_CHARS: usize = 80;
pub const DESCRIPTION_MAX_CHARS: usize = 400;
/// Fewest and most options a question may offer. Two is the point of asking; more
/// than six stops being a choice and starts being a list.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 6;
/// Cap on the free-text answer the user can type instead of picking an option.
pub const FREE_TEXT_MAX_CHARS: usize = 2000;

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.trim().to_string();
    }
    text.chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// One choice offered by a question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    /// What choosing this means: trade-offs, consequences, pitfalls.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Exactly one option should carry this; the picker marks it.
    #[serde(default)]
    pub recommended: bool,
}

impl QuestionOption {
    pub fn new(label: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            label: cap(&label.into(), LABEL_MAX_CHARS),
            description: cap(&description.into(), DESCRIPTION_MAX_CHARS),
            recommended: false,
        }
    }

    pub fn recommended(mut self) -> Self {
        self.recommended = true;
        self
    }
}

/// Who asked, and on behalf of which tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOrigin {
    pub agent: AgentId,
    pub agent_name: String,
    pub call_id: CallId,
}

impl QuestionOrigin {
    pub fn new(agent: AgentId, agent_name: impl Into<String>, call_id: CallId) -> Self {
        Self {
            agent,
            agent_name: agent_name.into(),
            call_id,
        }
    }
}

/// A pending question. Mirrors `PermissionRequest`: the turn waits on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: QuestionId,
    pub agent: AgentId,
    pub agent_name: String,
    pub call_id: CallId,
    /// Very short chip above the question, e.g. `Storage`.
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    /// Whether several options may be chosen at once.
    #[serde(default)]
    pub multi_select: bool,
    pub created_at_ms: u64,
}

/// Why a question could not be asked. `pacode-types` carries no error crate, so
/// this spells out `Display` the way `cron_expr` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionError {
    TooFewOptions(usize),
    TooManyOptions(usize),
    EmptyQuestion,
    EmptyLabel(usize),
    ManyRecommended(usize),
}

impl std::fmt::Display for QuestionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewOptions(n) => {
                write!(
                    f,
                    "a question needs at least {MIN_OPTIONS} options, got {n}"
                )
            }
            Self::TooManyOptions(n) => {
                write!(f, "a question takes at most {MAX_OPTIONS} options, got {n}")
            }
            Self::EmptyQuestion => write!(f, "the question text is empty"),
            Self::EmptyLabel(i) => write!(f, "option {i} has an empty label"),
            Self::ManyRecommended(n) => {
                write!(f, "only one option may be marked as recommended, {n} were")
            }
        }
    }
}

impl std::error::Error for QuestionError {}

impl Question {
    /// Build a question, capping every field and rejecting a shape the picker
    /// could not present honestly.
    pub fn new(
        id: QuestionId,
        origin: QuestionOrigin,
        header: impl Into<String>,
        question: impl Into<String>,
        options: Vec<QuestionOption>,
        multi_select: bool,
        created_at_ms: u64,
    ) -> Result<Self, QuestionError> {
        let question = cap(&question.into(), QUESTION_MAX_CHARS);
        if question.is_empty() {
            return Err(QuestionError::EmptyQuestion);
        }
        if options.len() < MIN_OPTIONS {
            return Err(QuestionError::TooFewOptions(options.len()));
        }
        if options.len() > MAX_OPTIONS {
            return Err(QuestionError::TooManyOptions(options.len()));
        }
        for (i, option) in options.iter().enumerate() {
            if option.label.trim().is_empty() {
                return Err(QuestionError::EmptyLabel(i));
            }
        }
        let recommended = options.iter().filter(|o| o.recommended).count();
        if recommended > 1 {
            return Err(QuestionError::ManyRecommended(recommended));
        }

        let options = options
            .into_iter()
            .map(|o| QuestionOption {
                label: cap(&o.label, LABEL_MAX_CHARS),
                description: cap(&o.description, DESCRIPTION_MAX_CHARS),
                recommended: o.recommended,
            })
            .collect();

        Ok(Self {
            id,
            agent: origin.agent,
            agent_name: origin.agent_name,
            call_id: origin.call_id,
            header: cap(&header.into(), HEADER_MAX_CHARS),
            question,
            options,
            multi_select,
            created_at_ms,
        })
    }

    /// Index of the recommended option, when there is one.
    pub fn recommended_index(&self) -> Option<usize> {
        self.options.iter().position(|o| o.recommended)
    }
}

/// What the user chose.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// Indices into `Question::options`, in the order they were picked.
    #[serde(default)]
    pub selected: Vec<usize>,
    /// Text the user typed instead of, or alongside, picking an option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
    /// The user dismissed the question without answering.
    #[serde(default)]
    pub cancelled: bool,
}

impl QuestionAnswer {
    pub fn cancelled() -> Self {
        Self {
            cancelled: true,
            ..Self::default()
        }
    }

    pub fn choice(index: usize) -> Self {
        Self {
            selected: vec![index],
            ..Self::default()
        }
    }

    pub fn typed(text: impl Into<String>) -> Self {
        Self {
            free_text: Some(cap(&text.into(), FREE_TEXT_MAX_CHARS)),
            ..Self::default()
        }
    }

    /// The answer as the model sees it: the chosen labels, then any typed text.
    pub fn render(&self, question: &Question) -> String {
        if self.cancelled {
            return "The user dismissed the question without answering.".to_string();
        }
        let mut parts: Vec<String> = self
            .selected
            .iter()
            .filter_map(|i| question.options.get(*i))
            .map(|o| o.label.clone())
            .collect();
        if let Some(text) = self.free_text.as_ref().filter(|t| !t.trim().is_empty()) {
            parts.push(cap(text, FREE_TEXT_MAX_CHARS));
        }
        if parts.is_empty() {
            return "The user answered with nothing.".to_string();
        }
        parts.join("\n")
    }
}
