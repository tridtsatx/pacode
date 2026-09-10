//! Retry policy for streaming completions.
//!
//! Two layers:
//! - [`StreamOpener`] owns everything needed to (re)issue the `POST /chat/completions`
//!   and retries the *connection* (transport error, 429, 5xx) with exponential backoff.
//! - [`retrying_stream`] wraps the resulting [`EventStream`]: a retryable error that
//!   arrives *inside* the stream (gateways report upstream 429/5xx as an `error` object
//!   in an HTTP 200 body) reopens the request — but only while the turn has not emitted
//!   any output yet, otherwise a restart would duplicate text or tool calls.

use std::time::Duration;

use futures::StreamExt;
use pacode_types::{Effort, StreamEvent};
use rand::Rng;
use serde_json::Value;

use crate::{EventStream, ProviderError};

#[cfg(test)]
#[path = "retry_tests.rs"]
mod retry_tests;

/// First backoff step; doubles per attempt up to [`MAX_BACKOFF_STEPS`] and is jittered ±20%.
pub(crate) const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(1);
/// The delay stops growing after this many doublings (base × 32, capped at base × 30).
const MAX_BACKOFF_STEPS: u32 = 5;
/// Ceiling of the backoff expressed in multiples of the base delay (1 s base → 30 s).
const MAX_BACKOFF_FACTOR: f64 = 30.0;

/// Exponential backoff with jitter, expressed relative to `base` so the schedule can be
/// shortened where waiting whole seconds has no value (tests, tight interactive loops).
pub(crate) async fn sleep_backoff(attempt: u32, base: Duration) {
    let exp = 1u64
        .checked_shl(attempt.min(MAX_BACKOFF_STEPS))
        .unwrap_or(32);
    let base_secs = base.as_secs_f64();
    let delay_secs = {
        let capped = (base_secs * exp as f64).min(base_secs * MAX_BACKOFF_FACTOR);
        let mut rng = rand::rng();
        let jitter_ratio = rng.random_range(-0.2..=0.2);
        (capped * (1.0 + jitter_ratio)).max(0.0)
    };
    tokio::time::sleep(Duration::from_secs_f64(delay_secs)).await;
}

/// Everything needed to open (and reopen) one streaming completion, owned so the
/// resulting stream does not borrow the provider.
pub(crate) struct StreamOpener {
    pub(crate) client: reqwest::Client,
    pub(crate) url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Value,
    pub(crate) model: String,
    pub(crate) stream_idle_secs: u64,
    pub(crate) effort: Effort,
    pub(crate) max_retries: u32,
    pub(crate) backoff_base: Duration,
}

impl StreamOpener {
    /// POST the request, retrying the connection itself; returns the parsed event stream.
    pub(crate) async fn open(&self) -> Result<EventStream, ProviderError> {
        let response = self.post().await?;
        Ok(crate::sse::create_event_stream(
            response,
            self.model.clone(),
            self.stream_idle_secs,
            self.effort,
        ))
    }

    async fn post(&self) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            let mut req_builder = self
                .client
                .post(&self.url)
                .header("Content-Type", "application/json")
                .json(&self.body);

            if let Some(key) = &self.api_key
                && !key.is_empty()
            {
                req_builder = req_builder.header("Authorization", format!("Bearer {key}"));
            }

            for (k, v) in &self.headers {
                req_builder = req_builder.header(k, v);
            }

            match req_builder.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }

                    let status_u16 = status.as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    let redacted_body = crate::redact(&text);
                    log::trace!("response body: {redacted_body}");
                    log::warn!("request failed ({status_u16}): {redacted_body}");

                    if status_u16 == 401 || status_u16 == 403 {
                        return Err(ProviderError::Auth(format!("{status_u16}: {text}")));
                    }

                    let is_retryable = status_u16 == 429 || status_u16 >= 500;
                    if is_retryable && attempt < self.max_retries {
                        sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }

                    if status_u16 == 429 {
                        return Err(ProviderError::RateLimited(text));
                    }
                    return Err(ProviderError::Http {
                        status: status_u16,
                        message: text,
                    });
                }
                Err(err) => {
                    let err_str = err.to_string();
                    let redacted_err = crate::redact(&err_str);
                    log::warn!("request transport error: {redacted_err}");
                    if attempt < self.max_retries {
                        sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(ProviderError::Transport(err.to_string()));
                }
            }
        }
    }
}

/// `true` once the event carries something the caller has already shown or accumulated;
/// from that point on a restart would duplicate output, so errors are surfaced instead.
///
/// `Usage` and `MessageEnd` are latched too: replaying them would double-count tokens or
/// end the turn twice. `MessageStart` is not, because the inner stream only emits it
/// immediately before the first content event — no content emitted means no `MessageStart`.
fn is_committing(event: &StreamEvent) -> bool {
    matches!(
        event,
        StreamEvent::TextDelta { .. }
            | StreamEvent::ReasoningDelta { .. }
            | StreamEvent::ToolCallStart { .. }
            | StreamEvent::ToolCallArgsDelta { .. }
            | StreamEvent::Usage(_)
            | StreamEvent::MessageEnd { .. }
    )
}

struct RetryState {
    opener: StreamOpener,
    inner: Option<EventStream>,
    attempt: u32,
    max_retries: u32,
    committed: bool,
    done: bool,
}

/// Wrap `initial` so that a retryable failure before any output reopens the request.
pub(crate) fn retrying_stream(opener: StreamOpener, initial: EventStream) -> EventStream {
    let max_retries = opener.max_retries;
    let state = RetryState {
        opener,
        inner: Some(initial),
        attempt: 0,
        max_retries,
        committed: false,
        done: false,
    };

    let stream = futures::stream::unfold(state, |mut state| async move {
        loop {
            if state.done {
                return None;
            }

            if state.inner.is_none() {
                match state.opener.open().await {
                    Ok(inner) => state.inner = Some(inner),
                    Err(err) => {
                        if err.is_retryable() && state.attempt < state.max_retries {
                            sleep_backoff(state.attempt, state.opener.backoff_base).await;
                            state.attempt += 1;
                            continue;
                        }
                        state.done = true;
                        return Some((Err(err), state));
                    }
                }
            }

            let next = match state.inner.as_mut() {
                Some(inner) => inner.next().await,
                None => return None,
            };

            match next {
                Some(Ok(event)) => {
                    if is_committing(&event) {
                        state.committed = true;
                    }
                    return Some((Ok(event), state));
                }
                Some(Err(err)) => {
                    if !state.committed && err.is_retryable() && state.attempt < state.max_retries {
                        let redacted = crate::redact(&err.to_string());
                        let attempt = state.attempt;
                        log::warn!(
                            "stream failed before any output (attempt {attempt}), reopening: {redacted}"
                        );
                        sleep_backoff(state.attempt, state.opener.backoff_base).await;
                        state.attempt += 1;
                        state.inner = None;
                        continue;
                    }
                    state.done = true;
                    return Some((Err(err), state));
                }
                // Inner stream finished cleanly: nothing more to hand out.
                None => return None,
            }
        }
    });

    Box::pin(stream)
}
