//! Every floating element paints a solid `panel` background over its whole
//! rect under a truecolor theme, and leaves the terminal background alone
//! under ANSI-16, where `panel` is empty.

use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use pacode_render::{ColorMode, Theme};
use pacode_types::{AgentId, Config};

use crate::state::{AppState, Focus, Overlay};
use crate::ui::menu::{MenuId, MenuStack, MenuUiState};

const COLS: u16 = 120;
const ROWS: u16 = 40;

fn app(theme: Theme, mode: ColorMode) -> AppState {
    let mut app = AppState::new(Config::default(), "test".to_string(), COLS, ROWS);
    app.theme = theme;
    app.color_mode = mode;
    app.truecolor = mode == ColorMode::Truecolor;
    app
}

/// Puts the state in front of one overlay.
type Opener = fn(&mut AppState);

/// Each overlay family, as the state that opens it.
fn families() -> Vec<(&'static str, Opener)> {
    vec![
        ("menu", |app| {
            app.focus = Focus::Overlay(Overlay::Menu {
                stack: MenuStack::root(MenuId::Root),
                ui: MenuUiState::default(),
            });
        }),
        ("settings", |app| {
            app.focus = Focus::Overlay(Overlay::Settings {
                ui: Default::default(),
            });
        }),
        ("model palette", |app| {
            app.focus = Focus::Overlay(Overlay::ModelPalette {
                palette: Default::default(),
            });
        }),
        ("analyze", |app| {
            app.analyze_report = Some(Default::default());
            app.focus = Focus::Overlay(Overlay::Analyze {
                view: Default::default(),
            });
        }),
        ("rail overlay", |app| {
            app.focus = Focus::Overlay(Overlay::RailOverlay {
                selected: AgentId::new("agent"),
            });
        }),
        ("effort picker", |app| {
            app.focus = Focus::Overlay(Overlay::EffortPicker { index: 0 });
        }),
        ("mode picker", |app| {
            app.focus = Focus::Overlay(Overlay::ModePicker { index: 0 });
        }),
        ("slash popup", |app| {
            app.input.insert_str("/");
        }),
    ]
}

/// Draw one frame with the overlay fully unfolded; the buffer and the rect the
/// overlay was given.
fn draw(app: &mut AppState) -> (Buffer, Rect) {
    app.last_focus_key = crate::ui::focus_key(&app.focus);
    app.overlay_since = Some(Instant::now() - Duration::from_secs(10));
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).expect("terminal");
    let mut rect = Rect::default();
    terminal
        .draw(|frame| {
            let prepared = crate::ui::prepare_frame(frame.area(), app, Instant::now());
            rect = prepared
                .full_popup
                .as_ref()
                .map(|popup| popup.area)
                .or(prepared.inline_popup.as_ref().map(|popup| popup.area))
                .or(prepared.is_picker.then_some(prepared.picker_base))
                .unwrap_or_default();
            crate::ui::draw(frame, app, &prepared);
        })
        .expect("draw");
    (terminal.backend().buffer().clone(), rect)
}

fn corner(rect: Rect) -> (u16, u16) {
    (rect.right() - 1, rect.bottom() - 1)
}

fn backgrounds(buffer: &Buffer, rect: Rect) -> Vec<Color> {
    rect.positions().map(|pos| buffer[pos].bg).collect()
}

#[test]
fn every_overlay_is_a_solid_panel_under_truecolor() {
    let theme = Theme::truecolor();
    let panel = theme.panel.bg.expect("truecolor has a panel");
    let selected = theme.selected_bg.bg.expect("truecolor has a selection bar");
    for (name, open) in families() {
        let mut app = app(theme.clone(), ColorMode::Truecolor);
        open(&mut app);
        let (buffer, rect) = draw(&mut app);
        assert!(!rect.is_empty(), "{name}: nothing was opened");
        let bgs = backgrounds(&buffer, rect);
        // No cell lets the terminal background through: the border, the
        // title and every padding span included.
        assert!(
            bgs.iter().all(|&bg| bg != Color::Reset),
            "{name}: a cell of {rect:?} kept the terminal background"
        );
        // The bottom-right corner is chrome or a blank cell, never a
        // selection or a badge.
        assert_eq!(buffer[corner(rect)].bg, panel, "{name}: corner");
        assert!(
            bgs.iter().filter(|&&bg| bg == panel).count() > bgs.len() / 2,
            "{name}: the panel is not the overlay's background"
        );
        // The panel is darker than the selection bar, and never mistaken for
        // it: a selected row, where there is one, stays distinct.
        assert_ne!(panel, selected);
    }
}

#[test]
fn the_selected_row_stays_distinct_on_the_panel() {
    let theme = Theme::truecolor();
    let selected = theme.selected_bg.bg.expect("truecolor has a selection bar");
    for (name, open) in families()
        .into_iter()
        .filter(|(name, _)| ["menu", "settings", "slash popup"].contains(name))
    {
        let mut app = app(theme.clone(), ColorMode::Truecolor);
        open(&mut app);
        let (buffer, rect) = draw(&mut app);
        assert!(
            backgrounds(&buffer, rect).contains(&selected),
            "{name}: no selection bar on the panel"
        );
    }
}

#[test]
fn ansi_overlays_keep_the_terminal_background() {
    let truecolor_panel = Theme::truecolor().panel.bg;
    for (name, open) in families() {
        let mut app = app(Theme::ansi(), ColorMode::Ansi);
        open(&mut app);
        let (buffer, rect) = draw(&mut app);
        assert!(!rect.is_empty(), "{name}: nothing was opened");
        assert_eq!(buffer[corner(rect)].bg, Color::Reset, "{name}: corner");
        assert!(
            backgrounds(&buffer, rect)
                .iter()
                .all(|&bg| Some(bg) != truecolor_panel),
            "{name}: a panel colour leaked into ANSI-16"
        );
    }
}
