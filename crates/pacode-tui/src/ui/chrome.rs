//! The shared pieces of the "plaques and bands" visual language: badges, the
//! band bar, section headers, the rounded frame and the permission-mode tone.
//! Every widget that draws one of these goes through here, so the look stays
//! one look and the colour fallbacks (RGB → ANSI-16 → glyphs only) live in
//! one place — `Theme::badge` and the derived `surface`/`border` slots.

use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};

use pacode_render::{RenderOptions, Theme};
use pacode_types::state::Mode;

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod chrome_tests;

/// `+-|` frame for `ascii_only` terminals.
const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// ` TEXT ` on the tone's badge style — one cell of padding each side, so the
/// plaque reads as a block even where the background cannot be drawn.
pub fn badge(text: &str, tone: Style, opts: &RenderOptions) -> Span<'static> {
    Span::styled(format!(" {text} "), opts.theme.badge(tone))
}

/// The colour a permission mode is drawn in: its footer badge and the prompt
/// frame both follow it, so the mode is visible where the typing happens.
pub fn mode_tone(mode: Mode, theme: &Theme) -> Style {
    match mode {
        Mode::Bypass => theme.red,
        Mode::Plan => theme.yellow,
        Mode::Build | Mode::Auto => theme.cyan,
    }
}

/// Section headers in menus, overlays and the rail: bold, uppercase, quiet.
pub fn section_style(opts: &RenderOptions) -> Style {
    opts.theme.dim.add_modifier(Modifier::BOLD)
}

/// A section header span — the text uppercased so every caller agrees.
pub fn section(text: &str, opts: &RenderOptions) -> Span<'static> {
    Span::styled(text.to_uppercase(), section_style(opts))
}

/// The left bar of a band (the user-message plaque).
pub fn band_bar(opts: &RenderOptions) -> &'static str {
    if opts.glyphs.ascii { "|" } else { "▌" }
}

/// Rounded corners, or `+-|` under `ascii_only`.
pub fn border_set(opts: &RenderOptions) -> border::Set<'static> {
    if opts.glyphs.ascii {
        ASCII_BORDER
    } else {
        border::ROUNDED
    }
}

/// A full rounded frame in `style`, with `title` on the top border after one
/// run of the border line: `╭─ title ───╮`.
pub fn frame_block(
    style: Style,
    title: Vec<Span<'static>>,
    opts: &RenderOptions,
) -> Block<'static> {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(border_set(opts))
        .border_style(style);
    if !title.is_empty() {
        let mut spans = vec![Span::styled(opts.glyphs.hline, style)];
        spans.extend(title);
        block = block.title(Line::from(spans));
    }
    block
}
