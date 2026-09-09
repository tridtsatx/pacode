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
    use codeapp_provider::CompletionRequest;
    use codeapp_types::{Message, StreamEvent};
    use futures::StreamExt;

    let system_static = "Generate a concise 3-6 word title summarizing the conversation in the language of the user's prompt. Reply with ONLY the title, no quotation marks, no punctuation at the end, no extra words.".to_string();
    let prompt_user = format!("User: {first_prompt}\n\nAssistant: {first_answer}");
    let req = CompletionRequest {
        model: model.to_string(),
        system_static,
        system_dynamic: String::new(),
        messages: vec![Message::user(prompt_user)],
        tools: Vec::new(),
        effort: None,
        max_output_tokens: Some(40),
    };

    let mut stream = provider.complete(req).await.ok()?;
    let mut title_acc = String::new();
    while let Some(event_res) = stream.next().await {
        match event_res {
            Ok(StreamEvent::TextDelta { text }) => {
                title_acc.push_str(&text);
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }

    clean_title(&title_acc)
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
