//! ANSI-16 palette and glyph table (spec §13, mockup CSS vars). Colour is never the
//! only carrier of meaning: every status has a glyph too.

use std::collections::BTreeMap;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

pub use crate::palettes::builtin_palettes;

/// Serializable color palette. Role names match `Theme` fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    pub name: String,
    pub light: bool,
    pub colors: BTreeMap<String, String>,
}

/// Semantic roles for syntax highlighting in bash mode (spec prompt-input feature 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashRole {
    Command,
    Flag,
    String,
    Operator,
    Variable,
    Argument,
}

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
    pub yellow: Style,
    pub bold: Style,
    pub selected_bg: Style,
    pub user_bar: Style,
    pub bash_command: Style,
    pub bash_flag: Style,
    pub bash_string: Style,
    pub bash_operator: Style,
    pub bash_variable: Style,
    pub bash_arg: Style,
}

/// Detects `COLORTERM` (`truecolor`/`24bit`) or `PACODE_COLOR=ansi|truecolor`.
pub fn detect_truecolor() -> bool {
    let forced = std::env::var("PACODE_COLOR").ok();
    match forced.as_deref() {
        Some("ansi") => false,
        Some("truecolor") | Some("24bit") => true,
        _ => std::env::var("COLORTERM")
            .map(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("truecolor") || v.contains("24bit")
            })
            .unwrap_or(false),
    }
}

impl Default for Theme {
    fn default() -> Self {
        if detect_truecolor() {
            Self::truecolor()
        } else {
            Self::ansi()
        }
    }
}

impl Theme {
    /// Build a `Theme` from a `Palette`. Missing keys fall back to the defaults
    /// for that role. When `truecolor` is false, RGB hex colors are mapped to
    /// the nearest ANSI-16 color.
    pub fn from_palette(p: &Palette, truecolor: bool) -> Self {
        let base = if truecolor {
            Self::truecolor()
        } else {
            Self::ansi()
        };

        let resolve = |key: &str| -> Option<Color> {
            let raw = p.colors.get(key)?;
            let parsed = parse_color(raw)?;
            if !truecolor {
                match parsed {
                    Color::Rgb(r, g, b) => Some(rgb_to_ansi16(r, g, b)),
                    other => Some(other),
                }
            } else {
                Some(parsed)
            }
        };

        let fg = resolve("fg")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.fg);
        let dim = resolve("dim")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.dim);
        let faint = resolve("faint")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.faint);
        let accent = resolve("accent")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.accent);
        let green = resolve("green")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.green);
        let cyan = resolve("cyan")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.cyan);
        let red = resolve("red")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.red);
        let violet = resolve("violet")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.violet);
        let yellow = resolve("yellow")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.yellow);
        let bold = resolve("bold")
            .map(|c| Style::default().fg(c).add_modifier(Modifier::BOLD))
            .unwrap_or(base.bold);
        let selected_bg = resolve("selected_bg")
            .map(|c| {
                if !truecolor && c == Color::Black {
                    Style::default()
                        .bg(Color::Black)
                        .add_modifier(Modifier::REVERSED)
                } else {
                    Style::default().bg(c)
                }
            })
            .unwrap_or(base.selected_bg);
        let user_bar = resolve("user_bar")
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.user_bar);
        let bash_command = resolve("bash_command")
            .or_else(|| resolve("command"))
            .map(|c| Style::default().fg(c).add_modifier(Modifier::BOLD))
            .unwrap_or(base.bash_command);
        let bash_flag = resolve("bash_flag")
            .or_else(|| resolve("flag"))
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.bash_flag);
        let bash_string = resolve("bash_string")
            .or_else(|| resolve("string"))
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.bash_string);
        let bash_operator = resolve("bash_operator")
            .or_else(|| resolve("operator"))
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.bash_operator);
        let bash_variable = resolve("bash_variable")
            .or_else(|| resolve("variable"))
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.bash_variable);
        let bash_arg = resolve("bash_arg")
            .or_else(|| resolve("argument"))
            .map(|c| Style::default().fg(c))
            .unwrap_or(base.bash_arg);

        Self {
            fg,
            dim,
            faint,
            accent,
            green,
            cyan,
            red,
            violet,
            yellow,
            bold,
            selected_bg,
            user_bar,
            bash_command,
            bash_flag,
            bash_string,
            bash_operator,
            bash_variable,
            bash_arg,
        }
    }

    /// Style for a semantic bash role.
    pub fn bash_style(&self, role: BashRole) -> Style {
        match role {
            BashRole::Command => self.bash_command,
            BashRole::Flag => self.bash_flag,
            BashRole::String => self.bash_string,
            BashRole::Operator => self.bash_operator,
            BashRole::Variable => self.bash_variable,
            BashRole::Argument => self.bash_arg,
        }
    }

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
            yellow: rgb(0xf5, 0xc5, 0x42),
            bold: Style::default()
                .fg(Color::Rgb(0xe7, 0xe5, 0xdd))
                .add_modifier(Modifier::BOLD),
            selected_bg: Style::default().bg(Color::Rgb(0x1e, 0x24, 0x1c)),
            user_bar: rgb(0x6e, 0xa9, 0xbd),
            bash_command: rgb(0x6e, 0xa9, 0xbd).add_modifier(Modifier::BOLD),
            bash_flag: rgb(0x9a, 0x8b, 0xc4),
            bash_string: rgb(0x96, 0xb3, 0x5d),
            bash_operator: rgb(0xd9, 0x9b, 0x4e),
            bash_variable: rgb(0xf5, 0xc5, 0x42),
            bash_arg: rgb(0xc9, 0xc7, 0xc0),
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
            yellow: Style::default().fg(Color::Yellow),
            bold: Style::default().add_modifier(Modifier::BOLD),
            selected_bg: Style::default()
                .bg(Color::Black)
                .add_modifier(Modifier::REVERSED),
            user_bar: Style::default().fg(Color::Cyan),
            bash_command: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            bash_flag: Style::default().fg(Color::Magenta),
            bash_string: Style::default().fg(Color::Green),
            bash_operator: Style::default().fg(Color::Yellow),
            bash_variable: Style::default().fg(Color::Yellow),
            bash_arg: Style::default(),
        }
    }
}

/// Parse a color string: `"#rrggbb"`, `"#rgb"`, a named ANSI colour, or an index `0-255`.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        } else if hex.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            return Some(Color::Rgb(r * 17, g * 17, b * 17));
        }
        return None;
    }

    if let Ok(idx) = s.parse::<u8>() {
        return Some(Color::Indexed(idx));
    }

    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" | "purple" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "dark_gray" | "dark-gray" | "darkgrey" => Some(Color::DarkGray),
        "lightred" | "light_red" | "light-red" => Some(Color::LightRed),
        "lightgreen" | "light_green" | "light-green" => Some(Color::LightGreen),
        "lightyellow" | "light_yellow" | "light-yellow" => Some(Color::LightYellow),
        "lightblue" | "light_blue" | "light-blue" => Some(Color::LightBlue),
        "lightmagenta" | "light_magenta" | "light-magenta" => Some(Color::LightMagenta),
        "lightcyan" | "light_cyan" | "light-cyan" => Some(Color::LightCyan),
        "white" => Some(Color::White),
        "reset" => Some(Color::Reset),
        _ => None,
    }
}

/// Map an RGB value to the nearest ANSI-16 colour using squared Euclidean distance.
pub fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Color {
    const ANSI_TABLE: [(Color, (u8, u8, u8)); 16] = [
        (Color::Black, (0, 0, 0)),
        (Color::Red, (128, 0, 0)),
        (Color::Green, (0, 128, 0)),
        (Color::Yellow, (128, 128, 0)),
        (Color::Blue, (0, 0, 128)),
        (Color::Magenta, (128, 0, 128)),
        (Color::Cyan, (0, 128, 128)),
        (Color::Gray, (192, 192, 192)),
        (Color::DarkGray, (128, 128, 128)),
        (Color::LightRed, (255, 0, 0)),
        (Color::LightGreen, (0, 255, 0)),
        (Color::LightYellow, (255, 255, 0)),
        (Color::LightBlue, (0, 0, 255)),
        (Color::LightMagenta, (255, 0, 255)),
        (Color::LightCyan, (0, 255, 255)),
        (Color::White, (255, 255, 255)),
    ];

    let mut best_color = Color::White;
    let mut best_dist = u64::MAX;

    for (color, (ar, ag, ab)) in ANSI_TABLE {
        let dr = (r as i32) - (ar as i32);
        let dg = (g as i32) - (ag as i32);
        let db = (b as i32) - (ab as i32);
        let dist = (dr * dr + dg * dg + db * db) as u64;
        if dist < best_dist {
            best_dist = dist;
            best_color = color;
        }
    }

    best_color
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
    pub disabled: &'static str,
    pub stopped: &'static str,
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
                disabled: "o",
                stopped: "-",
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
                chevrons: "»",
                disabled: "⊘",
                stopped: "■",
            }
        }
    }

    /// Braille spinner frame for `frame` (`-\|/` under ASCII). A method rather
    /// than a field because the glyph is a cycle, not a constant.
    pub fn spinner(&self, frame: u64) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        const ASCII: [&str; 4] = ["-", "\\", "|", "/"];
        if self.ascii {
            ASCII[(frame as usize) % ASCII.len()]
        } else {
            FRAMES[(frame as usize) % FRAMES.len()]
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
#[path = "theme_tests.rs"]
mod theme_tests;
