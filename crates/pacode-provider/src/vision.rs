//! Which models accept image input, and the shared image helpers of the transports.
//!
//! Vision capability comes from, in order: the provider catalogue (`/models` entries
//! that declare input modalities, OpenRouter `architecture.input_modalities`) and
//! otherwise the id heuristic below. Unknown models are treated as text-only so the
//! user gets a clear pre-send error instead of a provider 400.
//!
//! Images are never stored in messages; transports load the bytes lazily from the
//! content-addressed store (`pacode_image::store`) when they build a request.

use std::borrow::Cow;

use pacode_types::{ContentBlock, Message, Role};
use serde_json::Value;

use crate::ProviderError;

#[cfg(test)]
#[path = "vision_tests.rs"]
mod vision_tests;

/// Id heuristic for vision support. Conservative: `false` for anything unknown.
pub fn heuristic_vision(model: &str) -> bool {
    let id = model.to_ascii_lowercase();
    let base = id.rsplit('/').next().unwrap_or(&id);
    let has = |needles: &[&str]| needles.iter().any(|n| base.contains(n));

    if has(&[
        "gpt-4o",
        "gpt-4.1",
        "gpt-4-turbo",
        "gpt-5",
        "gemini",
        "gemma-3",
        "gemma3",
        "claude",
        "codex",
        "llava",
        "vision",
        "-vl",
        "vl-",
        "pixtral",
        "minicpm-v",
        "llama-4",
        "llama4",
        "kimi-k2.5",
        "kimi-k2.6",
        "kimi-k2.7",
        "kimi-k3",
        "mistral-small-3",
        "mistral-medium-3",
    ]) {
        return true;
    }
    // o-series: o1, o3, o4 families, minus the text-only minis of o1/o3.
    if (base.starts_with("o1") || base.starts_with("o3")) && !base.contains("mini") {
        return !base.contains("preview");
    }
    if base.starts_with("o4") {
        return true;
    }
    // GLM-*V: `glm-4v`, `glm-4.5v`, `glm-4.1v-thinking`.
    if base.starts_with("glm") {
        return base.split('-').skip(1).any(|tok| {
            tok.len() > 1 && tok.ends_with('v') && tok.as_bytes()[tok.len() - 2].is_ascii_digit()
        });
    }
    false
}

/// Vision flag declared by one `/models` catalogue entry, when it declares one.
pub fn catalog_vision(item: &Value) -> Option<bool> {
    let arch = item.get("architecture")?;
    if let Some(mods) = arch.get("input_modalities").and_then(Value::as_array) {
        return Some(mods.iter().any(|m| m.as_str() == Some("image")));
    }
    let modality = arch.get("modality").and_then(Value::as_str)?;
    let input = modality.split("->").next().unwrap_or(modality);
    Some(input.contains("image"))
}

/// `data:<mime>;base64,<payload>` for a stored image (payload cached, see `image_cache`).
pub(crate) fn data_url(mime: &str, hash: &str) -> Result<String, ProviderError> {
    let payload = crate::image_cache::payload(hash)?;
    Ok(format!("data:{mime};base64,{payload}"))
}

/// Base64 payload (no prefix) for a stored image (cached, see `image_cache`).
pub(crate) fn base64_payload(hash: &str) -> Result<String, ProviderError> {
    Ok(crate::image_cache::payload(hash)?.to_string())
}

/// Text standing in for an image the model cannot see.
pub(crate) fn omitted_note(source_path: Option<&str>) -> String {
    match source_path {
        Some(p) => format!("[image omitted: {p}]"),
        None => "[image omitted]".to_string(),
    }
}

/// Gate images for a model.
///
/// * vision model: messages pass through untouched;
/// * text-only model: images in the **last user message** (the one being sent now)
///   are a clear error `model <id> does not accept images`; older images (left over
///   from a model switch) become a one-line text note so history stays usable.
pub(crate) fn gate_images<'a>(
    messages: &'a [Message],
    model: &str,
    vision: bool,
) -> Result<Cow<'a, [Message]>, ProviderError> {
    if vision || !messages.iter().any(Message::has_images) {
        return Ok(Cow::Borrowed(messages));
    }
    let last_user = messages.iter().rposition(|m| m.role == Role::User);
    let tail_has_images = last_user.is_some_and(|i| messages[i..].iter().any(Message::has_images));
    if tail_has_images {
        return Err(ProviderError::Config(format!(
            "model {model} does not accept images"
        )));
    }
    let stripped: Vec<Message> = messages
        .iter()
        .map(|m| {
            if !m.has_images() {
                return m.clone();
            }
            let mut out = m.clone();
            out.content = m
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Image { source_path, .. } => ContentBlock::Text {
                        text: omitted_note(source_path.as_deref()),
                    },
                    ContentBlock::Text { .. }
                    | ContentBlock::Reasoning { .. }
                    | ContentBlock::ToolUse { .. }
                    | ContentBlock::ToolResult { .. } => b.clone(),
                })
                .collect();
            out
        })
        .collect();
    Ok(Cow::Owned(stripped))
}

/// Replace the payload of `data:<mime>;base64,<payload>` URLs by a size marker so
/// request bodies can be logged without megabytes of base64.
pub fn redact_data_urls(text: &str) -> String {
    const MARK: &str = ";base64,";
    let mut out = String::with_capacity(text.len().min(4096));
    let mut rest = text;
    while let Some(pos) = rest.find(MARK) {
        let (head, tail) = rest.split_at(pos + MARK.len());
        out.push_str(head);
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
            .unwrap_or(tail.len());
        out.push_str(&format!("<{end} bytes>"));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}
