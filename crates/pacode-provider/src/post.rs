//! One HTTP POST per attempt with the connection-level retry loop, bounded
//! error-body reads and status classification shared by every HTTP transport.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pacode_types::Effort;

use crate::ProviderError;
use crate::error_body::MAX_ERROR_BODY_BYTES;
use crate::governor::{GovernorPermit, ProviderGovernor};
use crate::retry::{
    RetryBudget, error_body_read_timeout, note_failure, retry_after_from_response, retry_delay,
    sleep_delay,
};

#[cfg(test)]
#[path = "post_tests.rs"]
mod post_tests;

struct OpenFailure {
    error: ProviderError,
    retry_after: Option<Duration>,
}

pub(crate) struct HttpPost<'a> {
    pub label: &'static str,
    pub client: &'a reqwest::Client,
    pub url: &'a str,
    pub api_key: Option<&'a str>,
    pub headers: &'a [(String, String)],
    pub body: &'a Bytes,
    pub stream_idle_secs: u64,
    pub effort: Effort,
    pub backoff_base: Duration,
}

impl HttpPost<'_> {
    fn request(&self) -> reqwest::RequestBuilder {
        let mut request = self.client.post(self.url).body(self.body.clone());
        let has_content_type = self
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
        if !has_content_type {
            request = request.header("Content-Type", "application/json");
        }
        if let Some(key) = self.api_key
            && !key.is_empty()
        {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request
    }
}

pub(crate) async fn post_with_retries(
    post: &HttpPost<'_>,
    governor: &Arc<ProviderGovernor>,
    budget: &mut RetryBudget,
) -> Result<(reqwest::Response, GovernorPermit), ProviderError> {
    let queued_bytes = post.body.len() as u64;
    loop {
        let permit = governor.acquire(queued_bytes).await?;
        match post_once(post).await {
            Ok(response) => return Ok((response, permit)),
            Err(failure) => {
                let delay = retry_delay(post.backoff_base, budget.used, failure.retry_after);
                // Before the slot is released: the waiter it goes to then sees the
                // cooldown (and the shrunk limit) instead of firing into the same 429.
                note_failure(governor, &failure.error, failure.retry_after, delay);
                drop(permit);
                if failure.error.is_retryable() && budget.reserve() {
                    sleep_delay(delay).await;
                    continue;
                }
                return Err(failure.error);
            }
        }
    }
}

async fn post_once(post: &HttpPost<'_>) -> Result<reqwest::Response, OpenFailure> {
    // Bound the wait for response headers with the same "nothing heard yet"
    // budget the body phase uses; `stream_idle_secs == 0` leaves it unbounded.
    let timeout_secs = post
        .stream_idle_secs
        .saturating_mul(post.effort.idle_timeout_factor());
    let header_wait = (post.stream_idle_secs > 0).then(|| Duration::from_secs(timeout_secs.max(1)));
    let url = crate::redact(post.url);

    let send_result = match header_wait {
        Some(wait) => match tokio::time::timeout(wait, post.request().send()).await {
            Ok(result) => result,
            Err(_) => {
                log::warn!(
                    "{label} request to {url}: no response headers within {timeout_secs}s",
                    label = post.label
                );
                return Err(OpenFailure {
                    error: ProviderError::IdleTimeout(timeout_secs),
                    retry_after: None,
                });
            }
        },
        None => post.request().send().await,
    };

    match send_result {
        Ok(mut response) => {
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }

            let status_u16 = status.as_u16();
            let retry_after = retry_after_from_response(&response);
            let text = crate::error_body::read_response_body_with_limits(
                &mut response,
                MAX_ERROR_BODY_BYTES,
                error_body_read_timeout(header_wait),
            )
            .await;
            let redacted_body = crate::redact(&text);
            log::trace!("{label} response body: {redacted_body}", label = post.label);
            log::warn!(
                "{label} request to {url} failed ({status_u16}): {redacted_body}",
                label = post.label
            );

            Err(OpenFailure {
                error: classify_http_error(status_u16, &text),
                retry_after,
            })
        }
        Err(error) => {
            let message = error.to_string();
            log::warn!(
                "{label} request transport error: {redacted}",
                label = post.label,
                redacted = crate::redact(&message)
            );
            Err(OpenFailure {
                error: ProviderError::Transport(message),
                retry_after: None,
            })
        }
    }
}

fn classify_http_error(status: u16, body: &str) -> ProviderError {
    let message = crate::error_body_message(body);
    match status {
        401 | 403 => ProviderError::Auth(format!("{status}: {message}")),
        429 => ProviderError::RateLimited(message),
        529 => ProviderError::RateLimited(format!("overloaded: {message}")),
        _ => ProviderError::Http { status, message },
    }
}
