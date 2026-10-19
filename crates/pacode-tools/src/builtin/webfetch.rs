//! `webfetch`: fetch a URL as text.

use std::borrow::Cow;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "webfetch";

/// Process-wide HTTP client shared by `webfetch` and `websearch`.
///
/// Owner: the tools crate. Cap: one client, hence one connection pool, for the whole
/// process; it is built once and never rebuilt or invalidated, because the proxy is read
/// from the environment only. Timeouts: the client-level timeout is 20 s; `websearch`
/// overrides it per request with `RequestBuilder::timeout`. Redirects: at most 5.
/// A build failure is cached too, so every caller sees the same outcome.
static WEB_CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();

/// Returns the shared web client, building it on first use.
pub(crate) fn shared_web_client() -> Result<&'static reqwest::Client, String> {
    WEB_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Bytes of the response body held before the transfer is stopped. A page the
/// model wants more of is pulled in pieces with `max_chars`/`offset`, which costs
/// one round trip instead of every turn re-reading a truncated 5 MiB wall.
pub const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// Window `max_chars` defaults to when the model does not name one: what a
/// single `webfetch` normally costs the context, and one range request's worth.
pub const MAX_CHARS_WINDOW: usize = 200_000;
/// Hard ceiling on a whole-body fetch, whatever the window and the page. Stops a
/// model from paging through a 40 MiB dump one 200 K slice at a time; the note
/// says how to go on (`offset`) when that is what it actually wants.
const MAX_TOTAL_CHARS: usize = 1_000_000;

/// Collects response-body chunks up to `MAX_BODY_BYTES`.
///
/// A budget, not an error: a 40 MiB dump linked from a page listing used to end
/// the call with nothing to show for it. The transfer stops at the budget —
/// dropping the rest of the stream closes the connection instead of reading
/// bytes nobody will look at — and `push` returning `false` is the caller learning
/// to stop pulling.
///
/// Split out from the request path so the cutoff is a property that can be
/// asserted directly: "the cap cuts and reports" is a claim about this type, not
/// about a live HTTP response.
struct BodyAccumulator {
    bytes: Vec<u8>,
    /// Set once a chunk did not fit. The partial chunk is kept up to the cap —
    /// the model is better served by a body cut at a byte boundary with a note
    /// than by no body at all.
    capped: bool,
}

impl BodyAccumulator {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            capped: false,
        }
    }

    /// Append `chunk`, up to the cap. Returns `false` once the budget is spent
    /// and the caller should stop reading the stream.
    fn push(&mut self, chunk: &[u8]) -> bool {
        let Some(room) = MAX_BODY_BYTES.checked_sub(self.bytes.len()) else {
            self.capped = true;
            return false;
        };
        if chunk.len() > room {
            self.bytes.extend_from_slice(&chunk[..room]);
            self.capped = true;
            return false;
        }
        self.bytes.extend_from_slice(chunk);
        true
    }
}

#[derive(Default)]
pub struct WebFetchTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WebFetchInput {
    url: String,
    max_chars: Option<usize>,
    /// Character offset to start at — the paging knob. The window is
    /// `[offset, offset + max_chars)` of the *rendered* text (Markdown for HTML,
    /// verbatim otherwise), so the same value addresses both kinds of page only
    /// when the render is deterministic. It is, for a given URL.
    offset: Option<usize>,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Fetch an http(s) URL and return its text (HTML extracted to readable Markdown). \
         A long page comes back as one window: the output says which slice of the page it is, \
         and `offset` pages through the rest. A response over 5 MiB is cut there — read what \
         you need of it in pieces instead of asking for the whole thing again."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {"type": "string"},
                "max_chars": {"type": "integer", "minimum": 100, "description": "Characters to return from `offset`; defaults to a 200000 window."},
                "offset": {"type": "integer", "minimum": 0, "description": "Character offset into the rendered text to start at; use the offset the previous call reported to continue."}
            }
        })
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    /// reqwest GET with 20 s timeout, follows ≤ 5 redirects, refuses non-http(s)
    /// schemes; `text/html` → readability extraction (`dom_smoothie`) and Markdown
    /// conversion (`htmd`); other text types verbatim; binary → error. A body over
    /// [`MAX_BODY_BYTES`] **truncates the transfer** rather than failing the call,
    /// and both that cut and the window cut are stated at the top of the output
    /// with the `offset` that continues.
    /// Title `Fetch <host>`.
    async fn call(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
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

        let client = shared_web_client()
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

        // The body is read under a byte budget, not a hard failure; see
        // `BodyAccumulator`. `content_length` is a header, so it is read before
        // the body is.
        let announced_len = resp.content_length();
        let mut body_bytes = BodyAccumulator::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res
                .map_err(|e| ToolError::failed(format!("failed reading response: {e}")))?;
            if !body_bytes.push(&chunk) {
                break;
            }
        }
        let transfer_capped = body_bytes.capped;
        let bytes = body_bytes.bytes;
        let bytes_read = bytes.len();

        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            return Err(ToolError::failed("binary content is not supported"));
        }

        // The body becomes a `String` once and is read by reference from here
        // on. The old shape cloned it — a second 5 MiB allocation for the same
        // bytes, on the non-HTML path cloned *again* before being capped to
        // 16 K chars.
        let body = String::from_utf8(bytes)
            .map_err(|e| ToolError::failed(format!("response is not valid UTF-8: {e}")))?;

        // The window is `[offset, offset + window)` of the *rendered* text
        // (Markdown for HTML, verbatim otherwise), so the same `offset` addresses
        // both kinds of page for a given URL — the render is deterministic.
        //
        // The render happens once and is then read by reference: a non-HTML body
        // costs nothing to "render" (it is a borrow of the bytes already in
        // hand), and an HTML body cannot be indexed by character without being
        // converted, so there is no cheaper honest option than one conversion.
        // What the old code did was convert *and* clone, and then measure the
        // clone.
        let is_html = content_type.to_lowercase().contains("text/html");
        let rendered: Cow<'_, str> = if is_html {
            Cow::Owned(extract_html_to_markdown(&body, Some(parsed_url.as_str())))
        } else {
            Cow::Borrowed(body.as_str())
        };
        let total_chars = rendered.chars().count();

        let window = args
            .max_chars
            .unwrap_or(MAX_CHARS_WINDOW)
            .min(max_window_chars(accept_large_output));
        let offset = args.offset.unwrap_or(0);
        let skip = offset.min(total_chars);
        let end = skip.saturating_add(window).min(total_chars);
        // Borrowed whole when the call asked for everything there is — the
        // common case, and the one where a copy is pure waste.
        let text: Cow<'_, str> = if skip == 0 && end >= total_chars {
            Cow::Borrowed(rendered.as_ref())
        } else {
            Cow::Owned(rendered.chars().skip(skip).take(end - skip).collect())
        };

        let dropped_bytes = if transfer_capped {
            announced_len
                .unwrap_or(MAX_BODY_BYTES as u64)
                .saturating_sub(bytes_read as u64)
        } else {
            0
        };
        let note = window_note(
            skip,
            end,
            total_chars,
            window,
            transfer_capped,
            dropped_bytes,
        );
        let content = note_window(&text, &note);
        let preview = preview_body(&content);

        Ok(ToolOutput::text(content)
            .with_title(title)
            .with_preview(preview))
    }
}

/// Window ceiling for one call. `accept_large_output` lifts it to
/// [`MAX_TOTAL_CHARS`] — the model asking for a bigger slice is the model
/// stating the token cost, which is exactly what that flag is for. The
/// [`MAX_BODY_BYTES`] transfer cap does *not* move: that is a network budget,
/// not a context one, and re-sending 40 MiB to reach a wider window is the
/// failure this tool was fixed for.
fn max_window_chars(accept_large_output: bool) -> usize {
    if accept_large_output {
        MAX_TOTAL_CHARS
    } else {
        MAX_CHARS_WINDOW
    }
}

/// A single-line header above a non-exact window, or `None` when the call
/// returned the whole rendered text. The model has to be able to tell "this is
/// all of it" from "this is a slice" without counting characters, because
/// counting characters is the one thing it reliably gets wrong here.
fn window_note(
    skip: usize,
    end: usize,
    total_chars: usize,
    window: usize,
    transfer_capped: bool,
    dropped_bytes: u64,
) -> Option<String> {
    if skip == 0 && end >= total_chars && !transfer_capped {
        return None;
    }
    let mut parts = Vec::new();
    if skip > 0 {
        parts.push(format!("chars {skip}..{end} of {total_chars}"));
    } else {
        parts.push(format!("first {end} of {total_chars} characters"));
    }
    if transfer_capped {
        // `content_length` is the raw body, which is not the rendered text, so
        // this is an estimate of what the server still has — phrased as one.
        parts.push(format!(
            "the response itself was cut at {} MiB (~{dropped_bytes} bytes unread)",
            MAX_BODY_BYTES / (1024 * 1024)
        ));
    }
    let paging = if total_chars > end {
        format!("read on with `offset: {end}` (up to {window} characters per call)")
    } else if skip > 0 {
        "read from the start with `offset: 0`".to_string()
    } else {
        String::new()
    };
    Some(format!("[{} — {paging}]\n\n", parts.join("; ")))
}

/// The note prepended to a capped window; the note is what tells the model the
/// result is a slice, so it rides along with the content the model reads.
fn note_window(content: &str, note: &Option<String>) -> String {
    match note {
        Some(note) => format!("{note}{content}"),
        None => content.to_string(),
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

#[cfg(test)]
#[path = "webfetch_tests.rs"]
mod webfetch_tests;
