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

/// Under ANSI-16 a palette's quiet greys must stay grey and its status
/// colours must keep their hue: nearest-by-distance turned `faint` yellow and
/// green/cyan grey, so every badge drew as a grey plaque.
#[test]
fn ansi_fallback_keeps_greys_grey_and_hues_hued() {
    let grey = |c: Option<Color>| {
        matches!(
            c,
            Some(Color::Black | Color::DarkGray | Color::Gray | Color::White)
        )
    };
    for palette in builtin_palettes() {
        let t = Theme::from_palette(palette, false);
        let name = &palette.name;
        assert!(grey(t.faint.fg), "{name}: faint {:?}", t.faint.fg);
        assert!(grey(t.dim.fg), "{name}: dim {:?}", t.dim.fg);
        // Roles named after a colour are that colour, as in `Theme::ansi()`.
        assert_eq!(t.green.fg, Some(Color::Green), "{name}");
        assert_eq!(t.red.fg, Some(Color::Red), "{name}");
        assert_eq!(t.cyan.fg, Some(Color::Cyan), "{name}");
        assert_eq!(t.yellow.fg, Some(Color::Yellow), "{name}");
        assert_eq!(t.violet.fg, Some(Color::Magenta), "{name}");
        // A badge is the hue reversed, never a grey plaque.
        let badge = t.badge(t.green);
        assert!(badge.add_modifier.contains(Modifier::REVERSED), "{name}");
        assert!(!grey(badge.fg), "{name}: badge {:?}", badge.fg);
    }
    // Nearest-by-distance put this dark grey on Yellow (128,128,0).
    assert_eq!(rgb_to_ansi16(0x43, 0x46, 0x3f), Color::DarkGray);
    assert_eq!(rgb_to_ansi16(0x6e, 0xa9, 0xbd), Color::Cyan);
}

/// An accent is never drawn in the error colour: oranges used to fall in a
/// ±30° red sector (pacode-light `#b45309` → Red, gruvbox `#fe8019` →
/// LightRed).
#[test]
fn ansi_accent_is_never_the_error_red() {
    for palette in builtin_palettes() {
        let t = Theme::from_palette(palette, false);
        assert_ne!(t.accent.fg, t.red.fg, "{}", palette.name);
        assert_eq!(t.accent.fg, Theme::ansi().accent.fg, "{}", palette.name);
    }
    // Free oranges (no role to borrow) land on yellow, not red.
    assert_eq!(ansi_hue(0xb4, 0x53, 0x09), Color::Yellow);
    assert_eq!(ansi_hue(0xfe, 0x80, 0x19), Color::LightYellow);
    assert_eq!(ansi_hue(0xc8, 0x10, 0x10), Color::Red);
}

/// The ANSI colour a palette role resolves to (`selected_bg` by background).
fn role_color(t: &Theme, key: &str) -> Option<Color> {
    match key {
        "fg" => t.fg.fg,
        "dim" => t.dim.fg,
        "faint" => t.faint.fg,
        "accent" => t.accent.fg,
        "green" => t.green.fg,
        "cyan" => t.cyan.fg,
        "red" => t.red.fg,
        "violet" => t.violet.fg,
        "yellow" => t.yellow.fg,
        "bold" => t.bold.fg,
        "selected_bg" => t.selected_bg.bg,
        "user_bar" => t.user_bar.fg,
        "bash_command" => t.bash_command.fg,
        "bash_flag" => t.bash_flag.fg,
        "bash_string" => t.bash_string.fg,
        "bash_operator" => t.bash_operator.fg,
        "bash_variable" => t.bash_variable.fg,
        "bash_arg" => t.bash_arg.fg,
        other => panic!("unmapped role {other}"),
    }
}

/// Syntax borrows status colours by hex (pacode-dark `bash_string` is the
/// green hex); under ANSI the borrowed colour must stay the same colour —
/// it used to land on Yellow while green was Green.
#[test]
fn ansi_same_hex_means_same_colour_within_a_palette() {
    for palette in builtin_palettes() {
        let t = Theme::from_palette(palette, false);
        let roles: Vec<(&String, &String)> = palette.colors.iter().collect();
        for (i, (a, hex_a)) in roles.iter().enumerate() {
            for (b, hex_b) in &roles[i + 1..] {
                if hex_a.eq_ignore_ascii_case(hex_b) {
                    assert_eq!(
                        role_color(&t, a),
                        role_color(&t, b),
                        "{}: {a} and {b} share {hex_a}",
                        palette.name
                    );
                }
            }
        }
    }
}

/// fg / dim / faint stay three levels, as in `Theme::ansi()`, and a
/// selection never paints the faint grey under faint text.
#[test]
fn ansi_greys_keep_their_hierarchy_and_selection_stays_readable() {
    for palette in builtin_palettes() {
        let t = Theme::from_palette(palette, false);
        let name = &palette.name;
        let levels = [t.fg.fg, t.dim.fg, t.faint.fg];
        assert_ne!(levels[0], levels[1], "{name}: fg == dim");
        assert_ne!(levels[1], levels[2], "{name}: dim == faint");
        assert_ne!(levels[0], levels[2], "{name}: fg == faint");
        assert_eq!(t.dim.fg, Some(Color::Gray), "{name}");
        assert_eq!(t.faint.fg, Some(Color::DarkGray), "{name}");
        assert_ne!(t.selected_bg.bg, t.faint.fg, "{name}");
        assert!(
            t.selected_bg.add_modifier.contains(Modifier::REVERSED),
            "{name}: an ANSI selection is reversed video"
        );
    }
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
    colors.insert("user_bar".to_string(), "#ff0000".to_string());
    colors.insert("bash_flag".to_string(), "#00ff00".to_string());
    colors.insert("red".to_string(), "#ff8080".to_string());
    colors.insert("fg".to_string(), "#ff0000".to_string());
    colors.insert("accent".to_string(), "magenta".to_string());
    let pal = Palette {
        name: "test".to_string(),
        light: false,
        colors,
    };

    let theme = Theme::from_palette(&pal, false);
    // A free hue role keeps its hue; a role named after a colour is that
    // colour; a grey role keeps the ANSI grey ladder; a named colour is kept.
    assert_eq!(theme.user_bar, Style::default().fg(Color::LightRed));
    assert_eq!(theme.bash_flag, Style::default().fg(Color::LightGreen));
    assert_eq!(theme.red, Style::default().fg(Color::Red));
    assert_eq!(theme.fg, Theme::ansi().fg);
    assert_eq!(theme.accent, Style::default().fg(Color::Magenta));
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
