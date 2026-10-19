use std::collections::BTreeMap;

use ratatui::style::{Color, Style};

use super::relative_luminance;
use crate::theme::{Palette, Theme, builtin_palettes};

fn rgb(c: Option<Color>, what: &str) -> (u8, u8, u8) {
    match c {
        Some(Color::Rgb(r, g, b)) => (r, g, b),
        other => panic!("{what}: expected RGB, got {other:?}"),
    }
}

/// WCAG 2 contrast ratio, >= 1.0.
fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let la = relative_luminance(a.0, a.1, a.2);
    let lb = relative_luminance(b.0, b.1, b.2);
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Euclidean sRGB distance: tints of one luminance differ by hue only.
fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let d = |x: u8, y: u8| (x as f32 - y as f32).powi(2);
    (d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)).sqrt()
}

fn backgrounds(t: &Theme) -> [(&'static str, Style); 6] {
    [
        ("panel", t.panel),
        ("tool_pending_bg", t.tool_pending_bg),
        ("tool_success_bg", t.tool_success_bg),
        ("tool_error_bg", t.tool_error_bg),
        ("status_bg", t.status_bg),
        ("status_bg_alt", t.status_bg_alt),
    ]
}

#[test]
fn every_builtin_palette_keeps_text_readable_on_every_background() {
    for palette in builtin_palettes() {
        let t = Theme::from_palette(palette, true);
        let name = &palette.name;
        let fg = rgb(t.fg.fg, "fg");
        let dim = rgb(t.dim.fg, "dim");
        for (key, style) in backgrounds(&t) {
            assert_eq!(style.fg, None, "{name}: {key} never recolours text");
            let bg = rgb(style.bg, &format!("{name}: {key}"));
            let (cf, cd) = (contrast(fg, bg), contrast(dim, bg));
            assert!(cf >= 4.5, "{name}: fg on {key} is {cf:.2}");
            assert!(cd >= 3.0, "{name}: dim on {key} is {cd:.2}");
        }
    }
}

#[test]
fn every_builtin_palette_keeps_its_backgrounds_apart() {
    for palette in builtin_palettes() {
        let t = Theme::from_palette(palette, true);
        let name = &palette.name;
        let panel = rgb(t.panel.bg, "panel");
        let selected = rgb(t.selected_bg.bg, "selected_bg");
        let surface = rgb(t.surface.bg, "surface");
        // A selected row is drawn over the panel; the user band is not a panel.
        let c = contrast(selected, panel);
        assert!(c >= 1.1, "{name}: selected_bg on panel is {c:.3}");
        let c = contrast(surface, panel);
        assert!(c >= 1.1, "{name}: surface vs panel is {c:.3}");
        // The panel sits further from the text than the selection.
        let lum = |c: (u8, u8, u8)| relative_luminance(c.0, c.1, c.2);
        assert_eq!(lum(panel) > lum(selected), palette.light, "{name}");

        let tools = [
            rgb(t.tool_pending_bg.bg, "pending"),
            rgb(t.tool_success_bg.bg, "success"),
            rgb(t.tool_error_bg.bg, "error"),
        ];
        for (i, a) in tools.iter().enumerate() {
            for b in &tools[i + 1..] {
                let d = distance(*a, *b);
                assert!(
                    d >= 8.0,
                    "{name}: tool tints {a:?} / {b:?} only {d:.1} apart"
                );
            }
        }
        let status = rgb(t.status_bg.bg, "status_bg");
        let alt = rgb(t.status_bg_alt.bg, "status_bg_alt");
        let c = contrast(status, alt);
        assert!(c >= 1.1, "{name}: status shades are {c:.3} apart");
    }
}

#[test]
fn ansi_and_mono_themes_draw_no_backgrounds() {
    let themes = builtin_palettes()
        .iter()
        .map(|p| (p.name.clone(), Theme::from_palette(p, false)))
        .chain([
            ("ansi".to_string(), Theme::ansi()),
            ("mono".to_string(), Theme::mono()),
        ]);
    for (name, t) in themes {
        for (key, style) in backgrounds(&t) {
            assert_eq!(style, Style::default(), "{name}: {key}");
        }
    }
}

fn custom(colors: &[(&str, &str)]) -> Palette {
    Palette {
        name: "custom".to_string(),
        light: false,
        colors: colors
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn explicit_background_keys_override_the_derivation() {
    let keys = [
        ("panel", "#101010"),
        ("tool_pending_bg", "#202020"),
        ("tool_success_bg", "#303030"),
        ("tool_error_bg", "#404040"),
        ("status_bg", "#505050"),
        ("status_bg_alt", "#606060"),
    ];
    let t = Theme::from_palette(&custom(&keys), true);
    for ((key, style), (_, hex)) in backgrounds(&t).into_iter().zip(keys) {
        let v = u8::from_str_radix(&hex[1..3], 16).unwrap();
        assert_eq!(style, Style::default().bg(Color::Rgb(v, v, v)), "{key}");
    }
}

#[test]
fn an_explicit_panel_feeds_the_tokens_built_on_it() {
    let derived = Theme::from_palette(&custom(&[]), true);
    let t = Theme::from_palette(&custom(&[("panel", "#000040")]), true);
    assert_eq!(t.panel, Style::default().bg(Color::Rgb(0, 0, 0x40)));
    assert_ne!(t.status_bg, derived.status_bg);
    assert_ne!(t.tool_success_bg, derived.tool_success_bg);
}

#[test]
fn an_explicit_rgb_background_is_dropped_under_ansi() {
    let t = Theme::from_palette(&custom(&[("panel", "#203040")]), false);
    assert_eq!(t.panel, Style::default());
}
