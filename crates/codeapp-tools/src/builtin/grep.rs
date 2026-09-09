//! `grep`: regex search with ripgrep's crates (`ignore` + `grep-searcher`).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "grep";

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression (Rust regex syntax), \
         respecting .gitignore. Returns `path:line:text` lines, at most `max_results`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Directory or file to search (default: working directory)."},
                "glob": {"type": "string", "description": "Only files matching this glob, e.g. `*.rs`."},
                "case_insensitive": {"type": "boolean", "default": false},
                "max_results": {"type": "integer", "minimum": 1, "default": 200},
                "context": {"type": "integer", "minimum": 0, "default": 0, "description": "Lines of context around each match."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// `ignore::WalkBuilder` (hidden files skipped, .gitignore honoured, binary files
    /// skipped by `grep_searcher`'s binary detection), `grep_regex::RegexMatcher`,
    /// `Searcher` with line numbers; runs in `tokio::task::spawn_blocking`; stops at
    /// `max_results`; output capped; title `Grep "<pattern>"`, preview `<n> hits`.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let _ = (input, ctx);
        todo!("GrepTool::call")
    }
}
