//! `memory_write` and `memory_read`: persistent markdown memory (spec §14).
//!
//! Stores bullet lines `- {text}` in:
//! - Global: `~/.config/pacode/memory.md`
//! - Project: `<cwd>/.pacode/memory.md`

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const WRITE_NAME: &str = "memory_write";
pub const READ_NAME: &str = "memory_read";

/// Resolve the global memory path (`~/.config/pacode/memory.md`).
/// Respects `PACODE_HOME`, `PACODE_CONFIG`, and `XDG_CONFIG_HOME`.
pub fn global_memory_path() -> PathBuf {
    if let Some(home) = std::env::var_os("PACODE_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(home).join("memory.md");
    }
    if let Some(cfg) = std::env::var_os("PACODE_CONFIG").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(cfg);
        if let Some(parent) = p.parent() {
            return parent.join("memory.md");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(xdg).join("pacode").join("memory.md");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(home)
            .join(".config")
            .join("pacode")
            .join("memory.md");
    }
    PathBuf::from(".pacode").join("memory.md")
}

/// Resolve the project memory path (`<cwd>/.pacode/memory.md`).
pub fn project_memory_path(cwd: &Path) -> PathBuf {
    cwd.join(".pacode").join("memory.md")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryScope {
    Global,
    Project,
}

impl MemoryScope {
    fn parse(s: &str) -> Result<Self, ToolError> {
        match s {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project),
            other => Err(ToolError::invalid(format!(
                "invalid scope '{other}', expected 'global' or 'project'"
            ))),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

pub struct MemoryWriteTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct MemoryWriteInput {
    scope: String,
    text: String,
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        WRITE_NAME
    }

    fn description(&self) -> &str {
        "Append a memory bullet point to persistent storage ('global' or 'project' scope). \
         Newlines are collapsed to spaces, trimmed, and capped at 500 chars. \
         Exact duplicate lines are not appended. Returns 'stored' or 'already stored'."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["scope", "text"],
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["global", "project"],
                    "description": "Storage scope: 'global' (~/.config/pacode/memory.md) or 'project' (<cwd>/.pacode/memory.md)."
                },
                "text": {
                    "type": "string",
                    "description": "Note content to remember."
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<MemoryWriteInput>(input)?;
        let scope = MemoryScope::parse(&args.scope)?;

        // Collapse newlines to spaces and trim
        let normalized = args.text.replace("\r\n", "\n").replace('\r', "\n");
        let collapsed = normalized
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let trimmed = collapsed.trim();
        if trimmed.is_empty() {
            return Err(ToolError::invalid("text cannot be empty"));
        }

        // Cap to 500 chars
        let capped_text: String = trimmed.chars().take(500).collect();
        let capped_text = capped_text.trim();
        let bullet_line = format!("- {capped_text}");

        let target_file = match scope {
            MemoryScope::Global => global_memory_path(),
            MemoryScope::Project => project_memory_path(&ctx.cwd),
        };

        let existing = match tokio::fs::read_to_string(&target_file).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(ToolError::Io(e)),
        };

        // Check exact duplicate lines
        let already_stored = existing.lines().any(|l| l.trim() == bullet_line);
        if already_stored {
            return Ok(ToolOutput::text("already stored")
                .with_title(format!("{WRITE_NAME} ({})", scope.as_str()))
                .with_preview("already stored"));
        }

        // Project scope requires permission as a write inside the project;
        // global scope is pacode-owned configuration and allowed without prompt.
        if scope == MemoryScope::Project {
            let rel_path = target_file
                .strip_prefix(&ctx.cwd)
                .unwrap_or(&target_file)
                .to_string_lossy()
                .to_string();
            let perm_title = format!("Write {rel_path}");
            let detail = format!("append to memory: {bullet_line}");
            ctx.require_permission(perm_title, detail, None).await?;
        }

        // Create parent directories if needed
        if let Some(parent) = target_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let new_content = if existing.is_empty() {
            format!("{bullet_line}\n")
        } else if existing.ends_with('\n') {
            format!("{existing}{bullet_line}\n")
        } else {
            format!("{existing}\n{bullet_line}\n")
        };

        tokio::fs::write(&target_file, new_content.as_bytes()).await?;

        Ok(ToolOutput::text("stored")
            .with_title(format!("{WRITE_NAME} ({})", scope.as_str()))
            .with_preview("stored"))
    }
}

pub struct MemoryReadTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct MemoryReadInput {
    scope: Option<String>,
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        READ_NAME
    }

    fn description(&self) -> &str {
        "Read persistent memory files. Optionally specify 'global' or 'project' scope; \
         if omitted, returns both."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["global", "project"],
                    "description": "Optional scope: 'global' (~/.config/pacode/memory.md) or 'project' (<cwd>/.pacode/memory.md). If omitted, returns both."
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<MemoryReadInput>(input)?;

        let raw = match args.scope.as_deref() {
            Some("global") => {
                let path = global_memory_path();
                tokio::fs::read_to_string(path).await.unwrap_or_default()
            }
            Some("project") => {
                let path = project_memory_path(&ctx.cwd);
                tokio::fs::read_to_string(path).await.unwrap_or_default()
            }
            Some(other) => {
                return Err(ToolError::invalid(format!(
                    "invalid scope '{other}', expected 'global' or 'project'"
                )));
            }
            None => {
                let global = tokio::fs::read_to_string(global_memory_path())
                    .await
                    .unwrap_or_default();
                let project = tokio::fs::read_to_string(project_memory_path(&ctx.cwd))
                    .await
                    .unwrap_or_default();

                let mut parts = Vec::new();
                if !global.trim().is_empty() {
                    parts.push(format!("# Memory (global)\n\n{}", global.trim()));
                }
                if !project.trim().is_empty() {
                    parts.push(format!("# Memory (project)\n\n{}", project.trim()));
                }
                parts.join("\n\n")
            }
        };

        let content = cap_output(&raw, accept_large_output, ctx.output_cap_chars);
        let line_count = content.lines().count();
        let preview = format!("{line_count} lines");

        Ok(ToolOutput::text(content)
            .with_title(READ_NAME)
            .with_preview(preview))
    }
}
