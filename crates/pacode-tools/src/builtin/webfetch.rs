//! `webfetch`: fetch a URL as text.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "webfetch";

pub struct WebFetchTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct WebFetchInput {
    url: String,
    max_chars: Option<usize>,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Fetch an http(s) URL and return its text (HTML extracted to readable Markdown). \
         Output above the cap is cut head+tail."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {"type": "string"},
                "max_chars": {"type": "integer", "minimum": 100}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    /// reqwest GET with 20 s timeout, follows ≤ 5 redirects, refuses non-http(s)
    /// schemes and bodies over 5 MiB; `text/html` → readability extraction (`dom_smoothie`)
    /// and Markdown conversion (`htmd`); other text types verbatim; binary → error.
    /// Title `Fetch <host>`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<WebFetchInput>(input)?;
        if args.url.trim().is_empty() {
            return Err(ToolError::invalid("url cannot be empty"));
        }

        let parsed_url = url::Url::parse(&args.url)
            .map_err(|e| ToolError::invalid(format!("invalid URL: {e}")))?;

        let scheme = parsed_url.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(ToolError::invalid(format!(
                "unsupported scheme '{scheme}': only http and https are supported"
            )));
        }

        let host = parsed_url.host_str().unwrap_or("unknown").to_string();
        let title = format!("Fetch {host}");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| ToolError::failed(format!("failed to build HTTP client: {e}")))?;

        let resp = client
            .get(parsed_url.clone())
            .send()
            .await
            .map_err(|e| ToolError::failed(format!("HTTP request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::failed(format!(
                "HTTP request returned status {status}"
            )));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .to_string();

        const MAX_BYTES: usize = 5 * 1024 * 1024;
        let mut stream = resp.bytes_stream();
        let mut bytes = Vec::new();

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res
                .map_err(|e| ToolError::failed(format!("failed reading response: {e}")))?;
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err(ToolError::failed("response body exceeds 5 MiB limit"));
            }
            bytes.extend_from_slice(&chunk);
        }

        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            return Err(ToolError::failed("binary content is not supported"));
        }

        let is_html = content_type.to_lowercase().contains("text/html");
        let text = if is_html {
            let html_str = String::from_utf8(bytes)
                .map_err(|e| ToolError::failed(format!("response is not valid UTF-8: {e}")))?;
            extract_html_to_markdown(&html_str, Some(parsed_url.as_str()))
        } else {
            String::from_utf8(bytes)
                .map_err(|e| ToolError::failed(format!("response is not valid UTF-8: {e}")))?
        };

        let cap = args.max_chars.unwrap_or(ctx.output_cap_chars);
        let content = cap_output(&text, accept_large_output, cap);
        let char_count = content.chars().count();
        let preview = format!("{char_count} chars");

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}

/// Extract readable article content and convert to Markdown using `dom_smoothie` and `htmd`.
/// If extraction yields nothing useful, falls back to converting the full document to Markdown
/// and explicitly notes in the output that it fell back.
pub fn extract_html_to_markdown(html: &str, url: Option<&str>) -> String {
    // 1. Attempt readability extraction
    let readability_result = dom_smoothie::Readability::new(html, url, None)
        .or_else(|_| dom_smoothie::Readability::new(html, None, None))
        .and_then(|mut r| r.parse());

    if let Ok(article) = readability_result {
        let content = article.content.to_string();
        if !content.trim().is_empty()
            && let Ok(md) = htmd::convert(&content)
        {
            let trimmed = md.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    // 2. Fallback: convert the entire document rather than returning an empty string
    let full_md = htmd::convert(html).unwrap_or_else(|_| html.to_string());
    let trimmed = full_md.trim();
    if trimmed.is_empty() {
        "[Extraction yielded no article content; fell back to full document]\n\n(empty document)"
            .to_string()
    } else {
        format!("[Extraction yielded no article content; fell back to full document]\n\n{trimmed}")
    }
}
