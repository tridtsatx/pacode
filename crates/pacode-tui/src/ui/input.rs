//! The prompt (spec §3): thin line above, `❯` dim + text + block cursor, thin line
//! below; no background, no border. Multi-line up to 6 rows.

use std::cell::RefCell;
use std::rc::Rc;

use ratatui::Frame;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;

use crate::commands::{self, SlashCommand};
use crate::layout::ScreenLayout;
use crate::state::AppState;
use crate::state::input::InputState;

pub(crate) struct PreparedInput {
    lines: Vec<String>,
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
    PreparedInput {
        lines: input.wrap_lines(width),
        cursor: input.cursor_position(width),
        scroll: input.input_scroll,
    }
}

pub(crate) fn draw(
    frame: &mut Frame,
    layout: &ScreenLayout,
    state: &AppState,
    opts: &RenderOptions,
    view: &Rc<PreparedInput>,
) {
    if layout.input_top.height > 0 {
        let hline = opts.glyphs.hline.repeat(layout.input_top.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hline, opts.theme.faint))),
            layout.input_top,
        );
    }

    if layout.input.height > 0 {
        let height = layout.input.height as usize;

        let named = named_command(&state.input.text);
        let matching_cmd_token = named.as_ref().map(|(token, _)| (*token).to_string());
        let inline_hint = match &named {
            Some((_, command)) if takes_arg_hint(&state.input.text) => {
                arg_hint(&state.input.text, command)
            }
            _ => None,
        };
        let is_bash = state.input.text.starts_with('!');

        let mut rendered = Vec::new();
        if is_bash {
            let prefix = if state.is_running_bash() {
                Span::styled(
                    format!("{} (busy) ", opts.glyphs.running),
                    opts.theme.yellow,
                )
            } else {
                Span::styled(
                    "! ",
                    opts.theme
                        .accent
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )
            };
            let mut line_spans = vec![prefix];
            let cmd_text = &state.input.text[1..];
            let bash_spans = crate::bash::highlight::tokenize(cmd_text);
            for span in bash_spans {
                line_spans.push(Span::styled(
                    span.text.to_string(),
                    opts.theme.bash_style(span.role),
                ));
            }
            rendered.push(Line::from(line_spans));
        } else {
            for (i, wl) in view.lines.iter().enumerate().skip(view.scroll).take(height) {
                let prefix = if i == 0 {
                    Span::styled(format!("{} ", opts.glyphs.prompt), opts.theme.dim)
                } else {
                    Span::raw("  ")
                };

                let mut line_spans = vec![prefix];
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
        }

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

    if layout.input_bottom.height > 0 {
        let hline = opts.glyphs.hline.repeat(layout.input_bottom.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hline, opts.theme.faint))),
            layout.input_bottom,
        );
    }
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
