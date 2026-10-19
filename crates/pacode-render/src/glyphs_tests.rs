use super::*;

#[test]
fn powerline_separators_only_when_the_font_has_them() {
    let plain = Glyphs::new(false);
    assert_eq!((plain.seg_sep_left, plain.seg_sep_right), ("", ""));

    let nerd = Glyphs::new(false).with_powerline(true);
    assert_eq!(nerd.seg_sep_left, "\u{e0b0}");
    assert_eq!(nerd.seg_sep_right, "\u{e0b2}");

    // `ascii_only` wins over a Nerd Font.
    let ascii = Glyphs::new(true).with_powerline(true);
    assert_eq!((ascii.seg_sep_left, ascii.seg_sep_right), ("", ""));
}

#[test]
fn nerd_and_powerline_font_names_are_recognised() {
    for family in [
        "JetBrainsMono Nerd Font",
        "Hack Nerd Font Mono",
        "JetBrainsMono NF",
        "CaskaydiaCove NFM",
        "FiraCode NFP",
        "Meslo LG S for Powerline",
    ] {
        assert!(font_has_powerline(family), "{family}");
    }
    for family in ["", "JetBrains Mono", "Iosevka Term", "Inferno", "Fira Code"] {
        assert!(!font_has_powerline(family), "{family}");
    }
}
