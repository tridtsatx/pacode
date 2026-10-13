//! What a tool call acted on, read out of its title or its (possibly still
//! streaming) raw JSON arguments — the "with what" half of the cell header and
//! the input rows of an expanded body.

use pacode_types::transcript::ToolStatus;

use super::{ToolCallView, strip_ansi, utf8_len};

/// `title` is `<name> <arg>` — the arg part, or the whole title when it does
/// not start with the name. The name prefix matches case-insensitively:
/// tools emit `Edit <path>` while the call is named `edit`.
pub(super) fn title_arg<'a>(call: &ToolCallView<'a>) -> &'a str {
    let prefix_len = call.name.len();
    if call
        .title
        .get(..prefix_len)
        .is_some_and(|head| head.eq_ignore_ascii_case(call.name))
    {
        return call.title[prefix_len..].trim_start();
    }
    call.title
}

/// Tools whose arguments are content rather than a locator: what they receive
/// is what they are about to write, and while the call is running that content
/// is still arriving.
pub(super) fn is_content_tool(name: &str) -> bool {
    matches!(name, "write" | "edit" | "multi_edit")
}

/// The text a running content tool is composing, read out of the *raw* argument
/// prefix.
///
/// Not parsed: while the model is still emitting the arguments the JSON is a
/// truncated prefix and would not parse, which is exactly the moment the user
/// wants to watch. So the value is cut out textually — everything after the
/// opening quote of the content field, up to its closing quote or the end of what
/// has arrived — and the usual escapes are undone, because a file containing
/// `\n` in its source should not appear as a literal backslash-n.
pub(super) fn streaming_content(call: &ToolCallView<'_>) -> String {
    let key = match call.name {
        "write" | "multi_edit" => "\"content\"",
        "edit" => "\"new_string\"",
        _ => return String::new(),
    };
    let Some(at) = call.preview.find(key) else {
        return String::new();
    };
    let rest = &call.preview[at + key.len()..];
    let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
    let Some(rest) = rest.strip_prefix('"') else {
        return String::new();
    };
    // The closing quote is the first one not preceded by a backslash; absent,
    // which is the normal case mid-stream, the text runs to the end.
    let mut end = rest.len();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            end = i;
            break;
        }
        i += 1;
    }
    unescape(&rest[..end])
}

/// The three escapes a streamed file body actually contains, in one pass.
fn unescape(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// The argument part worth displaying. Normally the daemon's title tail; a
/// title that is itself a raw JSON blob (a daemon that never derived one) is
/// parsed back into its salient field instead of being shown verbatim. While
/// the call is still `Running` with no dispatched title, `preview` holds the
/// streamed raw args — the same salient-field pick answers "with what".
pub(super) fn display_arg(call: &ToolCallView<'_>) -> String {
    let cleaned = strip_ansi(title_arg(call));
    let arg = cleaned.trim();
    if !arg.is_empty() {
        if arg.starts_with('{') {
            return arg_summary(call.name, arg).unwrap_or_default();
        }
        return arg.to_string();
    }
    if call.status == ToolStatus::Running {
        return arg_summary(call.name, call.preview).unwrap_or_default();
    }
    String::new()
}

/// Input keys that name what the tool acts on, in priority order — the same
/// list the daemon's `tool_title` uses (`command` beats `path`, `pattern`
/// beats `path`), plus per-tool orderings for the tools whose salient input
/// sits under another name (`agent` spawns by `name`, `bg` aims at a
/// `task_id`, …). Unknown tools take the shared list.
fn preferred_keys(tool: &str) -> &'static [&'static str] {
    match tool {
        "agent" => &["name", "prompt", "agent_id", "kind", "action"],
        "ask_question" => &["question", "header"],
        "ask_parent" => &["question"],
        "bg" => &["task_id", "action", "message"],
        "cron" => &["name", "schedule", "job_id", "action"],
        "monitor" => &["label", "monitor_id", "action"],
        "memory_write" => &["text", "scope"],
        "memory_read" => &["scope"],
        "plan" => &["item_id", "action"],
        "report_status" => &["text"],
        "skill" => &["name"],
        _ => &[
            "command",
            "pattern",
            "query",
            "path",
            "file_path",
            "file",
            "url",
            "uri",
        ],
    }
}

/// Input fields that describe the call rather than its target — never the
/// salient argument (the daemon's `tool_title` skips the same two).
const META_KEYS: &[&str] = &["intent", "accept_large_output"];

/// `cargo test`, `src/main.rs`, `TODO` — the salient input argument picked
/// from raw JSON args the way the daemon's `tool_title` picks it: the tool's
/// preferred keys first, then the first non-empty string field. `args` may
/// still be streaming — a strict parse is tried first, then a lenient
/// top-level scan reads the `"key": "value` pairs that have arrived. `None`
/// when nothing readable is there yet — the cell shows the bare label then.
fn arg_summary(tool: &str, args: &str) -> Option<String> {
    let args = args.trim();
    if args.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(args) {
        // A bare string/number/array is not an object arg list — the daemon
        // titles those by the tool name alone, and so do we.
        let obj = value.as_object()?;
        return pick_salient(tool, obj.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    let pairs = scan_json_object(args);
    pick_salient(
        tool,
        pairs.iter().map(|(k, v)| (k.as_str(), Some(v.as_str()))),
    )
}

/// Preferred keys in order, then the first non-empty string field that is
/// not a meta key — mirroring the daemon's `tool_title` so the provisional
/// row matches the dispatched one. The arg is capped at 60 chars, the same
/// bound the daemon puts on a title tail.
fn pick_salient<'a>(
    tool: &str,
    fields: impl Iterator<Item = (&'a str, Option<&'a str>)>,
) -> Option<String> {
    let fields: Vec<(&'a str, Option<&'a str>)> = fields.collect();
    for key in preferred_keys(tool) {
        if let Some(value) = fields.iter().find(|(k, _)| k == key).and_then(|(_, v)| *v)
            && !value.trim().is_empty()
        {
            return Some(value.trim().chars().take(60).collect());
        }
    }
    for (k, v) in fields {
        if META_KEYS.contains(&k) {
            continue;
        }
        if let Some(s) = v
            && !s.trim().is_empty()
        {
            return Some(s.trim().chars().take(60).collect());
        }
    }
    None
}

/// Top-level `"key": "value"` pairs of a JSON object that may still be
/// streaming. String values are unescaped; a value cut mid-string yields the
/// prefix received so far. Nested objects and arrays are skipped whole so
/// their inner strings are never mistaken for top-level fields, and the
/// first byte that stops making sense ends the scan — a partial tail never
/// poisons the pairs already read.
fn scan_json_object(text: &str) -> Vec<(String, String)> {
    let b = text.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0;
    skip_ws(b, &mut i);
    if !eat(b, &mut i, b'{') {
        return pairs;
    }
    loop {
        skip_ws(b, &mut i);
        match b.get(i) {
            Some(b'"') => {}
            _ => break,
        }
        let Some(key) = scan_json_string(text, &mut i) else {
            break;
        };
        skip_ws(b, &mut i);
        if !eat(b, &mut i, b':') {
            break;
        }
        skip_ws(b, &mut i);
        match b.get(i) {
            Some(b'"') => {
                if let Some(value) = scan_json_string(text, &mut i) {
                    pairs.push((key, value));
                }
            }
            Some(b'{') | Some(b'[') => skip_nested(b, &mut i),
            Some(_) => skip_literal(b, &mut i),
            None => break,
        }
        skip_ws(b, &mut i);
        match b.get(i) {
            Some(b',') => i += 1,
            _ => break,
        }
    }
    pairs
}

/// Decode a `"…"` JSON string at `i`. Returns `None` when `i` is not on a
/// quote; an unterminated string (args still streaming) yields the prefix
/// received so far.
fn scan_json_string(text: &str, i: &mut usize) -> Option<String> {
    let b = text.as_bytes();
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    while *i < b.len() {
        match b[*i] {
            b'"' => {
                *i += 1;
                return Some(out);
            }
            b'\\' => {
                *i += 1;
                let Some(&esc) = b.get(*i) else {
                    break;
                };
                *i += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        // `\uXXXX` — the code point when all four digits have
                        // arrived; a truncated escape ends the string scan.
                        let Some(hex) = b.get(*i..*i + 4) else {
                            return Some(out);
                        };
                        let Ok(hex) = std::str::from_utf8(hex) else {
                            return Some(out);
                        };
                        let Ok(cp) = u32::from_str_radix(hex, 16) else {
                            return Some(out);
                        };
                        *i += 4;
                        // Lone surrogates have no `char`; surrogate-pair
                        // decoding is out of scope for a preview string.
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                        }
                    }
                    _ => out.push(esc as char),
                }
            }
            _ => {
                // Copy one whole UTF-8 char — `i` only ever lands on a char
                // boundary, so the slice is always valid.
                let start = *i;
                *i += utf8_len(b[*i]);
                out.push_str(&text[start..*i]);
            }
        }
    }
    Some(out)
}

/// Skip a `{`/`[` … balanced close, honoring nested strings. Stops at EOF.
/// The caller sits on the opener.
fn skip_nested(b: &[u8], i: &mut usize) {
    let mut depth = 1usize;
    *i += 1;
    while *i < b.len() && depth > 0 {
        match b[*i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b'"' => skip_raw_string(b, i),
            _ => {}
        }
        *i += 1;
    }
}

/// Advance past a `"…"` literal without decoding it (inside skipped values).
fn skip_raw_string(b: &[u8], i: &mut usize) {
    *i += 1; // opening quote
    while *i < b.len() {
        match b[*i] {
            b'\\' => *i += 2,
            b'"' => {
                *i += 1;
                return;
            }
            _ => *i += 1,
        }
    }
}

/// Skip a `true`/`123`/`null`-style literal: everything to the next field
/// separator or closer.
fn skip_literal(b: &[u8], i: &mut usize) {
    while *i < b.len() && !matches!(b[*i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn eat(b: &[u8], i: &mut usize, byte: u8) -> bool {
    if b.get(*i) == Some(&byte) {
        *i += 1;
        true
    } else {
        false
    }
}
