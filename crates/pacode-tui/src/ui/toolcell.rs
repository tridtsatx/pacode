//! The tool-call cell — one frame for every invocation (spec §6):
//!
//! ```text
//! ▣ Command · docker · 0.4s                                ✓
//! │ $ docker ps --format "table {{.Names}}\t{{.Status}}"
//! │ NAMES              STATUS
//! │ … more lines · ctrl+o
//! └ Exited with code 0
//! ```
//!
//! The header answers "what and with what" and carries the outcome flush right.
//! The body hangs on the `│` gutter coloured by outcome while output lines
//! themselves stay `dim`. ANSI escapes in the program's own output are stripped
//! before render so they can never restyle the gutter. Shared strings come
//! from `pacode_render::text` so this crate keeps no display literals of its own.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use pacode_render::text as rtext;
use pacode_render::{
    RenderOptions, display_width, render_diff_ex, truncate_spans, truncate_to_width, wrap_line,
    wrap_text,
};
use pacode_types::time::format_duration_ms;
use pacode_types::transcript::{DiffStat, ToolStatus};

#[cfg(test)]
#[path = "toolcell_tests.rs"]
mod toolcell_tests;

/// Collapsed success: first `HEAD` + last `TAIL` output lines around the fold.
pub const COLLAPSED_HEAD_LINES: usize = 6;
pub const COLLAPSED_TAIL_LINES: usize = 3;
/// Failure output is read from the end — the error sits at the bottom.
pub const ERROR_TAIL_LINES: usize = 8;
/// Ceiling on an expanded cell — expansion is a reading aid, not a 200 MB log.
pub const EXPANDED_MAX_LINES: usize = 400;

/// The fields of `TranscriptKind::ToolCall` the cell reads, flattened.
pub struct ToolCallView<'a> {
    pub name: &'a str,
    pub title: &'a str,
    pub status: ToolStatus,
    pub preview: &'a str,
    pub diff: Option<&'a DiffStat>,
    pub duration_ms: Option<u64>,
}

/// Header + body on the outcome gutter + the blank separator every cell ends with.
pub fn render(
    call: &ToolCallView<'_>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let width = width as usize;
    // `│ ` is one glyph + one space — the body starts at column 2.
    let body_w = width.saturating_sub(2);
    let gutter_style = outcome_style(call.status, opts);

    let preview = strip_ansi(call.preview);
    let exit_code = match call.status {
        ToolStatus::Ok | ToolStatus::Error => parse_exit_code(&preview),
        ToolStatus::Running | ToolStatus::Backgrounded | ToolStatus::Denied => None,
    };

    out.push(header(call, width, opts, anim_frame));

    if call.status == ToolStatus::Running {
        // While the call runs, `preview` is the streamed raw-args JSON the
        // daemon keeps so the row does not blink empty — never rendered
        // verbatim (a JSON blob is not a preview). The input row shows the
        // parsed argument instead, formatted the same way a finished cell
        // spells out its input: `$ <command>` for bash, the arg itself for
        // every other tool.
        if expanded {
            if call.name == "bash" {
                push_command_lines(&mut out, call, body_w, gutter_style, opts);
            } else {
                push_arg_lines(&mut out, call, body_w, gutter_style, opts);
            }
        } else if call.name == "bash" {
            if !display_arg(call).trim().is_empty() {
                out.push(command_line(call, body_w, gutter_style, opts));
            }
        } else {
            let arg = display_arg(call);
            if let Some(first) = arg.lines().map(str::trim).find(|l| !l.is_empty()) {
                out.push(Line::from(vec![
                    gutter(opts, gutter_style),
                    Span::styled(truncate_to_width(first, body_w, true), opts.theme.fg),
                ]));
            }
        }
        // The daemon's own note when the 1 MiB args cap bit — still a row.
        if let Some(marker) = args_truncation_marker(&preview) {
            out.push(Line::from(vec![
                gutter(opts, gutter_style),
                Span::styled(marker.to_string(), opts.theme.faint),
            ]));
        }
    } else if is_diff_preview(&preview) {
        // A diff preview goes through the diff renderer at body width so its
        // background bands end exactly at the cell edge.
        let mut diff_opts = opts.clone();
        diff_opts.width = body_w as u16;
        for dl in render_diff_ex(&preview, &diff_opts, expanded) {
            let mut spans = vec![gutter(opts, gutter_style)];
            spans.extend(dl.spans);
            out.push(Line::from(spans));
        }
    } else {
        // The `[exit code N]` line is dropped from the body once the header
        // carries the code — and always on failure, matching the old cell.
        let drop_exit = exit_code.is_some() || call.status == ToolStatus::Error;
        let content_lines: Vec<&str> = preview
            .lines()
            .filter(|l| !(drop_exit && is_exit_line(l)))
            .collect();
        if expanded {
            // Expanded spells out the call's input first — the command on the
            // `$` row for bash, the title arg on its own row for every other
            // tool — then every output line wrapped, never cut or folded.
            if call.name == "bash" {
                push_command_lines(&mut out, call, body_w, gutter_style, opts);
            } else {
                push_arg_lines(&mut out, call, body_w, gutter_style, opts);
            }
            if content_lines.iter().any(|l| !l.trim().is_empty()) {
                push_expanded(&mut out, &content_lines, body_w, gutter_style, opts);
            }
        } else if content_lines.iter().any(|l| !l.trim().is_empty()) {
            if call.name == "bash" {
                out.push(command_line(call, body_w, gutter_style, opts));
            }
            push_collapsed(
                &mut out,
                call.status,
                &content_lines,
                body_w,
                gutter_style,
                opts,
            );
        }
    }

    // `└ Exited with code N` — the footer's elbow ends the gutter, the header
    // stays clean of it.
    if let Some(code) = exit_code {
        out.push(Line::from(vec![
            Span::styled(format!("{} ", opts.glyphs.vcorner), gutter_style),
            Span::styled(format!("Exited with code {code}"), opts.theme.faint),
        ]));
    }

    out.push(Line::default());
    out
}

/// Gutter cell: `│ ` tinted by the outcome.
fn gutter(opts: &RenderOptions, style: Style) -> Span<'static> {
    Span::styled(format!("{} ", opts.glyphs.vline), style)
}

/// Outcome → gutter colour: grey running, green ok, red failure, violet bg.
fn outcome_style(status: ToolStatus, opts: &RenderOptions) -> Style {
    match status {
        ToolStatus::Running => opts.theme.dim,
        ToolStatus::Ok => opts.theme.green,
        ToolStatus::Error | ToolStatus::Denied => opts.theme.red,
        ToolStatus::Backgrounded => opts.theme.violet,
    }
}

/// `▣ <tool> · <arg> · <duration>` left, outcome glyph at the right edge —
/// the arg is elastic, the duration hugs the title instead of the far edge.
fn header(
    call: &ToolCallView<'_>,
    width: usize,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Line<'static> {
    let right = outcome_spans(call, opts, anim_frame);
    let right_w: usize = right.iter().map(|s| display_width(&s.content)).sum();
    // At least one cell between title and outcome.
    let avail_left = width.saturating_sub(right_w + 1);

    let label = rtext::tool_label(call.name)
        .map(str::to_string)
        .unwrap_or_else(|| capitalize(call.name));
    let lead_raw = format!("{} {}", opts.glyphs.tool, label);
    let lead = if display_width(&lead_raw) <= avail_left {
        lead_raw
    } else {
        truncate_to_width(&lead_raw, avail_left, true)
    };
    let mut left_w = display_width(&lead);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if !lead.is_empty() {
        spans.push(Span::styled(lead, opts.theme.cyan));
    }

    let sep_w = display_width(opts.glyphs.dot_sep);
    let push_tail = |spans: &mut Vec<Span<'static>>, text: &str, left_w: &mut usize| {
        if text.is_empty() || *left_w + sep_w >= avail_left {
            return;
        }
        let shown = truncate_to_width(text, avail_left - *left_w - sep_w, true);
        spans.push(Span::styled(
            opts.glyphs.dot_sep.to_string(),
            opts.theme.dim,
        ));
        *left_w += sep_w + display_width(&shown);
        spans.push(Span::styled(shown, opts.theme.dim));
    };

    let arg = short_arg(call);
    push_tail(&mut spans, &arg, &mut left_w);
    if let Some(ms) = call.duration_ms {
        push_tail(&mut spans, &format_duration_ms(ms), &mut left_w);
    }

    let pad = width.saturating_sub(left_w + right_w);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.extend(right);
    Line::from(spans)
}

/// The header's "with what": the command label for `bash` (the whole command
/// is on the `$` row), the title arg for every other tool.
fn short_arg(call: &ToolCallView<'_>) -> String {
    let cleaned = display_arg(call);
    let first = cleaned.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return String::new();
    }
    if call.name == "bash" {
        pacode_types::task_label_from_command(first)
    } else {
        first.to_string()
    }
}

/// `title` is `<name> <arg>` — the arg part, or the whole title when it does
/// not start with the name. The name prefix matches case-insensitively:
/// tools emit `Edit <path>` while the call is named `edit`.
fn title_arg<'a>(call: &ToolCallView<'a>) -> &'a str {
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

/// The argument part worth displaying. Normally the daemon's title tail; a
/// title that is itself a raw JSON blob (a daemon that never derived one) is
/// parsed back into its salient field instead of being shown verbatim. While
/// the call is still `Running` with no dispatched title, `preview` holds the
/// streamed raw args — the same salient-field pick answers "with what".
fn display_arg(call: &ToolCallView<'_>) -> String {
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

/// The `…[arguments truncated at 1 MiB]` line the daemon appends to the
/// streamed-args preview when the raw-args cap bit — kept visible while the
/// args themselves are never rendered.
fn args_truncation_marker(preview: &str) -> Option<&str> {
    preview
        .lines()
        .map(str::trim)
        .find(|l| l.contains("arguments truncated"))
}

/// `│ $ <command>` wrapped across as many gutter rows as it needs — the
/// expanded bash body shows the whole command, highlighted; continuation
/// lines align under the command (the `$ ` prefix is two cells).
fn push_command_lines(
    out: &mut Vec<Line<'static>>,
    call: &ToolCallView<'_>,
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) {
    let cleaned = display_arg(call);
    if cleaned.trim().is_empty() {
        return;
    }
    let mut code = vec![Span::styled("$ ", opts.theme.dim)];
    code.extend(
        crate::bash::tokenize(&cleaned)
            .into_iter()
            .map(|s| Span::styled(s.text.to_string(), opts.theme.bash_style(s.role))),
    );
    for wrapped in wrap_line(Line::from(code), body_w.max(1), 2) {
        let mut spans = vec![gutter(opts, gutter_style)];
        spans.extend(wrapped.spans);
        out.push(Line::from(spans));
    }
}

/// `│ <arg>` — a non-bash call's full argument, wrapped; the header only ever
/// has room for a truncated summary, so expansion is where the whole arg shows.
/// Brighter than the dim output lines: it is the call's input, not its output.
fn push_arg_lines(
    out: &mut Vec<Line<'static>>,
    call: &ToolCallView<'_>,
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) {
    let cleaned = display_arg(call);
    for l in cleaned.lines() {
        for wrapped in wrap_text(l, body_w.max(1)) {
            out.push(Line::from(vec![
                gutter(opts, gutter_style),
                Span::styled(wrapped, opts.theme.fg),
            ]));
        }
    }
}

/// `│ $ <command>` — the bash command in the theme's bash-role highlighting.
fn command_line(
    call: &ToolCallView<'_>,
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) -> Line<'static> {
    let cleaned = display_arg(call);
    let cmd = cleaned.lines().next().unwrap_or("");
    let code: Vec<Span<'static>> = crate::bash::tokenize(cmd)
        .into_iter()
        .map(|s| Span::styled(s.text.to_string(), opts.theme.bash_style(s.role)))
        .collect();
    let mut spans = vec![
        gutter(opts, gutter_style),
        Span::styled("$ ", opts.theme.dim),
    ];
    spans.extend(truncate_spans(
        &code,
        body_w.saturating_sub(2),
        opts.glyphs.ellipsis,
    ));
    Line::from(spans)
}

/// One output row on the gutter, cut to the body width — never wrapped.
fn body_line(
    text: &str,
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) -> Line<'static> {
    Line::from(vec![
        gutter(opts, gutter_style),
        Span::styled(truncate_to_width(text, body_w, true), opts.theme.dim),
    ])
}

/// The fold marker: `│ … N more lines · ctrl+o to expand` — what the cap hides.
fn marker_line(hidden: usize, gutter_style: Style, opts: &RenderOptions) -> Line<'static> {
    Line::from(vec![
        gutter(opts, gutter_style),
        Span::styled(rtext::more_lines(hidden, opts), opts.theme.faint),
    ])
}

/// Collapsed caps (§6): ok = first 6 + last 3 around the marker; failure = tail 8.
fn push_collapsed(
    out: &mut Vec<Line<'static>>,
    status: ToolStatus,
    lines: &[&str],
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) {
    let total = lines.len();
    if status == ToolStatus::Error && total > ERROR_TAIL_LINES {
        let hidden = total - ERROR_TAIL_LINES;
        out.push(marker_line(hidden, gutter_style, opts));
        for l in &lines[hidden..] {
            out.push(body_line(l, body_w, gutter_style, opts));
        }
        return;
    }
    if status != ToolStatus::Error && total > COLLAPSED_HEAD_LINES + COLLAPSED_TAIL_LINES {
        let hidden = total - COLLAPSED_HEAD_LINES - COLLAPSED_TAIL_LINES;
        for l in &lines[..COLLAPSED_HEAD_LINES] {
            out.push(body_line(l, body_w, gutter_style, opts));
        }
        out.push(marker_line(hidden, gutter_style, opts));
        for l in &lines[total - COLLAPSED_TAIL_LINES..] {
            out.push(body_line(l, body_w, gutter_style, opts));
        }
        return;
    }
    for l in lines {
        out.push(body_line(l, body_w, gutter_style, opts));
    }
}

/// Expanded: the whole output wrapped, still bounded at `EXPANDED_MAX_LINES`.
fn push_expanded(
    out: &mut Vec<Line<'static>>,
    lines: &[&str],
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) {
    let mut drawn = 0usize;
    'lines: for l in lines {
        for wrapped in wrap_text(l, body_w.max(1)) {
            out.push(Line::from(vec![
                gutter(opts, gutter_style),
                Span::styled(wrapped, opts.theme.dim),
            ]));
            drawn += 1;
            if drawn >= EXPANDED_MAX_LINES {
                out.push(Line::from(vec![
                    gutter(opts, gutter_style),
                    Span::styled("[output truncated]", opts.theme.faint),
                ]));
                break 'lines;
            }
        }
    }
}

/// `✓`, `✗`, `◍ background`, `<spinner> running`, `denied` — the status glyph
/// plus a diff stat for edits. The exit code lives in the body's `└` footer
/// and the duration hugs the title, so the right edge stays a bare glyph.
fn outcome_spans(
    call: &ToolCallView<'_>,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<Span<'static>> {
    let sep = opts.glyphs.dot_sep;
    match call.status {
        ToolStatus::Running => {
            // The running cell bypasses the line cache, so this spinner turns
            // on the anim frame alone.
            let spinner = opts.glyphs.spinner(anim_frame);
            vec![Span::styled(
                format!("{spinner} running"),
                opts.theme.accent,
            )]
        }
        ToolStatus::Ok => {
            let mut groups: Vec<Vec<Span<'static>>> = Vec::new();
            if let Some(diff) = call.diff {
                groups.push(diff_stat_spans(diff, opts));
            }
            join_outcome(opts.glyphs.ok, opts.theme.green, groups, sep)
        }
        ToolStatus::Error => join_outcome(opts.glyphs.fail, opts.theme.red, vec![], sep),
        ToolStatus::Backgrounded => join_outcome(
            opts.glyphs.running,
            opts.theme.violet,
            vec![vec![Span::styled(
                rtext::in_background().to_string(),
                opts.theme.violet,
            )]],
            sep,
        ),
        ToolStatus::Denied => vec![Span::styled("denied", opts.theme.red)],
    }
}

/// `<glyph>` + groups joined by ` · `: the first group follows the glyph with
/// a plain space, later groups take the dot separator.
fn join_outcome(
    glyph: &str,
    style: Style,
    groups: Vec<Vec<Span<'static>>>,
    sep: &'static str,
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(glyph.to_string(), style)];
    for (i, group) in groups.into_iter().enumerate() {
        let between = if i == 0 { " " } else { sep };
        spans.push(Span::styled(between.to_string(), style));
        spans.extend(group);
    }
    spans
}

/// `+A −R` for edit/write calls.
fn diff_stat_spans(diff: &DiffStat, opts: &RenderOptions) -> Vec<Span<'static>> {
    let minus = if opts.glyphs.ascii { "-" } else { "−" };
    vec![
        Span::styled(format!("+{}", diff.added), opts.theme.green),
        Span::raw(" "),
        Span::styled(format!("{minus}{}", diff.removed), opts.theme.red),
    ]
}

/// Whether a preview is a unified diff and goes through the diff renderer.
/// `--- a/` and `--- /dev/null` are the headers `unified_diff` emits; a bare
/// `--- ` prefix alone is not enough (`batch` section headers look alike).
fn is_diff_preview(preview: &str) -> bool {
    preview.starts_with("--- a/")
        || preview.starts_with("--- /dev/null")
        || preview.contains("\n+++ ")
}

/// The `[exit code N]` marker the exec layer appends — the header shows the code.
fn is_exit_line(l: &str) -> bool {
    let t = l.trim();
    t.starts_with("[exit code ") && t.ends_with(']')
}

fn parse_exit_code(preview: &str) -> Option<i32> {
    for line in preview.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[exit code ")
            && let Some(num_str) = rest.strip_suffix(']')
            && let Ok(code) = num_str.trim().parse::<i32>()
        {
            return Some(code);
        }
    }
    if let Some(start) = preview.find("[exit code ") {
        let rest = &preview[start + "[exit code ".len()..];
        if let Some(end) = rest.find(']')
            && let Ok(code) = rest[..end].trim().parse::<i32>()
        {
            return Some(code);
        }
    }
    None
}

/// `read_file` → `Read_file` — fallback for tools without a display label.
/// MCP names are `server__tool`; the `__` reads better as a split.
fn capitalize(name: &str) -> String {
    let mut out = String::new();
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out.replace("__", ": ")
}

/// Remove ANSI escapes before render: CSI (`ESC [ … final`), OSC (`ESC ] …`
/// ended by BEL or ST), charset picks (`ESC ( X`), two-byte `ESC X`.
/// `pub(crate)`: the Background menu's output tail strips spool files with
/// the same table before they reach a detail card.
pub(crate) fn strip_ansi(input: &str) -> String {
    if !input.contains('\u{1b}') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                // CSI: parameter/intermediate bytes until the final byte.
                b'[' => {
                    i += 1;
                    while i < bytes.len() {
                        let b = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&b) {
                            break;
                        }
                    }
                }
                // OSC: until BEL or ST (`ESC \`).
                b']' => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                // Charset and other two-part sequences.
                b'(' | b')' | b'*' | b'+' | b'#' => i += 2,
                // `ESC` + single character.
                _ => i += 1,
            }
            continue;
        }
        // Copy one whole char — ESC is ASCII, so `i` only ever lands on a
        // char boundary and the slice is always valid UTF-8.
        let start = i;
        i += utf8_len(bytes[i]);
        out.push_str(&input[start..i]);
    }
    out
}

/// Length of the UTF-8 char starting with byte `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}
