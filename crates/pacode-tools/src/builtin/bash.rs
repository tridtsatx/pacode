//! `bash`: run a shell command as a session task (spec §9).

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput, WaitOutcome};

#[path = "ssh_multiplex.rs"]
mod ssh_multiplex;
#[cfg(test)]
#[path = "ssh_multiplex_tests.rs"]
mod ssh_multiplex_tests;

pub const NAME: &str = "bash";

pub struct BashTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct BashInput {
    command: String,
    background: bool,
    timeout_secs: Option<u64>,
    label: Option<String>,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Run a shell command in the working directory. Long commands (builds, tests, \
         servers) should use `background: true`, or they are moved to the background \
         automatically after a few seconds: you then get a task id, keep working, and \
         a notification arrives when the task finishes. Use `bg` to check, wait for, \
         or kill tasks. Output above the cap is cut head+tail."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string", "description": "Command line for `sh -c`."},
                "background": {"type": "boolean", "default": false, "description": "Return a task id immediately instead of waiting."},
                "timeout_secs": {"type": "integer", "minimum": 1, "description": "Kill the command after this many seconds (default: none for background, 3600 otherwise)."},
                "label": {"type": "string", "description": "Short label for the task list, e.g. `cargo test`."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }

    /// 1. `pacode_risk::classify(command, cwd)`; in `Mode::Plan` the command must also
    ///    satisfy `pacode_risk::is_read_only`, else `ToolError::Denied` without prompting.
    /// 2. `ctx.require_permission("Bash: <first 60 chars>", command + risk summary, Some(risk))`
    ///    (the host applies the matrix; Safe returns Allow without asking).
    /// 3. `host.spawn_task(TaskSpec{...})`; `background: true` → return
    ///    `{task_id, status: running}` immediately, title `Bash <label>`,
    ///    `ToolOutput.task = Some(id)`.
    /// 4. Foreground: `host.wait_task(id, yield_after, false)`; `Finished` → output from
    ///    `host.task_tail`/full output (capped) + exit code line; `Timeout` → the task
    ///    keeps running: output `Command still running after Ns; moved to background as
    ///    task <id>. Latest output:\n<tail>`, `ToolOutput.task = Some(id)`.
    ///    The core marks the task backgrounded when `task` is set on a foreground call.
    /// `yield_after` comes from `ctx` (`ToolCtx::exec_yield_after`, added below).
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<BashInput>(input)?;
        if args.command.trim().is_empty() {
            return Err(ToolError::invalid("command cannot be empty"));
        }

        let assessment = pacode_risk::classify(&args.command, &ctx.cwd);
        if ctx.mode == pacode_types::Mode::Plan && !pacode_risk::is_read_only(&args.command) {
            return Err(ToolError::Denied(
                "command is not read-only in Plan mode".to_string(),
            ));
        }

        let first_60: String = args.command.chars().take(60).collect();
        let perm_title = format!("Bash: {first_60}");
        let detail = if let Some(summary) = assessment.summary() {
            format!("{}\n\nRisk: {summary}", args.command)
        } else {
            args.command.clone()
        };
        ctx.require_permission(perm_title, detail, Some(assessment.level))
            .await?;

        ssh_multiplex::check_and_warn(&args.command, ctx.host.as_ref()).await;

        let label = args
            .label
            .clone()
            .unwrap_or_else(|| pacode_types::task_label_from_command(&args.command));
        let tool_title = format!("Bash {label}");

        let timeout = if args.background {
            None
        } else {
            args.timeout_secs
                .map(Duration::from_secs)
                .or(Some(ctx.exec_default_timeout))
        };

        let mut spec = pacode_exec::TaskSpec::new(
            ctx.session.clone(),
            ctx.agent.clone(),
            args.command.clone(),
            ctx.cwd.clone(),
        );
        spec.label = args.label.clone();
        spec.background = args.background;
        spec.timeout = timeout;

        let id = ctx.host.spawn_task(spec).await?;

        if args.background {
            let content = format!("task_id: {id}\nstatus: running\nTask started in background.");
            let preview = format!("task {id} running in background");
            return Ok(ToolOutput::text(content)
                .with_title(tool_title)
                .with_preview(preview)
                .with_task(id));
        }

        let outcome = ctx.host.wait_task(&id, ctx.exec_yield_after, false).await;
        match outcome {
            WaitOutcome::Finished => {
                let exit_code = ctx
                    .host
                    .task_info(&id)
                    .and_then(|i| i.exit_code)
                    .unwrap_or(0);
                let tail_lines = ctx.host.task_tail(&id, 400).await?;
                let joined = tail_lines.join("\n");
                let capped = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
                let mut content = capped;
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(&format!("[exit code {exit_code}]"));

                let preview = tail_lines
                    .iter()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .cloned()
                    .unwrap_or_default();

                let mut out = ToolOutput::text(content)
                    .with_title(tool_title)
                    .with_preview(preview);
                if exit_code != 0 {
                    out.is_error = true;
                }
                Ok(out)
            }
            WaitOutcome::Timeout | WaitOutcome::Progress => {
                let tail_lines = ctx.host.task_tail(&id, 40).await.unwrap_or_default();
                let tail_joined = tail_lines.join("\n");
                let secs = if ctx.exec_yield_after.subsec_millis() == 0 {
                    format!("{}s", ctx.exec_yield_after.as_secs())
                } else {
                    format!("{:.1}s", ctx.exec_yield_after.as_secs_f32())
                };
                let content = format!(
                    "Command still running after {secs}; moved to background as task {id}. Latest output:\n{tail_joined}"
                );
                let preview = tail_lines
                    .iter()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("task {id} moved to background"));

                Ok(ToolOutput::text(content)
                    .with_title(tool_title)
                    .with_preview(preview)
                    .with_task(id))
            }
            WaitOutcome::Cancelled => Err(ToolError::Cancelled),
        }
    }
}
