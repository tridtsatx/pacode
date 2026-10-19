//! Retry policy shared by every provider transport: the backoff schedule, the
//! `Retry-After` plumbing and one state machine that reopens a stream which died
//! before it produced any output.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::StreamExt;
use pacode_types::{Effort, StreamEvent};
use rand::Rng;

use crate::error_body::{ERROR_BODY_READ_TIMEOUT, MAX_ERROR_BODY_BYTES};
use crate::gate::{GatePermit, GatedStream, MAX_QUEUE_WAIT, ProviderGate};
use crate::governor::{GovernorPermit, ProviderGovernor, governor_for, hold_permit};
use crate::post::HttpPost;
use crate::{EventStream, ProviderError};

#[cfg(test)]
#[path = "retry_tests.rs"]
mod retry_tests;

pub(crate) const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(1);
const MAX_BACKOFF_STEPS: u32 = 5;
const MAX_BACKOFF_FACTOR: u32 = 30;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
pub(crate) const MIN_429_COOLDOWN: Duration = Duration::from_millis(1);

pub(crate) fn backoff_delay(attempt: u32, base: Duration) -> Duration {
    let exponent = 1u32
        .checked_shl(attempt.min(MAX_BACKOFF_STEPS))
        .unwrap_or(32);
    let capped = base
        .saturating_mul(exponent)
        .min(base.saturating_mul(MAX_BACKOFF_FACTOR));
    let max_jitter = capped / 5;
    if max_jitter.is_zero() {
        return capped;
    }

    let max_jitter_nanos = u64::try_from(max_jitter.as_nanos()).unwrap_or(u64::MAX);
    let offset = Duration::from_nanos(rand::rng().random_range(0..=max_jitter_nanos));
    if rand::rng().random_bool(0.5) {
        capped.saturating_add(offset)
    } else {
        capped.saturating_sub(offset)
    }
}

pub(crate) async fn sleep_delay(delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

pub(crate) fn retry_delay(
    backoff_base: Duration,
    attempt: u32,
    retry_after: Option<Duration>,
) -> Duration {
    let fallback = backoff_delay(attempt, backoff_base);
    match retry_after {
        Some(retry_after) => retry_after.max(fallback),
        None => fallback,
    }
}

#[derive(Clone)]
pub(crate) struct RetryAfterSignal(Arc<AtomicU64>);

impl Default for RetryAfterSignal {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }
}

impl RetryAfterSignal {
    pub(crate) fn set(&self, delay: Duration) {
        let millis = delay
            .min(MAX_RETRY_AFTER)
            .as_millis()
            .min((u64::MAX - 1) as u128);
        self.0.fetch_max(millis as u64 + 1, Ordering::AcqRel);
    }

    pub(crate) fn take(&self) -> Option<Duration> {
        self.0
            .swap(0, Ordering::AcqRel)
            .checked_sub(1)
            .map(Duration::from_millis)
    }
}

pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }

    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 6 || !parts[0].ends_with(',') {
        return None;
    }
    let day = parts[1].parse::<u32>().ok()?;
    let month = match parts[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = parts[3].parse::<i64>().ok()?;
    let mut time = parts[4].split(':');
    let hour = time.next()?.parse::<u32>().ok()?;
    let minute = time.next()?.parse::<u32>().ok()?;
    let second = time.next()?.parse::<u32>().ok()?;
    if time.next().is_some()
        || !parts[5].eq_ignore_ascii_case("GMT")
        || hour > 23
        || minute > 59
        || second > 59
        || !(0..=9999).contains(&year)
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }

    let adjusted_year = year - if month <= 2 { 1_i64 } else { 0_i64 };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from((month + 9) % 12);
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))?;
    if seconds < 0 {
        return None;
    }
    let seconds = u64::try_from(seconds).ok()?;
    let target = UNIX_EPOCH.checked_add(Duration::from_secs(seconds))?;
    Some(match target.duration_since(now) {
        Ok(delay) => delay.min(MAX_RETRY_AFTER),
        Err(_) => Duration::ZERO,
    })
}

pub(crate) fn retry_after_value(value: &serde_json::Value) -> Option<Duration> {
    match value {
        serde_json::Value::String(value) => parse_retry_after(value, SystemTime::now()),
        serde_json::Value::Number(value) => value
            .as_u64()
            .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER)),
        _ => None,
    }
}

pub(crate) fn retry_after_from_response(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, SystemTime::now()))
}

pub(crate) fn error_body_read_timeout(idle_wait: Option<Duration>) -> Duration {
    match idle_wait {
        Some(idle_wait) => idle_wait.min(ERROR_BODY_READ_TIMEOUT),
        None => ERROR_BODY_READ_TIMEOUT,
    }
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

pub struct RetryBudget {
    remaining: u32,
    pub(crate) used: u32,
}

impl RetryBudget {
    pub fn new(max_retries: u32) -> Self {
        Self {
            remaining: max_retries,
            used: 0,
        }
    }

    pub(crate) fn reserve(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        self.used = self.used.saturating_add(1);
        true
    }
}

pub struct OpenedStream {
    pub(crate) stream: EventStream,
    pub(crate) retry_after: RetryAfterSignal,
}

pub(crate) trait StreamTransport: Clone + Send + Sync + 'static {
    fn governor(&self) -> Arc<ProviderGovernor>;
    fn backoff_base(&self) -> Duration;
    fn open_once(
        &self,
        governor: &Arc<ProviderGovernor>,
        budget: &mut RetryBudget,
    ) -> impl Future<Output = Result<OpenedStream, ProviderError>> + Send;
    /// A rate limit arrived inside an open stream (gateways report upstream 429s in a
    /// 200 body): transports with a [`ProviderGate`] shrink its adaptive limit.
    fn note_stream_rate_limit(&self) {}
    /// The stream committed (no reopen is possible any more): free the request payload.
    fn release_body(&mut self) {}
}

/// A server-overload status for [`ProviderGate::note_response`] standing in for a rate
/// limit reported without HTTP headers (inside the stream).
const STREAM_RATE_LIMIT_STATUS: u16 = 429;

/// One failed HTTP attempt of [`post_gated`]: the classified error plus what the
/// response said about pacing.
struct GatedFailure {
    error: ProviderError,
    /// Status and headers of an HTTP answer (`None` for a transport failure).
    response: Option<(u16, reqwest::header::HeaderMap)>,
}

/// [`crate::post::post_with_retries`] behind a [`ProviderGate`]: the gate's cooldown and
/// adaptive (AIMD) limit are waited out first, then the per-account governor.
///
/// Every 429/503 shrinks the gate's limit. A server pacing hint (`Retry-After`,
/// `x-ratelimit-reset-*`) becomes a provider-wide cooldown that every queued request
/// sleeps out instead of retrying on its own schedule; such hinted waits do not spend
/// the retry budget, bounded in total by [`MAX_QUEUE_WAIT`]. Everything else follows
/// the shared budget and backoff exactly like `post_with_retries`.
pub(crate) async fn post_gated(
    post: &HttpPost<'_>,
    gate: &ProviderGate,
    governor: &Arc<ProviderGovernor>,
    budget: &mut RetryBudget,
) -> Result<(reqwest::Response, GovernorPermit, GatePermit), ProviderError> {
    let started = tokio::time::Instant::now();
    let queued_bytes = post.body.len() as u64;
    loop {
        // Cancel-safe: dropping this future while it waits leaves nothing behind.
        let gate_permit = gate.acquire().await;
        let permit = governor.acquire(queued_bytes).await?;
        let failure = match send_once(post).await {
            Ok(response) => return Ok((response, permit, gate_permit)),
            Err(failure) => failure,
        };
        drop(permit);
        drop(gate_permit);

        let (hint, header_retry_after) = match &failure.response {
            Some((status, headers)) => (
                gate.note_response(*status, headers),
                headers
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| parse_retry_after(value, SystemTime::now())),
            ),
            None => (None, None),
        };
        let retry_after = header_retry_after.or(hint);
        let delay = retry_delay(post.backoff_base, budget.used, retry_after);
        if retry_after.is_some() || matches!(failure.error, ProviderError::RateLimited(_)) {
            governor.start_cooldown(delay.max(MIN_429_COOLDOWN));
        }
        if failure.error.is_retryable() && hint.is_some() && started.elapsed() < MAX_QUEUE_WAIT {
            // The shared cooldown paces this retry; the next `acquire` waits it out.
            continue;
        }
        if failure.error.is_retryable() && budget.reserve() {
            sleep_delay(delay).await;
            continue;
        }
        return Err(failure.error);
    }
}

/// One POST of `post`, keeping the status and headers of a failed answer for the gate.
/// Mirrors `post::post_once` (header wait bounded by the idle budget, bounded error-body
/// read, the same status classification).
async fn send_once(post: &HttpPost<'_>) -> Result<reqwest::Response, GatedFailure> {
    let timeout_secs = post
        .stream_idle_secs
        .saturating_mul(post.effort.idle_timeout_factor());
    let header_wait = (post.stream_idle_secs > 0).then(|| Duration::from_secs(timeout_secs.max(1)));
    let url = crate::redact(post.url);

    let mut request = post.client.post(post.url).body(post.body.clone());
    if !post
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        request = request.header("Content-Type", "application/json");
    }
    if let Some(key) = post.api_key
        && !key.is_empty()
    {
        request = request.header("Authorization", format!("Bearer {key}"));
    }
    for (name, value) in post.headers {
        request = request.header(name, value);
    }

    let sent = match header_wait {
        Some(wait) => match tokio::time::timeout(wait, request.send()).await {
            Ok(result) => result,
            Err(_) => {
                log::warn!(
                    "{label} request to {url}: no response headers within {timeout_secs}s",
                    label = post.label
                );
                return Err(GatedFailure {
                    error: ProviderError::IdleTimeout(timeout_secs),
                    response: None,
                });
            }
        },
        None => request.send().await,
    };

    match sent {
        Ok(mut response) => {
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            let status = status.as_u16();
            let headers = response.headers().clone();
            let text = crate::error_body::read_response_body_with_limits(
                &mut response,
                MAX_ERROR_BODY_BYTES,
                error_body_read_timeout(header_wait),
            )
            .await;
            let redacted_body = crate::redact(&text);
            log::trace!("{label} response body: {redacted_body}", label = post.label);
            log::warn!(
                "{label} request to {url} failed ({status}): {redacted_body}",
                label = post.label
            );
            let message = crate::error_body_message(&text);
            let error = match status {
                401 | 403 => ProviderError::Auth(format!("{status}: {message}")),
                429 => ProviderError::RateLimited(message),
                529 => ProviderError::RateLimited(format!("overloaded: {message}")),
                _ => ProviderError::Http { status, message },
            };
            Err(GatedFailure {
                error,
                response: Some((status, headers)),
            })
        }
        Err(error) => {
            let message = error.to_string();
            log::warn!(
                "{label} request transport error: {redacted}",
                label = post.label,
                redacted = crate::redact(&message)
            );
            Err(GatedFailure {
                error: ProviderError::Transport(message),
                response: None,
            })
        }
    }
}

#[derive(Clone)]
pub(crate) struct StreamOpener {
    pub(crate) client: reqwest::Client,
    pub(crate) url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    /// Serialised once (see `request_body`); each attempt sends a refcounted clone.
    /// Replaced by an empty `Bytes` once the stream has committed (no retry possible).
    pub(crate) body: Bytes,
    pub(crate) model: String,
    pub(crate) stream_idle_secs: u64,
    pub(crate) effort: Effort,
    pub(crate) backoff_base: Duration,
    /// Provider-wide concurrency permits and cooldown; shared with every other request.
    pub(crate) gate: Arc<ProviderGate>,
}

impl StreamOpener {
    /// POST the request (connection retries inside), then wrap the event stream in the
    /// mid-stream reopen state machine; both share one retry budget.
    pub(crate) async fn open(&self, mut budget: RetryBudget) -> Result<EventStream, ProviderError> {
        let governor = self.governor();
        let opened = self.open_once(&governor, &mut budget).await?;
        Ok(retrying_transport_stream(
            self.clone(),
            governor,
            opened,
            budget,
        ))
    }
}

impl StreamTransport for StreamOpener {
    fn governor(&self) -> Arc<ProviderGovernor> {
        governor_for(&self.url, self.api_key.as_deref(), &self.headers)
    }

    fn backoff_base(&self) -> Duration {
        self.backoff_base
    }

    async fn open_once(
        &self,
        governor: &Arc<ProviderGovernor>,
        budget: &mut RetryBudget,
    ) -> Result<OpenedStream, ProviderError> {
        let post = HttpPost {
            label: "openai-compatible",
            client: &self.client,
            url: &self.url,
            api_key: self.api_key.as_deref(),
            headers: &self.headers,
            body: &self.body,
            stream_idle_secs: self.stream_idle_secs,
            effort: self.effort,
            backoff_base: self.backoff_base,
        };
        let (response, permit, gate_permit) =
            post_gated(&post, &self.gate, governor, budget).await?;
        let retry_after = RetryAfterSignal::default();
        let inner = crate::sse::create_event_stream_with_retry_after(
            response,
            self.model.clone(),
            self.stream_idle_secs,
            self.effort,
            retry_after.clone(),
        );
        // Both permits live exactly as long as the stream (freed on end, error or drop);
        // the gate permit also credits the AIMD increase on the first event.
        Ok(OpenedStream {
            stream: GatedStream::wrap(hold_permit(inner, permit), gate_permit),
            retry_after,
        })
    }

    fn note_stream_rate_limit(&self) {
        self.gate
            .note_response(STREAM_RATE_LIMIT_STATUS, &reqwest::header::HeaderMap::new());
    }

    fn release_body(&mut self) {
        if !self.body.is_empty() {
            self.body = Bytes::new();
        }
    }
}

fn is_committing(event: &StreamEvent) -> bool {
    match event {
        StreamEvent::MessageStart { .. } => false,
        StreamEvent::TextDelta { .. }
        | StreamEvent::ReasoningDelta { .. }
        | StreamEvent::ReasoningSignature { .. }
        | StreamEvent::ToolCallStart { .. }
        | StreamEvent::ToolCallArgsDelta { .. }
        | StreamEvent::Usage(_)
        | StreamEvent::MessageEnd { .. } => true,
    }
}

struct TransportRetryState<T: StreamTransport> {
    opener: T,
    governor: Arc<ProviderGovernor>,
    inner: Option<OpenedStream>,
    budget: RetryBudget,
    committed: bool,
    done: bool,
}

pub(crate) fn retrying_transport_stream<T: StreamTransport>(
    opener: T,
    governor: Arc<ProviderGovernor>,
    initial: OpenedStream,
    budget: RetryBudget,
) -> EventStream {
    let state = TransportRetryState {
        opener,
        governor,
        inner: Some(initial),
        budget,
        committed: false,
        done: false,
    };

    Box::pin(futures::stream::unfold(state, |mut state| async move {
        loop {
            if state.done {
                return None;
            }

            if state.inner.is_none() {
                let governor = Arc::clone(&state.governor);
                match state.opener.open_once(&governor, &mut state.budget).await {
                    Ok(opened) => state.inner = Some(opened),
                    Err(error) => {
                        let delay =
                            retry_delay(state.opener.backoff_base(), state.budget.used, None);
                        if !state.committed && error.is_retryable() && state.budget.reserve() {
                            sleep_delay(delay).await;
                            continue;
                        }
                        state.done = true;
                        return Some((Err(error), state));
                    }
                }
            }

            let next = match state.inner.as_mut() {
                Some(opened) => opened.stream.next().await,
                None => return None,
            };

            match next {
                Some(Ok(event)) => {
                    if !state.committed && is_committing(&event) {
                        state.committed = true;
                        // No reopen is possible any more: free the request payload.
                        state.opener.release_body();
                    }
                    return Some((Ok(event), state));
                }
                Some(Err(error)) => {
                    let retry_after = state
                        .inner
                        .as_ref()
                        .and_then(|opened| opened.retry_after.take());
                    state.inner = None;
                    let is_rate_limited = matches!(&error, ProviderError::RateLimited(_));
                    let delay =
                        retry_delay(state.opener.backoff_base(), state.budget.used, retry_after);
                    if is_rate_limited || retry_after.is_some() {
                        state.governor.start_cooldown(delay.max(MIN_429_COOLDOWN));
                    }
                    if is_rate_limited {
                        state.opener.note_stream_rate_limit();
                    }

                    if !state.committed && error.is_retryable() && state.budget.reserve() {
                        let attempt = state.budget.used;
                        let redacted = crate::redact(&error.to_string());
                        log::warn!(
                            "stream failed before any output (attempt {attempt}), reopening: {redacted}"
                        );
                        sleep_delay(delay).await;
                        continue;
                    }

                    state.done = true;
                    return Some((Err(error), state));
                }
                None => return None,
            }
        }
    }))
}
