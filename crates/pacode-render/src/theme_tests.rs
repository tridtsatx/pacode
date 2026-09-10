use super::*;

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
