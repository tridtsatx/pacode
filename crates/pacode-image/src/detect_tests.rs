use std::collections::HashMap;

use super::*;

fn make_env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |k| map.get(k).cloned()
}

#[test]
fn test_detect_kitty_term() {
    let env = make_env(&[("TERM", "xterm-kitty")]);
    assert_eq!(detect_from(&env), GraphicsProtocol::Kitty);
}

#[test]
fn test_detect_kitty_window_id() {
    let env = make_env(&[("TERM", "xterm-256color"), ("KITTY_WINDOW_ID", "1")]);
    assert_eq!(detect_from(&env), GraphicsProtocol::Kitty);
}

#[test]
fn test_detect_ghostty() {
    let env = make_env(&[("TERM_PROGRAM", "ghostty")]);
    assert_eq!(detect_from(&env), GraphicsProtocol::Kitty);
}

#[test]
fn test_detect_wezterm() {
    let env = make_env(&[("TERM_PROGRAM", "WezTerm")]);
    assert_eq!(detect_from(&env), GraphicsProtocol::Kitty);
}

#[test]
fn test_detect_iterm2() {
    let env = make_env(&[("TERM_PROGRAM", "iTerm.app")]);
    assert_eq!(detect_from(&env), GraphicsProtocol::Iterm2);
}

#[test]
fn test_detect_sixel_foot_falls_back_to_halfblocks() {
    let env = make_env(&[("TERM", "foot")]);
    assert!(is_sixel_env(&env));
    assert_eq!(detect_from(&env), GraphicsProtocol::HalfBlocks);

    let env_extra = make_env(&[("TERM", "foot-extra")]);
    assert!(is_sixel_env(&env_extra));
    assert_eq!(detect_from(&env_extra), GraphicsProtocol::HalfBlocks);
}

#[test]
fn test_detect_sixel_mlterm_falls_back_to_halfblocks() {
    let env = make_env(&[("TERM", "mlterm")]);
    assert!(is_sixel_env(&env));
    assert_eq!(detect_from(&env), GraphicsProtocol::HalfBlocks);
}

#[test]
fn test_detect_sixel_xterm_colorterm_sixel() {
    let env = make_env(&[("TERM", "xterm-256color"), ("COLORTERM", "sixel")]);
    assert!(is_sixel_env(&env));
    assert_eq!(detect_from(&env), GraphicsProtocol::HalfBlocks);

    // xterm without COLORTERM=sixel is not sixel
    let env_no_sixel = make_env(&[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]);
    assert!(!is_sixel_env(&env_no_sixel));
    assert_eq!(detect_from(&env_no_sixel), GraphicsProtocol::HalfBlocks);
}

#[test]
fn test_detect_fallback_halfblocks() {
    let empty_env = make_env(&[]);
    assert_eq!(detect_from(&empty_env), GraphicsProtocol::HalfBlocks);

    let alacritty_env = make_env(&[("TERM", "alacritty")]);
    assert_eq!(detect_from(&alacritty_env), GraphicsProtocol::HalfBlocks);

    let dumb_env = make_env(&[("TERM", "dumb")]);
    assert_eq!(detect_from(&dumb_env), GraphicsProtocol::HalfBlocks);
}

#[test]
fn test_priority_order() {
    // Kitty takes priority over sixel or iterm
    let env = make_env(&[("TERM", "xterm-kitty"), ("TERM_PROGRAM", "iTerm.app")]);
    assert_eq!(detect_from(&env), GraphicsProtocol::Kitty);

    let env_ghostty_foot = make_env(&[("TERM_PROGRAM", "ghostty"), ("TERM", "foot")]);
    assert_eq!(detect_from(&env_ghostty_foot), GraphicsProtocol::Kitty);
}
