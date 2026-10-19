//! Colour strings and the ANSI-16 fallback: parsing palette values, and
//! mapping RGB onto the 16 named colours by role and hue.

use ratatui::style::Color;

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

/// Palette roles that are greys by meaning. Under ANSI-16 they take the grey
/// ladder of `Theme::ansi()` (fg = the terminal's own, dim = Gray, faint =
/// DarkGray, selection reversed) whatever the palette's tint: the palette's
/// greys are tuned for its own background, which an ANSI terminal does not
/// share, and mapping them by brightness merges levels (dim onto faint, dim
/// onto fg) or puts faint text on a selection of the same grey.
pub(crate) const GREY_ROLES: &[&str] = &[
    "fg",
    "dim",
    "faint",
    "bold",
    "selected_bg",
    "bash_arg",
    "argument",
    "surface",
    "border",
    "panel",
    "tool_pending_bg",
    "tool_success_bg",
    "tool_error_bg",
    "status_bg",
    "status_bg_alt",
];

/// Palette roles whose ANSI-16 colour is fixed by meaning, as in
/// `Theme::ansi()`: the roles named after a colour, and the accent (Yellow —
/// never the error red). Any other role drawn in the same hex takes the same
/// colour, so syntax highlighting agrees with the status it borrows from.
pub(crate) const NAMED_ROLES: &[(&str, Color)] = &[
    ("green", Color::Green),
    ("red", Color::Red),
    ("yellow", Color::Yellow),
    ("cyan", Color::Cyan),
    ("violet", Color::Magenta),
    ("accent", Color::Yellow),
];

/// Saturation below which a colour is a grey, not a hue.
const GREY_SATURATION: f32 = 0.35;

/// Map an RGB value to an ANSI-16 colour by hue, not by distance.
///
/// Plain Euclidean distance to the 16 reference points is wrong for the
/// muted colours themes are made of: a dark grey lands nearer `Yellow`
/// (128,128,0) than `DarkGray`, and a sage green nearer `Gray` than `Green`
/// — so a palette's quiet text turned yellow and every status colour grey
/// under ANSI-16. Instead: low-saturation colours map to the grey ramp by
/// brightness; the rest keep their hue sector (red, yellow, green, cyan,
/// blue, magenta), bright when the colour is both bright and vivid.
pub fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Color {
    let (_, saturation, _) = hsv(r, g, b);
    if saturation < GREY_SATURATION {
        ansi_grey(r, g, b)
    } else {
        ansi_hue(r, g, b)
    }
}

/// `(max, saturation, chroma)` of an RGB colour, channels in `0..=1`.
fn hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
    (max, saturation, max - min)
}

/// The grey ramp by brightness — for the roles that are greys by meaning
/// (text, dim, faint, selection background), however tinted the palette is.
pub fn ansi_grey(r: u8, g: u8, b: u8) -> Color {
    match hsv(r, g, b).0 {
        v if v < 0.25 => Color::Black,
        v if v < 0.6 => Color::DarkGray,
        v if v < 0.85 => Color::Gray,
        _ => Color::White,
    }
}

/// Chroma below which even a status role is a grey: there is no hue to keep.
const NO_HUE_CHROMA: f32 = 0.04;

/// The hue sector — for the roles that are colours by meaning (status,
/// accent, syntax). Muted palettes (nord) keep their hues.
pub fn ansi_hue(r: u8, g: u8, b: u8) -> Color {
    let (value, saturation, delta) = hsv(r, g, b);
    if value < 0.2 || delta < NO_HUE_CHROMA {
        return ansi_grey(r, g, b);
    }
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = value;
    let hue = if max == rf {
        60.0 * ((gf - bf) / delta).rem_euclid(6.0)
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    let bright = value > 0.85 && saturation > 0.6;
    let (normal, light) = hue_sector(hue);
    if bright { light } else { normal }
}

/// The ANSI pair for a hue in degrees. Red is narrow (±18°): oranges are
/// accents and warnings, and a terminal's yellow carries them; folding them
/// into red would draw an accent in the error colour.
fn hue_sector(hue: f32) -> (Color, Color) {
    match hue {
        h if !(18.0..342.0).contains(&h) => (Color::Red, Color::LightRed),
        h if h < 75.0 => (Color::Yellow, Color::LightYellow),
        h if h < 165.0 => (Color::Green, Color::LightGreen),
        h if h < 200.0 => (Color::Cyan, Color::LightCyan),
        h if h < 270.0 => (Color::Blue, Color::LightBlue),
        _ => (Color::Magenta, Color::LightMagenta),
    }
}
