//! Web search tool using DuckDuckGo HTML scraping.
//!
//! Ported from the reference implementation in `jcode` (`jcode-app-core/src/tool/websearch.rs`).
//! DuckDuckGo's HTML endpoint serves an anti-bot challenge (HTTP 202, no results) for plain
//! GET requests. Submitting the query as a POST form with desktop browser headers returns
//! normal results markup with HTTP 200.
//!
//! Challenge statuses (202/403), challenge markup, and empty bodies surface as explicit
//! `Challenge` errors instead of a silent "no results". When the response is neither a
//! challenge nor a declared no-results page but still yields zero parsed results, the tool
//! reports `Ok` with a note that the backend markup may have changed.

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
    #[error("search backend blocked by anti-bot ({0}); try again later")]
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
                "search backend blocked by anti-bot ({reason}); try again later"
            )),
            WebSearchError::Parse(msg) => {
                ToolError::failed(format!("failed to parse search results: {msg}"))
            }
        }
    }
}

/// What a search backend returns. `note` carries a non-fatal diagnostic the tool
/// appends to its output — set when a response parsed to zero results even though
/// it was a well-formed, non-challenge page, meaning the markup probably changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    /// Non-fatal diagnostic surfaced to the model (e.g. suspected markup drift).
    pub note: Option<String>,
}

/// Search backend interface so alternative providers can be plugged in.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        timeout: Duration,
    ) -> Result<SearchResponse, WebSearchError>;
}

/// DuckDuckGo HTML endpoint. Overridable for tests and DDG-compatible mirrors.
const DDG_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// DuckDuckGo HTML form POST search backend.
pub struct DuckDuckGoBackend {
    client: reqwest::Client,
    endpoint: String,
}

impl DuckDuckGoBackend {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self::with_client(client)
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self {
            client,
            endpoint: DDG_ENDPOINT.to_string(),
        }
    }

    /// Point the backend at a different endpoint (tests, DDG-compatible mirrors).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
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
    ) -> Result<SearchResponse, WebSearchError> {
        let response = self
            .client
            .post(self.endpoint.as_str())
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
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
        // DDG answers suspected bots with 202 (the anomaly challenge) or 403.
        // 202 is a success status, so without this check it fell through to the
        // parser and surfaced as a silent empty result.
        if status == reqwest::StatusCode::ACCEPTED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(WebSearchError::Challenge(format!("HTTP {status}")));
        }
        if !status.is_success() {
            return Err(WebSearchError::Network(format!(
                "HTTP request returned status {status}"
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| WebSearchError::Network(format!("failed reading response: {e}")))?;

        if body.trim().is_empty() {
            return Err(WebSearchError::Challenge("empty response body".to_string()));
        }

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

        let response = self
            .backend
            .search(query, num_results, timeout)
            .await
            .map_err(ToolError::from)?;

        let note = response.note;
        let results = response.results;
        let mut formatted = if results.is_empty() {
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
        // A backend note (the page parsed to nothing although it was neither a
        // challenge nor a declared no-results page) is shown to the model so it
        // can retry later or report the breakage instead of trusting "no results".
        if let Some(note) = &note {
            formatted.push_str(&format!("\nNote: {note}\n"));
        }

        let cap = ctx.output_cap_chars;
        let content = cap_output(&formatted, accept_large_output, cap);
        let count = results.len();
        let preview = if count == 0 && note.is_some() {
            "0 results (markup may have changed)".to_string()
        } else {
            format!("{count} results")
        };
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
        ("challenge-form", "challenge form"),
    ];
    for (needle, reason) in MARKERS {
        if lowered.contains(needle) {
            return Some(reason);
        }
    }
    None
}

/// Whether the page explicitly declares the query found nothing — distinguishes a
/// genuine empty result page from markup the parser failed to understand.
fn page_reports_no_results(html: &str) -> bool {
    let lowered = html.to_ascii_lowercase();
    lowered.contains("no-results")
        || lowered.contains("no_results")
        || lowered.contains("no results")
}

/// Parse search results from DuckDuckGo HTML markup.
///
/// Zero parsed results on a challenge page is a `Challenge` error; on a page that
/// neither is a challenge nor declares "no results" it comes back `Ok` with a note
/// saying the markup may have changed.
pub fn parse_ddg_results(html: &str, max_results: usize) -> Result<SearchResponse, WebSearchError> {
    let results = parse_ddg_html_internal(html, max_results);
    if !results.is_empty() {
        return Ok(SearchResponse {
            results,
            note: None,
        });
    }
    if let Some(reason) = detect_anti_bot_page(html) {
        return Err(WebSearchError::Challenge(reason.to_string()));
    }
    let note = if html.trim().is_empty() || page_reports_no_results(html) {
        None
    } else {
        Some(
            "the response page could not be parsed; the search backend markup may have changed"
                .to_string(),
        )
    };
    Ok(SearchResponse { results, note })
}

/// A link candidate lifted from the page: byte offset plus raw href and inner HTML.
struct LinkMatch {
    pos: usize,
    href: String,
    inner: String,
}

/// Collect `<a>` elements matched by `re` (group 1 = all attributes, group 2 =
/// inner HTML), extracting each href from the attribute string so `href` and
/// `class` may appear in either order in the tag.
fn anchors_matching(html: &str, re: &regex::Regex) -> Vec<LinkMatch> {
    let href_re = search_regex::attr_href();
    re.captures_iter(html)
        .map(|cap| LinkMatch {
            pos: cap.get(0).map(|m| m.start()).unwrap_or(0),
            href: href_re
                .captures(&cap[1])
                .map(|h| h[1].to_string())
                .unwrap_or_default(),
            inner: cap[2].to_string(),
        })
        .collect()
}

/// Last-resort extraction for markup drift: take the first `<a>` carrying an href
/// inside each element whose class list contains a standalone `result` token.
fn links_in_result_containers(html: &str) -> Vec<LinkMatch> {
    let container_re = search_regex::result_container();
    let anchor_re = search_regex::anchor();
    let href_re = search_regex::attr_href();
    let starts: Vec<usize> = container_re.find_iter(html).map(|m| m.start()).collect();
    let mut links = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(html.len());
        for cap in anchor_re.captures_iter(&html[start..end]) {
            let Some(href) = href_re.captures(&cap[1]) else {
                continue;
            };
            links.push(LinkMatch {
                pos: start + cap.get(0).map(|m| m.start()).unwrap_or(0),
                href: href[1].to_string(),
                inner: cap[2].to_string(),
            });
            break;
        }
    }
    links
}

/// Snippet texts in document order, paired with byte offsets so each can be
/// matched to the title link it follows.
fn collect_snippets(html: &str) -> Vec<(usize, String)> {
    let tag_re = search_regex::tag();
    search_regex::result_snippet()
        .captures_iter(html)
        .map(|cap| {
            let pos = cap.get(0).map(|m| m.start()).unwrap_or(0);
            let text = html_decode(&tag_re.replace_all(&cap[1], ""));
            (pos, text)
        })
        .collect()
}

fn parse_ddg_html_internal(html: &str, max_results: usize) -> Vec<SearchResult> {
    // Title links are tried in the order of the markup variants DDG has served:
    // `result__a` (html endpoint), then `result-link` (lite endpoint and older
    // layouts). Both match regardless of attribute order or quote style.
    let mut links = anchors_matching(html, search_regex::result_link_a());
    if links.is_empty() {
        links = anchors_matching(html, search_regex::result_link_variant());
    }
    if links.is_empty() {
        links = links_in_result_containers(html);
    }

    let snippets = collect_snippets(html);
    let tag_re = search_regex::tag();

    let mut results = Vec::new();
    for (i, link) in links.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }

        let url = decode_ddg_url(&link.href);
        if !url.starts_with("http") || url.contains("duckduckgo.com") {
            continue;
        }

        let title = html_decode(&tag_re.replace_all(&link.inner, ""));

        // Snippets pair positionally: the first snippet element after this title
        // link but before the next title link belongs to this result. Skipped
        // links still bound their own snippet, so filtering cannot misalign them.
        let next_pos = links.get(i + 1).map(|l| l.pos).unwrap_or(usize::MAX);
        let snippet = snippets
            .iter()
            .find(|(pos, _)| *pos > link.pos && *pos < next_pos)
            .map(|(_, text)| text.clone())
            .unwrap_or_default();

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

    macro_rules! static_regex {
        ($name:ident, $pat:expr_2021) => {
            pub fn $name() -> &'static Regex {
                static RE: OnceLock<Regex> = OnceLock::new();
                // Provably impossible to fail: static literal pattern
                RE.get_or_init(|| Regex::new($pat).expect("static regex pattern is valid"))
            }
        };
    }

    // Anchor patterns capture the whole attribute string in group 1 and the inner
    // HTML in group 2, so `href` and `class` may appear in either order.
    static_regex!(
        result_link_a,
        r#"(?si)<a\b([^>]*class\s*=\s*["'][^"']*\bresult__a\b[^"']*["'][^>]*)>(.*?)</a>"#
    );
    static_regex!(
        result_link_variant,
        r#"(?si)<a\b([^>]*class\s*=\s*["'][^"']*\bresult[-_]+link\b[^"']*["'][^>]*)>(.*?)</a>"#
    );
    static_regex!(anchor, r#"(?si)<a\b([^>]*)>(.*?)</a>"#);
    // `href` must be preceded by whitespace (or be the first attribute) so
    // `data-href` and friends cannot be picked up instead.
    static_regex!(attr_href, r#"(?:^|\s)href\s*=\s*["']([^"']*)["']"#);
    // Elements whose class list carries a standalone `result` token — i.e. the
    // token ends at whitespace or the closing quote, excluding `result-link` /
    // `result__a`-style compound names.
    static_regex!(
        result_container,
        r#"(?si)<[a-zA-Z][a-zA-Z0-9]*[^>]*class\s*=\s*["'][^"']*\bresult[\s"']"#
    );
    // Snippet elements: `result__snippet` on html.duckduckgo.com, `result-snippet`
    // on lite.duckduckgo.com; matched on any tag DDG has used for them.
    static_regex!(
        result_snippet,
        r#"(?si)<(?:a|td|div|span)\b[^>]*class\s*=\s*["'][^"']*\bresult[_-]+snippet\b[^"']*["'][^>]*>(.*?)</(?:a|td|div|span)>"#
    );
    static_regex!(tag, r"<[^>]+>");
    static_regex!(numeric_entity, r"&#(?:(\d+)|x([0-9a-fA-F]+));");
}
