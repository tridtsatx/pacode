//! ANSI-16 palette and glyph table (spec §13, mockup CSS vars). Colour is never the
//! only carrier of meaning: every status has a glyph too.

use std::collections::BTreeMap;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

use crate::color::{GREY_ROLES, NAMED_ROLES};
pub use crate::color::{ansi_grey, ansi_hue, parse_color, rgb_to_ansi16};
pub use crate::glyphs::Glyphs;
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
    /// Background of a full-width band (the user-message plaque). Derived from
    /// `selected_bg` pulled towards `faint`, so a selection painted over a
    /// band stays visible. Empty under ANSI-16 and mono: there is no band, and
    /// the bar plus the bold text carry the plaque.
    pub surface: Style,
    /// Foreground of frames and borders (the prompt frame at rest, menus,
    /// overlays). Derived between `faint` and `dim`.
    pub border: Style,
}

/// How far `surface` moves from `selected_bg` towards `faint`.
const SURFACE_MIX: f32 = 0.35;
/// How far `border` moves from `faint` towards `dim`.
const BORDER_MIX: f32 = 0.4;
/// Badge foregrounds: not pure black/white, which glare on most palettes.
const BADGE_DARK: Color = Color::Rgb(0x16, 0x16, 0x16);
const BADGE_LIGHT: Color = Color::Rgb(0xf6, 0xf6, 0xf4);

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

/// How much colour the terminal can show; renderers degrade in steps: RGB +
/// background bands under `Truecolor` → ANSI-16 foregrounds → glyphs only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMode {
    /// `COLORTERM`/`PACODE_COLOR` = `truecolor`/`24bit`.
    #[default]
    Truecolor,
    /// ANSI-16 only (`ui.color = ansi`, `PACODE_COLOR=ansi`, no truecolor).
    Ansi,
    /// No colour (`PACODE_COLOR=mono` or a non-empty `NO_COLOR`).
    Mono,
}

impl ColorMode {
    pub fn detect() -> Self {
        match std::env::var("PACODE_COLOR").ok().as_deref() {
            Some("mono") => Self::Mono,
            Some("ansi") => Self::Ansi,
            Some("truecolor") | Some("24bit") => Self::Truecolor,
            _ if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) => Self::Mono,
            _ if detect_truecolor() => Self::Truecolor,
            _ => Self::Ansi,
        }
    }

    pub fn from_setting(setting: &str, detected: Self) -> Self {
        match setting.trim().to_ascii_lowercase().as_str() {
            "ansi" => Self::Ansi,
            "truecolor" | "24bit" => Self::Truecolor,
            "mono" => Self::Mono,
            _ => detected,
        }
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
                // A role's meaning picks the mapping: text and backgrounds
                // stay on the grey ramp, status and syntax roles keep a hue.
                // A role named after a colour is that colour, as in
                // `Theme::ansi()`: a palette's olive "green" is still green.
                let named = NAMED_ROLES
                    .iter()
                    .find(|(role, _)| *role == key)
                    .map(|(_, c)| *c);
                match parsed {
                    Color::Rgb(..) if named.is_some() => named,
                    Color::Rgb(r, g, b) if GREY_ROLES.contains(&key) => Some(ansi_grey(r, g, b)),
                    Color::Rgb(r, g, b) => Some(ansi_hue(r, g, b)),
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
        let surface = resolve("surface")
            .map(|c| Style::default().bg(c))
            .unwrap_or_else(|| derive_surface(selected_bg, faint));
        let border = resolve("border")
            .map(|c| Style::default().fg(c))
            .unwrap_or_else(|| derive_border(faint, dim));

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
            surface,
            border,
        }
    }

    /// A badge (plaque) in `tone`: the tone's colour as the background under a
    /// contrasting bold foreground. The fallback follows the colour itself, so
    /// a palette mixing RGB and named colours still degrades per badge: RGB →
    /// background band; a named or indexed colour (ANSI-16) →
    /// `REVERSED|BOLD`, which lets the terminal pick the contrast; no colour
    /// (mono) → bold only, the glyph and the text are the marker.
    pub fn badge(&self, tone: Style) -> Style {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        match tone.fg {
            Some(Color::Rgb(r, g, b)) => bold.bg(Color::Rgb(r, g, b)).fg(contrast_fg(r, g, b)),
            None | Some(Color::Reset) => bold,
            Some(c) => bold.fg(c).add_modifier(Modifier::REVERSED),
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
        let selected_bg = Style::default().bg(Color::Rgb(0x1e, 0x24, 0x1c));
        let dim = rgb(0x75, 0x77, 0x6f);
        let faint = rgb(0x43, 0x46, 0x3f);
        Self {
            fg: rgb(0xc9, 0xc7, 0xc0),
            dim,
            faint,
            accent: rgb(0xd9, 0x9b, 0x4e),
            green: rgb(0x96, 0xb3, 0x5d),
            cyan: rgb(0x6e, 0xa9, 0xbd),
            red: rgb(0xc8, 0x69, 0x5c),
            violet: rgb(0x9a, 0x8b, 0xc4),
            yellow: rgb(0xf5, 0xc5, 0x42),
            bold: Style::default()
                .fg(Color::Rgb(0xe7, 0xe5, 0xdd))
                .add_modifier(Modifier::BOLD),
            selected_bg,
            user_bar: rgb(0x6e, 0xa9, 0xbd),
            bash_command: rgb(0x6e, 0xa9, 0xbd).add_modifier(Modifier::BOLD),
            bash_flag: rgb(0x9a, 0x8b, 0xc4),
            bash_string: rgb(0x96, 0xb3, 0x5d),
            bash_operator: rgb(0xd9, 0x9b, 0x4e),
            bash_variable: rgb(0xf5, 0xc5, 0x42),
            bash_arg: rgb(0xc9, 0xc7, 0xc0),
            surface: derive_surface(selected_bg, faint),
            border: derive_border(faint, dim),
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
            surface: Style::default(),
            border: Style::default().fg(Color::DarkGray),
        }
    }

    pub fn mono() -> Self {
        Self {
            fg: Style::default(),
            dim: Style::default(),
            faint: Style::default(),
            accent: Style::default(),
            green: Style::default(),
            cyan: Style::default(),
            red: Style::default(),
            violet: Style::default(),
            yellow: Style::default(),
            bold: Style::default().add_modifier(Modifier::BOLD),
            selected_bg: Style::default().add_modifier(Modifier::REVERSED),
            user_bar: Style::default(),
            bash_command: Style::default().add_modifier(Modifier::BOLD),
            bash_flag: Style::default(),
            bash_string: Style::default(),
            bash_operator: Style::default(),
            bash_variable: Style::default(),
            bash_arg: Style::default(),
            surface: Style::default(),
            border: Style::default(),
        }
    }
}

/// `selected_bg` pulled towards `faint` — only when both are RGB. A terminal
/// limited to named colours gets no band: a named background is either the
/// terminal's own background (invisible) or a loud block.
fn derive_surface(selected_bg: Style, faint: Style) -> Style {
    match (selected_bg.bg, faint.fg) {
        (Some(Color::Rgb(r1, g1, b1)), Some(Color::Rgb(r2, g2, b2))) => {
            Style::default().bg(mix((r1, g1, b1), (r2, g2, b2), SURFACE_MIX))
        }
        _ => Style::default(),
    }
}

/// Between `faint` and `dim` when both are RGB; `faint` itself otherwise.
fn derive_border(faint: Style, dim: Style) -> Style {
    match (faint.fg, dim.fg) {
        (Some(Color::Rgb(r1, g1, b1)), Some(Color::Rgb(r2, g2, b2))) => {
            Style::default().fg(mix((r1, g1, b1), (r2, g2, b2), BORDER_MIX))
        }
        _ => faint,
    }
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> Color {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Near-black or near-white, whichever has the higher WCAG contrast ratio
/// against `(r, g, b)`.
fn contrast_fg(r: u8, g: u8, b: u8) -> Color {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    // Against black the ratio is (l + 0.05) / 0.05, against white
    // 1.05 / (l + 0.05); black wins when the first is larger.
    if (l + 0.05) * (l + 0.05) > 0.05 * 1.05 {
        BADGE_DARK
    } else {
        BADGE_LIGHT
    }
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod theme_tests;
