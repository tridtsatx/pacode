//! What the overlay actually puts on screen: the shape at a roomy size, and
//! something coherent at a size that leaves it no room.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use pacode_config::registry;
use pacode_render::Glyphs;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use pacode_types::Config;

use super::*;
use crate::ui::settings::model::on_key;
use crate::ui::settings::tabs::section_icon;

/// The overlay's rows as the backend holds them, unquoted and untrimmed.
fn render(config: &Config, ui: &SettingsState, width: u16, height: u16) -> Vec<String> {
    render_maybe_ascii(config, ui, width, height, false)
}

fn render_maybe_ascii(
    config: &Config,
    ui: &SettingsState,
    width: u16,
    height: u16,
    ascii: bool,
) -> Vec<String> {
    let opts = RenderOptions::new(width, ascii);
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

    // Tab row: the active tab spells its section out, the rest are icons.
    let tab_row = &lines[1];
    assert!(
        tab_row.contains("provider"),
        "the active tab names its section: {tab_row}"
    );
    assert!(
        tab_row.contains(section_icon("tools", &Glyphs::new(false))),
        "inactive tabs are icons: {tab_row}"
    );

    // Two columns: the section name, then the setting and its value.
    let row = lines
        .iter()
        .find(|line| line.contains("Default Route"))
        .expect("a provider row");
    let section_at = row.find("provider").expect("section column");
    let label_at = row.find("Default Route").expect("label column");
    assert!(section_at < label_at, "the section column comes first: {row}");
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
        assert!(display_width(line) <= 40, "a row ran past the frame: {line:?}");
    }
    assert!(lines[0].contains("Settings"), "{}", lines[0]);
    assert!(lines[1].contains("provider"), "the tabs survive: {}", lines[1]);
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
fn ascii_mode_draws_no_unicode() {
    let config = Config::default();
    let ui = SettingsState::new();
    for line in render_maybe_ascii(&config, &ui, 120, 40, true) {
        assert!(line.is_ascii(), "not ASCII: {line:?}");
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
    assert!(lines.iter().any(|line| line.contains(hit.label)), "{lines:?}");
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
