//! The prompt (spec §3, "plaques and bands"): a rounded frame whose colour
//! follows the permission mode, its title on the top border (`prompt`, or
//! `bash` once the line starts with `!`), then `❯` + text + block cursor.
//! Multi-line up to 6 rows. ASCII `+-|` under `ascii_only`; plain rules when
//! the column is too narrow for side borders.
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
    draw_frame(frame, layout, state, opts, is_bash);

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

/// The frame around the prompt, in the permission mode's tone (the border
/// colour while no session says which mode is on). Too narrow for side
/// borders, the top and bottom rows fall back to plain rules.
fn draw_frame(
    frame: &mut Frame,
    layout: &ScreenLayout,
    state: &AppState,
    opts: &RenderOptions,
    is_bash: bool,
) {
    let style = match state.meta.as_ref() {
        Some(meta) => crate::ui::chrome::mode_tone(meta.mode, &opts.theme),
        None => opts.theme.border,
    };
    let rect = layout.prompt_frame();
    if layout.prompt_framed() && layout.input_top.height > 0 && layout.input_bottom.height > 0 {
        let title = match (is_bash, state.is_running_bash()) {
            (true, true) => " bash · running ",
            (true, false) => " bash ",
            (false, _) => " prompt ",
        };
        let block = crate::ui::chrome::frame_block(
            style,
            vec![Span::styled(title, style.add_modifier(Modifier::BOLD))],
            opts,
        );
        frame.render_widget(block, rect);
        return;
    }
    for row in [layout.input_top, layout.input_bottom] {
        if row.height > 0 {
            let rule = opts.glyphs.hline.repeat(row.width as usize);
            frame.render_widget(Paragraph::new(Line::from(Span::styled(rule, style))), row);
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
/// turns the prompt glyph into the running glyph; the frame title says so.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_inline_hints() {
        assert_eq!(
            command_inline_hint("/effort"),
            Some(" [low|medium|high|xhigh|max]".into())
        );
        assert_eq!(
            command_inline_hint("/effort "),
            Some("[low|medium|high|xhigh|max]".into())
        );
        assert_eq!(
            command_inline_hint("/mode"),
            Some(" [build|auto|plan|bypass]".into())
        );
        assert_eq!(
            command_inline_hint("/mode "),
            Some("[build|auto|plan|bypass]".into())
        );
        assert_eq!(
            command_inline_hint("/model"),
            Some(" [provider/model]".into())
        );
        assert_eq!(
            command_inline_hint("/model "),
            Some("[provider/model]".into())
        );
        assert_eq!(command_inline_hint("/export"), Some(" [path]".into()));
        assert_eq!(command_inline_hint("/export "), Some("[path]".into()));

        assert_eq!(command_inline_hint("/config"), None);
        assert_eq!(command_inline_hint("/config "), None);

        assert_eq!(command_inline_hint("/effort hi"), None);
        assert_eq!(command_inline_hint("/mode plan"), None);

        assert_eq!(command_inline_hint("/unknown"), None);
        assert_eq!(command_inline_hint("hello"), None);
        assert_eq!(command_inline_hint(""), None);
    }

    #[test]
    fn a_slash_token_resolves_by_name_and_the_hint_needs_a_bare_command() {
        assert_eq!(
            named_command("/effort").map(|(token, _)| token),
            Some("effort")
        );
        assert_eq!(named_command("effort").map(|(token, _)| token), None);
        assert_eq!(named_command("/").map(|(token, _)| token), None);
        assert_eq!(named_command("/nope").map(|(token, _)| token), None);

        assert!(takes_arg_hint("/effort"));
        assert!(takes_arg_hint("/effort  "));
        assert!(!takes_arg_hint("/effort high"));
        assert!(!takes_arg_hint("effort"));
        assert!(!takes_arg_hint("/"));
    }

    #[test]
    fn an_unchanged_prompt_is_wrapped_once_and_reused_by_the_next_frame() {
        let mut input = InputState::default();
        input.insert_str("hello world");
        let first = prepare(&mut input, 20, 4);
        let second = prepare(&mut input, 20, 4);
        assert!(Rc::ptr_eq(&first, &second));

        input.insert_str("!");
        let third = prepare(&mut input, 20, 4);
        assert!(!Rc::ptr_eq(&first, &third));
        // Another width is another wrap, even for the same text.
        let other_width = prepare(&mut input, 21, 4);
        assert!(!Rc::ptr_eq(&third, &other_width));
        assert!(Rc::ptr_eq(&prepare(&mut input, 21, 4), &other_width));
    }

    fn draw_screen(
        state: &mut AppState,
        w: u16,
        h: u16,
    ) -> (
        ratatui::Terminal<ratatui::backend::TestBackend>,
        crate::layout::ScreenLayout,
    ) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut layout = crate::layout::ScreenLayout::default();
        terminal
            .draw(|f| {
                let prepared = crate::ui::prepare_frame(f.area(), state, std::time::Instant::now());
                layout = crate::ui::draw(f, state, &prepared);
            })
            .unwrap();
        (terminal, layout)
    }

    fn row_text(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        rect: ratatui::layout::Rect,
        y: u16,
    ) -> String {
        let buffer = terminal.backend().buffer();
        (rect.x..rect.right())
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    /// B09: bash mode used to draw one unwrapped line — a long command was
    /// clipped at the edge while the cursor and the layout followed the wrap.
    #[test]
    fn a_long_bash_command_wraps_and_the_cursor_follows_the_text() {
        let mut state = AppState::new(pacode_types::Config::default(), "t".into(), 60, 20);
        state
            .input
            .insert_str(&format!("!echo {}", "word ".repeat(18).trim_end()));
        let (mut terminal, layout) = draw_screen(&mut state, 60, 20);
        let pieces = state.input.wrap_lines(layout.input.width);
        assert!(pieces.len() > 1, "the command must wrap at this width");
        assert_eq!(layout.input.height as usize, pieces.len());
        for (i, piece) in pieces.iter().enumerate() {
            let row = row_text(&terminal, layout.input, layout.input.y + i as u16);
            assert_eq!(
                row[row.char_indices().nth(2).unwrap().0..].trim_end(),
                piece.trim_end()
            );
        }
        let last = pieces.last().unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(cursor.y, layout.input.y + pieces.len() as u16 - 1);
        assert_eq!(
            cursor.x,
            layout.input.x + 2 + pacode_render::display_width(last) as u16
        );
    }

    /// B09: a multi-line script used to sit inside one span, newlines and all.
    #[test]
    fn a_multi_line_bash_script_draws_one_row_per_line() {
        let mut state = AppState::new(pacode_types::Config::default(), "t".into(), 80, 24);
        state.input.insert_str("!echo one\necho two");
        let (terminal, layout) = draw_screen(&mut state, 80, 24);
        let first = row_text(&terminal, layout.input, layout.input.y);
        let second = row_text(&terminal, layout.input, layout.input.y + 1);
        assert!(first.contains("!echo one"), "{first}");
        assert!(second.contains("echo two"), "{second}");
    }

    #[test]
    fn bash_highlighting_follows_tokens_across_the_wrap() {
        let text = "!echo \"a b\" --flag";
        let lines = vec!["!echo \"a ".to_string(), "b\" --flag".to_string()];
        let view = PreparedInput {
            offsets: line_offsets(text, &lines),
            lines,
            cursor: (0, 0),
            scroll: 0,
        };
        assert_eq!(view.offsets, vec![0, 9]);
        let opts = RenderOptions::new(40, false);
        let rows = bash_rows(text, &view, 2, false, &opts);
        let string_style = opts.theme.bash_style(BashRole::String);
        // The quoted string keeps its colour on both sides of the wrap.
        assert!(
            rows[0]
                .spans
                .iter()
                .any(|s| s.content.contains("\"a") && s.style == string_style)
        );
        assert!(
            rows[1]
                .spans
                .iter()
                .any(|s| s.content.starts_with("b\"") && s.style == string_style)
        );
    }

    #[test]
    fn line_offsets_replay_newlines_and_empty_lines() {
        let text = "ab\n\ncd\r\nef";
        let lines = vec![
            "ab".to_string(),
            String::new(),
            "cd".to_string(),
            "ef".to_string(),
        ];
        assert_eq!(line_offsets(text, &lines), vec![0, 3, 4, 8]);
    }

    #[test]
    fn the_prompt_sits_in_a_titled_frame_with_the_mode_tone() {
        let mut state = AppState::new(pacode_types::Config::default(), "t".into(), 80, 24);
        let (terminal, layout) = draw_screen(&mut state, 80, 24);
        let top = row_text(&terminal, layout.input_top, layout.input_top.y);
        assert!(top.starts_with("╭─ prompt ─"), "{top}");
        assert!(top.ends_with('╮'), "{top}");
        let bottom = row_text(&terminal, layout.input_bottom, layout.input_bottom.y);
        assert!(bottom.starts_with('╰') && bottom.ends_with('╯'), "{bottom}");

        state.input.insert_str("!ls");
        let (terminal, layout) = draw_screen(&mut state, 80, 24);
        let top = row_text(&terminal, layout.input_top, layout.input_top.y);
        assert!(top.starts_with("╭─ bash ─"), "{top}");
    }

    #[test]
    fn prepare_scrolls_the_cursor_line_into_view_and_the_draw_reuses_it() {
        let mut input = InputState::default();
        input.insert_str(&"word ".repeat(40));
        input.cursor = input.text.chars().count();
        let prepared = prepare(&mut input, 20, 3);
        assert!(prepared.scroll > 0);
        assert_eq!(input.input_scroll, prepared.scroll);
        assert_eq!(prepared.cursor.0 as usize, prepared.scroll + 2);
        // The frame hands the paint the very wrap it prepared.
        assert!(Rc::ptr_eq(&prepare(&mut input, 20, 3), &prepared));
    }
}
