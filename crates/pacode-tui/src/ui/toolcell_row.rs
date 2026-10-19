//! The two row shapes of an exploration group (`ui::toolgroup`): its header,
//! drawn by the tool header itself, and one compact row per member call —
//! the label and argument a tool header shows, plus a short outcome.

use ratatui::text::{Line, Span};

use pacode_render::text as rtext;
use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::transcript::ToolStatus;

use super::{GUTTER_W, ToolCallView, capitalize, gutter, header, short_arg, strip_ansi};
use crate::ui::anim::sweep_spans;

#[cfg(test)]
thread_local! {
    /// How many call outcomes this thread has counted out of a preview: the
    /// group memo test asserts a finished call is counted once, not per frame.
    pub(crate) static OUTCOMES_COUNTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// ` ✓ EXPLORE  2 reads · 1 search` — a tool header whose label is the group
/// and whose argument is the tally, `running` swept on the right while any
/// member still runs.
pub(crate) fn group_header(
    tally: &str,
    running: bool,
    width: usize,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Line<'static> {
    let title = format!("explore {tally}");
    let view = ToolCallView {
        name: "explore",
        title: &title,
        status: if running {
            ToolStatus::Running
        } else {
            ToolStatus::Ok
        },
        preview: "",
        diff: None,
        duration_ms: None,
        streamed: None,
    };
    header(&view, width, opts, anim_frame)
}

/// ` │ Read src/main.rs → 120 lines` — one member of a group on the gutter:
/// the label and argument its own header would show, then how it came out.
/// The argument is the elastic part; a running call carries the swept
/// `running` in place of the outcome.
pub(crate) fn compact_row(
    call: &ToolCallView<'_>,
    width: usize,
    opts: &RenderOptions,
) -> Line<'static> {
    let body_w = width.saturating_sub(GUTTER_W);
    let label = rtext::tool_label(call.name)
        .map(str::to_string)
        .unwrap_or_else(|| capitalize(call.name));
    let tail = if call.status == ToolStatus::Running {
        sweep_spans("running".to_string(), opts.theme.accent, opts)
    } else {
        // A lone search hit can be a whole source line: it gets a third of
        // the row, so the argument keeps the rest.
        let outcome = truncate_to_width(&outcome(call), (body_w / 3).max(1), true);
        vec![Span::styled(
            format!("{} {outcome}", opts.glyphs.arrow_right),
            opts.theme.dim,
        )]
    };
    let tail_w: usize = tail.iter().map(|s| display_width(&s.content)).sum();

    let mut spans = vec![gutter(opts, opts.theme.dim)];
    let label = truncate_to_width(&label, body_w, true);
    let mut used = display_width(&label);
    spans.push(Span::styled(label, opts.theme.cyan));
    let arg = short_arg(call);
    // One cell before the arg and one before the outcome.
    let arg_room = body_w.saturating_sub(used + 2 + tail_w);
    if !arg.is_empty() && arg_room > 0 {
        let shown = truncate_to_width(&arg, arg_room, true);
        used += 1 + display_width(&shown);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(shown, opts.theme.fg));
    }
    if used + 1 + tail_w <= body_w {
        spans.push(Span::raw(" "));
        spans.extend(tail);
    }
    Line::from(spans)
}

/// ` │ … 5 more calls` — the members a long group keeps off screen, above
/// the newest ones it shows.
pub(crate) fn more_row(hidden: usize, opts: &RenderOptions) -> Line<'static> {
    let noun = if hidden == 1 { "call" } else { "calls" };
    Line::from(vec![
        gutter(opts, opts.theme.dim),
        Span::styled(
            format!("{} {hidden} more {noun}", opts.glyphs.ellipsis),
            opts.theme.faint,
        ),
    ])
}

/// `120 lines`, `4 matches`, `no hits` — what the call's preview holds,
/// counted in the tool's own unit. The tool's footnotes (`— …`) and the
/// preview cap's marker are not results; a capped preview counts as `N+`.
/// A lone row of a search is shown as itself: it is either the one result
/// or the tool saying it found nothing.
fn outcome(call: &ToolCallView<'_>) -> String {
    #[cfg(test)]
    OUTCOMES_COUNTED.with(|n| n.set(n.get() + 1));
    // Escapes stripped as the full cell strips them: a lone row is shown
    // verbatim, and colour codes would print as text.
    let preview = strip_ansi(call.preview);
    let capped = preview.contains(" characters truncated ...]");
    let rows: Vec<&str> = preview
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty() && !l.starts_with("— ") && !l.ends_with("characters truncated ...]")
        })
        .collect();
    if let [only] = rows.as_slice()
        && call.name != "read"
        && !capped
    {
        return (*only).to_string();
    }
    let n = rows.len();
    let (one, many) = match call.name {
        "read" => ("line", "lines"),
        "grep" => ("match", "matches"),
        "glob" => ("file", "files"),
        "ls" => ("entry", "entries"),
        _ => ("row", "rows"),
    };
    let plus = if capped { "+" } else { "" };
    format!("{n}{plus} {}", if n == 1 { one } else { many })
}
