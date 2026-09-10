//! Connection and mid-stream retries for the Anthropic Messages API.

use std::time::Duration;

use futures::StreamExt;
use pacode_types::{Effort, StreamEvent};
use serde_json::Value;

use super::stream::create_anthropic_event_stream;
use crate::{EventStream, ProviderError};

pub(crate) struct AnthropicStreamOpener {
    pub(crate) client: reqwest::Client,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Value,
    pub(crate) model: String,
    pub(crate) stream_idle_secs: u64,
    pub(crate) effort: Effort,
    pub(crate) max_retries: u32,
    pub(crate) backoff_base: Duration,
    pub(crate) is_oauth: bool,
}

impl AnthropicStreamOpener {
    pub(crate) async fn open(&self) -> Result<EventStream, ProviderError> {
        let response = self.post().await?;
        Ok(create_anthropic_event_stream(
            response,
            self.model.clone(),
            self.stream_idle_secs,
            self.effort,
            self.is_oauth,
        ))
    }

    async fn post(&self) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            let mut req_builder = self.client.post(&self.url);
            for (k, v) in &self.headers {
                req_builder = req_builder.header(k, v);
            }
            req_builder = req_builder.json(&self.body);

            match req_builder.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }

                    let status_u16 = status.as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    let redacted_body = crate::redact(&text);
                    log::trace!("anthropic response body: {redacted_body}");
                    log::warn!("anthropic request failed ({status_u16}): {redacted_body}");

                    if status_u16 == 401 || status_u16 == 403 {
                        return Err(ProviderError::Auth(format!("{status_u16}: {text}")));
                    }

                    let is_retryable = status_u16 == 429 || status_u16 >= 500;
                    if is_retryable && attempt < self.max_retries {
                        crate::retry::sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }

                    if status_u16 == 429 {
                        return Err(ProviderError::RateLimited(text));
                    }
                    if status_u16 == 529 {
                        return Err(ProviderError::RateLimited(format!("overloaded: {text}")));
                    }
                    return Err(ProviderError::Http {
                        status: status_u16,
                        message: text,
                    });
                }
                Err(err) => {
                    let err_str = err.to_string();
                    let redacted_err = crate::redact(&err_str);
                    log::warn!("anthropic request transport error: {redacted_err}");
                    if attempt < self.max_retries {
                        crate::retry::sleep_backoff(attempt, self.backoff_base).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(ProviderError::Transport(err.to_string()));
                }
            }
        }
    }
}

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

struct AnthropicRetryState {
    opener: AnthropicStreamOpener,
    inner: Option<EventStream>,
    attempt: u32,
    max_retries: u32,
    committed: bool,
    done: bool,
}

pub(crate) fn anthropic_retrying_stream(
    opener: AnthropicStreamOpener,
    initial: EventStream,
) -> EventStream {
    let max_retries = opener.max_retries;
    let state = AnthropicRetryState {
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
                            crate::retry::sleep_backoff(state.attempt, state.opener.backoff_base)
                                .await;
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
                            "anthropic stream failed before output (attempt {attempt}), reopening: {redacted}"
                        );
                        crate::retry::sleep_backoff(state.attempt, state.opener.backoff_base).await;
                        state.attempt += 1;
                        state.inner = None;
                        continue;
                    }
                    state.done = true;
                    return Some((Err(err), state));
                }
                None => return None,
            }
        }
    });

    Box::pin(stream)
}
