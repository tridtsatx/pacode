//! The tool-call cell — one frame for every invocation (spec §6):
//!
//! ```text
//! ▣ Command · docker                          ✓ exit 0 · 0.4s
//! │ $ docker ps --format "table {{.Names}}\t{{.Status}}"
//! │ NAMES              STATUS
//! │ … more lines · ctrl+o
//! ```
//!
//! The header answers "what and with what" and carries the outcome flush right.
//! The body hangs on the `│` gutter coloured by outcome while output lines
//! themselves stay `dim`. ANSI escapes in the program's own output are stripped
//! before render so they can never restyle the gutter. Russian UI strings come
//! from `pacode_render::text` — this crate forbids Cyrillic literals.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use pacode_render::text as rtext;
use pacode_render::{
    RenderOptions, display_width, render_diff_ex, truncate_spans, truncate_to_width, wrap_text,
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

    out.push(header(call, exit_code, width, opts, anim_frame));

    if preview.starts_with("--- ") || preview.contains("\n+++ ") {
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
        if content_lines.iter().any(|l| !l.trim().is_empty()) {
            if call.name == "bash" {
                out.push(command_line(call, body_w, gutter_style, opts));
            }
            if expanded {
                push_expanded(&mut out, &content_lines, body_w, gutter_style, opts);
            } else {
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

/// `▣ <tool> · <arg>` left, outcome at the right edge — the arg is elastic.
fn header(
    call: &ToolCallView<'_>,
    exit_code: Option<i32>,
    width: usize,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Line<'static> {
    let right = outcome_spans(call, exit_code, opts, anim_frame);
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

    let arg = short_arg(call);
    let sep_w = display_width(opts.glyphs.dot_sep);
    if !arg.is_empty() && left_w + sep_w < avail_left {
        let arg = truncate_to_width(&arg, avail_left - left_w - sep_w, true);
        spans.push(Span::styled(
            opts.glyphs.dot_sep.to_string(),
            opts.theme.dim,
        ));
        left_w += sep_w + display_width(&arg);
        spans.push(Span::styled(arg, opts.theme.dim));
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
    let cleaned = strip_ansi(title_arg(call));
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
/// not start with the name (snapshot rows differ).
fn title_arg<'a>(call: &ToolCallView<'a>) -> &'a str {
    call.title
        .strip_prefix(call.name)
        .map(str::trim_start)
        .unwrap_or(call.title)
}

/// `│ $ <command>` — the bash command in the theme's bash-role highlighting.
fn command_line(
    call: &ToolCallView<'_>,
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) -> Line<'static> {
    let cleaned = strip_ansi(title_arg(call));
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

/// The fold marker: `│ … ещё N строк · ctrl+o развернуть` — what the cap hides.
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

/// `✓ exit 0 · 0.4s`, `✗ exit 1 · 12s`, `◍ в фоне`, `<spinner> running`, `denied`.
fn outcome_spans(
    call: &ToolCallView<'_>,
    exit_code: Option<i32>,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<Span<'static>> {
    let sep = opts.glyphs.dot_sep;
    match call.status {
        ToolStatus::Running => {
            // The running cell bypasses the line cache, so this spinner turns
            // on the anim frame alone.
            let spinner = opts.glyphs.spinner(anim_frame);
            let label = match call.duration_ms {
                Some(ms) => format!("{spinner} running {}", format_duration_ms(ms)),
                None => format!("{spinner} running"),
            };
            vec![Span::styled(label, opts.theme.accent)]
        }
        ToolStatus::Ok => {
            let mut groups: Vec<Vec<Span<'static>>> = Vec::new();
            if let Some(code) = exit_code {
                groups.push(vec![Span::styled(format!("exit {code}"), opts.theme.green)]);
            }
            if let Some(diff) = call.diff {
                groups.push(diff_stat_spans(diff, opts));
            }
            if let Some(ms) = call.duration_ms {
                groups.push(vec![Span::styled(format_duration_ms(ms), opts.theme.green)]);
            }
            join_outcome(opts.glyphs.ok, opts.theme.green, groups, sep)
        }
        ToolStatus::Error => {
            let head = match exit_code {
                Some(code) => format!("exit {code}"),
                None => "fail".to_string(),
            };
            let mut groups = vec![vec![Span::styled(head, opts.theme.red)]];
            if let Some(ms) = call.duration_ms {
                groups.push(vec![Span::styled(format_duration_ms(ms), opts.theme.red)]);
            }
            join_outcome(opts.glyphs.fail, opts.theme.red, groups, sep)
        }
        ToolStatus::Backgrounded => {
            let mut groups = vec![vec![Span::styled(
                rtext::in_background().to_string(),
                opts.theme.violet,
            )]];
            if let Some(ms) = call.duration_ms {
                groups.push(vec![Span::styled(
                    format_duration_ms(ms),
                    opts.theme.violet,
                )]);
            }
            join_outcome(opts.glyphs.running, opts.theme.violet, groups, sep)
        }
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
fn capitalize(name: &str) -> String {
    let mut out = String::new();
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
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
