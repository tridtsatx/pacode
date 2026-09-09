use std::collections::HashMap;

use pacode_types::FontConfig;

use super::*;

fn make_env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |k| map.get(k).cloned()
}

#[test]
fn test_detect_from_table() {
    let cases: Vec<(&[(&str, &str)], FontControl)> = vec![
        (&[("TERM", "xterm-kitty")], FontControl::Kitty),
        (
            &[("TERM", "xterm-256color"), ("KITTY_WINDOW_ID", "1")],
            FontControl::Kitty,
        ),
        (&[("KITTY_WINDOW_ID", "42")], FontControl::Kitty),
        (
            &[("TERM", "xterm-kitty"), ("KITTY_WINDOW_ID", "1")],
            FontControl::Kitty,
        ),
        (&[("TERM", "xterm-256color")], FontControl::Xterm),
        (&[("TERM", "xterm")], FontControl::Xterm),
        (&[("TERM", "xterm-direct")], FontControl::Xterm),
        (&[("TERM", "alacritty")], FontControl::Unsupported),
        (&[("TERM", "foot")], FontControl::Unsupported),
        (&[("TERM", "rxvt-unicode")], FontControl::Unsupported),
        (&[("TERM", "screen-256color")], FontControl::Unsupported),
        (&[("TERM", "tmux-256color")], FontControl::Unsupported),
        (&[], FontControl::Unsupported),
    ];

    for (env_vars, expected) in cases {
        let env = make_env(env_vars);
        assert_eq!(
            detect_from(&env),
            expected,
            "failed detect_from for env {env_vars:?}"
        );
    }
}

#[test]
fn test_osc50_byte_sequence() {
    let seq = osc50_sequence("monospace");
    assert_eq!(seq, b"\x1b]50;monospace\x07");

    let seq2 = osc50_sequence("JetBrains Mono");
    assert_eq!(seq2, b"\x1b]50;JetBrains Mono\x07");

    let seq3 = osc50_sequence("-misc-fixed-medium-r-normal--14-130-75-75-c-70-iso8859-1");
    assert_eq!(
        seq3,
        b"\x1b]50;-misc-fixed-medium-r-normal--14-130-75-75-c-70-iso8859-1\x07"
    );
}

#[test]
fn test_empty_font_config_produces_empty_outcome() {
    let empty_cfg = FontConfig::default();

    for control in [
        FontControl::Kitty,
        FontControl::Xterm,
        FontControl::Unsupported,
    ] {
        let mut writer = Vec::new();
        let outcome = apply_with_writer(&empty_cfg, control, &mut writer);
        assert!(outcome.is_empty(), "expected empty outcome for {control:?}");
        assert!(outcome.applied.is_empty());
        assert!(outcome.unsupported.is_empty());
        assert!(writer.is_empty(), "expected no writes for {control:?}");
    }
}

#[test]
fn test_xterm_apply_family_and_size() {
    let cfg = FontConfig {
        family: Some("monospace".to_string()),
        size: Some(12),
        weight: None,
    };
    let mut writer = Vec::new();
    let outcome = apply_with_writer(&cfg, FontControl::Xterm, &mut writer);
    assert_eq!(writer, b"\x1b]50;monospace\x07");
    assert_eq!(outcome.applied, vec!["family".to_string()]);
    assert_eq!(outcome.unsupported, vec!["size".to_string()]);

    let cfg_carried = FontConfig {
        family: Some("monospace 12".to_string()),
        size: Some(12),
        weight: None,
    };
    let mut writer2 = Vec::new();
    let outcome2 = apply_with_writer(&cfg_carried, FontControl::Xterm, &mut writer2);
    assert_eq!(writer2, b"\x1b]50;monospace 12\x07");
    assert_eq!(
        outcome2.applied,
        vec!["family".to_string(), "size".to_string()]
    );
    assert!(outcome2.unsupported.is_empty());
}

#[test]
fn test_unsupported_terminal_reports_all_unsupported() {
    let cfg = FontConfig {
        family: Some("monospace".to_string()),
        size: Some(14),
        weight: Some("bold".to_string()),
    };
    let mut writer = Vec::new();
    let outcome = apply_with_writer(&cfg, FontControl::Unsupported, &mut writer);
    assert!(writer.is_empty());
    assert_eq!(outcome.applied, vec!["weight".to_string()]);
    assert_eq!(
        outcome.unsupported,
        vec!["family".to_string(), "size".to_string()]
    );
}

#[test]
fn test_kitty_family_unsupported() {
    let cfg = FontConfig {
        family: Some("monospace".to_string()),
        size: None,
        weight: None,
    };
    let mut writer = Vec::new();
    let outcome = apply_with_writer(&cfg, FontControl::Kitty, &mut writer);
    assert!(writer.is_empty());
    assert!(outcome.applied.is_empty());
    assert_eq!(outcome.unsupported, vec!["family".to_string()]);
}

#[test]
fn test_weight_applied_internally() {
    let cfg_bold = FontConfig {
        family: None,
        size: None,
        weight: Some("bold".to_string()),
    };
    let mut writer = Vec::new();
    let outcome = apply_with_writer(&cfg_bold, FontControl::Kitty, &mut writer);
    assert_eq!(outcome.applied, vec!["weight".to_string()]);
    assert!(outcome.unsupported.is_empty());
    assert!(writer.is_empty());

    let style = base_style(&cfg_bold);
    assert!(style.add_modifier.contains(ratatui::style::Modifier::BOLD));

    let cfg_normal = FontConfig {
        family: None,
        size: None,
        weight: Some("normal".to_string()),
    };
    let outcome_norm = apply_with_writer(&cfg_normal, FontControl::Kitty, &mut writer);
    assert_eq!(outcome_norm.applied, vec!["weight".to_string()]);
    assert!(outcome_norm.unsupported.is_empty());

    let cfg_invalid = FontConfig {
        family: None,
        size: None,
        weight: Some("heavy".to_string()),
    };
    let outcome_inv = apply_with_writer(&cfg_invalid, FontControl::Kitty, &mut writer);
    assert!(outcome_inv.applied.is_empty());
    assert_eq!(outcome_inv.unsupported, vec!["weight".to_string()]);
}

#[test]
fn test_family_carries_size() {
    assert!(family_carries_size("monospace 12"));
    assert!(family_carries_size("DejaVu Sans Mono:size=14"));
    assert!(family_carries_size("xft:Liberation Mono:pixelsize=16"));
    assert!(family_carries_size("fixed-14"));
    assert!(family_carries_size(
        "-misc-fixed-medium-r-normal--14-130-75-75-c-70-iso8859-1"
    ));

    assert!(!family_carries_size("monospace"));
    assert!(!family_carries_size("JetBrains Mono"));
    assert!(!family_carries_size(""));
}

#[test]
fn test_apply_weight_to_buffer() {
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 2));
    buffer.set_string(0, 0, "test", Style::default());

    let cfg_bold = FontConfig {
        family: None,
        size: None,
        weight: Some("bold".to_string()),
    };
    apply_weight_to_buffer(&mut buffer, &cfg_bold);

    let cell = buffer.cell((0, 0)).unwrap();
    assert!(
        cell.style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
}
