use pacode_types::Config;

use super::*;

/// The arrows are drawn when the flag or a known font family says the glyphs
/// exist; `ascii_only` wins over both.
#[test]
fn glyphs_for_truth_table() {
    let cases = [
        // (ascii_only, ui.powerline, font.family, arrows)
        (false, false, None, false),
        (false, true, None, true),
        (false, false, Some("JetBrains Mono"), false),
        (false, false, Some("JetBrainsMono Nerd Font"), true),
        (false, true, Some("JetBrains Mono"), true),
        (true, true, None, false),
        (true, false, Some("JetBrainsMono Nerd Font"), false),
    ];
    for (ascii, powerline, family, arrows) in cases {
        let mut config = Config::default();
        config.ui.ascii_only = ascii;
        config.ui.powerline = powerline;
        config.font.family = family.map(str::to_string);
        let glyphs = glyphs_for(&config);
        assert_eq!(
            !glyphs.seg_sep_left.is_empty(),
            arrows,
            "{ascii} {powerline} {family:?}"
        );
        assert_eq!(
            glyphs.seg_sep_left.is_empty(),
            glyphs.seg_sep_right.is_empty()
        );
    }
}

/// Toggling `ui.powerline` at runtime takes effect on the next draw, as
/// `ui.ascii_only` does: the render context rebuilds and the epoch moves.
#[test]
fn toggling_powerline_takes_effect_without_restart() {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 80, 24);
    state.sync_render_context();
    let epoch = state.render_epoch;
    assert!(state.render_options(80).glyphs.seg_sep_left.is_empty());

    state.config.ui.powerline = true;
    state.sync_render_context();
    assert_ne!(state.render_epoch, epoch);
    assert_eq!(state.render_options(80).glyphs.seg_sep_left, "\u{e0b0}");
    assert_eq!(state.render_context.glyphs.seg_sep_right, "\u{e0b2}");
}
