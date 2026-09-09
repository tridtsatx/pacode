//! Web search tool using DuckDuckGo HTML scraping.
//!
//! Ported from the reference implementation in `jcode` (`jcode-app-core/src/tool/websearch.rs`).
//! DuckDuckGo's HTML endpoint serves an anti-bot challenge (HTTP 202, no results) for plain
//! GET requests. Submitting the query as a POST form with desktop Chrome headers returns normal
//! results markup with HTTP 200.

#[cfg(test)]
#[path = "websearch_tests.rs"]
mod websearch_tests;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::WebConfig;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "websearch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebSearchError {
    #[error("network request failed: {0}")]
    Network(String),
    #[error(
        "anti-bot challenge encountered ({0}); requests may be blocked by TLS fingerprinting or IP reputation"
    )]
    Challenge(String),
    #[error("failed to parse search results: {0}")]
    Parse(String),
    #[error("invalid query: {0}")]
    InvalidInput(String),
}

impl From<WebSearchError> for ToolError {
    fn from(err: WebSearchError) -> Self {
        match err {
            WebSearchError::InvalidInput(msg) => ToolError::invalid(msg),
            WebSearchError::Network(msg) => {
                ToolError::failed(format!("network request failed: {msg}"))
            }
            WebSearchError::Challenge(reason) => ToolError::failed(format!(
                "anti-bot challenge encountered ({reason}); requests may be blocked by TLS fingerprinting or IP reputation"
            )),
            WebSearchError::Parse(msg) => {
                ToolError::failed(format!("failed to parse search results: {msg}"))
            }
        }
    }
}

/// Search backend interface so alternative providers can be plugged in.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        timeout: Duration,
    ) -> Result<Vec<SearchResult>, WebSearchError>;
}

/// DuckDuckGo HTML form POST search backend.
pub struct DuckDuckGoBackend {
    client: reqwest::Client,
}

impl DuckDuckGoBackend {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for DuckDuckGoBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SearchBackend for DuckDuckGoBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        timeout: Duration,
    ) -> Result<Vec<SearchResult>, WebSearchError> {
        let response = self
            .client
            .post("https://html.duckduckgo.com/html/")
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .form(&[("q", query), ("kl", "us-en")])
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| WebSearchError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(WebSearchError::Network(format!(
                "HTTP request returned status {status}"
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| WebSearchError::Network(format!("failed reading response: {e}")))?;

        parse_ddg_results(&body, num_results)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WebSearchInput {
    query: String,
    num_results: Option<usize>,
}

pub struct WebSearchTool {
    backend: Arc<dyn SearchBackend>,
    config: WebConfig,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self::with_backend_and_config(Arc::new(DuckDuckGoBackend::new()), WebConfig::default())
    }

    pub fn with_config(config: WebConfig) -> Self {
        Self::with_backend_and_config(Arc::new(DuckDuckGoBackend::new()), config)
    }

    pub fn with_backend(backend: Arc<dyn SearchBackend>) -> Self {
        Self::with_backend_and_config(backend, WebConfig::default())
    }

    pub fn with_backend_and_config(backend: Arc<dyn SearchBackend>, config: WebConfig) -> Self {
        Self { backend, config }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo. Returns a numbered list of results with title, URL, and snippet."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query."
                },
                "num_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Maximum number of search results to return."
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<WebSearchInput>(input)?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ToolError::invalid("query cannot be empty"));
        }

        let num_results = args
            .num_results
            .unwrap_or(self.config.default_num_results)
            .clamp(1, 50);
        let timeout = Duration::from_secs(self.config.request_timeout_secs);

        let results = self
            .backend
            .search(query, num_results, timeout)
            .await
            .map_err(ToolError::from)?;

        let formatted = if results.is_empty() {
            format!("No results found for: {query}\n")
        } else {
            let mut out = format!("Search results for: {query}\n\n");
            for (i, res) in results.iter().enumerate() {
                let idx = i + 1;
                let title = &res.title;
                let url = &res.url;
                let snippet = &res.snippet;
                out.push_str(&format!("{idx}. **{title}**\n   {url}\n   {snippet}\n\n"));
            }
            out
        };

        let cap = ctx.output_cap_chars;
        let content = cap_output(&formatted, accept_large_output, cap);
        let count = results.len();
        let preview = format!("{count} results");
        let title = format!("Search \"{query}\"");

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}

/// Detect whether an HTML body is an anti-bot/captcha challenge rather than a
/// real results page. DuckDuckGo (and similar) serve these with HTTP 200, so a
/// successful status plus zero parsed results is ambiguous without this check.
///
/// Returns a short human-readable reason when a challenge page is detected.
pub fn detect_anti_bot_page(html: &str) -> Option<&'static str> {
    let lowered = html.to_ascii_lowercase();
    const MARKERS: &[(&str, &str)] = &[
        ("anomaly-modal", "anomaly challenge"),
        ("anomaly.js", "anomaly challenge"),
        ("dpn=1", "anomaly challenge"),
        ("g-recaptcha", "recaptcha"),
        ("captcha", "captcha"),
        ("are you a robot", "bot check"),
        ("unusual traffic", "bot check"),
        ("verify you are human", "human verification"),
        ("challenge-platform", "cloudflare challenge"),
        ("cf-challenge", "cloudflare challenge"),
    ];
    for (needle, reason) in MARKERS {
        if lowered.contains(needle) {
            return Some(reason);
        }
    }
    None
}

/// Parse search results from DuckDuckGo HTML markup.
pub fn parse_ddg_results(
    html: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>, WebSearchError> {
    let results = parse_ddg_html_internal(html, max_results);
    if results.is_empty()
        && let Some(reason) = detect_anti_bot_page(html)
    {
        return Err(WebSearchError::Challenge(reason.to_string()));
    }
    Ok(results)
}

fn parse_ddg_html_internal(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let link_re = search_regex::result_link();
    let snippet_re = search_regex::result_snippet();
    let tag_re = search_regex::tag();

    // Strategy 1: Block-by-block parsing if result containers exist
    if html.contains("class=\"result ")
        || html.contains("class=\"result\"")
        || html.contains("class='result ")
    {
        let parts: Vec<&str> = html.split("class=\"result ").collect();
        for part in parts.iter().skip(1) {
            if results.len() >= max_results {
                break;
            }
            if let Some(link_cap) = link_re.captures(part) {
                let url = decode_ddg_url(&link_cap[1]);
                let title = html_decode(&tag_re.replace_all(&link_cap[2], ""));

                if !url.starts_with("http") || url.contains("duckduckgo.com") {
                    continue;
                }

                let snippet = if let Some(snip_cap) = snippet_re.captures(part) {
                    html_decode(&tag_re.replace_all(&snip_cap[1], ""))
                } else {
                    String::new()
                };

                results.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }
        if !results.is_empty() {
            return results;
        }
    }

    // Strategy 2: Global captures fallback (matching jcode reference)
    let links: Vec<_> = link_re.captures_iter(html).collect();
    let snippets: Vec<_> = snippet_re.captures_iter(html).collect();

    for (i, link_cap) in links.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }

        let url = decode_ddg_url(&link_cap[1]);
        let title = html_decode(&tag_re.replace_all(&link_cap[2], ""));

        if !url.starts_with("http") || url.contains("duckduckgo.com") {
            continue;
        }

        let snippet = if i < snippets.len() {
            let raw = &snippets[i][1];
            html_decode(&tag_re.replace_all(raw, ""))
        } else {
            String::new()
        };

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    results
}

/// Extract actual destination URL from DDG redirect wrappers (`//duckduckgo.com/l/?uddg=...`).
pub fn decode_ddg_url(url: &str) -> String {
    if let Some(uddg_start) = url.find("uddg=") {
        let start = uddg_start + 5;
        let end = url[start..]
            .find('&')
            .map(|i| start + i)
            .unwrap_or(url.len());
        let encoded = &url[start..end];
        percent_decode(encoded)
    } else {
        url.to_string()
    }
}

/// Decode percent-encoded byte strings safely without external crate dependency.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &bytes[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(hex).unwrap_or(""), 16) {
                decoded.push(byte);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            decoded.push(b' ');
            i += 1;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Decode common HTML entities from scraped snippets and titles.
pub fn html_decode(s: &str) -> String {
    let mut s = s
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'");

    if s.contains("&#") {
        let num_re = search_regex::numeric_entity();
        s = num_re
            .replace_all(&s, |caps: &regex::Captures<'_>| {
                let code = if let Some(hex) = caps.get(2) {
                    u32::from_str_radix(hex.as_str(), 16).ok()
                } else if let Some(dec) = caps.get(1) {
                    dec.as_str().parse::<u32>().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| caps[0].to_string())
            })
            .to_string();
    }

    s.trim().to_string()
}

mod search_regex {
    use std::sync::OnceLock;

    use regex::Regex;

    pub fn result_link() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            // Provably impossible to fail: static literal pattern
            Regex::new(
                r#"(?s)<a[^>]*class="[^"]*\bresult__a\b[^"]*"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#,
            )
            .expect("static regex pattern is valid")
        })
    }

    pub fn result_snippet() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            // Provably impossible to fail: static literal pattern
            Regex::new(r#"(?s)<a[^>]*class="[^"]*\bresult__snippet\b[^"]*"[^>]*>(.*?)</a>"#)
                .expect("static regex pattern is valid")
        })
    }

    pub fn tag() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            // Provably impossible to fail: static literal pattern
            Regex::new(r"<[^>]+>").expect("static regex pattern is valid")
        })
    }

    pub fn numeric_entity() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            // Provably impossible to fail: static literal pattern
            Regex::new(r"&#(?:(\d+)|x([0-9a-fA-F]+));").expect("static regex pattern is valid")
        })
    }
}
