//! Best-effort terminal font control.
//!
//! A terminal application does not own its font — the emulator does. This module
//! attempts font configuration only when supported by the detected emulator and
//! honestly reports unsupported capabilities.

use std::io::Write;

use pacode_types::FontConfig;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};

#[cfg(test)]
#[path = "font_tests.rs"]
mod font_tests;

/// Terminal font control protocol or capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontControl {
    Kitty,
    Xterm,
    Unsupported,
}

/// Outcome of attempting to apply font configuration to the terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontOutcome {
    pub applied: Vec<String>,
    pub unsupported: Vec<String>,
}

impl FontOutcome {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.applied.is_empty() && self.unsupported.is_empty()
    }
}

/// Detect terminal font control capability from the process environment.
pub fn detect() -> FontControl {
    detect_from(&|k| std::env::var(k).ok())
}

/// Pure detection function testable against any environment variable resolver.
///
/// Rules:
/// - Kitty: `TERM=xterm-kitty` or `KITTY_WINDOW_ID` is set and non-empty.
/// - Xterm: `TERM` starts with `xterm` and it is NOT Kitty.
/// - Unsupported: everything else.
pub fn detect_from(env: &dyn Fn(&str) -> Option<String>) -> FontControl {
    let term = env("TERM");
    let term_ref = term.as_deref().unwrap_or("");
    let kitty_window_id = env("KITTY_WINDOW_ID");

    let is_kitty = term_ref == "xterm-kitty" || kitty_window_id.is_some_and(|v| !v.is_empty());
    if is_kitty {
        return FontControl::Kitty;
    }

    if term_ref.starts_with("xterm") {
        return FontControl::Xterm;
    }

    FontControl::Unsupported
}

/// Returns the OSC 50 escape sequence bytes for setting font family in Xterm.
pub fn osc50_sequence(family: &str) -> Vec<u8> {
    format!("\x1b]50;{family}\x07").into_bytes()
}

/// Returns true if the font family string already carries a size specification.
///
/// Recognizes Fontconfig patterns (e.g. `Family:size=12`, `Family:pixelsize=14`, `Family 12`, `Family-12`)
/// as well as standard 14-token XLFD strings containing pixel sizes.
pub fn family_carries_size(family: &str) -> bool {
    if family.contains(":size=") || family.contains(":pixelsize=") {
        return true;
    }
    if family.starts_with('-') && family.matches('-').count() >= 14 {
        return true;
    }
    let parts: Vec<&str> = family.split([' ', '-', ':']).collect();
    if let Some(last) = parts.last()
        && !last.is_empty()
        && last.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    false
}

/// Base text style for the given font configuration.
/// If `weight` is "bold", adds `Modifier::BOLD`.
pub fn base_style(cfg: &FontConfig) -> Style {
    if cfg
        .weight
        .as_deref()
        .is_some_and(|w| w.eq_ignore_ascii_case("bold"))
    {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// Add `Modifier::BOLD` to all cells in the frame buffer if font weight is configured as bold.
pub fn apply_weight_to_frame(frame: &mut Frame, cfg: &FontConfig) {
    apply_weight_to_buffer(frame.buffer_mut(), cfg);
}

/// Add `Modifier::BOLD` to all cells in the buffer if font weight is configured as bold.
pub fn apply_weight_to_buffer(buffer: &mut Buffer, cfg: &FontConfig) {
    if cfg
        .weight
        .as_deref()
        .is_some_and(|w| w.eq_ignore_ascii_case("bold"))
    {
        let area = buffer.area;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_style(cell.style().add_modifier(Modifier::BOLD));
                }
            }
        }
    }
}

fn apply_weight_outcome(
    cfg: &FontConfig,
    applied: &mut Vec<String>,
    unsupported: &mut Vec<String>,
) {
    if let Some(ref weight) = cfg.weight {
        if weight.eq_ignore_ascii_case("bold") || weight.eq_ignore_ascii_case("normal") {
            applied.push("weight".to_string());
        } else {
            unsupported.push("weight".to_string());
        }
    }
}

/// Apply font configuration to the terminal using standard stdout.
pub fn apply(cfg: &FontConfig, control: FontControl) -> FontOutcome {
    let mut stdout = std::io::stdout();
    apply_with_writer(cfg, control, &mut stdout)
}

/// Apply font configuration to the provided terminal writer.
pub fn apply_with_writer<W: Write>(
    cfg: &FontConfig,
    control: FontControl,
    writer: &mut W,
) -> FontOutcome {
    let mut applied = Vec::new();
    let mut unsupported = Vec::new();

    if cfg.family.is_none() && cfg.size.is_none() && cfg.weight.is_none() {
        return FontOutcome {
            applied,
            unsupported,
        };
    }

    match control {
        FontControl::Kitty => {
            if let Some(ref _family) = cfg.family {
                unsupported.push("family".to_string());
            }
            if let Some(size) = cfg.size {
                let status = std::process::Command::new("kitten")
                    .args(["@", "set-font-size", &size.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                match status {
                    Ok(s) => {
                        if s.success() {
                            applied.push("size".to_string());
                        } else {
                            unsupported.push("size".to_string());
                        }
                    }
                    Err(_) => {
                        unsupported.push("size".to_string());
                    }
                }
            }
            apply_weight_outcome(cfg, &mut applied, &mut unsupported);
        }
        FontControl::Xterm => {
            if let Some(ref family) = cfg.family {
                let seq = osc50_sequence(family);
                match writer.write_all(&seq).and_then(|_| writer.flush()) {
                    Ok(()) => applied.push("family".to_string()),
                    Err(_) => unsupported.push("family".to_string()),
                }
            }
            if let Some(_size) = cfg.size {
                if cfg.family.as_deref().is_some_and(family_carries_size) {
                    applied.push("size".to_string());
                } else {
                    unsupported.push("size".to_string());
                }
            }
            apply_weight_outcome(cfg, &mut applied, &mut unsupported);
        }
        FontControl::Unsupported => {
            if let Some(ref _family) = cfg.family {
                unsupported.push("family".to_string());
            }
            if let Some(_size) = cfg.size {
                unsupported.push("size".to_string());
            }
            apply_weight_outcome(cfg, &mut applied, &mut unsupported);
        }
    }

    FontOutcome {
        applied,
        unsupported,
    }
}
