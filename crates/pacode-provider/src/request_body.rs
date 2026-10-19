//! Serialise a request body exactly once into shared [`Bytes`] (finding P1-1).
//!
//! The `serde_json::Value` tree is consumed here and dropped, so only one copy of the
//! (possibly multi-megabyte) payload stays alive while the request is in flight. Every
//! retry attempt re-sends a refcounted clone of the same bytes. Owner: the
//! `StreamOpener`/`CodexStreamOpener` that holds the `Bytes`; it releases them (replaces
//! with an empty `Bytes`) once the stream has committed and no retry is possible.

use bytes::Bytes;
use serde_json::Value;

use crate::ProviderError;

#[cfg(test)]
#[path = "request_body_tests.rs"]
mod request_body_tests;

/// Top-level keys of a body, for the debug log line.
pub(crate) fn body_keys(body: &Value) -> Vec<String> {
    body.as_object()
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Serialise `body` (consumed) into immutable bytes.
pub(crate) fn encode(body: Value) -> Result<Bytes, ProviderError> {
    serde_json::to_vec(&body)
        .map(Bytes::from)
        .map_err(|e| ProviderError::Config(format!("cannot serialise request body: {e}")))
}

/// Trace-log the body (image payloads and secrets redacted). Callers guard with
/// `log_enabled!(Trace)`; the work is skipped otherwise.
pub(crate) fn trace_body(bytes: &Bytes) {
    if !log::log_enabled!(log::Level::Trace) {
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    let redacted = crate::redact(&crate::vision::redact_data_urls(&text));
    log::trace!("request body: {redacted}");
}
