//! Human-readable extraction of provider error response bodies.
//!
//! Providers report HTTP failures as JSON in a few shapes (`{"error":{"message":..,
//! "type":..}}` for Anthropic/OpenAI, `{"message":..}`, `{"error":".."}`), while
//! proxies and gateways answer with plain text or HTML pages. [`ProviderError`]
//! messages reach the chat UI, so raw JSON must not leak: this module distils a
//! body into one bounded, readable line. The untouched body still goes to
//! `log::warn!`/`log::trace!` at the call sites (through [`crate::redact`]).

use std::time::{Duration, Instant};

use serde_json::Value;

#[cfg(test)]
#[path = "error_body_tests.rs"]
mod error_body_tests;

/// Cap for the surfaced text — everything that can reach the chat UI or the model
/// context is bounded (spec §6.4).
const MAX_ERROR_BODY_CHARS: usize = 300;
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
pub(crate) const ERROR_BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Distil an HTTP error response body into a short human-readable message.
///
/// JSON bodies are unpacked — `error.message` (prefixed by `error.type`, or
/// `error.code` for Codex/Connect-style payloads), top-level `message`, `error`
/// as a bare string (detailed by `error_description`), `detail`,
/// `error.description`. A non-string leaf (e.g. `error.message` being an object)
/// is skipped, not serialized. Anything unrecognised — plain text, HTML pages,
/// non-object JSON — is returned trimmed and capped at [`MAX_ERROR_BODY_CHARS`]
/// on a char boundary.
pub fn error_body_message(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(empty response body)".to_string();
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Some(extracted) = extract_message(&value)
    {
        return truncate_chars(&extracted, MAX_ERROR_BODY_CHARS);
    }

    truncate_chars(trimmed, MAX_ERROR_BODY_CHARS)
}

/// Pull a message string out of a parsed body; `None` when the shape is unknown.
fn extract_message(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => extract_from_object(map),
        // A bare JSON string *is* the message; other scalars and arrays follow no
        // convention we know, so the caller falls back to the capped raw body.
        Value::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        _ => None,
    }
}

fn extract_from_object(map: &serde_json::Map<String, Value>) -> Option<String> {
    // `error.message` — the Anthropic/OpenAI shape. `error.type` (or `error.code`
    // where no `type` exists) prefixes it when it carries extra information.
    if let Some(Value::Object(err)) = map.get("error")
        && let Some(msg) = non_empty_str(err.get("message"))
    {
        let kind = non_empty_str(err.get("type")).or_else(|| non_empty_str(err.get("code")));
        return Some(match kind {
            Some(kind) if kind != msg => format!("{kind}: {msg}"),
            _ => msg.to_string(),
        });
    }

    if let Some(msg) = non_empty_str(map.get("message")) {
        return Some(msg.to_string());
    }

    // `error` as a bare string (OAuth-style), detailed by `error_description`.
    if let Some(msg) = non_empty_str(map.get("error")) {
        let detail = non_empty_str(map.get("error_description"));
        return Some(match detail {
            Some(detail) if detail != msg => format!("{msg}: {detail}"),
            _ => msg.to_string(),
        });
    }

    if let Some(msg) = non_empty_str(map.get("detail")) {
        return Some(msg.to_string());
    }

    if let Some(Value::Object(err)) = map.get("error")
        && let Some(msg) = non_empty_str(err.get("description"))
    {
        return Some(msg.to_string());
    }

    non_empty_str(map.get("error_description")).map(str::to_string)
}

/// `value` as a trimmed string, `None` when absent, not a string, or blank.
fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Error body for a request with no idle budget of its own (catalogs, auth):
/// capped in size and in time so a broken endpoint cannot stream forever.
pub(crate) async fn read_error_body(response: &mut reqwest::Response) -> String {
    read_response_body_with_limits(response, MAX_ERROR_BODY_BYTES, ERROR_BODY_READ_TIMEOUT).await
}

pub(crate) async fn read_response_body_with_limits(
    response: &mut reqwest::Response,
    max_bytes: usize,
    read_timeout: Duration,
) -> String {
    let mut body = Vec::with_capacity(max_bytes.min(8192));
    let deadline = Instant::now().checked_add(read_timeout);
    while body.len() < max_bytes {
        let next = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, response.chunk()).await {
                Ok(result) => result,
                Err(_) => break,
            }
        } else {
            response.chunk().await
        };

        match next {
            Ok(Some(chunk)) => {
                append_response_body_chunk(&mut body, &chunk, max_bytes);
            }
            Ok(None) | Err(_) => break,
        }
    }
    let decoded = String::from_utf8_lossy(&body);
    truncate_bytes(&decoded, max_bytes)
}

fn truncate_bytes(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_string()
}

fn append_response_body_chunk(body: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(body.len());
    let take = chunk.len().min(remaining);
    body.extend_from_slice(&chunk[..take]);
}

/// Truncate to `max_chars` on a char boundary, appending `…` when truncated.
fn truncate_chars(input: &str, max_chars: usize) -> String {
    match input.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &input[..end]),
        None => input.to_string(),
    }
}
