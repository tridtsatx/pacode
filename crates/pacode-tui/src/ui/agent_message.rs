//! One leg of the orchestrator <-> subagent exchange, drawn the same in both
//! transcripts: `→ tests: <message>` for an `agent send`, `? tests asks:
//! <question>` for an `ask_parent` question, with the question's outcome
//! hanging under it once it has one. Folded to the first line; expanded
//! (`ctrl+o`) to the whole text.

use ratatui::text::{Line, Span};

use pacode_render::{RenderOptions, display_width, truncate_spans, truncate_to_width, wrap_text};
use pacode_types::{AgentMessageDirection, AgentReply};

#[cfg(test)]
#[path = "agent_message_tests.rs"]
mod agent_message_tests;

/// The fields of a `TranscriptKind::AgentMessage` the cell reads.
pub(super) struct AgentMessageView<'a> {
    pub subagent_name: &'a str,
    pub direction: AgentMessageDirection,
    pub text: &'a str,
    pub reply: Option<&'a AgentReply>,
}

/// Indent of the wrapped body and of the outcome line.
const INDENT: &str = "  ";

pub(super) fn render(
    view: &AgentMessageView<'_>,
    opts: &RenderOptions,
    expanded: bool,
) -> Vec<Line<'static>> {
    let width = opts.width as usize;
    let (lead, sep) = match view.direction {
        AgentMessageDirection::ToSubagent => (
            Span::styled(format!("{} ", opts.glyphs.arrow_right), opts.theme.accent),
            Span::styled(": ", opts.theme.faint),
        ),
        AgentMessageDirection::Question => (
            Span::styled("? ", opts.theme.yellow),
            Span::styled(" asks: ", opts.theme.faint),
        ),
    };
    let fixed = display_width(&lead.content) + display_width(&sep.content);
    // The name yields width before the message does: a folded row keeps a
    // few cells of the message, an expanded header gives the name the rest.
    let text_room = if expanded {
        0
    } else {
        display_width(first_line(view.text)).min(MIN_TEXT)
    };
    let name = truncate_to_width(
        view.subagent_name,
        width.saturating_sub(fixed + text_room).max(1),
        true,
    );
    let mut head = vec![lead, Span::styled(name, opts.theme.bold), sep];
    let mut lines = Vec::new();
    if expanded {
        lines.push(fit(head, width, opts));
        lines.extend(body(view.text, width, opts.theme.fg));
    } else {
        let used: usize = head.iter().map(|s| display_width(&s.content)).sum();
        head.push(Span::styled(
            folded(view.text, width.saturating_sub(used)),
            opts.theme.fg,
        ));
        lines.push(fit(head, width, opts));
    }
    if view.direction == AgentMessageDirection::Question {
        lines.extend(outcome(view.reply, width, opts, expanded));
    }
    lines.push(Line::default());
    lines
}

/// The question's state under it: still open, answered, left to the
/// subagent's judgment, or dropped with a stop.
fn outcome(
    reply: Option<&AgentReply>,
    width: usize,
    opts: &RenderOptions,
    expanded: bool,
) -> Vec<Line<'static>> {
    let lead = format!("{INDENT}{} ", opts.glyphs.vcorner);
    let note = |text: &str, style| {
        vec![fit(
            vec![
                Span::styled(lead.clone(), opts.theme.faint),
                Span::styled(text.to_string(), style),
            ],
            width,
            opts,
        )]
    };
    match reply {
        None => note("waiting for an answer", opts.theme.faint),
        Some(AgentReply::Unanswered) => note(
            "no answer: proceeding on its own judgment",
            opts.theme.yellow,
        ),
        Some(AgentReply::Failed) => note(
            "no answer: the orchestrator's turn failed",
            opts.theme.yellow,
        ),
        Some(AgentReply::Cancelled) => note("cancelled", opts.theme.faint),
        Some(AgentReply::Answered { text }) => {
            let label = "answer: ";
            if expanded {
                let mut lines = note(label, opts.theme.green);
                lines.extend(body(text, width, opts.theme.dim));
                return lines;
            }
            let room = width.saturating_sub(display_width(&lead) + display_width(label));
            vec![fit(
                vec![
                    Span::styled(lead, opts.theme.faint),
                    Span::styled(label, opts.theme.green),
                    Span::styled(folded(text, room), opts.theme.dim),
                ],
                width,
                opts,
            )]
        }
    }
}

/// Cells of the message a folded row keeps however long the name is.
const MIN_TEXT: usize = 8;

/// The row cut to the pane — a last guard for panes narrower than the row's
/// fixed parts.
fn fit(spans: Vec<Span<'static>>, width: usize, opts: &RenderOptions) -> Line<'static> {
    Line::from(truncate_spans(&spans, width, opts.glyphs.ellipsis))
}

fn first_line(text: &str) -> &str {
    text.trim().lines().next().unwrap_or_default()
}

/// The first line cut to `room`, with an ellipsis when anything was left out.
fn folded(text: &str, room: usize) -> String {
    let text = text.trim();
    let first = first_line(text);
    if first.len() < text.len() {
        return truncate_to_width(&format!("{first}…"), room, true);
    }
    truncate_to_width(first, room, true)
}

fn body(text: &str, width: usize, style: ratatui::style::Style) -> Vec<Line<'static>> {
    wrap_text(text.trim(), width.saturating_sub(INDENT.len()).max(1))
        .into_iter()
        .map(|line| Line::from(Span::styled(format!("{INDENT}{line}"), style)))
        .collect()
}
