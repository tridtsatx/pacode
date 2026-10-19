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

fn rule_styles(
    terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    row: ratatui::layout::Rect,
) -> Vec<ratatui::style::Style> {
    let buffer = terminal.backend().buffer();
    (row.x..row.right())
        .map(|x| buffer[(x, row.y)].style())
        .collect()
}

fn session_meta(mode: pacode_types::Mode) -> pacode_types::SessionMeta {
    pacode_types::SessionMeta {
        id: pacode_types::SessionId::new("ses_rules"),
        name: None,
        cwd: std::path::PathBuf::from("/work"),
        git_branch: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        model: pacode_types::ModelRoute::new("prov", "model-x"),
        effort: pacode_types::Effort::Max,
        mode,
        first_prompt: None,
        zdr: false,
    }
}

/// The prompt is two plain full-width rules and the text between them: no
/// side borders, no padding, no title — at every width.
#[test]
fn the_prompt_is_two_full_width_rules_at_every_width() {
    for w in [8u16, 11, 12, 40, 80, 140] {
        let mut state = AppState::new(pacode_types::Config::default(), "t".into(), w, 24);
        state.input.insert_str("hi");
        let (mut terminal, layout) = draw_screen(&mut state, w, 24);
        let hline = state.render_options(w).glyphs.hline;
        for row in [layout.input_top, layout.input_bottom] {
            assert_eq!(row.height, 1, "w={w}");
            let text = row_text(&terminal, row, row.y);
            assert_eq!(text, hline.repeat(row.width as usize), "w={w}");
        }
        assert_eq!(layout.input.x, layout.input_top.x, "w={w}");
        assert_eq!(layout.input.width, layout.input_top.width, "w={w}");
        let first = row_text(&terminal, layout.input, layout.input.y);
        assert!(first.starts_with("❯ hi"), "w={w}: {first}");
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(
            (cursor.x, cursor.y),
            (layout.input.x + 4, layout.input.y),
            "w={w}"
        );
    }
}

/// The rules stay in the border grey whatever the permission mode, in bash
/// mode, and while a captured command runs.
#[test]
fn the_rules_are_grey_regardless_of_mode_and_bash() {
    // Truecolor pinned: under `NO_COLOR` every tone is empty and the check
    // could not tell the grey from a mode colour.
    let mut config = pacode_types::Config::default();
    config.ui.color = "truecolor".to_string();
    let mut state = AppState::new(config, "t".into(), 80, 24);
    let grey = state.render_options(80).theme.border;
    let check = |state: &mut AppState, ctx: &str| {
        let (terminal, layout) = draw_screen(state, 80, 24);
        for row in [layout.input_top, layout.input_bottom] {
            assert!(
                rule_styles(&terminal, row).iter().all(|s| s.fg == grey.fg),
                "{ctx}"
            );
            let text = row_text(&terminal, row, row.y);
            assert!(
                !text.contains("prompt") && !text.contains("bash"),
                "{ctx}: {text}"
            );
        }
    };
    check(&mut state, "no session");
    for mode in pacode_types::Mode::CYCLE {
        state.meta = Some(session_meta(mode));
        check(&mut state, &format!("{mode:?}"));
    }
    state.input.insert_str("!ls");
    check(&mut state, "bash");
    let (cancel, _rx) = tokio::sync::oneshot::channel();
    state.running_bash = Some(cancel);
    check(&mut state, "bash running");
}

/// A running captured command shows in the running glyph that replaces `❯`.
#[test]
fn a_running_bash_command_swaps_the_prompt_glyph() {
    let mut state = AppState::new(pacode_types::Config::default(), "t".into(), 80, 24);
    state.input.insert_str("!sleep 9");
    let (cancel, _rx) = tokio::sync::oneshot::channel();
    state.running_bash = Some(cancel);
    let (terminal, layout) = draw_screen(&mut state, 80, 24);
    let first = row_text(&terminal, layout.input, layout.input.y);
    let running = state.render_options(80).glyphs.running;
    assert!(first.starts_with(&format!("{running} !sleep 9")), "{first}");
}

/// The wrap takes the whole column minus the two-cell prefix.
#[test]
fn the_wrap_width_is_the_full_column_minus_the_prefix() {
    let mut state = AppState::new(pacode_types::Config::default(), "t".into(), 60, 20);
    // Exactly one full row of text, then one more character.
    state.input.insert_str(&"x".repeat(59));
    let (terminal, layout) = draw_screen(&mut state, 60, 20);
    assert_eq!(layout.input.width, 60);
    assert_eq!(layout.input.height, 2);
    let first = row_text(&terminal, layout.input, layout.input.y);
    assert_eq!(first, format!("❯ {}", "x".repeat(58)));
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
