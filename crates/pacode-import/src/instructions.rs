//! Discovery of project instructions written for other coding agents
//! (Cursor, Cline, Copilot, Windsurf, Gemini CLI, Codex). Read-only: nothing
//! here writes to the workspace. Missing or unreadable files are skipped.

use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod instructions_tests;

/// Maximum bytes read from a single instruction file. Longer files are cut at this cap.
pub const MAX_FILE_BYTES: usize = 64 * 1024;

/// Maximum total body bytes across all discovered instruction sources.
/// Sources past this budget are dropped; a partially fitting source is cut.
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;

/// Which foreign agent format a discovered instruction file came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum InstructionKind {
    /// Legacy `.cursorrules` (plain text).
    CursorRules,
    /// `.cursor/rules/*.mdc` (MDC frontmatter).
    CursorMdc,
    /// `.clinerules` file, or `*.md` files inside a `.clinerules` directory.
    ClineRules,
    /// `.windsurfrules` (plain text).
    Windsurf,
    /// `GEMINI.md` (plain text).
    Gemini,
    /// `.codex/AGENTS.md` (plain text).
    Codex,
    /// `.github/copilot-instructions.md` (plain text).
    CopilotInstructions,
    /// `.github/instructions/*.instructions.md` (frontmatter `applyTo`).
    CopilotScoped,
}

impl InstructionKind {
    /// Short stable identifier for diagnostics and prompt hints.
    pub fn label(self) -> &'static str {
        match self {
            Self::CursorRules => "cursor-rules",
            Self::CursorMdc => "cursor-mdc",
            Self::ClineRules => "cline",
            Self::Windsurf => "windsurf",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
            Self::CopilotInstructions => "copilot",
            Self::CopilotScoped => "copilot-scoped",
        }
    }
}

/// One instruction file found in the project tree.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstructionSource {
    /// Absolute path of the file as found.
    pub path: PathBuf,
    pub kind: InstructionKind,
    /// `true` when the body belongs in the system prompt. `false` for glob-scoped rules.
    pub always_on: bool,
    /// Glob patterns from frontmatter (`globs` / `applyTo` / `paths`), `**` excluded.
    pub globs: Vec<String>,
    /// File content with frontmatter removed, trimmed and capped.
    pub body: String,
}

/// Discovers foreign-agent instruction files in `cwd` and each of its ancestors.
///
/// Directories are visited from the filesystem root down to `cwd`, so nearer files
/// come later, matching how `AGENTS.md` is ordered. Within a directory, files are
/// ordered by format, and directory entries are sorted by name.
pub fn discover_instructions(cwd: &Path) -> Vec<InstructionSource> {
    let mut dirs: Vec<&Path> = cwd.ancestors().collect();
    dirs.reverse();

    let mut collector = Collector::default();
    for dir in dirs {
        collect_dir(dir, &mut collector);
    }
    collector.sources
}

/// Renders discovered sources for the system prompt.
///
/// Always-on bodies are emitted as `# <relative path>` sections, like `AGENTS.md`.
/// Glob-scoped rules get one index line each and their bodies are left out.
/// Returns an empty string when there is nothing to show.
pub fn render_prompt_section(sources: &[InstructionSource], cwd: &Path) -> String {
    let mut parts: Vec<String> = sources
        .iter()
        .filter(|s| s.always_on)
        .map(|s| format!("# {}\n\n{}", display_rel(&s.path, cwd), s.body))
        .collect();

    let scoped: Vec<String> = sources
        .iter()
        .filter(|s| !s.always_on)
        .map(|s| {
            format!(
                "- {} [globs: {}]",
                display_rel(&s.path, cwd),
                s.globs.join(", ")
            )
        })
        .collect();
    if !scoped.is_empty() {
        parts.push(format!(
            "# Scoped project rules (not loaded; read the file when editing matching files)\n\n{}",
            scoped.join("\n")
        ));
    }

    parts.join("\n\n")
}

fn display_rel(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd).unwrap_or(path).display().to_string()
}

fn collect_dir(dir: &Path, c: &mut Collector) {
    c.add(
        dir.join(".cursorrules"),
        InstructionKind::CursorRules,
        false,
    );

    let cline = dir.join(".clinerules");
    if cline.is_dir() {
        c.add_dir(&cline, |n| n.ends_with(".md"), InstructionKind::ClineRules);
    } else {
        c.add(cline, InstructionKind::ClineRules, false);
    }

    c.add(dir.join(".windsurfrules"), InstructionKind::Windsurf, false);
    c.add(dir.join("GEMINI.md"), InstructionKind::Gemini, false);
    c.add(
        dir.join(".codex").join("AGENTS.md"),
        InstructionKind::Codex,
        false,
    );
    c.add(
        dir.join(".github").join("copilot-instructions.md"),
        InstructionKind::CopilotInstructions,
        false,
    );
    c.add_dir(
        &dir.join(".github").join("instructions"),
        |n| n.ends_with(".instructions.md"),
        InstructionKind::CopilotScoped,
    );
    c.add_dir(
        &dir.join(".cursor").join("rules"),
        |n| n.ends_with(".mdc"),
        InstructionKind::CursorMdc,
    );
}

#[derive(Default)]
struct Collector {
    sources: Vec<InstructionSource>,
    total: usize,
}

impl Collector {
    /// Reads one file and records it when it has a non-empty body within budget.
    fn add(&mut self, path: PathBuf, kind: InstructionKind, frontmatter: bool) {
        if self.total >= MAX_TOTAL_BYTES {
            return;
        }
        let Some(raw) = read_capped(&path, MAX_FILE_BYTES) else {
            return;
        };
        let content = raw.trim_start_matches('\u{feff}');
        let (meta, text) = if frontmatter {
            parse_frontmatter(content)
        } else {
            (Meta::default(), content)
        };
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let body = truncate_bytes(text, MAX_TOTAL_BYTES - self.total)
            .trim_end()
            .to_string();
        if body.is_empty() {
            return;
        }
        self.total += body.len();
        let always_on = meta.always_apply || meta.globs.is_empty();
        self.sources.push(InstructionSource {
            path,
            kind,
            always_on,
            globs: meta.globs,
            body,
        });
    }

    /// Adds every regular file in `dir` whose name passes `accept`, sorted by name.
    fn add_dir(&mut self, dir: &Path, accept: fn(&str) -> bool, kind: InstructionKind) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(accept))
            .collect();
        paths.sort();
        for path in paths {
            self.add(path, kind, true);
        }
    }
}

fn read_capped(path: &Path, cap: usize) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(cap as u64).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Meta {
    always_apply: bool,
    globs: Vec<String>,
}

/// Splits a leading `---` ... `---` block off `text`. Returns the parsed metadata and
/// the remaining body. Text without a closed frontmatter block is returned unchanged.
fn parse_frontmatter(text: &str) -> (Meta, &str) {
    let mut offset = 0;
    let mut yaml: Vec<&str> = Vec::new();
    for (idx, line) in text.split_inclusive('\n').enumerate() {
        offset += line.len();
        let trimmed = line.trim_end();
        if idx == 0 {
            if trimmed != "---" {
                return (Meta::default(), text);
            }
        } else if trimmed == "---" {
            return (parse_meta(&yaml), &text[offset..]);
        } else {
            yaml.push(trimmed);
        }
    }
    (Meta::default(), text)
}

/// Reads the handful of keys that matter from a flat YAML-ish frontmatter block.
fn parse_meta(lines: &[&str]) -> Meta {
    let mut meta = Meta::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "alwaysApply" => meta.always_apply = value.eq_ignore_ascii_case("true"),
            "globs" | "applyTo" | "paths" => {
                let mut items: Vec<&str> = Vec::new();
                if value.is_empty() {
                    // Block list form: `globs:` followed by `  - "*.ts"` lines.
                    while i < lines.len() {
                        let t = lines[i].trim_start();
                        if let Some(item) = t.strip_prefix('-') {
                            items.push(item.trim());
                        } else if !t.is_empty() {
                            break;
                        }
                        i += 1;
                    }
                } else {
                    items.push(value);
                }
                meta.globs = normalize_globs(&items);
            }
            _ => {}
        }
    }
    meta
}

/// Splits comma-separated and bracketed glob lists, strips quotes, and drops
/// the match-everything patterns that mean "always on".
fn normalize_globs(raw: &[&str]) -> Vec<String> {
    raw.iter()
        .flat_map(|s| s.split(','))
        .map(|s| {
            s.trim()
                .trim_matches(|c: char| matches!(c, '"' | '\'' | '[' | ']'))
                .trim()
        })
        .filter(|s| !s.is_empty() && *s != "**" && *s != "**/*")
        .map(str::to_string)
        .collect()
}
