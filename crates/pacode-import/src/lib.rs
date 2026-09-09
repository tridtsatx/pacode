//! Pure, IO-light library that discovers MCP servers and skills configured
//! for other coding agents and normalizes them into pacode's shapes.

pub mod apply;
pub mod error;
pub mod mcp;
pub mod skills;

use std::path::{Path, PathBuf};
use std::str::FromStr;

pub use apply::{
    AppliedEntry, ApplyOutcome, ApplyReport, ApplyTarget, EntryStatus, PlanEntry, PlanItem, apply,
    check_status,
};
pub use error::ParseImportSourceError;
pub use pacode_types::config::McpServerConfig;

/// Coding agents from which configuration and skills can be imported.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum ImportSource {
    ClaudeCode,
    Codex,
    OpenCode,
    Cursor,
    GeminiCli,
    VsCode,
}

impl ImportSource {
    pub const ALL: [ImportSource; 6] = [
        ImportSource::ClaudeCode,
        ImportSource::Codex,
        ImportSource::OpenCode,
        ImportSource::Cursor,
        ImportSource::GeminiCli,
        ImportSource::VsCode,
    ];

    /// Returns a slice of all known import sources.
    pub fn all() -> &'static [ImportSource] {
        &Self::ALL
    }

    /// Short stable identifier: "claude", "codex", "opencode", "cursor", "gemini", "vscode".
    pub fn id(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::GeminiCli => "gemini",
            Self::VsCode => "vscode",
        }
    }

    /// Human-friendly display label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
            Self::GeminiCli => "Gemini CLI",
            Self::VsCode => "VS Code",
        }
    }
}

impl std::fmt::Display for ImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl FromStr for ImportSource {
    type Err = ParseImportSourceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        for &source in Self::all() {
            if source.id().eq_ignore_ascii_case(trimmed) {
                return Ok(source);
            }
        }
        let normalized = trimmed.to_ascii_lowercase().replace(['-', '_', ' '], "");
        match normalized.as_str() {
            "claude" | "claudecode" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::OpenCode),
            "cursor" => Ok(Self::Cursor),
            "gemini" | "geminicli" => Ok(Self::GeminiCli),
            "vscode" => Ok(Self::VsCode),
            _ => Err(ParseImportSourceError(s.to_string())),
        }
    }
}

/// A normalized MCP server discovered from another agent's configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredMcp {
    pub source: ImportSource,
    pub origin: PathBuf,
    pub name: String,
    pub server: McpServerConfig,
}

/// A skill discovered from another agent's skills directory.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredSkill {
    pub source: ImportSource,
    pub origin: PathBuf,
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

/// Result of scanning configuration files and directories.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Discovery {
    pub mcp: Vec<DiscoveredMcp>,
    pub skills: Vec<DiscoveredSkill>,
    pub errors: Vec<ImportWarning>,
}

/// Non-fatal warning encountered while reading or parsing a source.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportWarning {
    pub source: ImportSource,
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for ImportWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source_id = self.source.id();
        let path_disp = self.path.display();
        let msg = &self.message;
        write!(f, "[{source_id}] {path_disp}: {msg}")
    }
}

/// Discovers MCP servers and skills for the requested sources relative to `home` and `cwd`.
pub fn discover(home: &Path, cwd: &Path, sources: &[ImportSource]) -> Discovery {
    let mut discovery = Discovery::default();

    for &source in sources {
        mcp::discover_mcp(home, cwd, source, &mut discovery);
        skills::discover_skills(home, cwd, source, &mut discovery);
    }

    // A name is the identity of an imported server or skill, and the same skill
    // ships in several plugin bundles, so keep the first one found and drop the
    // rest instead of offering the user a list full of duplicates.
    dedup_by_name(&mut discovery);
    discovery
}

fn dedup_by_name(discovery: &mut Discovery) {
    let mut seen_mcp: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    discovery.mcp.retain(|m| seen_mcp.insert(m.name.clone()));
    let mut seen_skill: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    discovery
        .skills
        .retain(|s| seen_skill.insert(s.name.clone()));
}
