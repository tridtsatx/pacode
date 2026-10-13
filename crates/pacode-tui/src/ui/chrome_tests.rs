use pacode_render::ColorMode;

use super::*;

fn opts(ascii: bool) -> RenderOptions {
    RenderOptions::with_theme_color(40, ascii, Theme::truecolor(), ColorMode::Truecolor)
}

#[test]
fn a_badge_is_padded_and_takes_the_tone_as_its_plaque() {
    let opts = opts(false);
    let badge = badge("BYPASS", opts.theme.red, &opts);
    assert_eq!(badge.content, " BYPASS ");
    assert_eq!(badge.style, opts.theme.badge(opts.theme.red));
}

#[test]
fn sections_are_uppercase_and_bold() {
    let opts = opts(false);
    let span = section("Recent", &opts);
    assert_eq!(span.content, "RECENT");
    assert!(span.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn the_border_set_is_rounded_or_ascii() {
    assert_eq!(border_set(&opts(false)), border::ROUNDED);
    assert_eq!(border_set(&opts(true)).top_left, "+");
}

#[test]
fn the_mode_tone_follows_the_permission_mode() {
    let theme = Theme::truecolor();
    assert_eq!(mode_tone(Mode::Bypass, &theme), theme.red);
    assert_eq!(mode_tone(Mode::Plan, &theme), theme.yellow);
    assert_eq!(mode_tone(Mode::Build, &theme), theme.cyan);
    assert_eq!(mode_tone(Mode::Auto, &theme), theme.cyan);
}
