use std::collections::BTreeMap;
use tempfile::TempDir;

use super::*;
use crate::Paths;

#[test]
fn test_load_builtin_theme() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::under(tmp.path());
    let cfg = ThemeConfig {
        name: "nord".to_string(),
        overrides: BTreeMap::new(),
    };

    let (palette, warnings) = load_theme(&paths, &cfg);
    assert_eq!(palette.name, "nord");
    assert!(warnings.is_empty());
}

#[test]
fn test_unknown_theme_warning_and_fallback() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::under(tmp.path());
    let cfg = ThemeConfig {
        name: "nonexistent-theme".to_string(),
        overrides: BTreeMap::new(),
    };

    let (palette, warnings) = load_theme(&paths, &cfg);
    assert_eq!(palette.name, "pacode-dark");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("unknown theme 'nonexistent-theme'"));
    assert!(warnings[0].contains("falling back to 'pacode-dark'"));
}

#[test]
fn test_theme_override_application() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::under(tmp.path());
    let mut overrides = BTreeMap::new();
    overrides.insert("accent".to_string(), "#ff5500".to_string());
    overrides.insert("invalid".to_string(), "not-a-valid-color".to_string());

    let cfg = ThemeConfig {
        name: "pacode-dark".to_string(),
        overrides,
    };

    let (palette, warnings) = load_theme(&paths, &cfg);
    assert_eq!(palette.colors.get("accent").unwrap(), "#ff5500");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("unparsable colour 'not-a-valid-color'"));
}

#[test]
fn test_write_and_load_user_theme() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::under(tmp.path());

    let mut colors = BTreeMap::new();
    colors.insert("accent".to_string(), "#123456".to_string());
    let custom = Palette {
        name: "my-custom".to_string(),
        light: false,
        colors,
    };

    let written_path = write_theme(&paths, &custom).unwrap();
    assert!(written_path.exists());

    let names = user_theme_names(&paths);
    assert_eq!(names, vec!["my-custom".to_string()]);

    let cfg = ThemeConfig {
        name: "my-custom".to_string(),
        overrides: BTreeMap::new(),
    };
    let (loaded, warnings) = load_theme(&paths, &cfg);
    assert_eq!(loaded.name, "my-custom");
    assert_eq!(loaded.colors.get("accent").unwrap(), "#123456");
    assert!(warnings.is_empty());
}

#[test]
fn test_next_custom_theme_name() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths::under(tmp.path());

    assert_eq!(next_custom_theme_name(&paths), "custom");

    let pal1 = Palette {
        name: "custom".to_string(),
        light: false,
        colors: BTreeMap::new(),
    };
    write_theme(&paths, &pal1).unwrap();
    assert_eq!(next_custom_theme_name(&paths), "custom-2");

    let pal2 = Palette {
        name: "custom-2".to_string(),
        light: false,
        colors: BTreeMap::new(),
    };
    write_theme(&paths, &pal2).unwrap();
    assert_eq!(next_custom_theme_name(&paths), "custom-3");
}
