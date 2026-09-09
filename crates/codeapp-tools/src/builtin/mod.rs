//! Built-in tools (spec §7). Each module owns one tool: name, description (prompt
//! text the model sees — keep it short and precise), schema, kind, and `call`.
//!
//! Shared conventions:
//! - every schema is an object; `intent`/`accept_large_output` are added centrally;
//! - model-visible output is capped to `ctx.output_cap_chars` with
//!   `codeapp_types::truncate_head_tail`; when a result is withheld for size the tool
//!   returns an error result naming the size and the `accept_large_output` flag;
//! - paths are resolved with `ctx.resolve`; symlinks are followed; writes outside
//!   `ctx.cwd` require permission even in Auto mode (spec §6.3 "запись только внутри cwd").

pub mod agent;
pub mod bash;
pub mod bg;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod ls;
pub mod mcp;
pub mod multi_edit;
pub mod plan;
pub mod read;
pub mod webfetch;
pub mod write;

use std::sync::Arc;

use crate::registry::ToolRegistry;

/// All built-in tools. MCP tools are added per session by the core.
pub fn builtin_tools() -> ToolRegistry {
    ToolRegistry::new()
        .with(Arc::new(read::ReadTool))
        .with(Arc::new(write::WriteTool))
        .with(Arc::new(edit::EditTool))
        .with(Arc::new(multi_edit::MultiEditTool))
        .with(Arc::new(bash::BashTool))
        .with(Arc::new(bg::BgTool))
        .with(Arc::new(grep::GrepTool))
        .with(Arc::new(glob::GlobTool))
        .with(Arc::new(ls::LsTool))
        .with(Arc::new(plan::PlanTool))
        .with(Arc::new(agent::AgentTool))
        .with(Arc::new(webfetch::WebFetchTool))
}

/// Tool names a subagent gets by default: everything except `agent` (depth 1).
pub fn subagent_tool_names(all: &ToolRegistry) -> Vec<String> {
    all.names()
        .filter(|name| *name != agent::NAME)
        .map(str::to_string)
        .collect()
}
