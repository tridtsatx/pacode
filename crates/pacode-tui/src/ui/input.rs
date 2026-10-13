//! The prompt (spec §3): two full-width horizontal rules in the neutral
//! border grey — independent of the permission mode and of bash mode — with
//! `❯` + text + block cursor between them. No side borders, no padding, no
//! title. Multi-line up to 6 rows. ASCII `-` rules under `ascii_only`.
//!
//! Bash mode draws the same wrapped rows as a plain prompt — the `!` is part
//! of the text — so the cursor, the scroll and the layout height share one
//! wrap; only the colours differ, mapped from one tokenization of the whole
//! command onto the rows.

use std::cell::RefCell;
use std::rc::Rc;

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{BashRole, RenderOptions};

use crate::commands::{self, SlashCommand};
use crate::layout::ScreenLayout;
use crate::state::AppState;
use crate::state::input::InputState;

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;

pub(crate) struct PreparedInput {
    lines: Vec<String>,
    /// Byte offset in the input text where each of `lines` starts.
    offsets: Vec<usize>,
    cursor: (u16, u16),
    scroll: usize,
}

struct WrapMemo {
    text: String,
    cursor: usize,
    scroll: usize,
    width: u16,
    height: u16,
    prepared: Rc<PreparedInput>,
}

thread_local! {
    static WRAP_MEMO: RefCell<Option<WrapMemo>> = const { RefCell::new(None) };
}

pub(crate) fn prepare(input: &mut InputState, width: u16, height: u16) -> Rc<PreparedInput> {
    if let Some(memo) = WRAP_MEMO.with(|cell| {
        cell.borrow()
            .as_ref()
            .filter(|memo| wrap_matches(memo, input, width, height))
            .map(|memo| Rc::clone(&memo.prepared))
    }) {
        return memo;
    }
    input.reconcile_scroll(width, height);
    let prepared = Rc::new(measure(input, width));
    WRAP_MEMO.with(|cell| {
        *cell.borrow_mut() = Some(WrapMemo {
            text: input.text.clone(),
            cursor: input.cursor,
            scroll: input.input_scroll,
            width,
            height,
            prepared: Rc::clone(&prepared),
        });
    });
    prepared
}

fn wrap_matches(memo: &WrapMemo, input: &InputState, width: u16, height: u16) -> bool {
    memo.width == width
        && memo.height == height
        && memo.cursor == input.cursor
        && memo.scroll == input.input_scroll
        && memo.text == input.text
}

fn measure(input: &InputState, width: u16) -> PreparedInput {
    let lines = input.wrap_lines(width);
    PreparedInput {
        offsets: line_offsets(&input.text, &lines),
        lines,
        cursor: input.cursor_position(width),
        scroll: input.input_scroll,
    }
}

/// Where each wrapped row starts in `text`. The wrap splits on `\n`, drops
/// one trailing `\r`, and cuts each logical line into pieces that partition
/// it in order — so the offsets replay that walk.
fn line_offsets(text: &str, lines: &[String]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut rows = lines.iter();
    let mut pos = 0usize;
    for raw in text.split('\n') {
        let logical = raw.strip_suffix('\r').unwrap_or(raw).len();
        let mut taken = 0usize;
        loop {
            let Some(row) = rows.next() else {
                return offsets;
            };
            offsets.push(pos + taken);
            taken += row.len();
            if taken >= logical {
                break;
            }
        }
        pos += raw.len() + 1;
    }
    offsets
}

pub(crate) fn draw(
    frame: &mut Frame,
    layout: &ScreenLayout,
    state: &AppState,
    opts: &RenderOptions,
    view: &Rc<PreparedInput>,
) {
    let text = &state.input.text;
    let is_bash = text.starts_with('!');
    draw_rules(frame, layout, opts);

    if layout.input.height == 0 || layout.input.width == 0 {
        return;
    }
    let height = layout.input.height as usize;
    let rendered: Vec<Line<'static>> = if is_bash {
        bash_rows(text, view, height, state.is_running_bash(), opts)
    } else {
        prompt_rows(text, view, height, opts)
    };
    frame.render_widget(Paragraph::new(rendered), layout.input);

    let cur_line = view.cursor.0;
    let cur_screen_line = cur_line.saturating_sub(view.scroll as u16);
    if cur_line >= view.scroll as u16 && (cur_screen_line as usize) < height {
        frame.set_cursor_position((
            layout.input.x + view.cursor.1,
            layout.input.y + cur_screen_line,
        ));
    }
}

/// The rules above and below the prompt text, always in the border grey.
fn draw_rules(frame: &mut Frame, layout: &ScreenLayout, opts: &RenderOptions) {
    for row in [layout.input_top, layout.input_bottom] {
        if row.height > 0 {
            let rule = opts.glyphs.hline.repeat(row.width as usize);
            let line = Line::from(Span::styled(rule, opts.theme.border));
            frame.render_widget(Paragraph::new(line), row);
        }
    }
}

/// The plain prompt: `❯` on the first row, the slash command in cyan with its
/// argument hint, the rest as typed.
fn prompt_rows(
    text: &str,
    view: &PreparedInput,
    height: usize,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    let named = named_command(text);
    let matching_cmd_token = named.as_ref().map(|(token, _)| (*token).to_string());
    let inline_hint = match &named {
        Some((_, command)) if takes_arg_hint(text) => arg_hint(text, command),
        _ => None,
    };
    let mut rendered = Vec::with_capacity(height);
    for (i, wl) in view.lines.iter().enumerate().skip(view.scroll).take(height) {
        let mut line_spans = vec![row_prefix(i, opts.theme.dim, opts)];
        if i == 0 {
            if let Some(ref token) = matching_cmd_token
                && wl.starts_with(token.as_str())
            {
                line_spans.push(Span::styled(token.clone(), opts.theme.cyan));
                line_spans.push(Span::styled(wl[token.len()..].to_string(), opts.theme.fg));
            } else {
                line_spans.push(Span::styled(wl.clone(), opts.theme.fg));
            }
            if let Some(ref hint) = inline_hint {
                line_spans.push(Span::styled(hint.clone(), opts.theme.dim));
            }
        } else {
            line_spans.push(Span::styled(wl.clone(), opts.theme.fg));
        }
        rendered.push(Line::from(line_spans));
    }
    rendered
}

/// `❯` on the first row, two cells of indent on the rest.
fn row_prefix(row: usize, style: Style, opts: &RenderOptions) -> Span<'static> {
    if row == 0 {
        Span::styled(format!("{} ", opts.glyphs.prompt), style)
    } else {
        Span::raw("  ")
    }
}

/// Bash mode: the same wrapped rows, each cut along the token spans of one
/// tokenization of the whole command, so a token that wraps keeps its colour
/// and the cursor lands where the text is (B09). A running captured command
/// turns the prompt glyph into the running glyph.
fn bash_rows(
    text: &str,
    view: &PreparedInput,
    height: usize,
    running: bool,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    let bang = opts.theme.accent.add_modifier(Modifier::BOLD);
    // (start, end, role) over `text`; the `!` itself is byte 0.
    let command = &text[1..];
    let tokens: Vec<(usize, usize, BashRole)> = crate::bash::highlight::tokenize(command)
        .into_iter()
        .map(|span| {
            let start = 1 + (span.text.as_ptr() as usize - command.as_ptr() as usize);
            (start, start + span.text.len(), span.role)
        })
        .collect();
    // Tokens come out in text order, so the one covering a byte is found by
    // bisection — a long pasted script costs a log per drawn character.
    let style_at = |byte: usize| -> Style {
        if byte == 0 {
            return bang;
        }
        let next = tokens.partition_point(|(start, _, _)| *start <= byte);
        next.checked_sub(1)
            .and_then(|i| tokens.get(i))
            .filter(|(_, end, _)| byte < *end)
            .map_or(opts.theme.fg, |(_, _, role)| opts.theme.bash_style(*role))
    };

    let mut rendered = Vec::with_capacity(height);
    for (i, wl) in view.lines.iter().enumerate().skip(view.scroll).take(height) {
        let prefix = if i == 0 && running {
            Span::styled(format!("{} ", opts.glyphs.running), opts.theme.yellow)
        } else {
            row_prefix(i, bang, opts)
        };
        let mut spans = vec![prefix];
        let base = view.offsets.get(i).copied().unwrap_or(0);
        let mut run_start = 0usize;
        let mut run_style: Option<Style> = None;
        for (at, _) in wl.char_indices() {
            let style = style_at(base + at);
            if run_style != Some(style) {
                if let Some(prev) = run_style {
                    spans.push(Span::styled(wl[run_start..at].to_string(), prev));
                }
                run_start = at;
                run_style = Some(style);
            }
        }
        if let Some(prev) = run_style {
            spans.push(Span::styled(wl[run_start..].to_string(), prev));
        }
        rendered.push(Line::from(spans));
    }
    rendered
}

fn named_command(text: &str) -> Option<(&str, SlashCommand)> {
    let token = text.strip_prefix('/')?.split_whitespace().next()?;
    Some((token, commands::find_command(token)?))
}

fn takes_arg_hint(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('/') else {
        return false;
    };
    let Some(name) = rest.split_whitespace().next() else {
        return false;
    };
    rest[name.len()..].trim().is_empty()
}

fn arg_hint(text: &str, command: &SlashCommand) -> Option<String> {
    if command.arg_hint.is_empty() {
        return None;
    }
    Some(if text.ends_with(' ') {
        command.arg_hint.to_string()
    } else {
        format!(" {}", command.arg_hint)
    })
}

pub fn command_inline_hint(text: &str) -> Option<String> {
    if !takes_arg_hint(text) {
        return None;
    }
    let (_, command) = named_command(text)?;
    arg_hint(text, &command)
}
