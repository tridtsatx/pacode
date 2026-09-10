//! Header mascot: Pac-Man or one of the four ghosts, picked at random once per
//! process. Sprites are pixel grids rendered with half blocks, so one cell holds
//! two vertically stacked pixels and the sprite keeps a 1:1 aspect on screen.
//!
//! Colours are the arcade palette rather than theme roles: these are the
//! characters' own colours, and a Pac-Man drawn in the user's accent would stop
//! being Pac-Man. They are given as RGB and folded down to the nearest ANSI
//! colour when the terminal has no truecolor.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use pacode_render::rgb_to_ansi16;

#[cfg(test)]
#[path = "mascot_tests.rs"]
mod mascot_tests;

/// Arcade palette, as (r, g, b).
const PACMAN_YELLOW: (u8, u8, u8) = (255, 255, 0);
const BLINKY_RED: (u8, u8, u8) = (255, 0, 0);
const PINKY_PINK: (u8, u8, u8) = (255, 184, 255);
const INKY_CYAN: (u8, u8, u8) = (0, 255, 255);
const CLYDE_ORANGE: (u8, u8, u8) = (255, 184, 82);
const EYE_WHITE: (u8, u8, u8) = (255, 255, 255);
const PUPIL_BLUE: (u8, u8, u8) = (33, 33, 222);
const PUPIL_BLACK: (u8, u8, u8) = (0, 0, 0);

/// Pixel legend: `' '` transparent, `'#'` body, `'w'` eye white, `'b'` blue
/// pupil (ghosts), `'k'` black pupil (Pac-Man's single eye).
struct Sprite {
    pixels: &'static [&'static str],
    body: (u8, u8, u8),
}

/// A circle 12 pixels across — rows 0/11 span 4 columns, 1/10 span 8, 2/9 span
/// 10 and the middle six span all 12 — with a wedge cut out on the right for the
/// mouth and a single eye above it. The row widths are symmetric top to bottom
/// and left to right outside the mouth; breaking that symmetry is what makes the
/// sprite read as an egg instead of a ball.
const PACMAN: &[&str] = &[
    "    ######    ",
    "  ##########  ",
    " ######kk#### ",
    "#######kk###  ",
    "##########    ",
    "#########     ",
    "#########     ",
    "##########    ",
    "############  ",
    " ############ ",
    "  ##########  ",
    "    ######    ",
];

/// Mouth-closed Pac-Man: the same circle with the wedge filled in. Alternating it
/// with `PACMAN` is the chomp. Row widths stay identical so the block never moves.
const PACMAN_CLOSED: &[&str] = &[
    "    ######    ",
    "  ##########  ",
    " ######kk#### ",
    "#######kk#####",
    "##############",
    "##############",
    "##############",
    "##############",
    "##############",
    " ############ ",
    "  ##########  ",
    "    ######    ",
];

const GHOST: &[&str] = &[
    "    ######    ",
    "  ##########  ",
    " ############ ",
    "##wwww##wwww##",
    "##wbbw##wbbw##",
    "##wbbw##wbbw##",
    "##wwww##wwww##",
    "##############",
    "##############",
    "##############",
    "##..##..##..##",
    "##..##..##..##",
];

/// The ghost with its skirt shifted one pixel: the tentacles wave.
const GHOST_ALT: &[&str] = &[
    "    ######    ",
    "  ##########  ",
    " ############ ",
    "##wwww##wwww##",
    "##wbbw##wbbw##",
    "##wbbw##wbbw##",
    "##wwww##wwww##",
    "##############",
    "##############",
    "##############",
    "#..##..##..###",
    "#..##..##..###",
];

/// Number of animation frames every mascot has.
pub const FRAMES: u64 = 2;

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

    fn sprite(self, frame: u64) -> Sprite {
        let second = !frame.is_multiple_of(2);
        let ghost = if second { GHOST_ALT } else { GHOST };
        match self {
            MascotKind::Pacman => Sprite {
                pixels: if second { PACMAN_CLOSED } else { PACMAN },
                body: PACMAN_YELLOW,
            },
            MascotKind::Blinky => Sprite {
                pixels: ghost,
                body: BLINKY_RED,
            },
            MascotKind::Inky => Sprite {
                pixels: ghost,
                body: INKY_CYAN,
            },
            MascotKind::Clyde => Sprite {
                pixels: ghost,
                body: CLYDE_ORANGE,
            },
            MascotKind::Pinky => Sprite {
                pixels: ghost,
                body: PINKY_PINK,
            },
        }
    }
}

/// Rendered height in terminal rows (two pixel rows per row).
pub const HEIGHT: usize = 6;
/// Rendered width in terminal columns.
pub const WIDTH: usize = 14;

fn color(rgb: (u8, u8, u8), truecolor: bool) -> Color {
    let (r, g, b) = rgb;
    if truecolor {
        Color::Rgb(r, g, b)
    } else {
        rgb_to_ansi16(r, g, b)
    }
}

fn pixel_color(ch: char, body: (u8, u8, u8), truecolor: bool) -> Option<Color> {
    let rgb = match ch {
        '#' => body,
        'w' => EYE_WHITE,
        'b' => PUPIL_BLUE,
        'k' => PUPIL_BLACK,
        _ => return None,
    };
    Some(color(rgb, truecolor))
}

/// Render `kind` into `HEIGHT` lines of `WIDTH` columns, at rest.
pub fn render(kind: MascotKind, truecolor: bool) -> Vec<Line<'static>> {
    render_frame(kind, 0, truecolor)
}

/// Render animation frame `frame` of `kind`.
///
/// The frames are static data of identical width and height, and nothing here
/// arms a timer: the caller passes the animation counter that already advances
/// for other reasons, so an idle session keeps drawing the same frame and costs
/// nothing.
pub fn render_frame(kind: MascotKind, frame: u64, truecolor: bool) -> Vec<Line<'static>> {
    render_sprite(&kind.sprite(frame % FRAMES), truecolor)
}

fn render_sprite(sprite: &Sprite, truecolor: bool) -> Vec<Line<'static>> {
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
            let tc = pixel_color(t, sprite.body, truecolor);
            let bc = pixel_color(b, sprite.body, truecolor);
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
pub fn render_ascii(truecolor: bool) -> Vec<Line<'static>> {
    render_ascii_frame(0, truecolor)
}

/// ASCII fallback, animated: the mouth opens and closes on the same two frames.
pub fn render_ascii_frame(frame: u64, truecolor: bool) -> Vec<Line<'static>> {
    let yellow = Style::default().fg(color(PACMAN_YELLOW, truecolor));
    let (mouth_top, mouth_bottom) = if frame.is_multiple_of(2) {
        ("\\    <", " '--'")
    } else {
        ("\\    |", " '--\'")
    };
    vec![
        Line::from(Span::styled(" .--.", yellow)),
        Line::from(Span::styled("/ o  \\", yellow)),
        Line::from(Span::styled(mouth_top, yellow)),
        Line::from(Span::styled(mouth_bottom, yellow)),
    ]
}
