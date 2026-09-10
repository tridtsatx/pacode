//! Built-in tools (spec §7). Each module owns one tool: name, description (prompt
//! text the model sees — keep it short and precise), schema, kind, and `call`.
//!
//! Shared conventions:
//! - every schema is an object; `intent`/`accept_large_output` are added centrally;
//! - model-visible output is capped to `ctx.output_cap_chars` with
//!   `pacode_types::truncate_head_tail`; when a result is withheld for size the tool
//!   returns an error result naming the size and the `accept_large_output` flag;
//! - paths are resolved with `ctx.resolve`; symlinks are followed; writes outside
//!   `ctx.cwd` require permission even in Auto mode (spec §6.3 "запись только внутри cwd").

pub mod agent;
pub mod ask_question;
pub mod bash;
pub mod bg;
pub mod edit;
pub mod glob;
pub mod grep;
pub(crate) mod helpers;
pub mod ls;
pub mod mcp;
pub mod memory;
pub mod multi_edit;
pub mod plan;
pub mod plugin;
pub mod read;
pub mod report_status;
pub mod schedule;
pub mod skill;
pub mod webfetch;
pub mod websearch;
pub mod write;

pub use skill::SkillTool;

use std::sync::Arc;

use pacode_types::WebConfig;

use crate::registry::ToolRegistry;

/// All built-in tools. MCP tools are added per session by the core. `web`
/// configures `websearch` (default result count, request timeout).
pub fn builtin_tools(web: &WebConfig) -> ToolRegistry {
    ToolRegistry::new()
        .with(Arc::new(read::ReadTool))
        .with(Arc::new(write::WriteTool))
        .with(Arc::new(edit::EditTool))
        .with(Arc::new(multi_edit::MultiEditTool))
        .with(Arc::new(bash::BashTool))
        .with(Arc::new(ask_question::AskQuestionTool))
        .with(Arc::new(bg::BgTool))
        .with(Arc::new(schedule::CronTool))
        .with(Arc::new(schedule::MonitorTool))
        .with(Arc::new(grep::GrepTool))
        .with(Arc::new(glob::GlobTool))
        .with(Arc::new(ls::LsTool))
        .with(Arc::new(plan::PlanTool))
        .with(Arc::new(agent::AgentTool))
        .with(Arc::new(webfetch::WebFetchTool))
        .with(Arc::new(websearch::WebSearchTool::with_config(web.clone())))
        .with(Arc::new(report_status::ReportStatusTool))
        .with(Arc::new(memory::MemoryWriteTool))
        .with(Arc::new(memory::MemoryReadTool))
}

/// Tool names a subagent gets by default: everything except `agent` (depth 1).
pub fn subagent_tool_names(all: &ToolRegistry) -> Vec<String> {
    all.names()
        .filter(|name| *name != agent::NAME)
        .map(str::to_string)
        .collect()
}
