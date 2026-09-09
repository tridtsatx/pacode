//! Skill discovery and YAML frontmatter parsing.

use std::path::{Path, PathBuf};

use crate::{DiscoveredSkill, Discovery, ImportSource, ImportWarning};

#[cfg(test)]
#[path = "skills_tests.rs"]
mod skills_tests;

/// Discovers skills for a given import source.
pub fn discover_skills(home: &Path, _cwd: &Path, source: ImportSource, discovery: &mut Discovery) {
    match source {
        ImportSource::ClaudeCode => {
            discover_claude_skills(home, discovery);
        }
        ImportSource::OpenCode => {
            discover_opencode_skills(home, discovery);
        }
        ImportSource::Codex => {}
        ImportSource::Cursor => {}
        ImportSource::GeminiCli => {}
        ImportSource::VsCode => {}
    }
}

fn discover_claude_skills(home: &Path, discovery: &mut Discovery) {
    let claude_dir = home.join(".claude");
    scan_skill_subdirs(
        &claude_dir.join("skills"),
        ImportSource::ClaudeCode,
        discovery,
    );

    let cache_dir = claude_dir.join("plugins").join("cache");
    if cache_dir.is_dir() {
        let mut skills_dirs = Vec::new();
        find_plugin_skills_dirs(&cache_dir, 0, &mut skills_dirs);
        for skills_dir in skills_dirs {
            scan_skill_subdirs(&skills_dir, ImportSource::ClaudeCode, discovery);
        }
    }
}

fn discover_opencode_skills(home: &Path, discovery: &mut Discovery) {
    let opencode_dir = home.join(".config").join("opencode");
    scan_skill_subdirs(
        &opencode_dir.join("skill"),
        ImportSource::OpenCode,
        discovery,
    );
    scan_skill_subdirs(
        &opencode_dir.join("skills"),
        ImportSource::OpenCode,
        discovery,
    );
}

fn find_plugin_skills_dirs(current: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let is_skills_dir = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| name.eq_ignore_ascii_case("skills"))
                .unwrap_or(false);

            if is_skills_dir && depth >= 2 {
                out.push(path);
            } else {
                find_plugin_skills_dirs(&path, depth + 1, out);
            }
        }
    }
}

fn scan_skill_subdirs(base: &Path, source: ImportSource, discovery: &mut Discovery) {
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            check_and_load_skill(&path, source, discovery);
        }
    }
}

fn check_and_load_skill(skill_dir: &Path, source: ImportSource, discovery: &mut Discovery) {
    let upper = skill_dir.join("SKILL.md");
    let lower = skill_dir.join("skill.md");
    let skill_file = if upper.is_file() {
        upper
    } else if lower.is_file() {
        lower
    } else {
        return;
    };

    if discovery.skills.iter().any(|s| s.origin == skill_file) {
        return;
    }

    let content = match std::fs::read_to_string(&skill_file) {
        Ok(c) => c,
        Err(err) => {
            discovery.errors.push(ImportWarning {
                source,
                path: skill_file,
                message: format!("failed to read skill file: {err}"),
            });
            return;
        }
    };

    let dir_name = skill_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("skill");

    let (name, description) = parse_skill_frontmatter(&content, dir_name);

    discovery.skills.push(DiscoveredSkill {
        source,
        origin: skill_file,
        name,
        description,
        dir: skill_dir.to_path_buf(),
    });
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum ActiveKey {
    None,
    Name,
    Description,
    Other,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum DescFoldMode {
    Folded,
    Literal,
}

/// Parses YAML-ish frontmatter delimited by `---` lines for `name` and `description` keys.
pub fn parse_skill_frontmatter(content: &str, dir_name: &str) -> (String, String) {
    let lines: Vec<&str> = content.lines().collect();

    let mut start_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "---" {
            start_idx = Some(i + 1);
            break;
        }
        break;
    }

    let Some(start) = start_idx else {
        return (dir_name.to_string(), String::new());
    };

    let mut end_idx = None;
    for (i, line) in lines[start..].iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            end_idx = Some(start + i);
            break;
        }
    }

    let Some(end) = end_idx else {
        return (dir_name.to_string(), String::new());
    };

    let fm_lines = &lines[start..end];
    let mut parsed_name: Option<String> = None;
    let mut desc_lines: Vec<String> = Vec::new();
    let mut desc_mode = DescFoldMode::Folded;
    let mut active = ActiveKey::None;

    for line in fm_lines {
        let is_indented = line.starts_with(' ') || line.starts_with('\t');

        if !is_indented {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if active == ActiveKey::Description && desc_mode == DescFoldMode::Literal {
                    desc_lines.push(String::new());
                }
                continue;
            }

            if let Some((raw_key, raw_val)) = trimmed.split_once(':') {
                let key = raw_key.trim();
                if key.eq_ignore_ascii_case("name") {
                    active = ActiveKey::Name;
                    let val = unquote(raw_val.trim());
                    if !val.is_empty() {
                        parsed_name = Some(val);
                    }
                } else if key.eq_ignore_ascii_case("description") {
                    active = ActiveKey::Description;
                    let val = raw_val.trim();
                    if val == ">" || val == ">-" || val == ">+" {
                        desc_mode = DescFoldMode::Folded;
                    } else if val == "|" || val == "|-" || val == "|+" {
                        desc_mode = DescFoldMode::Literal;
                    } else if val.is_empty() {
                        desc_mode = DescFoldMode::Folded;
                    } else {
                        desc_mode = DescFoldMode::Folded;
                        desc_lines.push(val.to_string());
                    }
                } else {
                    active = ActiveKey::Other;
                }
            } else {
                active = ActiveKey::Other;
            }
        } else {
            match active {
                ActiveKey::Description => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() || desc_mode == DescFoldMode::Literal {
                        desc_lines.push(trimmed.to_string());
                    }
                }
                ActiveKey::Name => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && parsed_name.is_none() {
                        parsed_name = Some(unquote(trimmed));
                    }
                }
                ActiveKey::None | ActiveKey::Other => {}
            }
        }
    }

    let final_name = parsed_name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| dir_name.to_string());

    let final_desc = match desc_mode {
        DescFoldMode::Literal => desc_lines.join("\n"),
        DescFoldMode::Folded => {
            let joined = desc_lines
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            unquote(&joined)
        }
    };

    (final_name, final_desc)
}

/// Unquotes a single or double quoted string.
pub fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        let inner = &s[1..s.len() - 1];
        let mut unescaped = String::with_capacity(inner.len());
        let mut chars = inner.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\'
                && let Some(&next) = chars.peek()
                && (next == '"' || next == '\'' || next == '\\')
            {
                unescaped.push(next);
                chars.next();
                continue;
            }
            unescaped.push(c);
        }
        unescaped
    } else {
        s.to_string()
    }
}
