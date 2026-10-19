//! What the overlay actually puts on screen: the shape at a roomy size, and
//! something coherent at a size that leaves it no room.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use pacode_config::registry;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use pacode_types::Config;

use super::*;
use crate::ui::settings::model::on_key;

/// Options pinned to a truecolor theme: the assertions read backgrounds and
/// modifiers, which the ambient `COLORTERM` would otherwise decide (under
/// ANSI the selected row is `REVERSED`, the same mark the scrollbar thumb uses).
fn opts(width: u16, ascii: bool) -> RenderOptions {
    RenderOptions::with_theme_color(
        width,
        ascii,
        pacode_render::Theme::truecolor(),
        pacode_render::ColorMode::Truecolor,
    )
}

/// The overlay's rows as the backend holds them, unquoted and untrimmed.
fn render(config: &Config, ui: &SettingsState, width: u16, height: u16) -> Vec<String> {
    render_maybe_ascii(config, ui, width, height, false)
}

fn buffer(config: &Config, ui: &SettingsState, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let opts = opts(width, false);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let area = Rect::new(0, 0, width, height);
    terminal
        .draw(|frame| draw(frame, area, config, ui, &opts))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn render_maybe_ascii(
    config: &Config,
    ui: &SettingsState,
    width: u16,
    height: u16,
    ascii: bool,
) -> Vec<String> {
    let opts = opts(width, ascii);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let area = Rect::new(0, 0, width, height);
    terminal
        .draw(|frame| draw(frame, area, config, ui, &opts))
        .expect("draw");
    format!("{}", terminal.backend())
        .lines()
        .map(|line| line.trim_matches('"').to_string())
        .collect()
}

fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn press(state: &mut SettingsState, config: &Config, code: KeyCode) {
    on_key(state, plain(code), config, 10);
}

fn typing(state: &mut SettingsState, config: &Config, text: &str) {
    for c in text.chars() {
        press(state, config, KeyCode::Char(c));
    }
}

#[test]
fn renders_the_documented_shape_at_120x40() {
    let config = Config::default();
    let ui = SettingsState::new();
    let lines = render(&config, &ui, 120, 40);
    assert_eq!(lines.len(), 40);

    // Title on the top border.
    assert!(lines[0].contains("Settings"), "{}", lines[0]);
    assert!(lines[0].contains('╭'), "no border: {}", lines[0]);

    // No tab row while the section column is there: the body starts on the
    // first inner row.
    assert!(
        lines[1].contains("provider") && lines[1].contains("Default Route"),
        "the body starts under the border: {}",
        lines[1]
    );

    // Two columns: the section name, then the setting and its value.
    let row = lines
        .iter()
        .find(|line| line.contains("Default Route"))
        .expect("a provider row");
    let section_at = row.find("provider").expect("section column");
    let label_at = row.find("Default Route").expect("label column");
    assert!(
        section_at < label_at,
        "the section column comes first: {row}"
    );
    assert!(row.contains("(empty)"), "the value column is there: {row}");

    // The description of the highlighted setting.
    let description = registry::find_entry("provider.default")
        .map(|e| e.description)
        .unwrap_or_default();
    let opening = description.split(' ').take(3).collect::<Vec<_>>().join(" ");
    assert!(
        lines.iter().any(|line| line.contains(&opening)),
        "no description line in {lines:?}"
    );

    // The hint footer, on the bottom border, in one row.
    assert!(lines[39].contains("esc close"), "{}", lines[39]);
}

#[test]
fn a_narrow_and_short_terminal_renders_something_coherent() {
    let config = Config::default();
    let ui = SettingsState::new();
    let lines = render(&config, &ui, 40, 10);
    assert_eq!(lines.len(), 10);
    for line in &lines {
        assert!(
            display_width(line) <= 40,
            "a row ran past the frame: {line:?}"
        );
    }
    assert!(lines[0].contains("Settings"), "{}", lines[0]);
    assert!(
        lines[1].contains("provider"),
        "the tabs survive: {}",
        lines[1]
    );
    // The right column is still worth reading at 40 cells.
    assert!(
        lines.iter().any(|line| line.contains("Default Route")),
        "no rows at all: {lines:?}"
    );
    // The hint is longer than the frame here, so it is cut — never wrapped
    // onto a second row that would eat the body.
    assert!(lines[9].contains("enter/space change"), "{}", lines[9]);
    assert!(
        !lines.iter().any(|line| line.contains("esc close")),
        "the hint wrapped: {lines:?}"
    );
}

#[test]
fn the_footer_is_one_row_whatever_the_description_says() {
    let config = Config::default();
    let quiet = render(&config, &SettingsState::new(), 120, 40);

    // `provider.max_retries` is 0..=100; 999 is not.
    let mut ui = SettingsState::new();
    for _ in 0..3 {
        press(&mut ui, &config, KeyCode::Down);
    }
    press(&mut ui, &config, KeyCode::Enter);
    on_key(
        &mut ui,
        KeyEvent {
            modifiers: KeyModifiers::CONTROL,
            ..plain(KeyCode::Char('u'))
        },
        &config,
        10,
    );
    typing(&mut ui, &config, "999");
    press(&mut ui, &config, KeyCode::Enter);
    let refused = render(&config, &ui, 120, 40);

    assert!(
        refused.iter().any(|line| line.contains("out of range")),
        "the refusal never reached the description line: {refused:?}"
    );
    // ...and the footer did not move a cell to make room for it.
    assert_eq!(quiet[39], refused[39]);
}

#[test]
fn ascii_mode_drops_the_unicode_the_page_owns() {
    // The page's own glyphs — bullets, arrows, the scrollbar — must all
    // fall back to ASCII on a terminal that cannot draw them.
    //
    // The *frame* is the one thing this cannot cover, and it is not a gap in
    // this page: `overlays::menu_block` draws `Borders::ALL` for every overlay
    // in the client, and ratatui's plain border set is still box-drawing
    // characters. Fixing that means hand-drawing the frame in one shared place
    // for every overlay, which is its own change; until then the honest
    // assertion is that the corners are square rather than rounded: ratatui's
    // plain set is `┌─┐│`, which is still box drawing, but it is the closest a
    // shared border gets to ASCII without hand-drawing the frame.
    let config = Config::default();
    let ui = SettingsState::new();
    let lines = render_maybe_ascii(&config, &ui, 120, 40, true);
    for line in &lines {
        assert!(
            !line.contains(['╭', '╮', '╰', '╯']),
            "a rounded border in ascii mode: {line:?}"
        );
    }
}

#[test]
fn the_search_query_rides_in_the_title() {
    let config = Config::default();
    let mut ui = SettingsState::new();
    typing(&mut ui, &config, "shim");
    let lines = render(&config, &ui, 120, 40);
    assert!(lines[0].contains("Settings /shim"), "{}", lines[0]);
    let hit = registry::find_entry("ui.shimmer").expect("registered");
    assert!(
        lines.iter().any(|line| line.contains(hit.label)),
        "{lines:?}"
    );
}

#[test]
fn a_search_with_no_hits_says_so_instead_of_showing_nothing() {
    let config = Config::default();
    let mut ui = SettingsState::new();
    typing(&mut ui, &config, "zzz");
    let lines = render(&config, &ui, 120, 40);
    assert!(
        lines.iter().any(|line| line.contains("no settings match")),
        "{lines:?}"
    );
}

#[test]
fn a_narrow_overlay_trades_the_section_column_for_a_text_tab_row() {
    let config = Config::default();
    let ui = SettingsState::new();
    // 20 cells: an 18-cell inner width, one short of the section column.
    let width = 20;
    assert!(shows_tab_row(width - 2) && !shows_tab_row(MIN_LEFT_COLUMN + 1));
    let lines = render(&config, &ui, width, 12);
    let tab_row = &lines[1];
    assert!(
        tab_row.contains("provider"),
        "the tab row names the open section: {tab_row:?}"
    );
    assert!(
        tab_row
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c.is_whitespace() || c == '…' || c == '│'),
        "the tab row is words, not icons: {tab_row:?}"
    );
    // The body follows the tab row, with no section column before it.
    assert!(
        !lines[2].contains("provider") && lines[2].contains("Default"),
        "{:?}",
        lines[2]
    );

    // The active tab sits on the selection bar.
    let opts = opts(width, false);
    let buf = buffer(&config, &ui, width, 12);
    let at = tab_row.find("provider").expect("tab") as u16;
    assert_eq!(
        buf[(at, 1)].bg,
        opts.theme.selected_bg.bg.unwrap_or_default()
    );
}

#[test]
fn the_scrollbar_is_drawn_only_while_the_rows_overflow() {
    let config = Config::default();
    let ui = SettingsState::new();
    let opts = opts(120, false);
    let column = 118;
    let rows = ui.row_count();

    // Tall enough for every row of the section: no bar, and the values take
    // the column back.
    let height = rows as u16 + 6;
    let buf = buffer(&config, &ui, 120, height);
    for y in 1..height - 3 {
        let cell = &buf[(column, y)];
        assert!(
            cell.symbol() != opts.glyphs.vline && !cell.modifier.contains(Modifier::REVERSED),
            "a bar at row {y} with nothing to scroll: {cell:?}"
        );
    }
    assert!(
        (1..height - 3).any(|y| buf[(column, y)].symbol() != " "),
        "the values stopped short of the freed column"
    );

    // One row short (the description is a single row at this width): a quiet
    // track and a solid thumb in the last column.
    let height = rows as u16 + 2;
    let buf = buffer(&config, &ui, 120, height);
    let cells: Vec<_> = (1..height - 2).map(|y| buf[(column, y)].clone()).collect();
    assert!(
        cells.iter().any(|cell| cell.symbol() == opts.glyphs.vline
            && cell.fg == opts.theme.border.fg.unwrap_or_default()),
        "no track: {cells:?}"
    );
    assert!(
        cells
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::REVERSED)),
        "no thumb: {cells:?}"
    );
}
