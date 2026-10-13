//! The tool-call cell — one frame for every invocation (spec §6, "plaques and
//! bands"):
//!
//! ```text
//!  ✓ BASH  docker                                       0.4s
//!  │ $ docker ps --format "table {{.Names}}\t{{.Status}}"
//!  │ NAMES              STATUS
//!  │ … more lines · ctrl+o
//!  └ Exited with code 0
//! ```
//!
//! The header is a status badge (glyph + tool name on the outcome colour), the
//! argument in normal weight and the duration flush right. The body hangs on a
//! dim `│` gutter under the badge's glyph; output lines stay `dim`. ANSI
//! escapes in the program's own output are stripped before render so they can
//! never restyle the gutter. Shared strings come from `pacode_render::text` so
//! this crate keeps no display literals of its own.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::anim::sweep_spans;
use super::chrome;

use pacode_render::text as rtext;
use pacode_render::{
    RenderOptions, display_width, render_diff_ex, truncate_spans, truncate_to_width, wrap_line,
    wrap_text,
};
use pacode_types::time::format_duration_ms;
use pacode_types::transcript::{DiffStat, ToolStatus};

#[path = "toolcell_args.rs"]
mod args;
#[path = "toolcell_bg.rs"]
mod bg;
#[path = "toolcell_row.rs"]
mod row;
#[path = "toolcell_stream.rs"]
mod stream;

pub(crate) use bg::{paint_block, status_bg};
#[cfg(test)]
pub(crate) use row::OUTCOMES_COUNTED;
pub(crate) use row::{compact_row, group_header, more_row};

#[cfg(test)]
use args::title_arg;
use args::{display_arg, is_content_tool, streaming_content};

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
    /// Output the call has produced while it is still running. `None` when
    /// nothing has arrived yet; the transcript keeps it (capped) until the
    /// call's final item brings the real preview.
    pub streamed: Option<crate::state::transcript::ToolStreamView<'a>>,
}

impl<'a> ToolCallView<'a> {
    /// The view of a `ToolCall` item; `None` for every other kind.
    pub fn of(
        kind: &'a pacode_types::TranscriptKind,
        streamed: Option<crate::state::transcript::ToolStreamView<'a>>,
    ) -> Option<Self> {
        let pacode_types::TranscriptKind::ToolCall {
            name,
            title,
            status,
            preview,
            diff,
            duration_ms,
            ..
        } = kind
        else {
            return None;
        };
        Some(Self {
            name,
            title,
            status: *status,
            preview,
            diff: diff.as_ref(),
            duration_ms: *duration_ms,
            streamed,
        })
    }
}

/// Header + body on the outcome gutter, all on the status band, + the blank
/// separator every cell ends with (never painted).
pub fn render(
    call: &ToolCallView<'_>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let width = width as usize;
    // ` │ ` hangs the gutter under the badge's glyph — the body starts at
    // column 3. The gutter is quiet; the badge carries the outcome.
    let body_w = width.saturating_sub(GUTTER_W);
    let gutter_style = opts.theme.dim;

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
        } else if is_content_tool(call.name) {
            // The arguments of a writing tool *are* the thing being written, and
            // they arrive as the model emits them. Drawing the whole growing
            // value — not the first line of it — is the difference between
            // "a write is running" and "this is what it is writing".
            let content = streaming_content(call);
            if content.is_empty() {
                // Nothing of the content has arrived: show the target instead,
                // so the row says what is being written rather than nothing.
                let arg = display_arg(call);
                if let Some(first) = arg.lines().map(str::trim).find(|l| !l.is_empty()) {
                    out.push(Line::from(vec![
                        gutter(opts, gutter_style),
                        Span::styled(truncate_to_width(first, body_w, true), opts.theme.fg),
                    ]));
                }
            } else {
                stream::push_live_body(
                    &mut out,
                    &strip_ansi(&content),
                    0,
                    body_w,
                    gutter_style,
                    opts,
                    true,
                );
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
        // Output the call has produced so far, on the rows a finished body
        // uses. It arrives with the same escapes the finished preview has, so
        // it is stripped here too — a live body must never restyle the gutter.
        if let Some(streamed) = call.streamed {
            let text = strip_ansi(streamed.text);
            stream::push_live_body(
                &mut out,
                &text,
                streamed.dropped,
                body_w,
                gutter_style,
                opts,
                expanded,
            );
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

    // ` └ Exited with code N` — the footer's elbow ends the gutter, the header
    // stays clean of it.
    if let Some(code) = exit_code {
        out.push(Line::from(vec![
            Span::styled(format!(" {} ", opts.glyphs.vcorner), gutter_style),
            Span::styled(format!("Exited with code {code}"), opts.theme.faint),
        ]));
    }

    paint_block(&mut out, status_bg(call.status, opts), width);
    out.push(Line::default());
    out
}

/// Columns the body gutter takes: ` │ `.
const GUTTER_W: usize = 3;

/// Gutter cell: ` │ `, its bar under the badge's glyph.
fn gutter(opts: &RenderOptions, style: Style) -> Span<'static> {
    Span::styled(format!(" {} ", opts.glyphs.vline), style)
}

/// Outcome → badge tone: accent while running, green ok, red failed, violet
/// backgrounded, dim when the call was refused.
fn badge_tone(status: ToolStatus, opts: &RenderOptions) -> Style {
    match status {
        ToolStatus::Running => opts.theme.accent,
        ToolStatus::Ok => opts.theme.green,
        ToolStatus::Error => opts.theme.red,
        ToolStatus::Denied => opts.theme.dim,
        ToolStatus::Backgrounded => opts.theme.violet,
    }
}

/// The badge's glyph: the spinner while running, the outcome after.
fn badge_glyph(status: ToolStatus, opts: &RenderOptions, anim_frame: u64) -> &'static str {
    match status {
        ToolStatus::Running => opts.glyphs.spinner(anim_frame),
        ToolStatus::Ok => opts.glyphs.ok,
        ToolStatus::Error => opts.glyphs.fail,
        ToolStatus::Denied => opts.glyphs.disabled,
        ToolStatus::Backgrounded => opts.glyphs.running,
    }
}

/// ` ✓ READ ` badge, the arg in normal weight, the outcome flush right —
/// the arg is the elastic part, the badge and the outcome never get cut
/// while the arg has a cell left to give.
fn header(
    call: &ToolCallView<'_>,
    width: usize,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Line<'static> {
    let right = outcome_spans(call, opts);
    let right_w: usize = right.iter().map(|s| display_width(&s.content)).sum();
    // At least one cell between the left part and the outcome.
    let avail_left = width.saturating_sub(right_w + usize::from(right_w > 0));

    let label = rtext::tool_label(call.name)
        .map(str::to_string)
        .unwrap_or_else(|| capitalize(call.name))
        .to_uppercase();
    let glyph = badge_glyph(call.status, opts, anim_frame);
    // Two cells of badge padding; the label yields before the glyph does.
    let badge_text = truncate_to_width(
        &format!("{glyph} {label}"),
        avail_left.saturating_sub(2),
        true,
    );
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left_w = 0;
    if !badge_text.is_empty() {
        let badge = chrome::badge(&badge_text, badge_tone(call.status, opts), opts);
        left_w = display_width(&badge.content);
        spans.push(badge);
    }

    let arg = short_arg(call);
    if !arg.is_empty() && left_w + 1 < avail_left {
        let shown = truncate_to_width(&arg, avail_left - left_w - 1, true);
        left_w += 1 + display_width(&shown);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(shown, opts.theme.fg));
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
///
/// Returns the index in `out` of the last *output* row pushed, or `None` when
/// the body was empty — a live body marks that row as still arriving, and a
/// marker row is not one.
fn push_collapsed(
    out: &mut Vec<Line<'static>>,
    status: ToolStatus,
    lines: &[&str],
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) -> Option<usize> {
    let total = lines.len();
    if status == ToolStatus::Error && total > ERROR_TAIL_LINES {
        let hidden = total - ERROR_TAIL_LINES;
        out.push(marker_line(hidden, gutter_style, opts));
        for l in &lines[hidden..] {
            out.push(body_line(l, body_w, gutter_style, opts));
        }
        return last_index(out);
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
        return last_index(out);
    }
    let first = out.len();
    for l in lines {
        out.push(body_line(l, body_w, gutter_style, opts));
    }
    if first == out.len() {
        return None;
    }
    Some(out.len() - 1)
}

/// Expanded: the whole output wrapped, still bounded at `EXPANDED_MAX_LINES`.
/// Returns the index of the last output row, as [`push_collapsed`] does — the
/// truncation marker that may follow it is not one.
fn push_expanded(
    out: &mut Vec<Line<'static>>,
    lines: &[&str],
    body_w: usize,
    gutter_style: Style,
    opts: &RenderOptions,
) -> Option<usize> {
    let mut drawn = 0usize;
    let mut last = None;
    'lines: for l in lines {
        for wrapped in wrap_text(l, body_w.max(1)) {
            out.push(Line::from(vec![
                gutter(opts, gutter_style),
                Span::styled(wrapped, opts.theme.dim),
            ]));
            drawn += 1;
            last = Some(out.len() - 1);
            if drawn >= EXPANDED_MAX_LINES {
                out.push(Line::from(vec![
                    gutter(opts, gutter_style),
                    Span::styled("[output truncated]", opts.theme.faint),
                ]));
                break 'lines;
            }
        }
    }
    last
}

/// Index of the last row of `out`, or `None` when there is none.
fn last_index(out: &[Line<'static>]) -> Option<usize> {
    out.len().checked_sub(1)
}

/// The right edge: `running` (swept) while the call runs, then the diff stat
/// for edits and the duration, `background` or `denied`. The status glyph is
/// in the badge and the exit code in the body's `└` footer.
fn outcome_spans(call: &ToolCallView<'_>, opts: &RenderOptions) -> Vec<Span<'static>> {
    let mut groups: Vec<Vec<Span<'static>>> = Vec::new();
    match call.status {
        ToolStatus::Running => {
            // The running cell bypasses the line cache, so the badge spinner
            // turns on the anim frame alone — and so can the sweep, which is
            // drawn from the same clock. A band crossing the row is what tells
            // the eye it is still moving rather than stuck.
            groups.push(sweep_spans("running".to_string(), opts.theme.accent, opts));
        }
        ToolStatus::Ok | ToolStatus::Error => {
            if let Some(diff) = call.diff {
                groups.push(diff_stat_spans(diff, opts));
            }
        }
        ToolStatus::Backgrounded => groups.push(vec![Span::styled(
            rtext::in_background().to_string(),
            opts.theme.violet,
        )]),
        ToolStatus::Denied => groups.push(vec![Span::styled("denied", opts.theme.dim)]),
    }
    if let Some(ms) = call.duration_ms {
        groups.push(vec![Span::styled(format_duration_ms(ms), opts.theme.dim)]);
    }
    let mut spans = Vec::new();
    for (i, group) in groups.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(opts.glyphs.dot_sep, opts.theme.faint));
        }
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
