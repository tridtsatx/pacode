//! Error notices in the transcript — the block form of a failed turn:
//!
//! ```text
//! ✗ turn failed · http 400
//! │ Invalid request — model foo is not supported
//! ```
//!
//! The title classifies the `turn failed: <ProviderError>` prefix (`· auth`,
//! `· http 400`, `· network`, …) so the body keeps just the message; a body
//! that is raw JSON yields its `error.message`/`message` field instead. The
//! `│ ` gutter in red follows the failed tool-call cell (`toolcell`).

use ratatui::text::{Line, Span};

use pacode_render::{RenderOptions, truncate_to_width, wrap_text};

/// An error notice as a block: `✗ <title>` in red bold, then the detail
/// wrapped on a red `│ ` gutter in `dim`.
pub(crate) fn render_error(text: &str, width: u16, opts: &RenderOptions) -> Vec<Line<'static>> {
    let width = width as usize;
    let (title, body) = split_error_notice(text, opts.glyphs.dot_sep);

    let head_style = opts.theme.bold.patch(opts.theme.red);
    let mut out = vec![Line::from(vec![
        Span::styled(format!("{} ", opts.glyphs.fail), head_style),
        Span::styled(
            truncate_to_width(&title, width.saturating_sub(2), true),
            head_style,
        ),
    ])];

    let body = extract_json_message(&body);
    if !body.is_empty() {
        let body_w = width.saturating_sub(4).max(1);
        for l in wrap_text(&body, body_w) {
            out.push(Line::from(vec![
                Span::styled(format!("{} ", opts.glyphs.vline), opts.theme.red),
                Span::styled(l, opts.theme.dim),
            ]));
        }
    }

    out.push(Line::default());
    out
}

/// `turn failed: <ProviderError Display>` → `(title, body)`. Known provider
/// prefixes map to a `turn failed · <kind>` title; anything else under the
/// wrapper keeps `Turn failed` and the whole rest as the body. Text without
/// the wrapper takes its own `: `-separated prefix (or first line) as title.
fn split_error_notice(text: &str, dot_sep: &str) -> (String, String) {
    let t = text.trim();

    if let Some(inner) = strip_prefix_ci(t, "turn failed:") {
        let inner = inner.trim();
        if inner.is_empty() {
            return ("Turn failed".to_string(), String::new());
        }
        for (prefix, tag) in [
            ("authentication failed:", "auth"),
            ("rate limited:", "rate limit"),
            ("overloaded:", "rate limit"),
            ("transport:", "network"),
            ("configuration:", "config"),
            ("malformed response:", "malformed"),
        ] {
            if let Some(rest) = inner.strip_prefix(prefix) {
                return (
                    format!("turn failed{dot_sep}{tag}"),
                    rest.trim().to_string(),
                );
            }
        }
        // `request failed (NNN): <body>` — the status code is the useful part.
        if let Some(rest) = inner.strip_prefix("request failed (")
            && let Some(close) = rest.find("):")
        {
            let code = &rest[..close];
            if !code.is_empty() && code.chars().all(|c| c.is_ascii_digit()) {
                return (
                    format!("turn failed{dot_sep}http {code}"),
                    rest[close + 2..].trim().to_string(),
                );
            }
        }
        // `stream idle for Ns` — no colon in this one.
        if let Some(rest) = inner.strip_prefix("stream idle") {
            return (
                format!("turn failed{dot_sep}timeout"),
                rest.trim().to_string(),
            );
        }
        return ("Turn failed".to_string(), inner.to_string());
    }

    // No wrapper: the text's own label-like prefix is the title, else its
    // first line. `foo: bar` splits; `http://x broke` does not.
    let (first, rest) = match t.split_once('\n') {
        Some((f, r)) => (f, r),
        None => (t, ""),
    };
    if let Some((prefix, after)) = first.split_once(": ")
        && is_label_prefix(prefix)
    {
        return (capitalize_first(prefix), join_body(after, rest));
    }
    if let Some(prefix) = first.strip_suffix(':')
        && is_label_prefix(prefix)
    {
        return (capitalize_first(prefix), rest.trim().to_string());
    }
    (capitalize_first(first), rest.trim().to_string())
}

/// `text` without `prefix`, ASCII case-insensitive — old sessions and other
/// emitters are not guaranteed to keep the exact casing.
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// A `: ` split is only a label when the left side reads like one: short,
/// single-line, letters and spaces — not a URL or a path fragment.
fn is_label_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.chars().count() <= 48
        && prefix
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')'))
        && prefix.chars().any(|c| c.is_alphabetic())
}

/// `rest` after the split prefix plus any lines that followed the first one.
fn join_body(after: &str, rest: &str) -> String {
    let mut body = after.trim().to_string();
    let rest = rest.trim();
    if !rest.is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(rest);
    }
    body
}

/// Provider error bodies sometimes carry raw JSON (`{"error":{"message":…}}`)
/// from older sessions or third-party sources; when one does, the message
/// field is the part a human wants. Anything unparseable stays as it was.
fn extract_json_message(body: &str) -> String {
    let t = body.trim();
    if !t.starts_with('{') && !t.starts_with('[') {
        return body.to_string();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
        return body.to_string();
    };
    v.get("error")
        .and_then(|e| e.get("message"))
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| body.to_string())
}

/// First char uppercased, the rest untouched (`some error` → `Some error`).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
