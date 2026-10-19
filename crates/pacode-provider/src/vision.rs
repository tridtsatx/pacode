//! Which models accept image input, and the shared image helpers of the transports.
//!
//! Vision capability comes from, in order: the provider catalogue (`/models` entries
//! that declare input modalities, OpenRouter `architecture.input_modalities`) and
//! otherwise the id heuristic below. Unknown models are treated as text-only so the
//! user gets a clear pre-send error instead of a provider 400.
//!
//! Images are never stored in messages; transports load the bytes lazily from the
//! content-addressed store (`pacode_image::store`) when they build a request.

use serde_json::Value;

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
