//! Built-in color palettes.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::theme::Palette;

fn make_palette(name: &str, light: bool, colors: &[(&str, &str)]) -> Palette {
    let mut map = BTreeMap::new();
    for (k, v) in colors {
        map.insert((*k).to_string(), (*v).to_string());
    }
    Palette {
        name: name.to_string(),
        light,
        colors: map,
    }
}

fn init_builtin_palettes() -> Vec<Palette> {
    vec![
        make_palette(
            "pacode-dark",
            false,
            &[
                ("fg", "#c9c7c0"),
                ("dim", "#75776f"),
                ("faint", "#43463f"),
                ("accent", "#d99b4e"),
                ("green", "#96b35d"),
                ("cyan", "#6ea9bd"),
                ("red", "#c8695c"),
                ("violet", "#9a8bc4"),
                ("yellow", "#f5c542"),
                ("bold", "#e7e5dd"),
                ("selected_bg", "#1e241c"),
                ("user_bar", "#6ea9bd"),
                ("bash_command", "#6ea9bd"),
                ("bash_flag", "#9a8bc4"),
                ("bash_string", "#96b35d"),
                ("bash_operator", "#d99b4e"),
                ("bash_variable", "#f5c542"),
                ("bash_arg", "#c9c7c0"),
            ],
        ),
        make_palette(
            "pacode-light",
            true,
            &[
                ("fg", "#2e3440"),
                ("dim", "#616e88"),
                ("faint", "#9aa5b5"),
                ("accent", "#b45309"),
                ("green", "#2f855a"),
                ("cyan", "#0e7490"),
                ("red", "#c53030"),
                ("violet", "#6b46c1"),
                ("yellow", "#b7791f"),
                ("bold", "#1a202c"),
                ("selected_bg", "#e2e8f0"),
                ("user_bar", "#0e7490"),
                ("bash_command", "#0e7490"),
                ("bash_flag", "#6b46c1"),
                ("bash_string", "#2f855a"),
                ("bash_operator", "#b45309"),
                ("bash_variable", "#b7791f"),
                ("bash_arg", "#2e3440"),
            ],
        ),
        make_palette(
            "gruvbox-dark",
            false,
            &[
                ("fg", "#ebdbb2"),
                ("dim", "#928374"),
                ("faint", "#504945"),
                ("accent", "#fe8019"),
                ("green", "#b8bb26"),
                ("cyan", "#8ec07c"),
                ("red", "#fb4934"),
                ("violet", "#d3869b"),
                ("yellow", "#fabd2f"),
                ("bold", "#fbf1c7"),
                ("selected_bg", "#3c3836"),
                ("user_bar", "#8ec07c"),
                ("bash_command", "#8ec07c"),
                ("bash_flag", "#d3869b"),
                ("bash_string", "#b8bb26"),
                ("bash_operator", "#fe8019"),
                ("bash_variable", "#fabd2f"),
                ("bash_arg", "#ebdbb2"),
            ],
        ),
        make_palette(
            "nord",
            false,
            &[
                ("fg", "#eceff4"),
                ("dim", "#d8dee9"),
                ("faint", "#4c566a"),
                ("accent", "#88c0d0"),
                ("green", "#a3be8c"),
                ("cyan", "#8fbcbb"),
                ("red", "#bf616a"),
                ("violet", "#b48ead"),
                ("yellow", "#ebcb8b"),
                ("bold", "#eceff4"),
                ("selected_bg", "#3b4252"),
                ("user_bar", "#81a1c1"),
                ("bash_command", "#88c0d0"),
                ("bash_flag", "#b48ead"),
                ("bash_string", "#a3be8c"),
                ("bash_operator", "#ebcb8b"),
                ("bash_variable", "#81a1c1"),
                ("bash_arg", "#eceff4"),
            ],
        ),
    ]
}

/// The four built-in palettes.
pub fn builtin_palettes() -> &'static [Palette] {
    static PALETTES: LazyLock<Vec<Palette>> = LazyLock::new(init_builtin_palettes);
    &PALETTES
}
