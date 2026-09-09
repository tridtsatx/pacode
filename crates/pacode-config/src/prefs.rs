//! User preferences persisted to `<paths.state_dir>/prefs.toml`.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use pacode_types::{Effort, Mode};

use crate::paths::Paths;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
}

pub fn prefs_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("prefs.toml")
}

pub fn load_prefs(paths: &Paths) -> Prefs {
    let path = prefs_file(paths);
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => Prefs::default(),
    }
}

pub fn save_prefs(paths: &Paths, prefs: &Prefs) -> std::io::Result<()> {
    paths.ensure_dirs()?;
    let path = prefs_file(paths);
    let text = toml::to_string_pretty(prefs)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text)
}
