//! Theme loading and management.
//!
//! Resolves theme names against built-ins and `<config dir>/themes/<name>.toml`,
//! applies overrides, lists user themes, and writes custom palettes to disk.

use std::path::PathBuf;

use pacode_render::Palette;
use pacode_types::ThemeConfig;

use crate::Paths;

/// Resolve `cfg.name` against built-in palettes first, then `<config dir>/themes/<name>.toml`.
/// Applies `cfg.overrides` last. Returns human-readable warnings for unknown theme names or
/// unparsable colours instead of failing.
pub fn load_theme(paths: &Paths, cfg: &ThemeConfig) -> (Palette, Vec<String>) {
    let mut warnings = Vec::new();
    let builtins = pacode_render::builtin_palettes();

    let mut palette = if let Some(p) = builtins.iter().find(|p| p.name == cfg.name) {
        p.clone()
    } else {
        let theme_dir = theme_dir(paths);
        let theme_file = theme_dir.join(format!("{}.toml", cfg.name));

        if theme_file.exists() {
            match std::fs::read_to_string(&theme_file) {
                Ok(text) => match toml::from_str::<Palette>(&text) {
                    Ok(p) => p,
                    Err(e) => {
                        let path_display = theme_file.display();
                        warnings.push(format!("failed to parse theme file {path_display}: {e}"));
                        builtins[0].clone()
                    }
                },
                Err(e) => {
                    let path_display = theme_file.display();
                    warnings.push(format!("failed to read theme file {path_display}: {e}"));
                    builtins[0].clone()
                }
            }
        } else {
            let name = &cfg.name;
            warnings.push(format!(
                "unknown theme '{name}', falling back to 'pacode-dark'"
            ));
            builtins[0].clone()
        }
    };

    for (key, val) in &cfg.overrides {
        if pacode_render::parse_color(val).is_some() {
            palette.colors.insert(key.clone(), val.clone());
        } else {
            warnings.push(format!("unparsable colour '{val}' for override '{key}'"));
        }
    }

    (palette, warnings)
}

/// Directory for user themes: `<config dir>/themes`.
fn theme_dir(paths: &Paths) -> PathBuf {
    paths
        .config_file
        .parent()
        .map(|p| p.join("themes"))
        .unwrap_or_else(|| PathBuf::from("themes"))
}

/// Names of user-defined themes found in `<config dir>/themes/*.toml` (sorted).
pub fn user_theme_names(paths: &Paths) -> Vec<String> {
    let dir = theme_dir(paths);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names
}

/// Write a palette to `<config dir>/themes/<name>.toml`.
pub fn write_theme(paths: &Paths, palette: &Palette) -> std::io::Result<PathBuf> {
    let dir = theme_dir(paths);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", palette.name));
    let serialized = toml::to_string_pretty(palette)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, serialized)?;
    Ok(path)
}

/// Return the next available custom theme name: `custom`, `custom-2`, `custom-3`, etc.
pub fn next_custom_theme_name(paths: &Paths) -> String {
    let dir = theme_dir(paths);
    let base = dir.join("custom.toml");
    if !base.exists() {
        return "custom".to_string();
    }
    let mut i = 2;
    loop {
        let name = format!("custom-{i}");
        let candidate = dir.join(format!("{name}.toml"));
        if !candidate.exists() {
            return name;
        }
        i += 1;
    }
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod theme_tests;
