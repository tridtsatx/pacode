//! Session title generation (spec §18.4): one short request after the first reply.

use std::sync::Arc;

use codeapp_provider::Provider;

/// Ask the model for a 3–6 word title in the language of the prompt. Returns `None`
/// on any error or an empty/overlong answer (title capped at 48 chars).
pub async fn generate_title(
    provider: Arc<dyn Provider>,
    model: &str,
    first_prompt: &str,
    first_answer: &str,
) -> Option<String> {
    let _ = (provider, model, first_prompt, first_answer);
    todo!("naming::generate_title")
}

/// Trim quotes/periods, collapse whitespace, cap at 48 chars.
pub fn clean_title(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '.')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() || cleaned.chars().count() > 48 {
        return None;
    }
    Some(cleaned)
}
