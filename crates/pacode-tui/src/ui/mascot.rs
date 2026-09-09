//! Header mascot: Pac-Man or one of the four ghosts, picked at random once per
//! process. Sprites are pixel grids rendered with half blocks, so one cell holds
//! two vertically stacked pixels and the sprite keeps a 1:1 aspect on screen.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

#[cfg(test)]
#[path = "mascot_tests.rs"]
mod mascot_tests;

/// Pixel legend: `' '` transparent, `'#'` body, `'w'` white, `'k'` black.
struct Sprite {
    pixels: &'static [&'static str],
    body: Color,
}

const PACMAN: &[&str] = &[
    "    ####    ",
    "  ########  ",
    " ########## ",
    " ########## ",
    "##########  ",
    "#######     ",
    "#######     ",
    "##########  ",
    " ########## ",
    " ########## ",
    "  ########  ",
    "    ####    ",
];

const GHOST: &[&str] = &[
    "    ####    ",
    "  ########  ",
    " ########## ",
    "#wwww##wwww#",
    "#wkkw##wkkw#",
    "#wkkw##wkkw#",
    "#wwww##wwww#",
    "############",
    "############",
    "############",
    "############",
    "##.##..##.##",
];

/// Which mascot the session shows. Picked once per run so the banner is stable
/// within a session and deterministic in tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MascotKind {
    Pacman,
    Blinky,
    Inky,
    Clyde,
    Pinky,
}

impl MascotKind {
    pub const ALL: [MascotKind; 5] = [
        MascotKind::Pacman,
        MascotKind::Blinky,
        MascotKind::Inky,
        MascotKind::Clyde,
        MascotKind::Pinky,
    ];

    pub fn random() -> Self {
        Self::ALL[rand::random_range(0..Self::ALL.len())]
    }

    fn sprite(self) -> Sprite {
        match self {
            MascotKind::Pacman => Sprite {
                pixels: PACMAN,
                body: Color::Yellow,
            },
            MascotKind::Blinky => Sprite {
                pixels: GHOST,
                body: Color::Red,
            },
            MascotKind::Inky => Sprite {
                pixels: GHOST,
                body: Color::Cyan,
            },
            MascotKind::Clyde => Sprite {
                pixels: GHOST,
                body: Color::LightRed,
            },
            MascotKind::Pinky => Sprite {
                pixels: GHOST,
                body: Color::Magenta,
            },
        }
    }
}

/// Rendered height in terminal rows (two pixel rows per row).
pub const HEIGHT: usize = 6;
/// Rendered width in terminal columns.
pub const WIDTH: usize = 12;

fn pixel_color(ch: char, body: Color) -> Option<Color> {
    match ch {
        '#' => Some(body),
        'w' => Some(Color::White),
        'k' => Some(Color::Black),
        _ => None,
    }
}

/// Render `kind` into `HEIGHT` lines of `WIDTH` columns.
pub fn render(kind: MascotKind) -> Vec<Line<'static>> {
    render_sprite(&kind.sprite())
}

fn render_sprite(sprite: &Sprite) -> Vec<Line<'static>> {
    let rows = sprite.pixels;
    let mut lines = Vec::with_capacity(rows.len().div_ceil(2));

    for pair in rows.chunks(2) {
        let top: Vec<char> = pair[0].chars().collect();
        let bottom: Vec<char> = pair.get(1).map(|r| r.chars().collect()).unwrap_or_default();
        let width = top.len().max(bottom.len());

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(width);
        for x in 0..width {
            let t = top.get(x).copied().unwrap_or(' ');
            let b = bottom.get(x).copied().unwrap_or(' ');
            let (tc, bc) = (pixel_color(t, sprite.body), pixel_color(b, sprite.body));
            let span = match (tc, bc) {
                (None, None) => Span::raw(" "),
                (Some(c), None) => Span::styled("▀", Style::default().fg(c)),
                (None, Some(c)) => Span::styled("▄", Style::default().fg(c)),
                (Some(t), Some(b)) => Span::styled("▀", Style::default().fg(t).bg(b)),
            };
            spans.push(span);
        }
        lines.push(Line::from(spans));
    }

    lines
}

/// ASCII fallback used when the terminal is restricted to ASCII glyphs.
pub fn render_ascii() -> Vec<Line<'static>> {
    let yellow = Style::default().fg(Color::Yellow);
    vec![
        Line::from(Span::styled(" .--.", yellow)),
        Line::from(Span::styled("/ o  \\", yellow)),
        Line::from(Span::styled("\\    <", yellow)),
        Line::from(Span::styled(" '--'", yellow)),
    ]
}
