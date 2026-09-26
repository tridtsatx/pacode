//! Built-in tools (spec §7). Each module owns one tool: name, description (prompt
//! text the model sees — keep it short and precise), schema, kind, and `call`.
//!
//! Shared conventions:
//! - every schema is an object; `intent`/`accept_large_output` are added centrally;
//! - model-visible output is capped to `ctx.output_cap_chars` with
//!   `pacode_types::truncate_head_tail`; when a result is withheld for size the tool
//!   returns an error result naming the size and the `accept_large_output` flag;
//! - paths are resolved with `ctx.resolve`; the shared walk does not follow
//!   symlinks; writes outside `ctx.cwd` require permission even in Auto mode
//!   (spec §6.3 "writes stay inside cwd").

pub mod agent;
pub mod ask_question;
pub mod bash;
pub mod batch;
pub mod bg;
pub mod checkpoint;
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
pub mod symbols;
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
        .with(Arc::new(read::ReadTool::default()))
        .with(Arc::new(write::WriteTool::default()))
        .with(Arc::new(edit::EditTool::default()))
        .with(Arc::new(multi_edit::MultiEditTool::default()))
        .with(Arc::new(bash::BashTool::default()))
        .with(Arc::new(ask_question::AskQuestionTool::default()))
        .with(Arc::new(bg::BgTool::default()))
        .with(Arc::new(schedule::CronTool::default()))
        .with(Arc::new(schedule::MonitorTool::default()))
        .with(Arc::new(grep::GrepTool::default()))
        .with(Arc::new(glob::GlobTool::default()))
        .with(Arc::new(ls::LsTool::default()))
        .with(Arc::new(symbols::SymbolsTool::default()))
        .with(Arc::new(plan::PlanTool::default()))
        .with(Arc::new(agent::AgentTool::default()))
        .with(Arc::new(webfetch::WebFetchTool::default()))
        .with(Arc::new(websearch::WebSearchTool::with_config(web.clone())))
        .with(Arc::new(report_status::ReportStatusTool::default()))
        .with(Arc::new(memory::MemoryWriteTool::default()))
        .with(Arc::new(memory::MemoryReadTool::default()))
        .with(Arc::new(batch::BatchTool::default()))
}

/// Tool names a subagent gets by default: everything except `agent` (depth 1).
pub fn subagent_tool_names(all: &ToolRegistry) -> Vec<String> {
    all.names()
        .filter(|name| *name != agent::NAME)
        .map(str::to_string)
        .collect()
}
