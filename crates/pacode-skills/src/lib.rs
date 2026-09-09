//! Skills subsystem: markdown skills in Claude Code style (`<dir>/*/SKILL.md`).
//!
//! Parses `name:` and `description:` out of YAML frontmatter without external YAML dependencies.

use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillWarning {
    #[error("failed to read skill file at {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read directory at {path}: {source}")]
    DirReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("unknown skill '{name}', available skills: {available}")]
    NotFound { name: String, available: String },
    #[error("failed to read skill '{name}' at {path}: {source}")]
    Io {
        name: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

const TRUNCATION_MARKER: &str = "\n\n[... truncated ...]";

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load skills from `<dir>/*/SKILL.md` across the given root directories.
    ///
    /// Skills are deduped by name (first encountered wins) and sorted by name.
    /// Unreadable files or directories return warnings, never panicking.
    pub fn load(dirs: &[PathBuf]) -> (Self, Vec<SkillWarning>) {
        let mut warnings = Vec::new();
        let mut discovered = Vec::new();

        for root in dirs {
            let entries = match std::fs::read_dir(root) {
                Ok(rd) => rd,
                Err(err) => {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        warnings.push(SkillWarning::DirReadFailed {
                            path: root.clone(),
                            source: err,
                        });
                    }
                    continue;
                }
            };

            let mut child_dirs = Vec::new();
            for entry_res in entries {
                let entry = match entry_res {
                    Ok(e) => e,
                    Err(err) => {
                        warnings.push(SkillWarning::DirReadFailed {
                            path: root.clone(),
                            source: err,
                        });
                        continue;
                    }
                };
                let path = entry.path();
                if path.is_dir() {
                    child_dirs.push(path);
                }
            }
            child_dirs.sort();

            for dir in child_dirs {
                let skill_path = dir.join("SKILL.md");
                if !skill_path.is_file() {
                    continue;
                }

                let content = match std::fs::read_to_string(&skill_path) {
                    Ok(c) => c,
                    Err(err) => {
                        warnings.push(SkillWarning::ReadFailed {
                            path: skill_path,
                            source: err,
                        });
                        continue;
                    }
                };

                let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();

                let (name, description, _body) = parse_frontmatter(&content, dir_name);
                discovered.push(Skill {
                    name,
                    description,
                    dir,
                    path: skill_path,
                });
            }
        }

        let mut skills = Vec::new();
        let mut seen = HashSet::new();
        for skill in discovered {
            if seen.insert(skill.name.clone()) {
                skills.push(skill);
            }
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));

        (Self { skills }, warnings)
    }

    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Frontmatter-stripped body, truncated to `max_bytes` on a char boundary with a trailing marker.
    pub fn body(&self, name: &str, max_bytes: usize) -> Result<String, SkillError> {
        let skill = self.get(name).ok_or_else(|| {
            let available = if self.skills.is_empty() {
                "(none)".to_string()
            } else {
                self.skills
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            SkillError::NotFound {
                name: name.to_string(),
                available,
            }
        })?;

        let content = std::fs::read_to_string(&skill.path).map_err(|source| SkillError::Io {
            name: name.to_string(),
            path: skill.path.clone(),
            source,
        })?;

        let dir_name = skill
            .dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let (_name, _desc, body) = parse_frontmatter(&content, dir_name);

        if body.len() <= max_bytes {
            Ok(body.to_string())
        } else {
            let mut cutoff = max_bytes.min(body.len());
            while cutoff > 0 && !body.is_char_boundary(cutoff) {
                cutoff -= 1;
            }
            let truncated = &body[..cutoff];
            Ok(format!("{truncated}{TRUNCATION_MARKER}"))
        }
    }
}

/// Parse ONLY `name:` and `description:` out of the `---` frontmatter by hand.
///
/// Missing name falls back to `dir_name`; missing description becomes empty.
/// Returns `(name, description, frontmatter_stripped_body)`.
pub fn parse_frontmatter<'a>(content: &'a str, dir_name: &str) -> (String, String, &'a str) {
    let clean = content.strip_prefix('\u{feff}').unwrap_or(content);
    if let Some((fm_text, body)) = split_frontmatter_text(clean) {
        let mut name: Option<String> = None;
        let mut description: Option<String> = None;

        for line in fm_text.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("name:") {
                let val = strip_quotes(rest);
                if !val.is_empty() && name.is_none() {
                    name = Some(val.to_string());
                }
            } else if let Some(rest) = trimmed.strip_prefix("description:") {
                let val = strip_quotes(rest);
                if description.is_none() {
                    description = Some(val.to_string());
                }
            }
        }

        let final_name = name.unwrap_or_else(|| dir_name.to_string());
        let final_desc = description.unwrap_or_default();
        (final_name, final_desc, body)
    } else {
        (dir_name.to_string(), String::new(), content)
    }
}

fn split_frontmatter_text(content: &str) -> Option<(&str, &str)> {
    if !content.starts_with("---") {
        return None;
    }
    let after_dashes = &content[3..];
    let after_first_newline = if let Some(rest) = after_dashes.strip_prefix("\r\n") {
        rest
    } else {
        after_dashes.strip_prefix('\n')?
    };

    let mut offset = 0;
    while offset < after_first_newline.len() {
        let slice = &after_first_newline[offset..];
        if let Some(rest) = slice.strip_prefix("---") {
            if let Some(body) = rest.strip_prefix("\r\n") {
                let fm = &after_first_newline[..offset];
                return Some((fm, body));
            } else if let Some(body) = rest.strip_prefix('\n') {
                let fm = &after_first_newline[..offset];
                return Some((fm, body));
            } else if rest.is_empty() {
                let fm = &after_first_newline[..offset];
                return Some((fm, ""));
            }
        }
        if let Some(pos) = slice.find('\n') {
            offset += pos + 1;
        } else {
            break;
        }
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].trim()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod skills_tests;
