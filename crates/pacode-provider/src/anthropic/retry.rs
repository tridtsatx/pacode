//! Connection and mid-stream retries for the Anthropic Messages API.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pacode_types::Effort;

use super::stream::create_anthropic_event_stream;
use crate::governor::{ProviderGovernor, governor_for, hold_permit};
use crate::post::{HttpPost, post_with_retries};
use crate::retry::{OpenedStream, RetryAfterSignal, RetryBudget, StreamTransport};
use crate::{EventStream, ProviderError};

#[derive(Clone)]
pub(crate) struct AnthropicStreamOpener {
    pub(crate) client: reqwest::Client,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    /// Body serialized once at request build; `Bytes` makes each retry a
    /// refcount bump instead of a DOM re-serialization.
    pub(crate) body: Bytes,
    pub(crate) model: String,
    pub(crate) stream_idle_secs: u64,
    pub(crate) effort: Effort,
    pub(crate) backoff_base: Duration,
    pub(crate) is_oauth: bool,
}

impl AnthropicStreamOpener {
    pub(crate) async fn open(&self, mut budget: RetryBudget) -> Result<EventStream, ProviderError> {
        let governor = self.governor();
        let opened = self.open_once(&governor, &mut budget).await?;
        Ok(crate::retry::retrying_transport_stream(
            self.clone(),
            governor,
            opened,
            budget,
        ))
    }
}

impl StreamTransport for AnthropicStreamOpener {
    fn governor(&self) -> Arc<ProviderGovernor> {
        governor_for(&self.url, None, &self.headers)
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
            label: "anthropic",
            client: &self.client,
            url: &self.url,
            api_key: None,
            headers: &self.headers,
            body: &self.body,
            stream_idle_secs: self.stream_idle_secs,
            effort: self.effort,
            backoff_base: self.backoff_base,
        };
        let (response, permit) = post_with_retries(&post, governor, budget).await?;
        let retry_after = RetryAfterSignal::default();
        let inner = create_anthropic_event_stream(
            response,
            self.model.clone(),
            self.stream_idle_secs,
            self.effort,
            self.is_oauth,
        );
        Ok(OpenedStream {
            stream: hold_permit(inner, permit),
            retry_after,
        })
    }
}
