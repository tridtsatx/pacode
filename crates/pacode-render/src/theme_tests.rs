use super::*;
use crate::RenderOptions;

#[test]
fn test_parse_color_hex() {
    assert_eq!(parse_color("#c9c7c0"), Some(Color::Rgb(0xc9, 0xc7, 0xc0)));
    assert_eq!(parse_color("#1e241c"), Some(Color::Rgb(0x1e, 0x24, 0x1c)));
    assert_eq!(parse_color("#fff"), Some(Color::Rgb(255, 255, 255)));
    assert_eq!(parse_color("#000"), Some(Color::Rgb(0, 0, 0)));
}

#[test]
fn test_parse_color_named() {
    assert_eq!(parse_color("red"), Some(Color::Red));
    assert_eq!(parse_color("RED"), Some(Color::Red));
    assert_eq!(parse_color("DarkGray"), Some(Color::DarkGray));
    assert_eq!(parse_color("dark-gray"), Some(Color::DarkGray));
    assert_eq!(parse_color("dark_gray"), Some(Color::DarkGray));
    assert_eq!(parse_color("cyan"), Some(Color::Cyan));
    assert_eq!(parse_color("magenta"), Some(Color::Magenta));
    assert_eq!(parse_color("purple"), Some(Color::Magenta));
    assert_eq!(parse_color("white"), Some(Color::White));
    assert_eq!(parse_color("reset"), Some(Color::Reset));
}

#[test]
fn test_parse_color_index() {
    assert_eq!(parse_color("0"), Some(Color::Indexed(0)));
    assert_eq!(parse_color("9"), Some(Color::Indexed(9)));
    assert_eq!(parse_color("255"), Some(Color::Indexed(255)));
    assert_eq!(parse_color("256"), None);
}

#[test]
fn test_parse_color_garbage() {
    assert_eq!(parse_color(""), None);
    assert_eq!(parse_color("   "), None);
    assert_eq!(parse_color("not-a-color"), None);
    assert_eq!(parse_color("#xyz"), None);
    assert_eq!(parse_color("#1234"), None);
    assert_eq!(parse_color("#1234567"), None);
}

#[test]
fn mono_render_options_replace_all_theme_colors() {
    let opts = RenderOptions::with_theme_color(80, false, Theme::truecolor(), ColorMode::Mono);
    assert_eq!(opts.theme, Theme::mono());
    assert_eq!(opts.theme.accent.fg, None);
    assert_eq!(opts.theme.selected_bg.bg, None);
}

#[test]
fn test_rgb_to_ansi16_fallback() {
    assert_eq!(rgb_to_ansi16(0, 0, 0), Color::Black);
    assert_eq!(rgb_to_ansi16(255, 0, 0), Color::LightRed);
    assert_eq!(rgb_to_ansi16(128, 0, 0), Color::Red);
    assert_eq!(rgb_to_ansi16(0, 255, 0), Color::LightGreen);
    assert_eq!(rgb_to_ansi16(0, 128, 0), Color::Green);
    assert_eq!(rgb_to_ansi16(0, 0, 255), Color::LightBlue);
    assert_eq!(rgb_to_ansi16(0, 0, 128), Color::Blue);
    assert_eq!(rgb_to_ansi16(255, 255, 255), Color::White);
    assert_eq!(rgb_to_ansi16(128, 128, 128), Color::DarkGray);
    assert_eq!(rgb_to_ansi16(192, 192, 192), Color::Gray);
}

#[test]
fn test_pacode_dark_equals_theme_default_field_for_field() {
    let builtins = builtin_palettes();
    let pacode_dark = builtins.iter().find(|p| p.name == "pacode-dark").unwrap();

    let from_pal = Theme::from_palette(pacode_dark, true);
    let previous = Theme::truecolor();

    assert_eq!(from_pal.fg, previous.fg);
    assert_eq!(from_pal.dim, previous.dim);
    assert_eq!(from_pal.faint, previous.faint);
    assert_eq!(from_pal.accent, previous.accent);
    assert_eq!(from_pal.green, previous.green);
    assert_eq!(from_pal.cyan, previous.cyan);
    assert_eq!(from_pal.red, previous.red);
    assert_eq!(from_pal.violet, previous.violet);
    assert_eq!(from_pal.yellow, previous.yellow);
    assert_eq!(from_pal.bold, previous.bold);
    assert_eq!(from_pal.selected_bg, previous.selected_bg);
    assert_eq!(from_pal.user_bar, previous.user_bar);
    assert_eq!(from_pal.bash_command, previous.bash_command);
    assert_eq!(from_pal.bash_flag, previous.bash_flag);
    assert_eq!(from_pal.bash_string, previous.bash_string);
    assert_eq!(from_pal.bash_operator, previous.bash_operator);
    assert_eq!(from_pal.bash_variable, previous.bash_variable);
    assert_eq!(from_pal.bash_arg, previous.bash_arg);
    assert_eq!(from_pal, previous);
}

#[test]
fn test_partial_palette_fallback() {
    let mut colors = BTreeMap::new();
    colors.insert("accent".to_string(), "#ff0000".to_string());
    let partial = Palette {
        name: "partial".to_string(),
        light: false,
        colors,
    };

    let theme = Theme::from_palette(&partial, true);
    let default_theme = Theme::truecolor();

    assert_eq!(theme.accent, Style::default().fg(Color::Rgb(255, 0, 0)));
    assert_eq!(theme.fg, default_theme.fg);
    assert_eq!(theme.dim, default_theme.dim);
    assert_eq!(theme.green, default_theme.green);
    assert_eq!(theme.bold, default_theme.bold);
    assert_eq!(theme.selected_bg, default_theme.selected_bg);
}

#[test]
fn test_non_truecolor_converts_hex_to_ansi16() {
    let mut colors = BTreeMap::new();
    colors.insert("fg".to_string(), "#ff0000".to_string());
    let pal = Palette {
        name: "test".to_string(),
        light: false,
        colors,
    };

    let theme = Theme::from_palette(&pal, false);
    assert_eq!(theme.fg, Style::default().fg(Color::LightRed));
}

#[test]
fn test_spinner_cycles_and_falls_back_to_ascii() {
    let uni = Glyphs::new(false);
    assert_eq!(uni.spinner(0), "⠋");
    assert_eq!(uni.spinner(1), "⠙");
    assert_eq!(uni.spinner(9), "⠏");
    assert_eq!(uni.spinner(10), "⠋");
    // The whole cycle is drawn, not just the ends.
    let frames: Vec<&str> = (0..10).map(|f| uni.spinner(f)).collect();
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(*f, uni.spinner(i as u64));
    }
    assert!(frames.iter().all(|f| f.chars().count() == 1));

    let ascii = Glyphs::new(true);
    assert_eq!(ascii.spinner(0), "-");
    assert_eq!(ascii.spinner(1), "\\");
    assert_eq!(ascii.spinner(2), "|");
    assert_eq!(ascii.spinner(3), "/");
    assert_eq!(ascii.spinner(4), "-");
    assert!(ascii.spinner(u64::MAX).chars().count() == 1);
}

#[test]
fn test_builtins_palettes_exist() {
    let builtins = builtin_palettes();
    assert_eq!(builtins.len(), 4);
    assert!(builtins.iter().any(|p| p.name == "pacode-dark" && !p.light));
    assert!(builtins.iter().any(|p| p.name == "pacode-light" && p.light));
    assert!(
        builtins
            .iter()
            .any(|p| p.name == "gruvbox-dark" && !p.light)
    );
    assert!(builtins.iter().any(|p| p.name == "nord" && !p.light));
}

#[test]
fn every_builtin_palette_derives_a_distinct_band_and_border_in_truecolor() {
    for palette in builtin_palettes() {
        let theme = Theme::from_palette(palette, true);
        let name = &palette.name;
        assert!(
            matches!(theme.surface.bg, Some(Color::Rgb(..))),
            "{name}: truecolor surface must be an RGB background"
        );
        // A selection painted over a band must stay visible.
        assert_ne!(theme.surface.bg, theme.selected_bg.bg, "{name}");
        assert_eq!(
            theme.surface.fg, None,
            "{name}: a band never recolours text"
        );
        assert!(
            matches!(theme.border.fg, Some(Color::Rgb(..))),
            "{name}: truecolor border must be an RGB foreground"
        );
        assert_ne!(theme.border.fg, theme.fg.fg, "{name}");
    }
}

#[test]
fn ansi_and_mono_themes_draw_no_band() {
    for palette in builtin_palettes() {
        let ansi = Theme::from_palette(palette, false);
        assert_eq!(ansi.surface, Style::default(), "{}", palette.name);
        assert!(
            !matches!(ansi.border.fg, Some(Color::Rgb(..)) | None),
            "{}: an ANSI border is a named colour",
            palette.name
        );
    }
    assert_eq!(Theme::ansi().surface, Style::default());
    assert_eq!(Theme::mono().surface, Style::default());
    assert_eq!(Theme::mono().border, Style::default());
}

#[test]
fn explicit_surface_and_border_keys_override_the_derivation() {
    let mut colors = BTreeMap::new();
    colors.insert("surface".to_string(), "#102030".to_string());
    colors.insert("border".to_string(), "#405060".to_string());
    let pal = Palette {
        name: "custom".to_string(),
        light: false,
        colors,
    };
    let theme = Theme::from_palette(&pal, true);
    assert_eq!(
        theme.surface,
        Style::default().bg(Color::Rgb(0x10, 0x20, 0x30))
    );
    assert_eq!(
        theme.border,
        Style::default().fg(Color::Rgb(0x40, 0x50, 0x60))
    );
}

#[test]
fn badge_degrades_with_the_colour_mode() {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    // Truecolor: the tone becomes the background, the foreground contrasts.
    let tc = Theme::truecolor();
    let badge = tc.badge(tc.green);
    assert_eq!(badge.bg, tc.green.fg);
    assert!(badge.add_modifier.contains(Modifier::BOLD));
    assert!(!badge.add_modifier.contains(Modifier::REVERSED));
    assert_ne!(badge.fg, badge.bg);
    // ANSI-16: the terminal swaps the named colour in.
    let ansi = Theme::ansi();
    assert_eq!(
        ansi.badge(ansi.red),
        bold.fg(Color::Red).add_modifier(Modifier::REVERSED)
    );
    // Mono: nothing but weight — the glyph and the text are the marker.
    let mono = Theme::mono();
    assert_eq!(mono.badge(mono.red), bold);
}

#[test]
fn badge_foreground_contrasts_with_light_and_dark_tones() {
    let theme = Theme::truecolor();
    let on_light = theme.badge(Style::default().fg(Color::Rgb(0xf5, 0xc5, 0x42)));
    let on_dark = theme.badge(Style::default().fg(Color::Rgb(0x1a, 0x20, 0x2c)));
    assert_eq!(on_light.fg, Some(BADGE_DARK));
    assert_eq!(on_dark.fg, Some(BADGE_LIGHT));
    for palette in builtin_palettes() {
        let t = Theme::from_palette(palette, true);
        for tone in [t.accent, t.green, t.red, t.yellow, t.cyan, t.violet, t.dim] {
            let b = t.badge(tone);
            assert!(
                b.bg.is_some() && b.fg.is_some() && b.fg != b.bg,
                "{}",
                palette.name
            );
        }
    }
}
