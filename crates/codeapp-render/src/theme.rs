//! ANSI-16 palette and glyph table (spec §13, mockup CSS vars). Colour is never the
//! only carrier of meaning: every status has a glyph too.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub fg: Style,
    pub dim: Style,
    pub faint: Style,
    pub accent: Style,
    pub green: Style,
    pub cyan: Style,
    pub red: Style,
    pub violet: Style,
    pub bold: Style,
    pub selected_bg: Style,
    pub user_bar: Style,
}

impl Default for Theme {
    /// Detects `COLORTERM` (`truecolor`/`24bit`) → mockup RGB palette, else ANSI-16.
    /// `CODEAPP_COLOR=ansi|truecolor` forces one.
    fn default() -> Self {
        let forced = std::env::var("CODEAPP_COLOR").ok();
        let truecolor = match forced.as_deref() {
            Some("ansi") => false,
            Some("truecolor") | Some("24bit") => true,
            _ => std::env::var("COLORTERM")
                .map(|v| {
                    let v = v.to_ascii_lowercase();
                    v.contains("truecolor") || v.contains("24bit")
                })
                .unwrap_or(false),
        };
        if truecolor {
            Self::truecolor()
        } else {
            Self::ansi()
        }
    }
}

impl Theme {
    /// The mockup palette (see the design artifact CSS variables). Terminals whose
    /// ANSI-16 palette is monochrome still get colour this way.
    pub fn truecolor() -> Self {
        let rgb = |r: u8, g: u8, b: u8| Style::default().fg(Color::Rgb(r, g, b));
        Self {
            fg: rgb(0xc9, 0xc7, 0xc0),
            dim: rgb(0x75, 0x77, 0x6f),
            faint: rgb(0x43, 0x46, 0x3f),
            accent: rgb(0xd9, 0x9b, 0x4e),
            green: rgb(0x96, 0xb3, 0x5d),
            cyan: rgb(0x6e, 0xa9, 0xbd),
            red: rgb(0xc8, 0x69, 0x5c),
            violet: rgb(0x9a, 0x8b, 0xc4),
            bold: Style::default()
                .fg(Color::Rgb(0xe7, 0xe5, 0xdd))
                .add_modifier(Modifier::BOLD),
            selected_bg: Style::default().bg(Color::Rgb(0x1e, 0x24, 0x1c)),
            user_bar: rgb(0x6e, 0xa9, 0xbd),
        }
    }

    /// Plain ANSI-16 palette.
    pub fn ansi() -> Self {
        Self {
            fg: Style::default(),
            dim: Style::default().fg(Color::Gray),
            faint: Style::default().fg(Color::DarkGray),
            accent: Style::default().fg(Color::Yellow),
            green: Style::default().fg(Color::Green),
            cyan: Style::default().fg(Color::Cyan),
            red: Style::default().fg(Color::Red),
            violet: Style::default().fg(Color::Magenta),
            bold: Style::default().add_modifier(Modifier::BOLD),
            selected_bg: Style::default()
                .bg(Color::Black)
                .add_modifier(Modifier::REVERSED),
            user_bar: Style::default().fg(Color::Cyan),
        }
    }
}

/// Glyphs with ASCII fallback (`ui.ascii_only`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyphs {
    pub ascii: bool,
    pub bar_full: &'static str,
    pub bar_empty: &'static str,
    pub running: &'static str,
    pub pointer: &'static str,
    pub ok: &'static str,
    pub fail: &'static str,
    pub main_dot: &'static str,
    pub agent_dot: &'static str,
    pub tool: &'static str,
    pub prompt: &'static str,
    pub check_done: &'static str,
    pub check_active: &'static str,
    pub check_pending: &'static str,
    pub arrow_down: &'static str,
    pub arrow_up: &'static str,
    pub arrow_right: &'static str,
    pub dot_sep: &'static str,
    pub ellipsis: &'static str,
    pub vline: &'static str,
    pub hline: &'static str,
    pub chevrons: &'static str,
}

impl Glyphs {
    pub fn new(ascii_only: bool) -> Self {
        if ascii_only {
            Self {
                ascii: true,
                bar_full: "#",
                bar_empty: "-",
                running: "*",
                pointer: ">",
                ok: "+",
                fail: "x",
                main_dot: "@",
                agent_dot: "o",
                tool: "#",
                prompt: ">",
                check_done: "[x]",
                check_active: "[*]",
                check_pending: "[ ]",
                arrow_down: "v",
                arrow_up: "^",
                arrow_right: "->",
                dot_sep: " . ",
                ellipsis: "...",
                vline: "|",
                hline: "-",
                chevrons: ">>",
            }
        } else {
            Self {
                ascii: false,
                bar_full: "▰",
                bar_empty: "▱",
                running: "◍",
                pointer: "▸",
                ok: "✓",
                fail: "✗",
                main_dot: "●",
                agent_dot: "○",
                tool: "▣",
                prompt: "❯",
                check_done: "[✓]",
                check_active: "[•]",
                check_pending: "[ ]",
                arrow_down: "↓",
                arrow_up: "↑",
                arrow_right: "→",
                dot_sep: " · ",
                ellipsis: "…",
                vline: "│",
                hline: "─",
                chevrons: "▸▸",
            }
        }
    }

    /// `▰▰▰▰▰▱▱▱` for `percent` over `width` cells.
    pub fn progress_bar(&self, percent: u8, width: usize) -> String {
        let width = width.max(1);
        let filled = ((percent.min(100) as usize) * width)
            .div_ceil(100)
            .min(width);
        let mut out = String::with_capacity(width * 3);
        for _ in 0..filled {
            out.push_str(self.bar_full);
        }
        for _ in filled..width {
            out.push_str(self.bar_empty);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_rounds_up() {
        let g = Glyphs::new(true);
        assert_eq!(g.progress_bar(60, 8), "#####---");
        assert_eq!(g.progress_bar(0, 4), "----");
        assert_eq!(g.progress_bar(100, 4), "####");
    }
}
