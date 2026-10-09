use pacode_render::ColorMode;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use super::*;

fn opts(ascii: bool) -> RenderOptions {
    RenderOptions::with_theme_color(40, ascii, Theme::truecolor(), ColorMode::Truecolor)
}

fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect()
}

#[test]
fn a_badge_is_padded_and_takes_the_tone_as_its_plaque() {
    let opts = opts(false);
    let badge = badge("BYPASS", opts.theme.red, &opts);
    assert_eq!(badge.content, " BYPASS ");
    assert_eq!(badge.style, opts.theme.badge(opts.theme.red));
}

#[test]
fn sections_are_uppercase_and_bold() {
    let opts = opts(false);
    let span = section("Recent", &opts);
    assert_eq!(span.content, "RECENT");
    assert!(span.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn the_frame_is_rounded_with_its_title_after_one_border_run() {
    let opts = opts(false);
    let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
    terminal
        .draw(|f| {
            let block = frame_block(opts.theme.border, vec![Span::raw(" prompt ")], &opts);
            f.render_widget(block, Rect::new(0, 0, 20, 3));
        })
        .unwrap();
    assert_eq!(row(&terminal, 0), "╭─ prompt ─────────╮");
    assert_eq!(row(&terminal, 2), "╰──────────────────╯");
}

#[test]
fn the_frame_falls_back_to_ascii() {
    let opts = opts(true);
    let mut terminal = Terminal::new(TestBackend::new(12, 3)).unwrap();
    terminal
        .draw(|f| {
            let block = frame_block(opts.theme.border, vec![Span::raw(" p ")], &opts);
            f.render_widget(block, Rect::new(0, 0, 12, 3));
        })
        .unwrap();
    assert_eq!(row(&terminal, 0), "+- p ------+");
    assert_eq!(row(&terminal, 1), "|          |");
    assert_eq!(row(&terminal, 2), "+----------+");
}

#[test]
fn the_mode_tone_follows_the_permission_mode() {
    let theme = Theme::truecolor();
    assert_eq!(mode_tone(Mode::Bypass, &theme), theme.red);
    assert_eq!(mode_tone(Mode::Plan, &theme), theme.yellow);
    assert_eq!(mode_tone(Mode::Build, &theme), theme.cyan);
    assert_eq!(mode_tone(Mode::Auto, &theme), theme.cyan);
}
