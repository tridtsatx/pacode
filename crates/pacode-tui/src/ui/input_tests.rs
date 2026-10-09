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
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
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
