//! Rule sets: layering (built-in < global < project), disable list, lookup by file,
//! and the fingerprint used to invalidate a cached set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::builtin::builtin_rules;
use crate::parse::parse_rule;
use crate::rule::{Mode, Origin, Rule, RuleError};

/// Max `*.md` files read per rules directory.
pub const MAX_RULE_FILES_PER_DIR: usize = 64;
/// Max size of one rule file; bigger files are reported as errors.
pub const MAX_RULE_FILE_BYTES: u64 = 64 * 1024;

/// Which rules a scan evaluates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeFilter {
    /// Only `mode: stream` rules (evaluated while tool arguments stream in).
    StreamOnly,
    /// Every rule (after the write; `mode: stream` rules are the fallback here).
    AfterWrite,
}

#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    /// `[rules] builtin`.
    pub builtin: bool,
    /// `[rules] disable`: rule names removed from every layer.
    pub disable: Vec<String>,
    /// `<project>/.pacode/rules`.
    pub project_dir: Option<PathBuf>,
    /// `<config>/rules`.
    pub global_dir: Option<PathBuf>,
}

/// Result of loading: a usable set plus every problem found (bad files are skipped).
#[derive(Debug, Default)]
pub struct Loaded {
    pub set: RuleSet,
    pub errors: Vec<RuleError>,
}

/// Cache key of a loaded set: options plus (path, mtime, size) of every rule file.
/// Comparing two fingerprints costs one directory listing, no parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    builtin: bool,
    disable: Vec<String>,
    files: Vec<(PathBuf, Option<SystemTime>, u64)>,
}

#[derive(Clone, Debug, Default)]
pub struct RuleSet {
    rules: Vec<Rule>,
}

impl RuleSet {
    /// Later layers override earlier ones by rule name; `disable` then removes names.
    /// Result is sorted by name.
    pub fn from_layers(layers: Vec<Vec<Rule>>, disable: &[String]) -> RuleSet {
        let mut by_name: BTreeMap<String, Rule> = BTreeMap::new();
        for rule in layers.into_iter().flatten() {
            by_name.insert(rule.name.clone(), rule);
        }
        for name in disable {
            by_name.remove(name);
        }
        RuleSet {
            rules: by_name.into_values().collect(),
        }
    }

    /// Read built-ins and rule directories. Never fails: problems land in `errors`.
    pub fn load(opts: &LoadOptions) -> Loaded {
        let mut errors = Vec::new();
        let mut layers = Vec::new();
        if opts.builtin {
            let (rules, errs) = builtin_rules();
            layers.push(rules);
            errors.extend(errs);
        }
        if let Some(dir) = &opts.global_dir {
            let (rules, errs) = load_dir(dir, Origin::Global);
            layers.push(rules);
            errors.extend(errs);
        }
        if let Some(dir) = &opts.project_dir {
            let (rules, errs) = load_dir(dir, Origin::Project);
            layers.push(rules);
            errors.extend(errs);
        }
        Loaded {
            set: RuleSet::from_layers(layers, &opts.disable),
            errors,
        }
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.name == name)
    }

    /// Rules that apply to `path` (by extension and `exclude`) under `filter`.
    pub fn applicable(&self, path: &Path, filter: ModeFilter) -> Vec<&Rule> {
        let ext = ext_of(path);
        let norm = path.to_string_lossy().replace('\\', "/");
        self.rules
            .iter()
            .filter(|r| filter == ModeFilter::AfterWrite || r.mode == Mode::Stream)
            .filter(|r| r.languages.contains(&ext))
            .filter(|r| !path_excluded(&r.exclude, &norm))
            .collect()
    }
}

impl LoadOptions {
    pub fn fingerprint(&self) -> Fingerprint {
        let mut files = Vec::new();
        for dir in [&self.global_dir, &self.project_dir].into_iter().flatten() {
            files.extend(list_rule_files(dir));
        }
        Fingerprint {
            builtin: self.builtin,
            disable: self.disable.clone(),
            files,
        }
    }
}

/// Lowercase extension of `path` (`""` when none).
pub fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Exclusion patterns: `dir/` = any directory component with that name;
/// `*suffix` = file name ends with suffix; `a/b.rs` = path ends with it;
/// otherwise the file name must equal the pattern.
pub fn path_excluded(patterns: &[String], path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let parts: Vec<&str> = norm.split('/').collect();
    let file = parts.last().copied().unwrap_or("");
    let dirs = &parts[..parts.len().saturating_sub(1)];
    patterns.iter().any(|p| {
        if let Some(dir) = p.strip_suffix('/') {
            dirs.contains(&dir)
        } else if let Some(suffix) = p.strip_prefix('*') {
            file.ends_with(suffix)
        } else if p.contains('/') {
            norm == *p || norm.ends_with(&format!("/{p}"))
        } else {
            file == p
        }
    })
}

fn list_rule_files(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, Option<SystemTime>, u64)> = read
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            meta.is_file()
                .then(|| (path, meta.modified().ok(), meta.len()))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.truncate(MAX_RULE_FILES_PER_DIR);
    files
}

fn load_dir(dir: &Path, origin: fn(PathBuf) -> Origin) -> (Vec<Rule>, Vec<RuleError>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for (path, _, len) in list_rule_files(dir) {
        let label = path.display().to_string();
        if len > MAX_RULE_FILE_BYTES {
            errors.push(RuleError::Io {
                origin: label,
                message: format!("rule file larger than {MAX_RULE_FILE_BYTES} bytes"),
            });
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(source) => match parse_rule(&source, origin(path)) {
                Ok(rule) => rules.push(rule),
                Err(e) => errors.push(e),
            },
            Err(e) => errors.push(RuleError::Io {
                origin: label,
                message: e.to_string(),
            }),
        }
    }
    (rules, errors)
}
