//! `util`: common text utilities run in-process (no shell, no fork).
//!
//! Backed by `pacode-coreutils`. The model gives `cmd` as a simple pipeline
//! (`wc -l src/*.rs | sort -n`); quoting follows the shell, globs are expanded, and
//! redirection, `;`, `&`, subshells and variable expansion are refused. The tool is
//! read-only (no utility writes files); paths outside the workspace ask permission.

use std::path::Path;

use async_trait::async_trait;
use pacode_coreutils::argscan::path_operands;
use pacode_coreutils::pipeline::{self, PipelineOutput, Stage};
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, is_outside_workspace, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

#[cfg(test)]
#[path = "utils_tests.rs"]
mod utils_tests;

pub const NAME: &str = "util";

pub struct UtilTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct UtilInput {
    cmd: String,
}

/// Stdout followed by stderr (newline-separated).
fn body(out: &PipelineOutput) -> String {
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&out.stderr);
    }
    text
}

fn push_exit_line(text: &mut String, code: i32) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("[exit code {code}]"));
}

/// Output for the `util` tool: an exit-code line only when something failed.
fn render(out: &PipelineOutput) -> String {
    let mut text = body(out);
    if !out.is_benign() || out.code != 0 {
        let code = out.failure.as_ref().map(|(_, c)| *c).unwrap_or(out.code);
        push_exit_line(&mut text, code);
    }
    text
}

fn preview_of(text: &str) -> String {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

/// Paths named by the pipeline that resolve outside the workspace.
fn outside_paths(stages: &[Stage], cwd: &Path) -> Vec<String> {
    let mut outside = Vec::new();
    for stage in stages {
        for operand in path_operands(&stage.name, &stage.args) {
            let full = if Path::new(&operand).is_absolute() {
                Path::new(&operand).to_path_buf()
            } else {
                cwd.join(&operand)
            };
            if is_outside_workspace(&full, cwd) {
                outside.push(operand);
            }
        }
    }
    outside
}

/// For `bash`: run `command` in-process when it is a pure pipeline of supported utilities
/// (no redirection, globs, subshells or expansion) that succeeds. Returns `None` when the
/// command must go to a real shell, including whenever the in-process run reported any
/// failure other than `diff` finding differences, so error text and edge-case behaviour
/// always come from the real tools.
pub(crate) async fn try_in_process(command: &str, cwd: &Path) -> Option<PipelineOutput> {
    let stages = pipeline::parse(command, cwd, false).ok()?;
    let cwd = cwd.to_path_buf();
    let out = tokio::task::spawn_blocking(move || pipeline::run_pipeline(&stages, None, &cwd))
        .await
        .ok()?;
    if out.is_benign() { Some(out) } else { None }
}

/// Model-facing text of an in-process `bash` result: same shape as a spawned one (capped
/// output, then `[exit code N]`), plus a one-line preview.
pub(crate) fn render_for_bash(
    out: &PipelineOutput,
    accept_large_output: bool,
    cap: usize,
) -> (String, String) {
    let text = body(out);
    let preview = preview_of(&text);
    let mut content = cap_output(&text, accept_large_output, cap);
    push_exit_line(&mut content, out.code);
    (content, preview)
}

#[async_trait]
impl Tool for UtilTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("../prompts/util.md").trim()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["cmd"],
            "properties": {
                "cmd": {"type": "string", "description": "Pipeline of supported utilities, e.g. `sort -n a.txt | uniq -c | head -5`."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    /// Parse the pipeline (globs expanded against `ctx.cwd`), ask permission when any file
    /// operand lies outside the workspace, run it on a blocking thread, and return the
    /// output (capped) plus stderr and an `[exit code N]` line on failure.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<UtilInput>(input)?;
        if args.cmd.trim().is_empty() {
            return Err(ToolError::invalid("cmd cannot be empty"));
        }
        let stages = pipeline::parse(&args.cmd, &ctx.cwd, true)
            .map_err(|e| ToolError::invalid(e.to_string()))?;

        let outside = outside_paths(&stages, &ctx.cwd);
        if !outside.is_empty() {
            let first_60: String = args.cmd.chars().take(60).collect();
            ctx.require_permission(
                format!("Util: {first_60} (reads outside workspace)"),
                format!(
                    "{}\n\nOutside the workspace: {}",
                    args.cmd,
                    outside.join(", ")
                ),
                None,
            )
            .await?;
        }
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let cwd = ctx.cwd.clone();
        let out = tokio::task::spawn_blocking(move || pipeline::run_pipeline(&stages, None, &cwd))
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;

        let text = render(&out);
        let preview = preview_of(&text);
        let content = cap_output(&text, accept_large_output, ctx.output_cap_chars);
        let first_60: String = args.cmd.chars().take(60).collect();
        let mut result = ToolOutput::text(content)
            .with_title(format!("Util {first_60}"))
            .with_preview(preview);
        result.is_error = !out.is_benign();
        Ok(result)
    }
}
